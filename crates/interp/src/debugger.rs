//! A Debug Adapter Protocol server on the interpreter. The adapter runs on the interpreter's
//! thread: it reads the client's requests through a [`Transport`] the host provides, runs the
//! program, and stops it at statements. While the program is stopped, the adapter answers
//! inspection requests (the stack, scopes, variables, `evaluate`) from the live frames, so no
//! interpreter state ever crosses a thread.
//!
//! The evaluator calls [`statement`] before each statement a `{` or a top-level loop runs, but
//! only while an adapter is attached, so a program run without one pays a single `Option` check
//! per statement. A statement without a srcref has no location and never stops. A statement is
//! located by its srcref: the srcfile's `filename` and the srcref's first line. The program
//! itself is parsed with one srcfile for the whole file, so its srcrefs carry the file's own line
//! numbers. Code with no file behind it, such as console input, is shown from the srcfile's
//! `lines`, which the client fetches with a `source` request.
//!
//! An adapter either launches a program (`ry debug-adapter`) or is attached to a running console
//! ([`crate::console_server`]). Attached, ending the session detaches it and the console runs on.

use {
    crate::{
        deparse, envir, eval, interp, lower, session,
        unwind::{Jump, R, Unwind},
        values::{self, global_env, length, nil, symbol_name, symbols},
    },
    heap::object::{Sexp, SexpType, gp},
    serde_json::{Map, Value, json},
    std::{
        collections::{BTreeMap, BTreeSet},
        path::{Path, PathBuf},
    },
};

/// The client's side of the conversation. Messages are DAP protocol objects, already parsed
/// from their wire framing.
pub trait Transport {
    /// The next message from the client. With `wait`, blocks until one arrives, and `None`
    /// means the client is gone; without, `None` means no message is waiting.
    fn receive(&mut self, wait: bool) -> Option<Value>;
    /// Sends a response or event. The transport gives it its `seq`, because the host also
    /// sends console output as `output` events that share the numbering.
    fn send(&mut self, message: Value);
}

/// What the client asked to run, from its `launch` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub stop_on_entry: bool,
    /// R code run before the program, such as `load_all()` for a package's tests. The adapter
    /// does not stop on entry in it.
    pub prelude: Option<String>,
    /// Runs only the `test_that()` block of this name, and the program's other top-level code.
    pub test_name: Option<String>,
}

/// The adapter's state. It lives in [`crate::Interp::debugger`] while the program runs, and the
/// hook takes it out while it handles a statement, so code the adapter evaluates for the client
/// never re-enters it.
pub struct Adapter {
    transport: Box<dyn Transport>,
    /// Breakpoint lines by canonical file path.
    breakpoints: BTreeMap<String, BTreeSet<u32>>,
    mode: Mode,
    pause_requested: bool,
    /// Whether an error no handler catches stops the program, from the client's `error`
    /// exception filter.
    break_on_error: bool,
    /// Set once the client asked to end the session; the program then unwinds to the top level
    /// and does not run another statement.
    terminating: bool,
    /// Canonical paths by the `filename` a srcfile records, so a statement costs no syscall.
    canonical: BTreeMap<String, Option<String>>,
    /// The srcfiles of code with no file behind it, preserved while the adapter lives; a
    /// `sourceReference` is an index into it plus one.
    texts: Vec<Sexp>,
    /// Attached to a console rather than running a launched program.
    console: bool,
    /// Set when the client of a console session left or ended it: the adapter is dropped and the
    /// console runs on without it.
    detached: bool,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        for &srcfile in &self.texts {
            crate::heap().release(srcfile);
        }
    }
}

