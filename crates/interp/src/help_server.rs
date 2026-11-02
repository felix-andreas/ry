//! R's dynamic help server: the HTTP server behind `tools::startDynamicHelp()`, which serves
//! `help(help_type = "html")` pages, `help.start()` and any handler a package registers in
//! `tools:::.httpd.handlers.env`.
//!
//! GNU R's server (`Rhttpd.c`) only answers while R waits in its event loop: at the console
//! prompt, in `Sys.sleep()` and in `process.events()`. ry keeps that contract. Socket threads
//! accept connections and parse requests, then queue each one and wait. The interpreter's thread
//! answers queued requests at those same points by calling the R handler, so R code never runs
//! off its thread and never interrupts the user's own code.
//!
//! The wire format follows GNU R's server, which was probed request by request: the path is
//! percent-decoded, the query becomes a named character vector, the handler gets
//! `(path, query, body, headers)`, and its result is `list(payload, content-type, headers,
//! status)` or `list(file = path, content-type)`. A character result is taken as an error message
//! and answered with status 500, which is how a `try()` failure reaches the browser.

use {
    crate::{
        eval, interp, lower,
        unwind::{R, Unwind},
        values::{self, alloc, base_env, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType},
    std::{
        collections::{HashMap, VecDeque},
        io::{BufRead, BufReader, Read, Write},
        net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
        sync::{
            Condvar, Mutex, MutexGuard, PoisonError,
            mpsc::{SyncSender, sync_channel},
        },
        time::Duration,
    },
};

/// The outcome codes `startHTTPD` returns to `startDynamicHelp()`: it tries the next port only
/// after [`PORT_IN_USE`].
pub const STARTED: i32 = 0;
pub const FAILED: i32 = -1;
pub const PORT_IN_USE: i32 = -2;

/// Starts serving on `ip:port`. Returns [`STARTED`], [`PORT_IN_USE`] when the port is taken, or
/// [`FAILED`] for any other failure, including a server that is already running.
pub fn start(ip: &str, port: u16) -> i32 {
    let mut state = lock(&STATE);
    if state.server.is_some() {
        return FAILED;
    }
    let listener = match TcpListener::bind((ip, port)) {
        Ok(listener) => listener,
        Err(failure) if failure.kind() == std::io::ErrorKind::AddrInUse => return PORT_IN_USE,
        Err(_) => return FAILED,
    };
    let Ok(address) = listener.local_addr() else {
        return FAILED;
    };
    state.generation += 1;
    let generation = state.generation;
    let accepting = std::thread::Builder::new()
        .name("ry-help-accept".to_owned())
        .spawn(move || accept(listener, generation));
    if accepting.is_err() {
        return FAILED;
    }
    state.server = Some(Server {
        address,
        connections: HashMap::new(),
        next_connection: 0,
    });
    STARTED
}

/// Stops accepting connections, closes every open one, and drops the requests still queued.
pub fn stop() {
    let server = {
        let mut state = lock(&STATE);
        state.queue.clear();
        state.server.take()
    };
    let Some(server) = server else {
        return;
    };
    for stream in server.connections.values() {
        stream.shutdown(Shutdown::Both).ok();
    }
    // `accept` blocks, so a connection of our own wakes it to see that its generation ended. A
    // listener on every address is reached through loopback. A failure means it is already gone.
    let mut wake = server.address;
    if wake.ip().is_unspecified() {
        wake.set_ip(match wake {
            SocketAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    TcpStream::connect_timeout(&wake, Duration::from_secs(1)).ok();
}

/// Whether the server is running, so the console knows to answer requests while it waits.
pub fn running() -> bool {
    lock(&STATE).server.is_some()
}

/// Answers every queued request. A Ctrl-C while a handler runs answers that request with an
/// error and is passed on once the queue is empty, so the code that was waiting stops too.
///
/// # Safety
/// Called on the interpreter's thread, at a point where R code may run: the console's prompt,
/// `Sys.sleep()`, `process.events()` or an HTTP transfer.
pub unsafe fn serve_pending() -> R<()> {
    let mut interrupted = false;
    loop {
        let Some(exchange) = lock(&STATE).queue.pop_front() else {
            break;
        };
        let mark = crate::heap().handle_mark();
        let (response, stopped) = unsafe { answer(&exchange.request) };
        crate::heap().release_handles(mark);
        interrupted |= stopped;
        // The connection may have closed meanwhile; then nobody wants the answer.
        exchange.reply.send(response).ok();
    }
    if interrupted {
        return Err(Unwind::Interrupt);
    }
    Ok(())
}

/// Waits up to `timeout` for a request and answers what arrived; without a running server it
/// only sleeps. `Sys.sleep()` waits in slices of this.
///
/// # Safety
/// As for [`serve_pending`].
pub unsafe fn wait_and_serve(timeout: Duration) -> R<()> {
    {
        let state = lock(&STATE);
        if state.server.is_none() {
            drop(state);
            std::thread::sleep(timeout);
            return Ok(());
        }
        if state.queue.is_empty() {
            // A spurious wake-up only ends this slice early.
            drop(
                ARRIVED
                    .wait_timeout(state, timeout)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }
    unsafe { serve_pending() }
}

/// The server and the requests waiting for the interpreter share one lock, so a request is
/// queued only while the server that accepted it still runs.
struct State {
    server: Option<Server>,
    /// Counts started servers; an accept thread whose server is no longer the current one ends.
    generation: u64,
    queue: VecDeque<Exchange>,
}

struct Server {
    address: SocketAddr,
    /// Clones of the open connections, which [`stop`] shuts down to end their threads.
    connections: HashMap<u64, TcpStream>,
    next_connection: u64,
}

/// A request waiting for the interpreter, and where its response goes.
struct Exchange {
    request: Request,
    reply: SyncSender<Response>,
}

struct Request {
    method: String,
    path: String,
    /// `None` without a `?` in the target; otherwise every `&`-separated field, empty ones too.
    query: Option<Vec<(String, String)>>,
    /// `Request-Method: GET` followed by the header lines, each ending in a newline.
    headers: Vec<u8>,
    body: Option<(Vec<u8>, Option<String>)>,
}

struct Response {
    status: u16,
    reason: String,
    content_type: String,
    headers: Vec<String>,
    body: Vec<u8>,
    /// GNU R closes the connection after an error and sends no length.
    close: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    server: None,
    generation: 0,
    queue: VecDeque::new(),
});
static ARRIVED: Condvar = Condvar::new();

/// A request body larger than this is refused; help pages and handlers take small forms.
const MAX_BODY: u64 = 64 << 20;
/// The request line and headers together may not exceed this. GNU R's server sets no limit, and
/// takes a header line of 70 kB.
const MAX_HEAD: u64 = 1 << 20;
/// A connection that sends nothing for this long is closed, so an idle client cannot hold a
/// thread forever.
const IDLE: Duration = Duration::from_secs(120);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept(listener: TcpListener, generation: u64) {
    for stream in listener.incoming() {
        let id = {
            let mut state = lock(&STATE);
            if state.generation != generation {
                return;
            }
            let Some(server) = state.server.as_mut() else {
                return;
            };
            let stream = match &stream {
                Ok(stream) => stream.try_clone(),
                Err(failure) => Err(failure.kind().into()),
            };
            match stream {
                Ok(stream) => {
                    let id = server.next_connection;
                    server.next_connection += 1;
                    server.connections.insert(id, stream);
                    Some(id)
                }
                Err(_) => None,
            }
        };
        let (Some(id), Ok(stream)) = (id, stream) else {
            // Out of descriptors, say; retrying at once would only spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let spawned = std::thread::Builder::new()
            .name("ry-help-connection".to_owned())
            .spawn(move || {
                converse(stream, generation);
                forget(id, generation);
            });
        if spawned.is_err() {
            forget(id, generation);
        }
    }
}

/// Drops a closed connection's entry, unless its server has stopped already.
fn forget(id: u64, generation: u64) {
    let mut state = lock(&STATE);
    if state.generation == generation
        && let Some(server) = state.server.as_mut()
        && let Some(stream) = server.connections.remove(&id)
    {
        stream.shutdown(Shutdown::Both).ok();
    }
}

/// Reads requests from one connection and writes each answer, until either side closes.
fn converse(stream: TcpStream, generation: u64) {
    if stream.set_read_timeout(Some(IDLE)).is_err() {
        return;
    }
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let (version, keep_alive, request) = match read_request(&mut reader, &mut writer) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return,
            Err(()) => {
                writer.write_all(b"HTTP/1.0 400 Bad Request\r\n\r\n").ok();
                return;
            }
        };
        let head_only = request.method == "HEAD";
        let (reply, response) = sync_channel(1);
        {
            let mut state = lock(&STATE);
            if state.generation != generation || state.server.is_none() {
                return;
            }
            state.queue.push_back(Exchange { request, reply });
        }
        ARRIVED.notify_all();
        crate::console_server::wake();
        // The server stopped and dropped the request.
        let Ok(response) = response.recv() else {
            return;
        };
        if writer
            .write_all(&encode(&response, &version, head_only))
            .is_err()
            || response.close
            || !keep_alive
        {
            return;
        }
    }
}

/// One request, with its HTTP version and whether the connection stays open after it. `Ok(None)`
/// is a clean close (or an idle timeout) before a request began; `Err` a request GNU R's server
/// would refuse.
fn read_request(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) -> Result<Option<(String, bool, Request)>, ()> {
    let mut budget = MAX_HEAD;
    // Blank lines before a request line are skipped, as HTTP allows.
    let line = loop {
        let mut line = Vec::new();
        match reader.by_ref().take(budget).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return Ok(None),
            Ok(read) => budget -= read as u64,
        }
        if !line.ends_with(b"\n") {
            return Err(());
        }
        if line.iter().any(|byte| !byte.is_ascii_whitespace()) {
            break String::from_utf8(line).map_err(|_| ())?;
        }
    };
    let mut parts = line.split_ascii_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(());
    };
    if !version.starts_with("HTTP/") {
        return Err(());
    }
    let mut headers = format!("Request-Method: {method}\n").into_bytes();
    let mut content_length = 0;
    let mut content_type = None;
    let mut connection = None;
    let mut expect_continue = false;
    loop {
        let mut header = Vec::new();
        let read = reader
            .by_ref()
            .take(budget)
            .read_until(b'\n', &mut header)
            .map_err(|_| ())?;
        budget -= read as u64;
        if !header.ends_with(b"\n") {
            return Err(());
        }
        let header = String::from_utf8_lossy(&header);
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        headers.extend_from_slice(header.as_bytes());
        headers.push(b'\n');
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<u64>().map_err(|_| ())?;
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("connection") {
            connection = Some(value.to_ascii_lowercase());
        } else if name.eq_ignore_ascii_case("expect") {
            expect_continue = value.eq_ignore_ascii_case("100-continue");
        }
    }
    if content_length > MAX_BODY {
        return Err(());
    }
    let body = if content_length > 0 {
        // A client that announced `Expect: 100-continue` waits for this before sending the body.
        if expect_continue {
            writer
                .write_all(format!("{version} 100 Continue\r\n\r\n").as_bytes())
                .map_err(|_| ())?;
        }
        let mut body = Vec::new();
        reader
            .by_ref()
            .take(content_length)
            .read_to_end(&mut body)
            .map_err(|_| ())?;
        if body.len() as u64 != content_length {
            return Err(());
        }
        Some((body, content_type))
    } else {
        None
    };
    let keep_alive = match connection.as_deref() {
        Some("close") => false,
        Some("keep-alive") => true,
        _ => version == "HTTP/1.1",
    };
    let target = target.split_once('#').map_or(target, |(before, _)| before);
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(parse_query(query))),
        None => (target, None),
    };
    let request = Request {
        method: method.to_owned(),
        path: percent_decode(path, false),
        query,
        headers,
        body,
    };
    Ok(Some((version.to_owned(), keep_alive, request)))
}

