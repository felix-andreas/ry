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
    reader: &mut impl BufRead,
    writer: &mut impl Write,
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
    // GNU R keeps a `#` and what follows it: browsers never send a fragment, and the server
    // does not strip one.
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(parse_query(query))),
        None => (target, None),
    };
    let request = Request {
        method: method.to_owned(),
        path: percent_decode(path),
        query,
        headers,
        body,
    };
    Ok(Some((version.to_owned(), keep_alive, request)))
}

/// `a=1&b&&c=x=y` gives `a = "1"`, `"" = "b"`, `"" = ""` and `c = "x=y"`, as in GNU R. The
/// field count comes from the raw `&`s, but an escape takes its two digits whatever they are, so
/// `k=%2&j` is the one field `k = " j"` followed by an empty one, exactly as GNU R's server reads
/// it.
fn parse_query(query: &str) -> Vec<(String, String)> {
    let bytes = query.as_bytes();
    let count = bytes.iter().filter(|byte| **byte == b'&').count() + 1;
    let mut fields = Vec::with_capacity(count);
    let mut name = None;
    let mut value = Vec::new();
    let mut index = 0;
    loop {
        match bytes.get(index) {
            Some(b'=') if name.is_none() => {
                name = Some(std::mem::take(&mut value));
                index += 1;
            }
            Some(b'&') | None => {
                fields.push((
                    decoded_text(name.take().unwrap_or_default()),
                    decoded_text(std::mem::take(&mut value)),
                ));
                if index >= bytes.len() {
                    break;
                }
                index += 1;
            }
            Some(_) => index = decode_one(bytes, index, &mut value),
        }
    }
    fields.resize(count, (String::new(), String::new()));
    fields
}

/// A path decoded the way GNU R's server decodes it, `+` becoming a space there too.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        index = decode_one(bytes, index, &mut decoded);
    }
    decoded_text(decoded)
}

/// Decodes the character at `index` onto `out` and returns where the next one starts. `+` is a
/// space and `%XX` the byte `0xXX`. GNU R's server checks nothing: a digit that is not hex counts
/// as 0, and a missing one (at the end of the text) also counts as 0 without being consumed.
fn decode_one(bytes: &[u8], index: usize, out: &mut Vec<u8>) -> usize {
    let digit = |at: usize| match bytes.get(at) {
        Some(byte @ b'0'..=b'9') => byte - b'0',
        Some(byte @ b'a'..=b'f') => byte - b'a' + 10,
        Some(byte @ b'A'..=b'F') => byte - b'A' + 10,
        _ => 0,
    };
    match bytes.get(index) {
        Some(b'%') => {
            out.push(digit(index + 1) << 4 | digit(index + 2));
            (index + 3).min(bytes.len())
        }
        Some(b'+') => {
            out.push(b' ');
            index + 1
        }
        Some(byte) => {
            out.push(*byte);
            index + 1
        }
        None => index,
    }
}