/// Where a statement is: a file on disk, or the srcfile of code with no file behind it. A text
/// gets its `sourceReference` only when a stack trace shows it, because registering one preserves
/// it for the adapter's life, and the statement hook sees every command typed at a console.
#[derive(Clone, PartialEq, Eq)]
enum Source {
    File(String),
    Text(Sexp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Run,
    /// Stop at the very next statement.
    Entry,
    StepIn,
    /// Stop at the next statement at most this many frames deep.
    StepOver(usize),
    /// Stop at the next statement fewer than this many frames deep.
    StepOut(usize),
    /// A closure `debug()` flagged was entered at this depth: stop at its first statement, or
    /// give up once it returns without one.
    FunctionEntry(usize),
}

/// The program's state while it is stopped: the frames the client may name, innermost first,
/// and the values its variable references stand for.
struct Stopped {
    frames: Vec<StackFrame>,
    /// Each reference's object and the expression that evaluates to it in the selected frame,
    /// from which its children's `evaluateName`s are built.
    references: Vec<(Sexp, Option<String>)>,
}

struct StackFrame {
    name: String,
    env: Sexp,
    srcref: Sexp,
}

/// What a request does to a stopped program.
enum Resume {
    Stay,
    Continue(Mode),
    Terminate,
    /// The client of a console session left: the console runs on without the adapter.
    Detach,
}

/// Called by the evaluator before each statement with a srcref, while an adapter is attached.
/// An `Err` unwinds the program to the top level because the client ended the session.
pub unsafe fn statement() -> R<()> {
    with_adapter(|adapter| adapter.at_statement(None))
}

/// `browser()` with an adapter attached: stop where the call is.
pub unsafe fn browser() -> R<()> {
    with_adapter(|adapter| adapter.at_statement(Some("breakpoint")))
}

/// Called when a closure that `debug()` or `debugonce()` flagged is entered, `depth` frames
/// deep: the adapter stops at the first statement inside it.
pub unsafe fn debugged_call(depth: usize) {
    if let Some(adapter) = interp().debugger.as_mut() {
        adapter.mode = Mode::FunctionEntry(depth);
    }
}

/// Called by the default error handler while an adapter is attached, before the program
/// unwinds, so the client sees the frames the error was raised in. `text` is the error as R
/// prints it.
pub unsafe fn uncaught_error(text: &str) -> R<()> {
    with_adapter(|adapter| {
        if adapter.break_on_error {
            adapter.stop("exception", Some(text), interp().srcref);
        }
    })
}

/// Runs `body` on the attached adapter, taken out of the interpreter so that code it evaluates
/// for the client does not re-enter it.
unsafe fn with_adapter(body: impl FnOnce(&mut Adapter)) -> R<()> {
    let Some(mut adapter) = interp().debugger.take() else {
        return Ok(());
    };
    if !adapter.terminating && !adapter.detached {
        body(&mut adapter);
    }
    let terminating = adapter.terminating;
    match adapter.detached {
        // A console's command unwinds once when its client terminates it; the console then
        // runs on without the adapter.
        true => drop(adapter),
        false => interp().debugger = Some(adapter),
    }
    match terminating {
        true => Err(Unwind::Jump(Box::new(Jump::TopLevel))),
        false => Ok(()),
    }
}

impl Adapter {
    pub fn new(transport: Box<dyn Transport>) -> Adapter {
        Adapter {
            transport,
            breakpoints: BTreeMap::new(),
            mode: Mode::Run,
            pause_requested: false,
            break_on_error: false,
            terminating: false,
            canonical: BTreeMap::new(),
            texts: Vec::new(),
            console: false,
            detached: false,
        }
    }

    /// An adapter for a console session, which the client attaches to rather than launching a
    /// program.
    pub fn for_console(transport: Box<dyn Transport>) -> Adapter {
        let mut adapter = Adapter::new(transport);
        adapter.console = true;
        adapter
    }

    /// Whether the client of a console session left or ended it, so the adapter should go.
    pub fn is_detached(&self) -> bool {
        self.detached
    }

    pub fn is_terminating(&self) -> bool {
        self.terminating
    }

    /// Answers requests until the client has both launched and finished configuring
    /// (`configurationDone`), in either order. `None` means the client left, or ended the
    /// session, before that.
    pub fn configure(&mut self) -> Option<Launch> {
        let mut launch = None;
        let mut configured = false;
        loop {
            let message = self.transport.receive(true)?;
            let Some(request) = Request::of(&message) else {
                continue;
            };
            match request.command.as_str() {
                "initialize" => {
                    self.respond(&request, Ok(capabilities()));
                    self.event("initialized", json!({}));
                }
                "launch" => match launch_of(&request.arguments) {
                    Ok(requested) => {
                        launch = Some(requested);
                        self.respond(&request, Ok(json!({})));
                        if configured {
                            return launch;
                        }
                    }
                    Err(message) => self.respond(&request, Err(message)),
                },
                // The protocol lets a client finish configuring before it launches.
                "configurationDone" => {
                    self.respond(&request, Ok(json!({})));
                    configured = true;
                    if launch.is_some() {
                        return launch;
                    }
                }
                "disconnect" | "terminate" => {
                    self.respond(&request, Ok(json!({})));
                    return None;
                }
                _ => self.answer_anytime(&request),
            }
        }
    }

    /// Runs `launch.program` under the adapter and returns its exit status. The interpreter
    /// must already be running a session. After the program ends, the adapter keeps answering
    /// until the client disconnects.
    pub unsafe fn run(mut self: Box<Self>, launch: &Launch) -> i32 {
        let status = match std::fs::read_to_string(&launch.program) {
            Ok(text) => {
                let path = canonical_or_given(&launch.program);
                interp().debugger = Some(self);
                let status = run_program(&text, &path, launch);
                match interp().debugger.take() {
                    Some(adapter) => {
                        self = adapter;
                        status
                    }
                    None => panic!("the debug adapter left the interpreter while it ran"),
                }
            }
            Err(failure) => {
                let message = format!(
                    "cannot open file '{}': {failure}\n",
                    launch.program.display()
                );
                self.event("output", json!({ "category": "stderr", "output": message }));
                2
            }
        };
        self.event("exited", json!({ "exitCode": status }));
        self.event("terminated", json!({}));
        while !self.terminating {
            let Some(message) = self.transport.receive(true) else {
                break;
            };
            if let Some(request) = Request::of(&message) {
                self.answer_running(&request);
            }
        }
        status
    }