/// `a=1&b&&c=x=y` gives `a = "1"`, `"" = "b"`, `"" = ""` and `c = "x=y"`, as in GNU R.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .map(|field| match field.split_once('=') {
            Some((name, value)) => (percent_decode(name, true), percent_decode(value, true)),
            None => (String::new(), percent_decode(field, true)),
        })
        .collect()
}

/// Decodes `%XX` escapes, and `+` as a space in a query. Like GNU R's server, the text ends at
/// the first malformed escape.
fn percent_decode(text: &str, plus_is_space: bool) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'%' => {
                let hex = bytes
                    .get(index + 1..index + 3)
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                let Some(value) = hex else { break };
                decoded.push(value);
                index += 3;
            }
            b'+' if plus_is_space => {
                decoded.push(b' ');
                index += 1;
            }
            _ => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn encode(response: &Response, version: &str, head_only: bool) -> Vec<u8> {
    let mut head = format!("{version} {} {}\r\n", response.status, response.reason);
    if response.close {
        head.push_str("Connection: close\r\n");
    }
    head.push_str(&format!("Content-type: {}\r\n", response.content_type));
    for header in &response.headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    if !response.close {
        head.push_str(&format!("Content-length: {}\r\n", response.body.len()));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    if !head_only {
        bytes.extend_from_slice(&response.body);
    }
    bytes
}

/// The R side of a request: picks the handler as GNU R does, a `/custom/<name>` path going to
/// `tools:::.httpd.handlers.env[[name]]` when one is registered and everything else to
/// `tools:::httpd`, and calls it under `try()`. The call's function is the closure itself, so an
/// error message reads `Error in (function (path, query, ...)`, as GNU R's does.
const DISPATCH: &str = r#"local({
    handler <- tools:::httpd
    if (startsWith(path, "/custom/")) {
        name <- sub("/.*", "", substring(path, 9L))
        handlers <- tools:::.httpd.handlers.env
        if (nzchar(name) && exists(name, envir = handlers, inherits = FALSE))
            handler <- get(name, envir = handlers, inherits = FALSE)
    }
    try(eval(as.call(list(handler, path, query, body, headers))), silent = TRUE)
})"#;