/// GNU R makes each decoded field a C string, so it ends at the first NUL an escape produced.
fn decoded_text(mut bytes: Vec<u8>) -> String {
    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(end);
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
    use {
        super::*,
        std::io::Cursor,
        syntax::testing::{SplitMix64, for_each_seed, mutate_bytes},
    };

    fn pairs(fields: &[(&str, &str)]) -> Vec<(String, String)> {
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// Each pair was read off GNU R's own server by a handler that deparses its `path` and
    /// `query`.
    #[test]
    fn paths_decode_like_gnu_r() {
        for (raw, decoded) in [
            ("/a%20b+c", "/a b c"),
            ("/q/%2Fx%zz%4", "/q//x"),
            ("/a%+1b", "/a\u{1}b"),
            ("/a%G1b", "/a\u{1}b"),
            ("/a%1+b", "/a\u{10}b"),
            ("/a%2", "/a "),
            ("/a%1", "/a\u{10}"),
            ("/a%4", "/a@"),
            ("/a%", "/a"),
            ("/a%%41", "/a\u{4}1"),
            ("/a%2&b", "/a b"),
            ("/a%Gzb", "/a"),
            ("/a%41%42", "/aAB"),
            ("/a+%2B#x", "/a +#x"),
        ] {
            assert_eq!(percent_decode(raw), decoded, "{raw}");
        }
    }

    #[test]
    fn queries_split_like_gnu_r() {
        for (raw, fields) in [
            (
                "a=%3D&b=c=d&&e=",
                &[("a", "="), ("b", "c=d"), ("", ""), ("e", "")][..],
            ),
            ("=v&k&y=a+b%26c", &[("", "v"), ("", "k"), ("y", "a b&c")]),
            ("k=%2&j=3", &[("k", " j=3"), ("", "")]),
            ("k=%&j=3&z", &[("k", ""), ("", "z"), ("", "")]),
            ("k=a%00b&j=2", &[("k", "a"), ("j", "2")]),
            ("k=%00%2&j=3", &[("k", ""), ("", "")]),
            ("k%3D=1", &[("k=", "1")]),
            ("%41=%", &[("A", "")]),
            ("k=1#frag&j", &[("k", "1#frag"), ("", "j")]),
            ("", &[("", "")]),
            ("&", &[("", ""), ("", "")]),
        ] {
            assert_eq!(parse_query(raw), pairs(fields), "{raw}");
        }
    }

    /// A request ry builds itself: what the wire bytes say and what parsing them must give.
    struct Generated {
        version: &'static str,
        keep_alive: bool,
        request: Request,
    }

    /// Random requests written to one stream back to back come back field for field, and the
    /// stream then ends cleanly.
    #[test]
    fn generated_requests_parse_back() {
        for_each_seed(300, |seed| {
            let mut rng = SplitMix64(seed);
            let mut wire = Vec::new();
            let mut expected = Vec::new();
            for _ in 0..1 + rng.below(4) {
                let generated = generate(&mut rng, &mut wire);
                expected.push(generated);
            }
            let mut reader = Cursor::new(wire);
            let mut writer = Vec::new();
            for generated in &expected {
                let Ok(Some((version, keep_alive, request))) =
                    read_request(&mut reader, &mut writer)
                else {
                    panic!("a generated request was refused");
                };
                assert_eq!(version, generated.version);
                assert_eq!(keep_alive, generated.keep_alive);
                assert_eq!(request.method, generated.request.method);
                assert_eq!(request.path, generated.request.path);
                assert_eq!(request.query, generated.request.query);
                assert_eq!(
                    String::from_utf8_lossy(&request.headers),
                    String::from_utf8_lossy(&generated.request.headers)
                );
                assert_eq!(request.body, generated.request.body);
            }
            assert!(matches!(read_request(&mut reader, &mut writer), Ok(None)));
        });
    }

    /// Mutated requests never panic, and whatever parses keeps the invariants the handler call
    /// relies on.
    #[test]
    fn mutated_requests_never_break_the_parser() {
        for_each_seed(600, |seed| {
            let mut rng = SplitMix64(seed);
            let mut wire = Vec::new();
            for _ in 0..1 + rng.below(3) {
                generate(&mut rng, &mut wire);
            }
            let wire = mutate_bytes(&mut rng, &String::from_utf8_lossy(&wire));
            let length = wire.len() as u64;
            let mut reader = Cursor::new(wire);
            let mut writer = Vec::new();
            // Every accepted request consumes at least its request line, so this terminates.
            for _ in 0..=length {
                let before = reader.position();
                match read_request(&mut reader, &mut writer) {
                    Ok(Some((version, _, request))) => {
                        assert!(reader.position() > before, "a request consumed nothing");
                        assert!(version.starts_with("HTTP/"));
                        assert!(request.headers.starts_with(b"Request-Method: "));
                        assert!(!request.path.contains('\0'));
                    }
                    Ok(None) | Err(()) => return,
                }
            }
            panic!("the parser looped on one stream");
        });
    }

    fn generate(rng: &mut SplitMix64, wire: &mut Vec<u8>) -> Generated {
        const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "DELETE"];
        const VERSIONS: &[&str] = &["HTTP/1.0", "HTTP/1.1"];
        let method = METHODS[rng.below(METHODS.len())];
        let version = VERSIONS[rng.below(VERSIONS.len())];
        if rng.chance(1, 4) {
            wire.extend_from_slice(b"\r\n");
        }
        let mut path = String::from("/");
        let mut target = String::from("/");
        for _ in 0..rng.below(4) {
            let (raw, text) = segment(rng);
            target.push_str(&raw);
            path.push_str(&text);
            target.push('/');
            path.push('/');
        }
        let query = rng.chance(1, 2).then(|| {
            let mut fields = Vec::new();
            target.push('?');
            for index in 0..1 + rng.below(4) {
                if index > 0 {
                    target.push('&');
                }
                let (raw_value, value) = segment(rng);
                let name = if rng.chance(3, 4) {
                    let (raw_name, name) = segment(rng);
                    target.push_str(&raw_name);
                    target.push('=');
                    name
                } else {
                    String::new()
                };
                target.push_str(&raw_value);
                fields.push((name, value));
            }
            fields
        });
        let mut head = format!("{method} {target} {version}\r\n");
        let mut headers = format!("Request-Method: {method}\n");
        let connection = match rng.below(3) {
            0 => Some("close"),
            1 => Some("Keep-Alive"),
            _ => None,
        };
        let keep_alive = match connection {
            Some("close") => false,
            Some(_) => true,
            None => version == "HTTP/1.1",
        };
        let body = (method != "GET" && rng.chance(1, 2)).then(|| {
            let length = 1 + rng.below(40);
            (0..length)
                .map(|_| (rng.next_u64() & 0xFF) as u8)
                .collect::<Vec<u8>>()
        });
        let content_type = (body.is_some() && rng.chance(1, 2)).then(|| "text/plain".to_owned());
        let mut lines = vec![format!("Host: 127.0.0.1:{}", rng.below(65536))];
        if let Some(connection) = connection {
            lines.push(format!("Connection: {connection}"));
        }
        if let Some(body) = &body {
            lines.push(format!("Content-Length: {}", body.len()));
        }
        if let Some(content_type) = &content_type {
            lines.push(format!("Content-Type: {content_type}"));
        }
        if rng.chance(1, 3) {
            lines.push("X-Odd-Header-Without-Colon".to_owned());
        }
        for _ in 0..rng.below(4) {
            let swap = rng.below(lines.len());
            lines.swap(0, swap);
        }
        for line in &lines {
            head.push_str(line);
            head.push_str(if rng.chance(1, 5) { "\n" } else { "\r\n" });
            headers.push_str(line);
            headers.push('\n');
        }
        head.push_str("\r\n");
        wire.extend_from_slice(head.as_bytes());
        if let Some(body) = &body {
            wire.extend_from_slice(body);
        }
        Generated {
            version,
            keep_alive,
            request: Request {
                method: method.to_owned(),
                path,
                query,
                headers: headers.into_bytes(),
                body: body.map(|body| (body, content_type)),
            },
        }
    }

    /// One path segment or query word, as it goes on the wire and as it decodes. It spells
    /// characters plainly, as `+`, or as escapes in either case, and never produces a NUL, a
    /// `#`, a `?` or anything that would split it.
    fn segment(rng: &mut SplitMix64) -> (String, String) {
        const PLAIN: &[u8] = b"abcXYZ019-._~!$'()*,;:@";
        let mut raw = String::new();
        let mut text = String::new();
        for _ in 0..rng.below(6) {
            match rng.below(4) {
                0 => {
                    raw.push('+');
                    text.push(' ');
                }
                1 => {
                    let character = (0x20 + rng.below(0x5F)) as u8 as char;
                    if rng.chance(1, 2) {
                        raw.push_str(&format!("%{:02X}", character as u32));
                    } else {
                        raw.push_str(&format!("%{:02x}", character as u32));
                    }
                    text.push(character);
                }
                _ => {
                    let character = PLAIN[rng.below(PLAIN.len())] as char;
                    raw.push(character);
                    text.push(character);
                }
            }
        }
        (raw, text)
    }
}