    /// Ends a session whose program could not run: tells the client why, and that it ended.
    pub fn fail(&mut self, message: &str) {
        self.event(
            "output",
            json!({ "category": "stderr", "output": format!("{message}\n") }),
        );
        self.event("exited", json!({ "exitCode": 2 }));
        self.event("terminated", json!({}));
    }

    unsafe fn at_statement(&mut self, forced: Option<&'static str>) {
        while let Some(message) = self.transport.receive(false) {
            if let Some(request) = Request::of(&message) {
                self.answer_running(&request);
            }
        }
        if self.terminating {
            return;
        }
        let depth = interp().frames.len();
        if let Mode::FunctionEntry(limit) = self.mode
            && depth < limit
        {
            self.mode = Mode::Run;
        }
        let srcref = interp().srcref;
        let Some((source, line, _)) = self.location(srcref) else {
            return;
        };
        let at_breakpoint = match &source {
            Source::File(path) => self
                .breakpoints
                .get(path)
                .is_some_and(|lines| lines.contains(&line)),
            Source::Text(_) => false,
        };
        let reason = forced.or_else(|| match () {
            _ if self.pause_requested => Some("pause"),
            _ if at_breakpoint => Some("breakpoint"),
            _ => match self.mode {
                Mode::Run => None,
                Mode::Entry => Some("entry"),
                Mode::StepIn => Some("step"),
                Mode::StepOver(limit) => (depth <= limit).then_some("step"),
                Mode::StepOut(limit) => (depth < limit).then_some("step"),
                Mode::FunctionEntry(_) => Some("function breakpoint"),
            },
        });
        if let Some(reason) = reason {
            self.stop(reason, None, srcref);
        }
    }

    unsafe fn stop(&mut self, reason: &str, text: Option<&str>, srcref: Sexp) {
        self.pause_requested = false;
        self.mode = Mode::Run;
        let heap = crate::heap();
        let mark = heap.handle_mark();
        let mut frames = stack(srcref);
        // An error stops inside base R's own `stop` or `signalCondition`, which have no source
        // to show; the client lands on the innermost frame it can show instead.
        let hidden = frames
            .iter()
            .take(frames.len().saturating_sub(1))
            .take_while(|frame| self.location(frame.srcref).is_none())
            .count();
        frames.drain(..hidden);
        let mut stopped = Stopped {
            frames,
            references: Vec::new(),
        };
        let mut body =
            json!({ "reason": reason, "threadId": THREAD_ID, "allThreadsStopped": true });
        if let (Some(text), Some(object)) = (text, body.as_object_mut()) {
            object.insert("description".to_owned(), json!("Error"));
            object.insert("text".to_owned(), json!(text.trim_end()));
        }
        self.event("stopped", body);
        loop {
            let Some(message) = self.transport.receive(true) else {
                match self.console {
                    true => self.detached = true,
                    false => self.terminating = true,
                }
                break;
            };
            let Some(request) = Request::of(&message) else {
                continue;
            };
            match self.answer_stopped(&request, &mut stopped) {
                Resume::Stay => {}
                Resume::Continue(mode) => {
                    self.mode = mode;
                    break;
                }
                Resume::Terminate => {
                    self.terminating = true;
                    self.detached = self.console;
                    break;
                }
                Resume::Detach => {
                    self.detached = true;
                    break;
                }
            }
        }
        heap.release_handles(mark);
    }

    unsafe fn answer_stopped(&mut self, request: &Request, stopped: &mut Stopped) -> Resume {
        let depth = interp().frames.len();
        let resume = |mode| Resume::Continue(mode);
        match request.command.as_str() {
            "continue" => {
                self.respond(request, Ok(json!({ "allThreadsContinued": true })));
                resume(Mode::Run)
            }
            "next" => {
                self.respond(request, Ok(json!({})));
                resume(Mode::StepOver(depth))
            }
            "stepIn" => {
                self.respond(request, Ok(json!({})));
                resume(Mode::StepIn)
            }
            "stepOut" => {
                self.respond(request, Ok(json!({})));
                resume(Mode::StepOut(depth))
            }
            "stackTrace" => {
                let body = self.stack_trace(&request.arguments, stopped);
                self.respond(request, Ok(body));
                Resume::Stay
            }
            "scopes" => {
                let body = scopes(&request.arguments, stopped);
                self.respond(request, body);
                Resume::Stay
            }
            "variables" => {
                let body = variables(&request.arguments, stopped);
                self.respond(request, body);
                Resume::Stay
            }
            "evaluate" => {
                let body = evaluate(&request.arguments, stopped);
                self.respond(request, body);
                Resume::Stay
            }
            "ryView" => {
                let body = view_in_frame(&request.arguments, stopped);
                self.respond(request, body);
                Resume::Stay
            }
            "pause" => {
                self.respond(request, Ok(json!({})));
                Resume::Stay
            }
            "disconnect" if self.console => {
                self.respond(request, Ok(json!({})));
                Resume::Detach
            }
            "disconnect" | "terminate" => {
                self.respond(request, Ok(json!({})));
                if request.command == "terminate" && self.console {
                    self.event("terminated", json!({}));
                }
                Resume::Terminate
            }
            _ => {
                self.answer_anytime(request);
                Resume::Stay
            }
        }
    }