/// The response to a request, and whether a Ctrl-C stopped its handler.
unsafe fn answer(request: &Request) -> (Response, bool) {
    match unsafe { call_handler(request) } {
        Ok(result) => (unsafe { response_of(result) }, false),
        Err(Unwind::Interrupt) => (
            error_response("Evaluation error", "interrupted".to_owned()),
            true,
        ),
        Err(Unwind::Error(failure)) => (error_response("Evaluation error", failure.message), false),
        // The handler runs with no handlers or restarts of the waiting code in reach, so no jump
        // can target a frame outside it.
        Err(Unwind::Jump(jump)) => (
            error_response(
                "Evaluation error",
                format!("unexpected jump out of the handler: {jump:?}"),
            ),
            false,
        ),
    }
}

unsafe fn call_handler(request: &Request) -> R<Sexp> {
    unsafe {
        let env = values::new_env(base_env())?;
        eval::define(
            values::install("path")?,
            values::scalar_string(&request.path)?,
            env,
        )?;
        let query = match &request.query {
            None => nil(),
            Some(fields) => {
                let answers: Vec<&str> = fields.iter().map(|(_, value)| value.as_str()).collect();
                let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
                let query = values::string_vector(&answers)?;
                values::set_attr(query, symbols().names, values::string_vector(&names)?)?;
                query
            }
        };
        eval::define(values::install("query")?, query, env)?;
        let body = match &request.body {
            None => nil(),
            Some((bytes, content_type)) => {
                let body = raw_vector(bytes)?;
                if let Some(content_type) = content_type {
                    values::set_attr(
                        body,
                        values::install("content-type")?,
                        values::scalar_string(content_type)?,
                    )?;
                }
                body
            }
        };
        eval::define(values::install("body")?, body, env)?;
        eval::define(
            values::install("headers")?,
            raw_vector(&request.headers)?,
            env,
        )?;
        let expressions = lower::parse_text(DISPATCH, "<help server>")?;
        crate::heap().root(expressions);
        // Like GNU R's `R_ToplevelExec`, the handler sees none of the waiting code's condition
        // handlers or restarts, so a warning it signals cannot jump into a `tryCatch()` around
        // `Sys.sleep()`.
        let state = interp();
        let shown = std::mem::replace(&mut state.show_error_messages, false);
        let suspended = (state.handlers.take(), state.restarts.take());
        state.suspended_handlers.push(suspended);
        let srcref = state.srcref;
        let result = eval::eval(expressions.element(0), env);
        let state = interp();
        state.show_error_messages = shown;
        if let Some((handlers, restarts)) = state.suspended_handlers.pop() {
            state.handlers = handlers;
            state.restarts = restarts;
        }
        state.srcref = srcref;
        let result = result?;
        crate::heap().root(result);
        Ok(result)
    }
}