    /// A request that arrives while the program runs, or after it ended, or while an attached
    /// console waits for input.
    pub(crate) fn answer_running(&mut self, request: &Request) {
        match request.command.as_str() {
            "pause" => {
                self.pause_requested = true;
                self.respond(request, Ok(json!({})));
            }
            "disconnect" if self.console => {
                self.detached = true;
                self.respond(request, Ok(json!({})));
            }
            "terminate" if self.console => {
                self.detached = true;
                self.terminating = true;
                self.respond(request, Ok(json!({})));
                self.event("terminated", json!({}));
            }
            "disconnect" | "terminate" => {
                self.terminating = true;
                self.respond(request, Ok(json!({})));
            }
            "continue" => self.respond(request, Ok(json!({ "allThreadsContinued": true }))),
            "next" | "stepIn" | "stepOut" | "stackTrace" | "scopes" | "variables" | "evaluate"
            | "ryView" => self.respond(request, Err("the program is not stopped".to_owned())),
            _ => self.answer_anytime(request),
        }
    }

    /// Requests whose answer does not depend on whether the program runs.
    fn answer_anytime(&mut self, request: &Request) {
        match request.command.as_str() {
            // A launched program answers these in `configure`; an attached console here.
            "initialize" => {
                self.respond(request, Ok(capabilities()));
                self.event("initialized", json!({}));
            }
            "attach" | "configurationDone" => self.respond(request, Ok(json!({}))),
            "source" => {
                let body = self.source(&request.arguments);
                self.respond(request, body);
            }
            "ryTable" | "ryRelease" => self.respond(request, data_request(request)),
            "threads" => self.respond(
                request,
                Ok(json!({ "threads": [{ "id": THREAD_ID, "name": "R" }] })),
            ),
            "setBreakpoints" => {
                let body = self.set_breakpoints(&request.arguments);
                self.respond(request, body);
            }
            "setExceptionBreakpoints" => {
                let filters = request.arguments.get("filters").and_then(Value::as_array);
                self.break_on_error =
                    filters.is_some_and(|filters| filters.iter().any(|filter| filter == "error"));
                let verified = filters.map_or(0, Vec::len);
                let breakpoints = vec![json!({ "verified": true }); verified];
                self.respond(request, Ok(json!({ "breakpoints": breakpoints })))
            }
            command => self.respond(request, Err(format!("`{command}` is not supported"))),
        }
    }

    fn set_breakpoints(&mut self, arguments: &Value) -> Result<Value, String> {
        let path = arguments
            .pointer("/source/path")
            .and_then(Value::as_str)
            .ok_or("setBreakpoints needs `source.path`")?;
        let lines: BTreeSet<u32> = arguments
            .get("breakpoints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|breakpoint| breakpoint.get("line").and_then(Value::as_u64))
            .filter_map(|line| u32::try_from(line).ok())
            .collect();
        let breakpoints: Vec<Value> = lines
            .iter()
            .map(|line| json!({ "verified": true, "line": line }))
            .collect();
        self.breakpoints
            .insert(canonical_or_given(Path::new(path)), lines);
        Ok(json!({ "breakpoints": breakpoints }))
    }

    /// The text of a source with no file behind it.
    fn source(&self, arguments: &Value) -> Result<Value, String> {
        let reference = argument_index(arguments, "sourceReference")
            .or_else(|| {
                arguments
                    .pointer("/source/sourceReference")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
            })
            .ok_or("source needs a `sourceReference`")?;
        let srcfile = reference
            .checked_sub(1)
            .and_then(|index| self.texts.get(index))
            .ok_or("no such source")?;
        // SAFETY: the srcfile is preserved while the adapter lives, on the interpreter's thread.
        let lines = unsafe { srcfile_lines(*srcfile) }.ok_or("the source has no text")?;
        Ok(json!({ "content": lines.join("\n") }))
    }

    unsafe fn stack_trace(&mut self, arguments: &Value, stopped: &Stopped) -> Value {
        let start = argument_index(arguments, "startFrame").unwrap_or(0);
        let levels = argument_index(arguments, "levels")
            .filter(|levels| *levels > 0)
            .unwrap_or(usize::MAX);
        let frames: Vec<Value> = stopped
            .frames
            .iter()
            .enumerate()
            .skip(start)
            .take(levels)
            .map(|(index, frame)| {
                let mut object = Map::new();
                object.insert("id".to_owned(), json!(index + 1));
                object.insert("name".to_owned(), json!(frame.name));
                match self.location(frame.srcref) {
                    Some((source, line, column)) => {
                        let source = match source {
                            Source::File(path) => {
                                let name = Path::new(&path)
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.clone());
                                json!({ "name": name, "path": path })
                            }
                            Source::Text(srcfile) => match self.text_index(srcfile) {
                                Some(index) => json!({
                                    "name": self.text_name(index),
                                    "sourceReference": index + 1,
                                    "presentationHint": "deemphasize",
                                }),
                                None => json!({ "name": "<console>" }),
                            },
                        };
                        object.insert("source".to_owned(), source);
                        object.insert("line".to_owned(), json!(line));
                        object.insert("column".to_owned(), json!(column));
                    }
                    None => {
                        object.insert("line".to_owned(), json!(0));
                        object.insert("column".to_owned(), json!(0));
                        object.insert("presentationHint".to_owned(), json!("subtle"));
                    }
                }
                Value::Object(object)
            })
            .collect();
        json!({ "stackFrames": frames, "totalFrames": stopped.frames.len() })
    }

    /// The source, line and column of the statement `srcref` covers.
    unsafe fn location(&mut self, srcref: Sexp) -> Option<(Source, u32, u32)> {
        if srcref.kind() != SexpType::Integer || length(srcref) < 6 {
            return None;
        }
        let srcfile = values::get_attr(srcref, symbols().srcfile);
        if srcfile.kind() != SexpType::Environment {
            return None;
        }
        let filename = values::install("filename").ok()?;
        let cell = crate::heap().frame_cell(srcfile, filename)?;
        let name = values::as_string_scalar(cell.cdr()).ok()??;
        let path = match name.is_empty() {
            true => None,
            false => self
                .canonical
                .entry(name)
                .or_insert_with_key(|name| {
                    let path = Path::new(name);
                    path.is_file().then(|| canonical_or_given(path))
                })
                .clone(),
        };
        let source = match path {
            Some(path) => Source::File(path),
            None => Source::Text(srcfile),
        };
        let positions = srcref.slice::<i32>();
        let line = u32::try_from(*positions.first()?).ok()?;
        let column = u32::try_from(*positions.get(4)?).ok()?;
        Some((source, line, column))
    }

    /// The index of a srcfile with no file behind it, which must hold its `lines` to be shown.
    unsafe fn text_index(&mut self, srcfile: Sexp) -> Option<usize> {
        if let Some(index) = self.texts.iter().position(|&text| text == srcfile) {
            return Some(index);
        }
        srcfile_lines(srcfile)?;
        crate::heap().preserve(srcfile);
        self.texts.push(srcfile);
        Some(self.texts.len() - 1)
    }

    /// The name the client shows for a text source: its srcfile's `filename`, or `<console>`
    /// for console input, which has none.
    unsafe fn text_name(&self, index: usize) -> String {
        let name = self.texts.get(index).and_then(|&srcfile| {
            let filename = values::install("filename").ok()?;
            let cell = crate::heap().frame_cell(srcfile, filename)?;
            values::as_string_scalar(cell.cdr()).ok()?
        });
        match name {
            Some(name) if !name.is_empty() => name,
            _ => "<console>".to_owned(),
        }
    }

    fn respond(&mut self, request: &Request, outcome: Result<Value, String>) {
        self.transport.send(response(request, outcome));
    }