unsafe fn response_of(result: Sexp) -> Response {
    unsafe {
        if result.kind() == SexpType::String && length(result) > 0 {
            let message = values::string_at(result, 0).unwrap_or_default();
            return error_response("Evaluation error", message.into_owned());
        }
        let invalid = || {
            error_response(
                "Invalid response from R",
                "Server error: invalid response from R\n".to_owned(),
            )
        };
        if result.kind() != SexpType::List || length(result) == 0 {
            return invalid();
        }
        let first = result.element(0);
        let names = values::names(result);
        let is_file = names.kind() == SexpType::String
            && values::string_at(names, 0).is_some_and(|name| name == "file");
        let body = if is_file && first.kind() == SexpType::String {
            let path = values::string_at(first, 0).unwrap_or_default();
            // GNU R answers a file it cannot read with an empty page.
            std::fs::read(crate::connections::expand_path(&path)).unwrap_or_default()
        } else {
            match first.kind() {
                SexpType::String if length(first) > 0 => values::string_at(first, 0)
                    .unwrap_or_default()
                    .into_owned()
                    .into_bytes(),
                SexpType::Raw => values::raws(first).to_vec(),
                _ => return invalid(),
            }
        };
        let element = |index: usize| (length(result) > index).then(|| result.element(index));
        let content_type = element(1)
            .filter(|value| value.kind() == SexpType::String && length(*value) > 0)
            .and_then(|value| values::string_at(value, 0))
            .map_or_else(|| "text/html".to_owned(), |value| value.into_owned());
        let headers = element(2)
            .filter(|value| value.kind() == SexpType::String)
            .map(|value| {
                (0..length(value))
                    .filter_map(|index| values::string_at(value, index))
                    .map(|header| header.into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let status = element(3)
            .map(|value| values::as_integer_scalar(value))
            .filter(|&status| status != NA_INTEGER && (100..1000).contains(&status))
            .map_or(200, |status| status as u16);
        Response {
            status,
            reason: match status {
                200 => "OK".to_owned(),
                status => format!("Code {status}"),
            },
            content_type,
            headers,
            body,
            close: false,
        }
    }
}

fn error_response(reason: &str, message: String) -> Response {
    Response {
        status: 500,
        reason: reason.to_owned(),
        content_type: "text/plain".to_owned(),
        headers: Vec::new(),
        body: message.into_bytes(),
        close: true,
    }
}

unsafe fn raw_vector(bytes: &[u8]) -> R<Sexp> {
    unsafe {
        let vector = alloc(SexpType::Raw, bytes.len())?;
        values::raws_mut(vector).copy_from_slice(bytes);
        Ok(vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_split_like_gnu_r() {
        assert_eq!(
            parse_query("a=%3D&b=c=d&&e="),
            vec![
                ("a".to_owned(), "=".to_owned()),
                ("b".to_owned(), "c=d".to_owned()),
                (String::new(), String::new()),
                ("e".to_owned(), String::new()),
            ]
        );
        assert_eq!(
            parse_query("=v&k&y=a+b%26c"),
            vec![
                (String::new(), "v".to_owned()),
                (String::new(), "k".to_owned()),
                ("y".to_owned(), "a b&c".to_owned()),
            ]
        );
    }

    #[test]
    fn a_path_decodes_until_a_malformed_escape() {
        assert_eq!(percent_decode("/a%20b+c", false), "/a b+c");
        assert_eq!(percent_decode("/q/%2Fx%zz%4", false), "/q//x");
    }
}