    fn event(&mut self, event: &str, body: Value) {
        self.transport
            .send(json!({ "type": "event", "event": event, "body": body }));
    }
}

/// R runs on one thread, which the client sees as the only one.
const THREAD_ID: u64 = 1;

/// Variables of a long vector or list beyond this many are not listed.
const MAX_CHILDREN: usize = 1000;

pub(crate) struct Request {
    pub(crate) seq: u64,
    pub(crate) command: String,
    pub(crate) arguments: Value,
}

impl Request {
    pub(crate) fn of(message: &Value) -> Option<Request> {
        if message.get("type").and_then(Value::as_str) != Some("request") {
            return None;
        }
        Some(Request {
            seq: message.get("seq").and_then(Value::as_u64).unwrap_or(0),
            command: message.get("command")?.as_str()?.to_owned(),
            arguments: message.get("arguments").cloned().unwrap_or(Value::Null),
        })
    }
}

/// The response to `request`: its body, or the message it failed with.
pub(crate) fn response(request: &Request, outcome: Result<Value, String>) -> Value {
    match outcome {
        Ok(body) => json!({
            "type": "response",
            "request_seq": request.seq,
            "success": true,
            "command": request.command,
            "body": body,
        }),
        Err(message) => json!({
            "type": "response",
            "request_seq": request.seq,
            "success": false,
            "command": request.command,
            "message": message,
        }),
    }
}

/// A srcfile's `lines`, when it holds them.
unsafe fn srcfile_lines(srcfile: Sexp) -> Option<Vec<String>> {
    let lines = crate::heap().frame_cell(srcfile, values::install("lines").ok()?)?;
    let lines = lines.cdr();
    if lines.kind() != SexpType::String || length(lines) == 0 {
        return None;
    }
    Some(
        (0..length(lines))
            .map(|index| values::char_str(values::string_elt(lines, index)).into_owned())
            .collect(),
    )
}

fn capabilities() -> Value {
    json!({
        "supportsConfigurationDoneRequest": true,
        "supportsEvaluateForHovers": true,
        "supportsTerminateRequest": true,
        "exceptionBreakpointFilters": [{
            "filter": "error",
            "label": "Uncaught errors",
            "description": "Stop where an error is raised that no handler catches",
            "default": true,
        }],
    })
}

fn launch_of(arguments: &Value) -> Result<Launch, String> {
    let program = arguments
        .get("program")
        .and_then(Value::as_str)
        .ok_or("launch needs a `program` to run")?;
    let args = arguments
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let stop_on_entry = arguments
        .get("stopOnEntry")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Ok(Launch {
        program: PathBuf::from(program),
        args,
        stop_on_entry,
        prelude: text("prelude"),
        test_name: text("testName"),
    })
}

fn argument_index(arguments: &Value, name: &str) -> Option<usize> {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn canonical_or_given(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Runs the launch's prelude, then the program's top-level expressions as `Rscript` does: each
/// one is its own top-level call, and the first error stops the program unless
/// `options(error =)` holds a handler.
unsafe fn run_program(text: &str, path: &str, launch: &Launch) -> i32 {
    let mark = crate::heap().handle_mark();
    let prelude = match &launch.prelude {
        Some(prelude) => lower::parse_text(prelude, "<prelude>"),
        None => Ok(nil()),
    };
    let status = match prelude.and_then(|prelude| {
        crate::heap().root(prelude);
        Ok((prelude, program_expressions(text, path)?))
    }) {
        Ok((prelude, program)) => {
            crate::heap().root(program);
            match run_expressions(prelude, None) {
                Some(0) => {
                    if launch.stop_on_entry
                        && let Some(adapter) = interp().debugger.as_mut()
                    {
                        adapter.mode = Mode::Entry;
                    }
                    run_expressions(program, launch.test_name.as_deref()).unwrap_or(0)
                }
                status => status.unwrap_or(0),
            }
        }
        Err(unwind) => {
            session::report_toplevel(unwind);
            1
        }
    };
    interp().srcref = nil();
    crate::heap().release_handles(mark);
    status
}

/// Runs each expression as a top-level call and returns the status so far: 1 after an error
/// that halts, and `None` when the client ended the session.
unsafe fn run_expressions(expressions: Sexp, test_name: Option<&str>) -> Option<i32> {
    if expressions == nil() {
        return Some(0);
    }
    let srcrefs = values::get_attr(expressions, symbols().srcref);
    for index in 0..length(expressions) {
        let expression = expressions.element(index);
        if test_name.is_some_and(|name| is_other_test(expression, name)) {
            continue;
        }
        interp().srcref = eval::statement_srcref(srcrefs, index);
        let succeeded = statement().is_ok() && session::run_toplevel(expression);
        if interp()
            .debugger
            .as_ref()
            .is_some_and(|adapter| adapter.terminating)
        {
            return None;
        }
        if !succeeded && envir::get_option("error") == nil() {
            crate::write_console(b"Execution halted\n", true);
            return Some(1);
        }
    }
    Some(0)
}

/// Whether `expression` is a `test_that()` call for a test other than `name`.
unsafe fn is_other_test(expression: Sexp, name: &str) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let function = expression.car();
    let callee = match function.kind() {
        SexpType::Symbol => symbol_name(function).into_owned(),
        // `testthat::test_that`
        SexpType::Language if length(function) == 3 => {
            let name = function.cdr().cdr().car();
            match name.kind() {
                SexpType::Symbol => symbol_name(name).into_owned(),
                _ => return false,
            }
        }
        _ => return false,
    };
    if callee != "test_that" {
        return false;
    }
    let description = expression.cdr().car();
    if description.kind() != SexpType::String {
        return false;
    }
    match values::as_string_scalar(description) {
        Ok(Some(description)) => description != name,
        _ => false,
    }
}

/// The program parsed against a srcfile that names it, so every srcref carries its path and the
/// file's own line numbers.
unsafe fn program_expressions(text: &str, path: &str) -> R<Sexp> {
    let srcfile = lower::file_srcfile(path, text)?;
    lower::parse_with_srcfile(text, path, srcfile)
}

/// The frames of the stopped program, innermost first, ending with the top level. Each closure
/// frame is at the srcref its callee was called from, and the innermost at `srcref`.
unsafe fn stack(srcref: Sexp) -> Vec<StackFrame> {
    let frames = &interp().frames;
    let mut stack = Vec::with_capacity(frames.len() + 1);
    let mut current = srcref;
    for frame in frames.iter().rev() {
        stack.push(StackFrame {
            name: deparse::deparse_first_line(frame.call.car()),
            env: frame.env,
            srcref: current,
        });
        current = frame.srcref;
    }
    stack.push(StackFrame {
        name: "<top level>".to_owned(),
        env: global_env(),
        srcref: current,
    });
    stack
}

fn frame_of<'a>(arguments: &Value, stopped: &'a Stopped) -> Result<&'a StackFrame, String> {
    argument_index(arguments, "frameId")
        .and_then(|id| id.checked_sub(1))
        .and_then(|index| stopped.frames.get(index))
        .ok_or_else(|| "no such frame".to_owned())
}

unsafe fn scopes(arguments: &Value, stopped: &mut Stopped) -> Result<Value, String> {
    let env = frame_of(arguments, stopped)?.env;
    let mut scopes = vec![json!({
        "name": "Locals",
        "presentationHint": "locals",
        "variablesReference": reference(stopped, env, Some(String::new())),
        "expensive": false,
    })];
    if env != global_env() {
        scopes.push(json!({
            "name": "Global",
            "variablesReference": reference(stopped, global_env(), Some(".GlobalEnv".to_owned())),
            "expensive": false,
        }));
    }
    Ok(json!({ "scopes": scopes }))
}

/// The object is rooted until the program resumes (`stop` releases its handles), because code
/// evaluated in the debug console can drop the last other reference to it.
/// `path` is the expression for the object, empty for the selected frame's own environment.
unsafe fn reference(stopped: &mut Stopped, object: Sexp, path: Option<String>) -> usize {
    crate::heap().root(object);
    stopped.references.push((object, path));
    stopped.references.len()
}

unsafe fn variables(arguments: &Value, stopped: &mut Stopped) -> Result<Value, String> {
    let object = argument_index(arguments, "variablesReference")
        .and_then(|reference| reference.checked_sub(1))
        .and_then(|index| stopped.references.get(index).cloned())
        .ok_or("no such variable reference")?;
    let (object, path) = object;
    let mut variables = Vec::new();
    if object.kind() == SexpType::Environment {
        for symbol in envir::env_symbols(object, false, true) {
            let Some(cell) = crate::heap().frame_cell(object, symbol) else {
                continue;
            };
            let name = symbol_name(symbol).into_owned();
            let value = if cell.has_gp(gp::ACTIVE_BINDING) {
                variable_text(&name, "<active binding>", "active binding")
            } else {
                // `...` and `..1` are not values an expression can name on their own.
                let dots = name == "..."
                    || name.strip_prefix("..").is_some_and(|rest| {
                        !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit())
                    });
                let path = path.as_deref().filter(|_| !dots).map(|path| match path {
                    "" => quoted_name(&name),
                    path => format!("{path}${}", quoted_name(&name)),
                });
                variable(stopped, &name, cell.cdr(), path)
            };
            variables.push(value);
        }
    } else if object.kind() == SexpType::List {
        let names = values::names(object);
        for index in 0..length(object).min(MAX_CHILDREN) {
            let element = element_name(names, index);
            let path = path.as_deref().map(|path| match &element {
                Some(name) => format!("{path}[[{}]]", string_literal(name)),
                None => format!("{path}[[{}]]", index + 1),
            });
            let name = element.unwrap_or_else(|| format!("[[{}]]", index + 1));
            variables.push(variable(stopped, &name, object.element(index), path));
        }
    }
    Ok(json!({ "variables": variables }))
}

unsafe fn element_name(names: Sexp, index: usize) -> Option<String> {
    if names.kind() != SexpType::String || index >= length(names) {
        return None;
    }
    let name = values::char_str(values::string_elt(names, index)).into_owned();
    (!name.is_empty() && name != "NA").then_some(name)
}

/// A name as R code: backquoted unless it is syntactic.
fn quoted_name(name: &str) -> String {
    let mut characters = name.chars();
    let syntactic = match characters.next() {
        Some('.') => !name[1..].starts_with(|character: char| character.is_ascii_digit()),
        Some(first) => first.is_alphabetic(),
        None => false,
    } && name
        .chars()
        .all(|character| character.is_alphanumeric() || character == '.' || character == '_')
        && !matches!(
            name,
            "if" | "else"
                | "repeat"
                | "while"
                | "function"
                | "for"
                | "next"
                | "break"
                | "TRUE"
                | "FALSE"
                | "NULL"
                | "Inf"
                | "NaN"
                | "NA"
                | "NA_integer_"
                | "NA_real_"
                | "NA_character_"
                | "NA_complex_"
                | "in"
        );
    match syntactic {
        true => name.to_owned(),
        false => format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`")),
    }
}

fn string_literal(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
}

/// `path` is the expression that evaluates to the value in the selected frame, sent as the
/// variable's `evaluateName` so the client can watch it or open it in the data viewer.
unsafe fn variable(stopped: &mut Stopped, name: &str, value: Sexp, path: Option<String>) -> Value {
    // An unforced promise is a lazy argument; forcing it to show it would run its code.
    let value = match value.kind() {
        SexpType::Promise if value.car() == values::unbound() => {
            return variable_text(name, "<not yet evaluated>", "promise");
        }
        SexpType::Promise => value.car(),
        _ => value,
    };
    let expandable = match value.kind() {
        SexpType::Environment => true,
        SexpType::List => length(value) > 0,
        _ => false,
    };
    let reference = match expandable {
        true => reference(stopped, value, path.clone()),
        false => 0,
    };
    let mut variable = json!({
        "name": name,
        "value": summary(value),
        "type": crate::coerce::type_name(value),
        "variablesReference": reference,
    });
    if let Some(path) = path {
        variable["evaluateName"] = Value::String(path);
    }
    variable
}

fn variable_text(name: &str, value: &str, kind: &str) -> Value {
    json!({ "name": name, "value": value, "type": kind, "variablesReference": 0 })
}

/// One line that shows a value. A long vector is shown by its type and length, because
/// deparsing a million elements to show sixty characters of them would stall the client.
pub(crate) unsafe fn summary(value: Sexp) -> String {
    const LONGEST: usize = 200;
    match value.kind() {
        SexpType::Environment => {
            return match envir::namespace_name(value) {
                Some(name) => format!("<environment: namespace:{name}>"),
                None if value == global_env() => "<environment: R_GlobalEnv>".to_owned(),
                None => "<environment>".to_owned(),
            };
        }
        kind if (kind.is_atomic() || kind == SexpType::List) && !is_small(value, &mut 20) => {
            return format!("{}[{}]", crate::coerce::type_name(value), length(value));
        }
        _ => {}
    }
    // Shown as a reader expects a value rather than as code that recreates it: `1`, not `1L`.
    let options = deparse::options::NICE_NAMES | deparse::options::USE_SOURCE;
    let text = deparse::deparse_lines(value, 60, options)
        .into_iter()
        .next()
        .unwrap_or_default();
    match text.char_indices().nth(LONGEST) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text,
    }
}

/// Whether `value` holds at most `budget` elements, counting into nested lists, so a data frame
/// of long columns is summarised rather than deparsed.
unsafe fn is_small(value: Sexp, budget: &mut usize) -> bool {
    let count = length(value);
    if count > *budget {
        return false;
    }
    *budget -= count;
    value.kind() != SexpType::List
        || (0..count).all(|index| {
            let element = value.element(index);
            !(element.kind().is_atomic() || element.kind() == SexpType::List)
                || is_small(element, budget)
        })
}

/// `evaluate`: runs the expression in the selected frame, or at the top level, and shows the
/// value. Errors come back as a failed response rather than console output.
unsafe fn evaluate(arguments: &Value, stopped: &mut Stopped) -> Result<Value, String> {
    let value = evaluate_in_frame(arguments, stopped)?;
    let reference = match value.kind() {
        SexpType::Environment => reference(stopped, value, None),
        SexpType::List if length(value) > 0 => reference(stopped, value, None),
        _ => 0,
    };
    Ok(json!({ "result": summary(value), "variablesReference": reference }))
}

/// `ryView`: opens a data viewer on the value of an expression in the selected frame.
unsafe fn view_in_frame(arguments: &Value, stopped: &mut Stopped) -> Result<Value, String> {
    let value = evaluate_in_frame(arguments, stopped)?;
    let title = arguments
        .get("expression")
        .and_then(Value::as_str)
        .unwrap_or_default();
    crate::viewer::open(value, title)
}

/// Answers `ryTable` and `ryRelease`, which page through an open view whether or not the program
/// is stopped.
fn data_request(request: &Request) -> Result<Value, String> {
    // SAFETY: requests are answered on the interpreter's thread.
    unsafe { crate::viewer::request(&request.command, &request.arguments) }
}

/// The value of the request's `expression` in its `frameId`, or at the top level. The value is
/// rooted until the program resumes.
unsafe fn evaluate_in_frame(arguments: &Value, stopped: &mut Stopped) -> Result<Sexp, String> {
    let expression = arguments
        .get("expression")
        .and_then(Value::as_str)
        .ok_or("the request needs an `expression`")?;
    let env = match arguments.get("frameId") {
        Some(_) => frame_of(arguments, stopped)?.env,
        None => global_env(),
    };
    evaluate_text(expression, env)
}

/// Evaluates `expression` in `env` for the client: an error is returned rather than written to
/// the console, and the interpreter's error state is restored afterwards.
pub(crate) unsafe fn evaluate_text(expression: &str, env: Sexp) -> Result<Sexp, String> {
    let state = interp();
    let shown = std::mem::replace(&mut state.show_error_messages, false);
    // Stopped on an uncaught error, the program is still inside its error handling, where a
    // nested error writes to the console; an evaluation's error belongs in its response only.
    let in_error = std::mem::replace(&mut state.in_error, 0);
    let srcref = state.srcref;
    let result = (|| {
        let expressions = lower::parse_text(expression, "<debug console>")?;
        crate::heap().root(expressions);
        let mut value = nil();
        for index in 0..length(expressions) {
            value = eval::eval(expressions.element(index), env)?;
        }
        crate::heap().root(value);
        Ok(value)
    })();
    let state = interp();
    state.show_error_messages = shown;
    state.in_error = in_error;
    state.srcref = srcref;
    match result {
        Ok(value) => Ok(value),
        Err(Unwind::Error(failure)) => Err(failure.message),
        Err(Unwind::Interrupt) => Err("interrupted".to_owned()),
        // An error no handler caught ran the default handler, which records the message and
        // jumps to the top level; the program itself does not unwind.
        Err(Unwind::Jump(jump)) if matches!(*jump, Jump::TopLevel) => {
            Err(interp().error_message.trim_end().to_owned())
        }
        Err(Unwind::Jump(_)) => Err("the expression jumped out of the debugger".to_owned()),
    }
}
