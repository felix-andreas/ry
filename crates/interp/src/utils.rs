//! The utils package's native routines: reading and writing delimited data, type conversion,
//! object sizes, hash tables and checksums, and the routines that need a console, a network
//! or an editor, which fail with a clear error in a non-interactive session.
//!
//! Base's `scan` lives here too, because `read.table` drives it together with
//! `readtablehead` and `countfields`, and all three share one character reader. The readers
//! port GNU R's byte by byte, because `read.table` users depend on their quirks: a nul ends a
//! field, a quote opens only at a field's start, and a quoted line end makes a line count `NA`.

use {
    crate::{
        builtin_libraries::{NativeCall, NativeFn},
        builtins::{Arg, BuiltinDef, Call, internal},
        coerce::{coerce_vector, is_blank, is_c_space},
        conditions::warning,
        format::{format_complex_alone, format_real_alone_scipen},
        platform,
        sort::{collator, shell_sort},
        unwind::{R, Unwind, error},
        values::{
            self, alloc, length, na_string, nil, scalar_bool, scalar_integer, scalar_string,
            string_vector, symbols,
        },
        write_console,
    },
    heap::{
        activity::set_profile_tick,
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp},
        strings::Encoding,
    },
    numeric::long_double::{Exactness, strtod},
    rustc_hash::{FxHashMap, FxHashSet},
    std::{
        cell::RefCell,
        collections::BTreeMap,
        fs::File,
        io::{BufWriter, Read, Write},
        net::{Ipv4Addr, TcpListener, TcpStream, ToSocketAddrs},
        sync::{
            Arc, Mutex, MutexGuard, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
    },
};

pub const BUILTINS: &[BuiltinDef] = &[internal("scan", 19, do_scan)];

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("readtablehead", do_read_table_head),
    ("countfields", do_count_fields),
    ("typeconvert", do_type_convert),
    ("writetable", do_write_table),
    ("crc64", do_crc64),
    ("objectSize", do_object_size),
    ("octsize", do_octal_size),
    ("charClass", do_char_class),
    ("flushconsole", do_flush_console),
    ("processevents", |_| {
        // SAFETY: `process.events()` is a point where GNU R's event loop runs R handlers.
        unsafe { crate::help_server::serve_pending()? };
        Ok(nil())
    }),
    ("tzcode_type", |_| unsafe { scalar_string("internal") }),
    // GNU R for Windows' console routines, for a session without its GUI: `Rterm`'s answers.
    ("winver", |_| unsafe {
        let name = crate::platform::system_name();
        scalar_string(&format!("{} {}", name.sysname, name.release))
    }),
    ("getIdentification", |_| unsafe { scalar_string("RTerm") }),
    ("getWindowTitle", |_| unsafe { scalar_string("") }),
    ("setWindowTitle", |_| Ok(nil())),
    ("setStatusBar", |_| Ok(nil())),
    ("shortpath", |call| Ok(call.arg(0))),
    ("nsl", do_nsl),
    ("menu", do_menu),
    ("hashtab_Ext", do_hashtab),
    ("gethash_Ext", do_gethash),
    ("sethash_Ext", do_sethash),
    ("remhash_Ext", do_remhash),
    ("numhash_Ext", do_numhash),
    ("typhash_Ext", do_typhash),
    ("maphash_Ext", do_maphash),
    ("clrhash_Ext", do_clrhash),
    ("ishashtab_Ext", do_is_hashtab),
    ("Rprof", do_rprof),
    ("Rprofmem", do_rprofmem),
    ("unzip", do_unzip),
    ("download", do_download),
    ("sockconnect", do_sockconnect),
    ("sockread", do_sockread),
    ("sockclose", do_sockclose),
    ("sockopen", do_sockopen),
    ("socklisten", do_socklisten),
    ("sockwrite", do_sockwrite),
    ("addhistory", |_| Ok(nil())),
    // The `.External2` routines report errors against their own call, as GNU R's do.
    ("loadhistory", |call| {
        Err(call.error("no history mechanism available"))
    }),
    ("savehistory", |call| {
        Err(call.error("no history available to save"))
    }),
    ("dataentry", |call| {
        Err(call.error("data entry cannot be used non-interactively"))
    }),
    // utils' `View()` formats the whole object before it calls this; ry's own `View()` in
    // `tools:ry` stays lazy.
    ("dataviewer", |call| unsafe {
        crate::console_server::show(
            call.arg(0),
            &values::as_string_scalar(call.arg(1))?.unwrap_or_default(),
        )
        .map_err(|message| call.error(message))?;
        Ok(nil())
    }),
    ("edit", |call| unsafe { edit(call) }),
    ("fileedit", |call| {
        Err(call.error("file.edit() cannot be used non-interactively"))
    }),
    ("selectlist", |call| {
        Err(call.error("select.list() cannot be used non-interactively"))
    }),
];

/// The console half of `menu(choices)`, which R code calls only when `interactive()` is true:
/// reads one line after "Selection: " and returns the number it starts with. A line that is not a
/// number in `0:length(choices)` answers one past the last choice, which makes `menu()` print its
/// hint and ask again. The end of the input answers 0, so the menu ends instead of looping.
unsafe fn do_menu(call: &NativeCall<'_>) -> R<Sexp> {
    let choices = call.arg(0);
    if choices.kind() != SexpType::String {
        return Err(error("invalid 'choices' argument"));
    }
    let count = length(choices) as i32;
    let Some(line) = (unsafe { crate::console_server::prompt("Selection: ") }) else {
        return values::scalar_integer(0);
    };
    let chosen = leading_number(&line).filter(|number| (0.0..=f64::from(count)).contains(number));
    values::scalar_integer(chosen.map_or(count + 1, |number| number as i32))
}

/// The number a line starts with after any white space, read as C's `strtod` reads decimal text.
fn leading_number(line: &str) -> Option<f64> {
    let text = line.trim_start();
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        bytes[from.min(bytes.len())..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let whole = digits(end);
    end += whole;
    let mut fraction = 0;
    if bytes.get(end) == Some(&b'.') {
        fraction = digits(end + 1);
        end += 1 + fraction;
    }
    if whole + fraction == 0 {
        return None;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        let exponent = digits(end + 1 + sign);
        if exponent > 0 {
            end += 1 + sign + exponent;
        }
    }
    text[..end].parse().ok()
}

/// `edit(name, file, title, editor)`: writes the object's deparse to the file, runs the editor
/// on it, and returns what the file then evaluates to. Nothing here needs a console, so an
/// editor such as `cat` works in a script, as in GNU R.
const EDIT: &str = r#"
if (!nzchar(file)) file <- tempfile(fileext = ".R")
if (!is.null(name) || !file.exists(file)) {
  control <- c("keepInteger", "showAttributes", "keepNA", "useSource")
  writeLines(if (is.null(name)) character() else deparse(name, control = control), file)
}
if (system(paste(editor, shQuote(file))) != 0L)
  stop(sprintf("problem with running editor %s", editor), call. = FALSE)
expressions <- tryCatch(parse(file), error = function(failure) {
  line <- regmatches(conditionMessage(failure), regexpr("(?<=:)[0-9]+(?=:)", conditionMessage(failure), perl = TRUE))
  stop(sprintf("an error occurred on line %s\n use a command like\n x <- edit()\n to recover", line), call. = FALSE)
})
value <- NULL
for (expression in expressions) value <- eval(expression, globalenv())
if (is.function(value) && is.function(name)) environment(value) <- environment(name)
value
"#;

unsafe fn edit(call: &NativeCall<'_>) -> R<Sexp> {
    let env = values::new_env(values::base_namespace())?;
    for (index, name) in ["name", "file", "title", "editor"].into_iter().enumerate() {
        crate::heap().define(env, values::install(name)?, call.arg(index))?;
    }
    crate::session::eval_text(EDIT, env)
}

/// The sockets of `make.socket()` and friends, keyed by their OS handle, which is the number
/// GNU R hands to R code.
static SOCKETS: Mutex<BTreeMap<i32, UtilsSocket>> = Mutex::new(BTreeMap::new());

enum UtilsSocket {
    Stream(TcpStream),
    Listener(TcpListener),
}

/// `.Call(C_sockconnect, port, host)`: a connected socket's number, or 0 after printing why
/// the connection failed, which `make.socket()` turns into "socket not established".
unsafe fn do_sockconnect(call: &NativeCall<'_>) -> R<Sexp> {
    let port = values::as_integer_scalar(call.arg(0));
    let host = values::require_str(call.arg(1), "host")?;
    let Ok(port) = u16::try_from(port) else {
        return scalar_integer(0);
    };
    let addresses = match (host.as_ref(), port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(reason) => {
            report_socket_error(&reason);
            return scalar_integer(0);
        }
    };
    let mut failure = None;
    for address in addresses {
        match TcpStream::connect(address) {
            Ok(stream) => {
                let handle = platform::stream_handle(&stream);
                return register_socket(handle, UtilsSocket::Stream(stream));
            }
            Err(reason) => failure = Some(reason),
        }
    }
    if let Some(reason) = failure {
        report_socket_error(&reason);
    }
    scalar_integer(0)
}

/// `.Call(C_sockopen, port)`: a socket listening on `port`.
unsafe fn do_sockopen(call: &NativeCall<'_>) -> R<Sexp> {
    let port = values::as_integer_scalar(call.arg(0));
    let port = u16::try_from(port).map_err(|_| error("invalid 'port' argument"))?;
    match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
        Ok(listener) => {
            let handle = platform::listener_handle(&listener);
            register_socket(handle, UtilsSocket::Listener(listener))
        }
        Err(reason) => {
            report_socket_error(&reason);
            scalar_integer(-1)
        }
    }
}

/// `.Call(C_socklisten, socket)`: waits for one connection on a listening socket and returns
/// the connected socket, with the peer's host name as its `host` attribute.
unsafe fn do_socklisten(call: &NativeCall<'_>) -> R<Sexp> {
    let key = values::as_integer_scalar(call.arg(0));
    let accepted = {
        let sockets = lock_sockets();
        match sockets.get(&key) {
            Some(UtilsSocket::Listener(listener)) => listener.try_clone(),
            _ => return Err(error("invalid 'socket' argument")),
        }
    };
    let accepted = accepted.and_then(|listener| listener.accept());
    let (stream, peer) = match accepted {
        Ok(connection) => connection,
        Err(reason) => {
            report_socket_error(&reason);
            return scalar_integer(-1);
        }
    };
    let host = if peer.ip().is_loopback() {
        "localhost".to_owned()
    } else {
        peer.ip().to_string()
    };
    let handle = platform::stream_handle(&stream);
    let result = register_socket(handle, UtilsSocket::Stream(stream))?;
    crate::heap().root(result);
    values::set_attr(result, values::install("host")?, scalar_string(&host)?)?;
    Ok(result)
}

/// `.Call(C_sockread, socket, maxlen)`: at most `maxlen` bytes from one read, or `""` at the
/// end of the stream.
unsafe fn do_sockread(call: &NativeCall<'_>) -> R<Sexp> {
    let key = values::as_integer_scalar(call.arg(0));
    let maximum = values::as_integer_scalar(call.arg(1));
    let maximum = usize::try_from(maximum).map_err(|_| error("maxlen must be non-negative"))?;
    let stream = socket_stream(key);
    let mut buffer = vec![0; maximum];
    let count = match stream.and_then(|mut stream| stream.read(&mut buffer)) {
        Ok(count) => count,
        Err(reason) => {
            report_socket_error(&reason);
            0
        }
    };
    buffer.truncate(count);
    // GNU R keeps the bytes as they arrived, so invalid UTF-8 travels as ry's byte stand-ins
    // instead of being replaced.
    values::scalar_string(&values::escape_invalid_bytes(&buffer))
}

/// `.Call(C_sockwrite, socket, string)`: the number of bytes written.
unsafe fn do_sockwrite(call: &NativeCall<'_>) -> R<Sexp> {
    let key = values::as_integer_scalar(call.arg(0));
    let text = values::require_str(call.arg(1), "string")?;
    let written = socket_stream(key).and_then(|mut stream| stream.write(text.as_bytes()));
    match written {
        Ok(count) => scalar_integer(i32::try_from(count).unwrap_or(i32::MAX)),
        Err(reason) => {
            report_socket_error(&reason);
            scalar_integer(-1)
        }
    }
}

/// `.Call(C_sockclose, socket)`: FALSE when the socket closed and TRUE when it could not, as
/// GNU R returns.
unsafe fn do_sockclose(call: &NativeCall<'_>) -> R<Sexp> {
    let key = values::as_integer_scalar(call.arg(0));
    let closed = lock_sockets().remove(&key).is_some();
    if !closed {
        report_socket_error(&std::io::Error::from_raw_os_error(libc::EBADF));
    }
    scalar_bool(!closed)
}

unsafe fn register_socket(handle: platform::SocketHandle, socket: UtilsSocket) -> R<Sexp> {
    // A socket handle is already an `i32` on Unix but a 64-bit `SOCKET` on Windows.
    #[allow(clippy::useless_conversion)]
    let key = i32::try_from(handle).map_err(|_| error("socket handle out of range"))?;
    lock_sockets().insert(key, socket);
    scalar_integer(key)
}

/// A second handle on a connected socket, so a blocking read or write does not hold the table.
fn socket_stream(key: i32) -> std::io::Result<TcpStream> {
    match lock_sockets().get(&key) {
        Some(UtilsSocket::Stream(stream)) => stream.try_clone(),
        _ => Err(std::io::Error::from_raw_os_error(libc::EBADF)),
    }
}

fn lock_sockets() -> MutexGuard<'static, BTreeMap<i32, UtilsSocket>> {
    SOCKETS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn report_socket_error(reason: &std::io::Error) {
    let message = format!("socket error: {}\n", crate::connections::os_message(reason));
    write_console(message.as_bytes(), true);
}

// The character reader.

/// What [`Reader::next`] returns at the end of the input.
const EOF: i32 = -1;

/// The character reader `scan`, `readtablehead` and `countfields` share, a port of GNU R's
/// `scanchar`. Characters are bytes as `i32`, with [`EOF`] past the end.
struct Reader<'a> {
    input: &'a [u8],
    position: usize,
    /// The character handed back by [`Reader::unread`], 0 for none. As in GNU R, a handed-back
    /// nul is lost.
    saved: i32,
    /// The separator byte, 0 for fields separated by white space.
    separator: i32,
    quotes: Vec<u8>,
    /// The comment byte, `NO_COMMENT` for none.
    comment: i32,
    escapes: bool,
    skip_nul: bool,
    /// Whether a nul was read (and dropped from the field it ended) while `skip_nul` was off.
    embedded_nul: bool,
}

const NO_COMMENT: i32 = 100_000;

impl<'a> Reader<'a> {
    fn new(input: &'a [u8], position: usize, syntax: Syntax) -> Reader<'a> {
        Reader {
            input,
            position,
            saved: 0,
            separator: syntax.separator,
            quotes: syntax.quotes,
            comment: syntax.comment,
            escapes: false,
            skip_nul: false,
            embedded_nul: false,
        }
    }

    fn raw(&mut self) -> i32 {
        let mut character = self.byte();
        if character == 0 {
            if self.skip_nul {
                while character == 0 {
                    character = self.byte();
                }
            } else {
                self.embedded_nul = true;
            }
        }
        character
    }

    fn byte(&mut self) -> i32 {
        match self.input.get(self.position) {
            Some(&byte) => {
                self.position += 1;
                byte as i32
            }
            None => EOF,
        }
    }

    fn unread(&mut self, character: i32) {
        self.saved = character;
    }

    /// The next character: a comment outside quotes reads as the line end that ends it, and
    /// with `escapes` a backslash sequence reads as the character it names.
    fn next(&mut self, in_quote: bool) -> i32 {
        let mut next = if self.saved != 0 {
            std::mem::replace(&mut self.saved, 0)
        } else {
            self.raw()
        };
        if next == self.comment && !in_quote {
            loop {
                next = self.raw();
                if next == b'\n' as i32 || next == EOF {
                    break;
                }
            }
        }
        if next != b'\\' as i32 || !self.escapes {
            return next;
        }
        next = self.raw();
        if (b'0' as i32..=b'8' as i32).contains(&next) {
            let mut octal = next - b'0' as i32;
            for _ in 0..2 {
                next = self.raw();
                if !(b'0' as i32..=b'8' as i32).contains(&next) {
                    self.unread(next);
                    break;
                }
                octal = 8 * octal + next - b'0' as i32;
            }
            return octal;
        }
        match u8::try_from(next).unwrap_or(0) {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n' as i32,
            b'r' => b'\r' as i32,
            b't' => b'\t' as i32,
            b'v' => 11,
            b'x' => {
                let mut value = 0;
                for _ in 0..2 {
                    next = self.raw();
                    let Some(digit) = u8::try_from(next)
                        .ok()
                        .and_then(|byte| (byte as char).to_digit(16))
                    else {
                        self.unread(next);
                        break;
                    };
                    value = 16 * value + digit as i32;
                }
                value
            }
            _ if in_quote && self.is_quote(next) => {
                self.unread(next);
                b'\\' as i32
            }
            _ => next,
        }
    }

    /// Whether `character` is in the quote set, as C's `strchr` finds it: a nul always is.
    fn is_quote(&self, character: i32) -> bool {
        character == 0 || self.quotes.contains(&(character as u8))
    }

    /// Skips to the end of the line; returns the line end or [`EOF`].
    fn skip_line(&mut self) -> i32 {
        loop {
            let character = self.next(false);
            if character == b'\n' as i32 || character == EOF {
                return character;
            }
        }
    }

    /// The position to store back on the connection: a character handed back is unread.
    fn rest_position(&self) -> usize {
        if self.saved > 0 && self.position > 0 && self.input[self.position - 1] as i32 == self.saved
        {
            self.position - 1
        } else {
            self.position
        }
    }
}

/// White space as GNU R's `Rspace` sees it.
fn is_space(character: i32) -> bool {
    matches!(u8::try_from(character), Ok(b' ' | b'\t' | b'\n' | b'\r'))
}

/// How fields are delimited, quoted and commented, as the `sep`, `quote` and `comment.char`
/// arguments give them.
struct Syntax {
    separator: i32,
    quotes: Vec<u8>,
    comment: i32,
}

/// `sep` as `scan` takes it: one byte, or none for white space.
unsafe fn scan_separator(separator: Sexp) -> R<i32> {
    match separator.kind() {
        SexpType::String | SexpType::Nil => match first_bytes(separator) {
            None => Ok(0),
            Some(bytes) if bytes.len() > 1 => Err(error("invalid 'sep' value: must be one byte")),
            Some(bytes) => Ok(bytes.first().map_or(0, |&byte| byte as i32)),
        },
        _ => Err(error("invalid 'sep' argument")),
    }
}

/// The first string's bytes, `None` for an empty or non-character vector.
unsafe fn first_bytes<'a>(vector: Sexp) -> Option<&'a [u8]> {
    (vector.kind() == SexpType::String && length(vector) > 0).then(|| vector.element(0).bytes())
}

unsafe fn quote_set(quotes: Sexp) -> R<Vec<u8>> {
    match quotes.kind() {
        SexpType::String => Ok(first_bytes(quotes).unwrap_or_default().to_vec()),
        SexpType::Nil => Ok(Vec::new()),
        _ => Err(error("invalid quote symbol set")),
    }
}

unsafe fn comment_char(comment: Sexp) -> R<i32> {
    if comment.kind() != SexpType::String || length(comment) != 1 {
        return Err(error("invalid 'comment.char' argument"));
    }
    match comment.element(0).bytes() {
        [] => Ok(NO_COMMENT),
        [byte] => Ok(*byte as i32),
        _ => Err(error("invalid 'comment.char' argument")),
    }
}

/// `sep` as `readtablehead` and `countfields` take it: its first byte, unchecked.
unsafe fn first_byte_separator(separator: Sexp) -> R<i32> {
    match separator.kind() {
        SexpType::String | SexpType::Nil => Ok(first_bytes(separator)
            .and_then(|bytes| bytes.first())
            .map_or(0, |&byte| byte as i32)),
        _ => Err(error("invalid 'sep' argument")),
    }
}

// scan.

/// The type a field converts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
}

impl Kind {
    fn of(kind: SexpType) -> Option<Kind> {
        Some(match kind {
            SexpType::Logical => Kind::Logical,
            SexpType::Integer => Kind::Integer,
            SexpType::Real => Kind::Double,
            SexpType::Complex => Kind::Complex,
            SexpType::String => Kind::Character,
            SexpType::Raw => Kind::Raw,
            _ => return None,
        })
    }

    fn sexp_type(self) -> SexpType {
        match self {
            Kind::Logical => SexpType::Logical,
            Kind::Integer => SexpType::Integer,
            Kind::Double => SexpType::Real,
            Kind::Complex => SexpType::Complex,
            Kind::Character => SexpType::String,
            Kind::Raw => SexpType::Raw,
        }
    }
}

/// A converted field.
enum Value {
    Missing,
    Logical(i32),
    Integer(i32),
    Double(f64),
    Complex(Rcomplex),
    Text(Vec<u8>),
    Raw(u8),
}

/// One output column of `scan`: its type, or `None` for a column `what` skips with `NULL`.
struct Column {
    kind: Option<Kind>,
    values: Vec<Value>,
}

/// The settings of one `scan` call that its fields are read and converted with.
struct ScanSettings<'a> {
    decimal: u8,
    na_strings: &'a [Vec<u8>],
    max_items: usize,
    max_lines: usize,
    flush: bool,
    fill: bool,
    strip: &'a [bool],
    blank_lines_skip: bool,
    multi_line: bool,
    console: bool,
    /// Whether a byte-order mark at the start of the first field is dropped.
    at_start: bool,
}

/// `scan(file, what, nmax, sep, dec, quote, skip, nlines, na.strings, flush, fill,
/// strip.white, quiet, blank.lines.skip, multi.line, comment.char, allowEscapes, encoding,
/// skipNul)`, a port of GNU R's `do_scan`.
unsafe fn do_scan(call: &Call<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let what = call.arg(1);
    let max_items = count_argument(call.arg(2));
    let max_lines = count_argument(call.arg(7));
    let skip = count_argument(call.arg(6));
    let na_strings = call.arg(8);
    let flush = values::as_logical_scalar(call.arg(9));
    let fill = values::as_logical_scalar(call.arg(10));
    let strip = call.arg(11);
    let mut quiet = values::as_logical_scalar(call.arg(12));
    let mut blank_lines_skip = values::as_logical_scalar(call.arg(13));
    let mut multi_line = values::as_logical_scalar(call.arg(14));
    let escapes = values::as_logical_scalar(call.arg(16));
    let encoding = call.arg(17);
    if encoding.kind() != SexpType::String || length(encoding) != 1 {
        return Err(error("invalid 'encoding' argument"));
    }
    let mut mark = match encoding.element(0).bytes() {
        b"latin1" => Some(Encoding::Latin1),
        b"UTF-8" => Some(Encoding::Utf8),
        _ => None,
    };
    let skip_nul = values::as_logical_scalar(call.arg(18));
    if quiet == NA_LOGICAL {
        quiet = 0;
    }
    if blank_lines_skip == NA_LOGICAL {
        blank_lines_skip = 1;
    }
    if multi_line == NA_LOGICAL {
        multi_line = 1;
    }
    if strip.kind() != SexpType::Logical {
        return Err(error("invalid 'strip.white' argument"));
    }
    if length(strip) != 1 && length(strip) != length(what) {
        return Err(error("invalid 'strip.white' length"));
    }
    if na_strings.kind() != SexpType::String {
        return Err(error("invalid 'na.strings' argument"));
    }
    let comment = call.arg(15);
    if comment.kind() != SexpType::String || length(comment) != 1 {
        return Err(error("invalid 'comment.char' argument"));
    }
    let separator = scan_separator(call.arg(3))?;
    let decimal = scan_decimal(call.arg(4))?;
    let syntax = Syntax {
        separator,
        quotes: quote_set(call.arg(5))?,
        comment: comment_char(comment)?,
    };
    if escapes == NA_LOGICAL {
        return Err(error("invalid 'allowEscapes' argument"));
    }
    if skip_nul == NA_LOGICAL {
        return Err(error("invalid 'skipNul' argument"));
    }
    let na_strings: Vec<Vec<u8>> = (0..length(na_strings))
        .map(|index| na_strings.element(index).bytes().to_vec())
        .collect();
    let strip: Vec<bool> = values::ints(strip)
        .iter()
        .map(|&value| value != 0)
        .collect();
    let mut columns: Vec<Column> = match what.kind() {
        SexpType::List => {
            if length(what) == 0 {
                return Err(error("empty 'what' specified"));
            }
            let mut columns = Vec::with_capacity(length(what));
            for index in 0..length(what) {
                let element = what.element(index);
                let kind = if element == nil() {
                    None
                } else {
                    Some(Kind::of(element.kind()).ok_or_else(|| error("invalid 'what' argument"))?)
                };
                columns.push(Column {
                    kind,
                    values: Vec::new(),
                });
            }
            columns
        }
        kind => vec![Column {
            kind: Some(Kind::of(kind).ok_or_else(|| error("invalid 'what' argument"))?),
            values: Vec::new(),
        }],
    };
    let is_list = what.kind() == SexpType::List;
    let console_text = crate::connections::console_scan_text(file, max_items as i32)?;
    let settings = ScanSettings {
        decimal,
        na_strings: &na_strings,
        max_items,
        max_lines,
        flush: flush != 0,
        fill: fill != 0,
        strip: &strip,
        blank_lines_skip: blank_lines_skip != 0,
        multi_line: multi_line != 0,
        console: console_text.is_some(),
        at_start: console_text.is_none() && skip == 0,
    };
    let read = |input: &[u8], position: &mut usize| {
        let mut reader = Reader::new(input, *position, syntax);
        reader.escapes = escapes != 0;
        reader.skip_nul = skip_nul != 0;
        let mut warnings = Vec::new();
        if !settings.console {
            for _ in 0..skip {
                reader.skip_line();
            }
        }
        reader.saved = 0;
        let result = if is_list {
            scan_frame(&mut reader, &mut columns, &settings, &mut warnings)
        } else {
            scan_vector(&mut reader, &mut columns[0], &settings, &mut warnings)
        };
        *position = reader.rest_position();
        (result, warnings, reader.embedded_nul)
    };
    let (outcome, warnings, embedded_nul) = match &console_text {
        Some(text) => read(text, &mut 0),
        None => {
            let con = crate::connections::resolve(file, "invalid connection")?.1;
            let was_open = (*con).isopen != 0;
            let outcome = crate::connections::with_input(file, read)?;
            // GNU R asks a connection scan opens for UTF-8 output, which a re-encoding
            // connection gives.
            if !was_open && (*con).UTF8out != 0 {
                mark = Some(Encoding::Utf8);
            }
            outcome
        }
    };
    for message in warnings {
        warning(message)?;
    }
    let count = outcome?;
    if quiet == 0 {
        let noun = match (is_list, count) {
            (true, 1) => "record",
            (true, _) => "records",
            (false, 1) => "item",
            (false, _) => "items",
        };
        write_console(format!("Read {count} {noun}\n").as_bytes(), true);
    }
    let result = if is_list {
        let result = alloc(SexpType::List, columns.len())?;
        crate::heap().root(result);
        for (index, column) in columns.iter().enumerate() {
            values::set_elt(result, index, column_vector(column, mark)?);
        }
        let names = values::get_attr(what, symbols().names);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
        result
    } else {
        column_vector(&columns[0], mark)?
    };
    if skip_nul == 0 && embedded_nul {
        crate::heap().root(result);
        warning("embedded nul(s) found in input")?;
    }
    Ok(result)
}

/// A count argument as `asXLength` reads it: missing or negative is zero.
unsafe fn count_argument(argument: Sexp) -> usize {
    let value = values::as_real_scalar(argument);
    if value.is_finite() && value > 0.0 {
        value as usize
    } else {
        0
    }
}

/// `dec` as `scan` takes it.
unsafe fn scan_decimal(decimal: Sexp) -> R<u8> {
    match decimal.kind() {
        SexpType::String | SexpType::Nil => match first_bytes(decimal) {
            None => Ok(b'.'),
            Some([byte]) => Ok(*byte),
            Some(_) => Err(error("invalid decimal separator: must be one byte")),
        },
        _ => Err(error("invalid decimal separator")),
    }
}

/// GNU R's `scanVector`: fields into one vector; returns the number of items.
unsafe fn scan_vector(
    reader: &mut Reader<'_>,
    column: &mut Column,
    settings: &ScanSettings<'_>,
    warnings: &mut Vec<String>,
) -> R<usize> {
    let kind = column.kind;
    let strip = settings.strip.first().copied().unwrap_or(false);
    let mut buffer = Vec::new();
    let mut at_start = settings.at_start;
    let mut lines_read = 0;
    let mut previous = 0;
    let mut end = 1;
    loop {
        if end == EOF {
            break;
        }
        if end == b'\n' as i32 {
            lines_read += 1;
            if lines_read == settings.max_lines {
                break;
            }
            previous = column.values.len();
        }
        end = fill_buffer(reader, kind, strip, &mut buffer, &mut at_start, warnings);
        if previous == column.values.len()
            && buffer.is_empty()
            && ((settings.blank_lines_skip && end == b'\n' as i32) || end == EOF)
        {
            if settings.console || end == EOF {
                break;
            }
        } else {
            column.values.push(extract_item(&buffer, kind, settings)?);
            if column.values.len() == settings.max_items {
                break;
            }
        }
        if settings.flush && end != b'\n' as i32 && end != EOF {
            end = reader.skip_line();
        }
    }
    Ok(column.values.len())
}

/// GNU R's `scanFrame`: records of one field per column; returns the number of records.
unsafe fn scan_frame(
    reader: &mut Reader<'_>,
    columns: &mut [Column],
    settings: &ScanSettings<'_>,
    warnings: &mut Vec<String>,
) -> R<usize> {
    let width = columns.len();
    let per_column_strip = settings.strip.len() == width;
    let mut strip = settings.strip.first().copied().unwrap_or(false);
    let mut buffer = Vec::new();
    let mut at_start = settings.at_start;
    let mut records = 0;
    let mut lines_read = 0;
    let mut filled = 0;
    let mut end = 1;
    loop {
        if end == EOF {
            break;
        }
        if end == b'\n' as i32 {
            lines_read += 1;
            if filled != 0 {
                if settings.fill {
                    pad_record(columns, filled, settings)?;
                    records += 1;
                    filled = 0;
                } else if !settings.multi_line {
                    return Err(error(format!(
                        "line {lines_read} did not have {width} elements"
                    )));
                }
            }
            if settings.max_items > 0 && records >= settings.max_items {
                break;
            }
            if lines_read == settings.max_lines {
                break;
            }
        }
        if per_column_strip {
            strip = settings.strip[filled];
        }
        let kind = columns[filled].kind;
        end = fill_buffer(reader, kind, strip, &mut buffer, &mut at_start, warnings);
        if filled == 0
            && buffer.is_empty()
            && ((settings.blank_lines_skip && end == b'\n' as i32) || end == EOF)
        {
            if settings.console || end == EOF {
                break;
            }
        } else {
            let value = extract_item(&buffer, kind, settings)?;
            columns[filled].values.push(value);
            filled += 1;
            if filled == width {
                records += 1;
                filled = 0;
                if settings.flush && end != b'\n' as i32 && end != EOF {
                    end = reader.skip_line();
                }
            }
        }
    }
    if filled != 0 {
        if !settings.fill {
            warnings
                .push("number of items read is not a multiple of the number of columns".to_owned());
        }
        pad_record(columns, filled, settings)?;
        records += 1;
    }
    Ok(records)
}

/// Fills the rest of a short record with empty fields, which a character column keeps as `""`
/// and every other column reads as `NA`.
unsafe fn pad_record(columns: &mut [Column], from: usize, settings: &ScanSettings<'_>) -> R<()> {
    for column in columns.iter_mut().skip(from) {
        let value = extract_item(b"", column.kind, settings)?;
        column.values.push(value);
    }
    Ok(())
}

/// GNU R's `fillBuffer`: reads the next field into `buffer` and returns what ended it, a
/// separator, a line end, [`EOF`], or (between white-space separated fields) the field's
/// first byte, which is handed back. Quotes apply only to character and skipped columns, and
/// the field ends at its first nul, as a C string does.
fn fill_buffer(
    reader: &mut Reader<'_>,
    kind: Option<Kind>,
    strip: bool,
    buffer: &mut Vec<u8>,
    at_start: &mut bool,
    warnings: &mut Vec<String>,
) -> i32 {
    let is_line_end =
        |character: i32| character == b'\n' as i32 || character == b'\r' as i32 || character == EOF;
    let quoting = matches!(kind, None | Some(Kind::Character));
    buffer.clear();
    let mut kept = 0;
    let mut filled = 1;
    let mut strip = strip;
    'fill: {
        if reader.separator == 0 {
            strip = false;
            let mut character = reader.next(false);
            while character == b' ' as i32 || character == b'\t' as i32 {
                character = reader.next(false);
            }
            if is_line_end(character) {
                filled = character;
                break 'fill;
            }
            if quoting && reader.is_quote(character) {
                let quote = character;
                loop {
                    character = reader.next(true);
                    if character == EOF || character == quote {
                        break;
                    }
                    if character == b'\\' as i32 {
                        character = reader.next(true);
                        if character == EOF {
                            break;
                        }
                        if character != quote {
                            buffer.push(b'\\');
                        }
                    }
                    buffer.push(character as u8);
                }
                if character == EOF {
                    warnings.push("EOF within quoted string".to_owned());
                }
                character = reader.next(false);
                kept = buffer.len();
            } else {
                loop {
                    buffer.push(character as u8);
                    character = reader.next(false);
                    if is_space(character) || character == EOF {
                        break;
                    }
                }
            }
            while character == b' ' as i32 || character == b'\t' as i32 {
                character = reader.next(false);
            }
            if is_line_end(character) {
                filled = character;
            } else {
                reader.unread(character);
            }
            break 'fill;
        }
        let separator = reader.separator;
        loop {
            let mut character = reader.next(false);
            if character == separator || is_line_end(character) {
                filled = character;
                break 'fill;
            }
            if kind != Some(Kind::Character) {
                while character == b' ' as i32 || character == b'\t' as i32 {
                    character = reader.next(false);
                    if character == separator || is_line_end(character) {
                        filled = character;
                        break 'fill;
                    }
                }
            }
            if quoting && character != 0 && reader.is_quote(character) {
                let quote = character;
                loop {
                    character = reader.next(true);
                    while character != EOF && character != quote {
                        buffer.push(character as u8);
                        character = reader.next(true);
                    }
                    if character == EOF {
                        warnings.push("EOF within quoted string".to_owned());
                    }
                    character = reader.next(true);
                    if character != quote {
                        break;
                    }
                    buffer.push(quote as u8);
                }
                kept = buffer.len();
                if character == separator || is_line_end(character) {
                    filled = character;
                    break 'fill;
                }
                reader.unread(character);
                continue;
            }
            if !strip || !buffer.is_empty() || !is_space(character) {
                buffer.push(character as u8);
            }
        }
    }
    if strip {
        while buffer.len() > kept && buffer.last().is_some_and(|&byte| is_space(byte as i32)) {
            buffer.pop();
        }
    }
    if let Some(nul) = buffer.iter().position(|&byte| byte == 0) {
        buffer.truncate(nul);
    }
    if *at_start && buffer.starts_with(b"\xef\xbb\xbf") {
        buffer.drain(..3);
    }
    *at_start = false;
    filled
}

/// GNU R's `extractItem`: a field converted to `kind`, or the error `scan` reports for it.
unsafe fn extract_item(buffer: &[u8], kind: Option<Kind>, settings: &ScanSettings<'_>) -> R<Value> {
    let Some(kind) = kind else {
        return Ok(Value::Missing);
    };
    let is_na = |empty_is_na: bool| {
        (empty_is_na && buffer.is_empty()) || settings.na_strings.iter().any(|na| na == buffer)
    };
    let expected = |what: &str| {
        let mut message = format!("scan() expected '{what}', got '").into_bytes();
        message.extend_from_slice(buffer);
        message.push(b'\'');
        crate::conditions::c_error(&message)
    };
    if kind == Kind::Character {
        return Ok(if is_na(false) {
            Value::Missing
        } else {
            Value::Text(buffer.to_vec())
        });
    }
    if is_na(true) {
        return Ok(Value::Missing);
    }
    Ok(match kind {
        Kind::Logical => match buffer {
            b"T" | b"True" | b"TRUE" | b"true" => Value::Logical(1),
            b"F" | b"False" | b"FALSE" | b"false" => Value::Logical(0),
            _ => return Err(expected("a logical")),
        },
        Kind::Integer => match parse_integer(buffer) {
            Some(value) => Value::Integer(value),
            None => return Err(expected("an integer")),
        },
        Kind::Double => {
            let (value, end) =
                parse_double_prefix(buffer, settings.decimal, false, Exactness::Allow)?;
            if !is_blank(&buffer[end..])? {
                return Err(expected("a real"));
            }
            Value::Double(value)
        }
        Kind::Complex => {
            let (value, end) =
                parse_complex_prefix(buffer, settings.decimal, false, Exactness::Allow, false)?;
            if !is_blank(&buffer[end..])? {
                return Err(expected("a complex"));
            }
            Value::Complex(value)
        }
        Kind::Raw => {
            let start = buffer
                .iter()
                .take_while(|&&byte| is_space(byte as i32))
                .count();
            let mut value = 0u32;
            let mut end = start;
            for _ in 0..2 {
                match buffer
                    .get(end)
                    .and_then(|&byte| (byte as char).to_digit(16))
                {
                    Some(digit) => {
                        value = 16 * value + digit;
                        end += 1;
                    }
                    None => {
                        value = 0;
                        break;
                    }
                }
            }
            if !is_blank(&buffer[end..])? {
                return Err(expected("a raw"));
            }
            Value::Raw(value as u8)
        }
        Kind::Character => Value::Missing,
    })
}

/// GNU R's `Strtoi`: a whole decimal integer, after optional white space, that fits an R
/// integer; `None` for anything else, `NA`'s bit pattern included.
fn parse_integer(text: &[u8]) -> Option<i32> {
    let mut position = text.iter().take_while(|&&byte| is_c_space(byte)).count();
    let negative = match text.get(position) {
        Some(b'-') => {
            position += 1;
            true
        }
        Some(b'+') => {
            position += 1;
            false
        }
        _ => false,
    };
    let digits = text[position..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return text.is_empty().then_some(0);
    }
    if position + digits != text.len() {
        return None;
    }
    let mut value: i64 = 0;
    for &byte in &text[position..] {
        value = value.checked_mul(10)?.checked_add((byte - b'0') as i64)?;
        if value > i32::MAX as i64 + 1 {
            return None;
        }
    }
    let value = if negative { -value } else { value };
    (value > i32::MIN as i64 && value <= i32::MAX as i64).then_some(value as i32)
}

/// GNU R's `R_strtod5`: the double at the start of `text` and the bytes it spans, 0 when there
/// is none. With `na`, `NA` reads as missing.
unsafe fn parse_double_prefix(
    text: &[u8],
    decimal: u8,
    na: bool,
    exactness: Exactness,
) -> R<(f64, usize)> {
    let number = strtod(text, decimal, na, exactness);
    if number.accuracy_lost {
        warning(format!(
            "accuracy loss in conversion from \"{}\" to numeric",
            values::escape_invalid_bytes(text)
        ))?;
    }
    Ok((number.value, number.length))
}

/// GNU R's `strtoc`: the complex number at the start of `text` (`a`, `bi` or `a+bi`) and the
/// bytes it spans. `scan` reads a failure as zero and `type.convert` (`missing_on_failure`)
/// as `NA`, which also refuses a lone `i`.
unsafe fn parse_complex_prefix(
    text: &[u8],
    decimal: u8,
    na: bool,
    exactness: Exactness,
    missing_on_failure: bool,
) -> R<(Rcomplex, usize)> {
    let failed = if missing_on_failure { NA_REAL } else { 0.0 };
    let (real, end) = parse_double_prefix(text, decimal, na, exactness)?;
    if is_blank(&text[end..])? {
        return Ok((Rcomplex { r: real, i: 0.0 }, end));
    }
    if text[end] == b'i' {
        if missing_on_failure && end == 0 {
            return Ok((
                Rcomplex {
                    r: NA_REAL,
                    i: NA_REAL,
                },
                0,
            ));
        }
        return Ok((Rcomplex { r: 0.0, i: real }, end + 1));
    }
    let (imaginary, length) = parse_double_prefix(&text[end..], decimal, na, exactness)?;
    if text.get(end + length) == Some(&b'i') {
        return Ok((
            Rcomplex {
                r: real,
                i: imaginary,
            },
            end + length + 1,
        ));
    }
    Ok((
        Rcomplex {
            r: failed,
            i: failed,
        },
        0,
    ))
}

unsafe fn column_vector(column: &Column, mark: Option<Encoding>) -> R<Sexp> {
    let Some(kind) = column.kind else {
        return Ok(nil());
    };
    let count = column.values.len();
    let vector = alloc(kind.sexp_type(), count)?;
    for (index, value) in column.values.iter().enumerate() {
        match kind {
            Kind::Logical | Kind::Integer => {
                values::ints_mut(vector)[index] = match value {
                    Value::Logical(value) | Value::Integer(value) => *value,
                    _ => NA_INTEGER,
                }
            }
            Kind::Double => {
                values::reals_mut(vector)[index] = if let Value::Double(value) = value {
                    *value
                } else {
                    NA_REAL
                }
            }
            Kind::Complex => {
                values::complexes_mut(vector)[index] = if let Value::Complex(value) = value {
                    *value
                } else {
                    Rcomplex {
                        r: NA_REAL,
                        i: NA_REAL,
                    }
                }
            }
            Kind::Raw => {
                values::raws_mut(vector)[index] = if let Value::Raw(value) = value {
                    *value
                } else {
                    0
                }
            }
            Kind::Character => {
                let charsxp = match value {
                    Value::Text(text) => read_string(text, mark)?,
                    _ => na_string(),
                };
                values::set_elt(vector, index, charsxp);
            }
        }
    }
    Ok(vector)
}

/// A string read from input, holding the input's bytes. Text read without a declared
/// `encoding` is native (`"unknown"`) even when it is valid UTF-8, as GNU R's `mkChar` makes it,
/// and ASCII text carries no mark.
unsafe fn read_string(text: &[u8], mark: Option<Encoding>) -> R<Sexp> {
    let encoding = match mark {
        Some(mark) if !text.is_ascii() => mark,
        _ => Encoding::Native,
    };
    values::mkchar_bytes(text, encoding)
}

// readtablehead and countfields.

/// `readtablehead(file, nlines, comment.char, blank.lines.skip, quote, sep, skipNul)`, a port
/// of GNU R's: the first `nlines` lines that are not blank, a quoted field running over line
/// ends joining its lines into one.
unsafe fn do_read_table_head(call: &NativeCall<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let count = values::as_integer_scalar(call.arg(1));
    let mut blank_lines_skip = values::as_logical_scalar(call.arg(3));
    if count == NA_INTEGER || count <= 0 {
        return Err(error("invalid 'nlines' argument"));
    }
    if blank_lines_skip == NA_LOGICAL {
        blank_lines_skip = 1;
    }
    let quotes = quote_set(call.arg(4))?;
    let comment = comment_char(call.arg(2))?;
    let separator = first_byte_separator(call.arg(5))?;
    let skip_nul = values::as_logical_scalar(call.arg(6));
    if skip_nul == NA_LOGICAL {
        return Err(error("invalid 'skipNul' argument"));
    }
    let con = crate::connections::resolve(file, "invalid connection")?.1;
    let console = crate::connections::console_scan_text(file, 0)?;
    let syntax = Syntax {
        separator,
        quotes,
        comment,
    };
    let read = |input: &[u8], position: &mut usize| {
        let mut reader = Reader::new(input, *position, syntax);
        reader.skip_nul = skip_nul != 0;
        let outcome = head_lines(
            &mut reader,
            count as usize,
            blank_lines_skip != 0,
            console.is_some(),
        );
        *position = reader.rest_position();
        outcome
    };
    let outcome = match &console {
        Some(text) => read(text, &mut 0),
        None => crate::connections::with_input(file, read)?,
    };
    let HeadLines {
        lines,
        nul_lines,
        incomplete,
    } = outcome?;
    for line in nul_lines {
        warning(format!("line {line} appears to contain embedded nulls"))?;
    }
    if incomplete {
        let message = format!(
            "incomplete final line found by readTableHeader on '{}'",
            crate::connections::description(file).unwrap_or_default()
        );
        if (*con).text != 0 && (*con).blocking != 0 {
            warning(message)?;
        } else {
            return Err(error(message));
        }
    }
    let vector = alloc(SexpType::String, lines.len())?;
    for (index, line) in lines.iter().enumerate() {
        values::set_elt(vector, index, read_string(line, None)?);
    }
    Ok(vector)
}

struct HeadLines {
    lines: Vec<Vec<u8>>,
    /// The (1-based) numbers of the lines a nul cut short.
    nul_lines: Vec<usize>,
    /// Whether the input ended inside a line.
    incomplete: bool,
}

fn head_lines(
    reader: &mut Reader<'_>,
    count: usize,
    blank_lines_skip: bool,
    console: bool,
) -> R<HeadLines> {
    let mut head = HeadLines {
        lines: Vec::new(),
        nul_lines: Vec::new(),
        incomplete: false,
    };
    let separator = reader.separator;
    let comment = reader.comment;
    let mut quote = 0;
    let mut line = Vec::new();
    while head.lines.len() < count {
        line.clear();
        let mut empty = true;
        let mut skip = false;
        let mut first_nonwhite = true;
        let mut character;
        loop {
            character = reader.next(true);
            if character == EOF {
                break;
            }
            if quote != 0 {
                if separator == 0 && character == b'\\' as i32 {
                    line.push(character as u8);
                    character = reader.next(true);
                    if character == EOF {
                        return Err(error("\\ followed by EOF"));
                    }
                    line.push(character as u8);
                    continue;
                } else if character == quote {
                    if separator == 0 {
                        quote = 0;
                    } else {
                        let next = reader.next(true);
                        if next == quote {
                            line.push(character as u8);
                        } else {
                            reader.unread(next);
                            quote = 0;
                        }
                    }
                }
            } else if !skip && (first_nonwhite || separator != 0) && reader.is_quote(character) {
                quote = character;
            } else if !skip && separator == 0 && is_space(character) {
                first_nonwhite = true;
            } else if character != b' ' as i32 && character != b'\t' as i32 {
                first_nonwhite = false;
            }
            if empty && !skip && character != b'\n' as i32 && character != comment {
                empty = false;
            }
            if quote == 0 && !skip && character == comment {
                skip = true;
            }
            if quote == 0 && character == b'\n' as i32 {
                break;
            }
            line.push(character as u8);
        }
        if console && empty {
            break;
        }
        if !empty || (character != EOF && !blank_lines_skip) {
            let mut stored = line.clone();
            if let Some(nul) = stored.iter().position(|&byte| byte == 0) {
                stored.truncate(nul);
                head.nul_lines.push(head.lines.len() + 1);
            }
            head.lines.push(stored);
        }
        if character == EOF {
            head.incomplete = !line.is_empty();
            break;
        }
    }
    Ok(head)
}

/// `countfields(file, sep, quote, skip, blank.lines.skip, comment.char)`, a port of GNU R's:
/// the number of fields on each line, `NA` for a line a quoted field runs over.
unsafe fn do_count_fields(call: &NativeCall<'_>) -> R<Sexp> {
    let file = call.arg(0);
    let comment = comment_char(call.arg(5))?;
    let skip = values::as_integer_scalar(call.arg(3));
    let skip = if skip == NA_INTEGER || skip < 0 {
        0
    } else {
        skip as usize
    };
    let mut blank_lines_skip = values::as_logical_scalar(call.arg(4));
    if blank_lines_skip == NA_LOGICAL {
        blank_lines_skip = 1;
    }
    let separator = first_byte_separator(call.arg(1))?;
    let syntax = Syntax {
        separator,
        quotes: quote_set(call.arg(2))?,
        comment,
    };
    let console = crate::connections::console_scan_text(file, 0)?;
    let read = |input: &[u8], position: &mut usize| {
        let mut reader = Reader::new(input, *position, syntax);
        if console.is_none() {
            for _ in 0..skip {
                reader.skip_line();
            }
        }
        reader.saved = 0;
        let counts = count_fields(&mut reader, blank_lines_skip != 0);
        *position = reader.rest_position();
        counts
    };
    let counts = match &console {
        Some(text) => read(text, &mut 0),
        None => crate::connections::with_input(file, read)?,
    }?;
    match counts {
        Some(counts) => values::integer_vector(&counts),
        None => Ok(nil()),
    }
}

/// The field counts of the lines `reader` has left, `None` when there are none.
fn count_fields(reader: &mut Reader<'_>, blank_lines_skip: bool) -> R<Option<Vec<i32>>> {
    let mut counts: Vec<i32> = Vec::new();
    let set = |counts: &mut Vec<i32>, line: usize, value: i32| {
        if counts.len() <= line {
            counts.resize(line + 1, 0);
        }
        counts[line] = value;
    };
    let mut lines: usize = 0;
    let mut fields = 0;
    let mut quoted_on: usize = 0;
    let mut quote = 0;
    loop {
        let mut character = reader.next(quoted_on > 0);
        if character == EOF {
            if fields != 0 {
                set(&mut counts, lines, fields);
            } else if lines == 0 {
                return Ok(None);
            } else {
                lines -= 1;
            }
            break;
        }
        if character == b'\n' as i32 {
            if quoted_on != 0 {
                set(&mut counts, lines, NA_INTEGER);
                lines += 1;
            } else if fields != 0 || !blank_lines_skip {
                set(&mut counts, lines, fields);
                lines += 1;
                fields = 0;
                quoted_on = 0;
            }
            continue;
        }
        if reader.separator != 0 {
            if fields == 0 {
                fields += 1;
            }
            if quoted_on != 0 {
                if character == quote {
                    quoted_on = 0;
                }
            } else if reader.is_quote(character) {
                quoted_on = lines + 1;
                quote = character;
            }
            if character == reader.separator && quoted_on == 0 {
                fields += 1;
            }
        } else if !is_space(character) {
            if reader.is_quote(character) {
                quote = character;
                quoted_on = lines + 1;
                loop {
                    character = reader.next(true);
                    if character == quote {
                        break;
                    }
                    if character == EOF {
                        return Err(error(format!(
                            "quoted string on line {quoted_on} terminated by EOF"
                        )));
                    }
                    if character == b'\n' as i32 {
                        set(&mut counts, lines, NA_INTEGER);
                        lines += 1;
                    }
                }
                quoted_on = 0;
            } else {
                loop {
                    character = reader.next(false);
                    if is_space(character) || character == EOF {
                        break;
                    }
                }
                if character == EOF {
                    character = b'\n' as i32;
                }
                reader.unread(character);
            }
            fields += 1;
        }
    }
    counts.resize(lines + 1, 0);
    Ok(Some(counts))
}

// type.convert.

/// `typeconvert(x, na.strings, as.is, dec, numerals, tryLogical)`, a port of GNU R's: the
/// first of logical, integer, double and complex that reads every element, or character (a
/// factor unless `as.is`). Empty and blank strings and `na.strings` are missing.
unsafe fn do_type_convert(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x.kind() != SexpType::String {
        return Err(error("the first argument must be of mode character"));
    }
    let na_strings = call.arg(1);
    if na_strings.kind() != SexpType::String {
        return Err(error("invalid 'na.strings' argument"));
    }
    let na_strings: Vec<Vec<u8>> = (0..length(na_strings))
        .map(|index| na_strings.element(index).bytes().to_vec())
        .collect();
    let as_is = values::as_logical_scalar(call.arg(2)) == 1;
    let decimal_argument = call.arg(3);
    let decimal = match decimal_argument.kind() {
        SexpType::String | SexpType::Nil => {
            first_bytes(decimal_argument).map_or(b'.', |bytes| bytes.first().copied().unwrap_or(0))
        }
        _ => b'.',
    };
    let (exactness, refuse) = match first_bytes(call.arg(4)) {
        None | Some(b"allow.loss") => (Exactness::Allow, Exactness::Allow),
        Some(b"warn.loss") => (Exactness::Warn, Exactness::Allow),
        Some(b"no.loss") => (Exactness::Refuse, Exactness::Refuse),
        Some(other) => {
            return Err(error(format!(
                "invalid 'numerals' string: \"{}\"",
                values::escape_invalid_bytes(other)
            )));
        }
    };
    let try_logical = values::as_logical_scalar(call.arg(5)) == 1;
    let count = length(x);
    let elements: Vec<&[u8]> = (0..count).map(|index| x.element(index).bytes()).collect();
    let is_na_string = |text: &[u8]| na_strings.iter().any(|na| na == text);
    let missing = |index: usize| {
        let text = elements[index];
        R::Ok(
            x.element(index) == na_string()
                || text.is_empty()
                || is_na_string(text)
                || is_blank(text)?,
        )
    };
    let mut possible = Possible {
        logical: try_logical,
        integer: true,
        real: true,
        complex: true,
    };
    for index in 0..count {
        if !missing(index)? {
            possible.rule_out(elements[index], decimal, refuse)?;
            break;
        }
    }
    let mut result = None;
    if possible.logical {
        let vector = alloc(SexpType::Logical, count)?;
        for index in 0..count {
            values::ints_mut(vector)[index] = match elements[index] {
                _ if missing(index)? => NA_LOGICAL,
                b"F" | b"FALSE" => 0,
                b"T" | b"TRUE" => 1,
                text => {
                    possible.logical = false;
                    possible.rule_out(text, decimal, refuse)?;
                    break;
                }
            };
        }
        if possible.logical {
            result = Some(vector);
        }
    }
    if result.is_none() && possible.integer {
        let vector = alloc(SexpType::Integer, count)?;
        for index in 0..count {
            values::ints_mut(vector)[index] = if missing(index)? {
                NA_INTEGER
            } else {
                match parse_integer(elements[index]) {
                    Some(value) => value,
                    None => {
                        possible.integer = false;
                        possible.rule_out(elements[index], decimal, refuse)?;
                        break;
                    }
                }
            };
        }
        if possible.integer {
            result = Some(vector);
        }
    }
    if result.is_none() && possible.real {
        let vector = alloc(SexpType::Real, count)?;
        crate::heap().root(vector);
        for index in 0..count {
            values::reals_mut(vector)[index] = if missing(index)? {
                NA_REAL
            } else {
                let text = elements[index];
                let (value, end) = parse_double_prefix(text, decimal, false, exactness)?;
                if !is_blank(&text[end..])? {
                    possible.real = false;
                    possible.rule_out(text, decimal, refuse)?;
                    break;
                }
                value
            };
        }
        if possible.real {
            result = Some(vector);
        }
    }
    if result.is_none() && possible.complex {
        let vector = alloc(SexpType::Complex, count)?;
        crate::heap().root(vector);
        for index in 0..count {
            values::complexes_mut(vector)[index] = if missing(index)? {
                Rcomplex {
                    r: NA_REAL,
                    i: NA_REAL,
                }
            } else {
                let text = elements[index];
                let (value, end) = parse_complex_prefix(text, decimal, false, exactness, true)?;
                if !is_blank(&text[end..])? {
                    possible.complex = false;
                    break;
                }
                value
            };
        }
        if possible.complex {
            result = Some(vector);
        }
    }
    let is_array = values::get_attr(x, symbols().dim) != nil();
    let result = match result {
        Some(result) => result,
        None if as_is => {
            let result = values::duplicate(x)?;
            for index in 0..count {
                if is_na_string(values::string_elt(result, index).bytes()) {
                    values::set_elt(result, index, na_string());
                }
            }
            result
        }
        None => character_factor(x, &is_na_string)?,
    };
    crate::heap().root(result);
    values::set_attr(result, symbols().dim, values::get_attr(x, symbols().dim))?;
    let names = if is_array {
        symbols().dimnames
    } else {
        symbols().names
    };
    values::set_attr(result, names, values::get_attr(x, names))?;
    Ok(result)
}

/// The types `type.convert` has not ruled out.
struct Possible {
    logical: bool,
    integer: bool,
    real: bool,
    complex: bool,
}

impl Possible {
    /// GNU R's `ruleout_types`: rules out the types `text` cannot be read as, `NA` reading as
    /// a missing number.
    unsafe fn rule_out(&mut self, text: &[u8], decimal: u8, exactness: Exactness) -> R<()> {
        if self.logical {
            if matches!(text, b"F" | b"T" | b"FALSE" | b"TRUE") {
                self.integer = false;
                self.real = false;
                self.complex = false;
                return Ok(());
            }
            self.logical = false;
        }
        if self.integer && parse_integer(text).is_none() {
            self.integer = false;
        }
        if self.real {
            let (_, end) = parse_double_prefix(text, decimal, true, exactness)?;
            if !is_blank(&text[end..])? {
                self.real = false;
            }
        }
        if self.complex {
            let (_, end) = parse_complex_prefix(text, decimal, true, exactness, true)?;
            if !is_blank(&text[end..])? {
                self.complex = false;
            }
        }
        Ok(())
    }
}

/// The factor `type.convert` makes of strings that are not numbers: its levels are the
/// distinct strings that are not missing, each the first string that spells it, in collation
/// order. GNU R's Shell sort places the levels the collation calls equal.
unsafe fn character_factor(x: Sexp, is_na_string: &dyn Fn(&[u8]) -> bool) -> R<Sexp> {
    let strings = values::strings(x);
    let mut levels: Vec<&str> = Vec::new();
    let mut level_elements: Vec<Sexp> = Vec::new();
    let mut seen = FxHashSet::default();
    for (index, text) in strings.iter().enumerate() {
        let Some(text) = text.as_deref() else {
            continue;
        };
        if is_na_string(x.element(index).bytes()) || !seen.insert(text) {
            continue;
        }
        levels.push(text);
        level_elements.push(x.element(index));
    }
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let collator = collator();
    shell_sort(&mut order, |left, right| {
        collator.compare(levels[left], levels[right]).is_gt()
    });
    drop(collator);
    let code_of: FxHashMap<&str, i32> = order
        .iter()
        .zip(1..)
        .map(|(&index, code)| (levels[index], code))
        .collect();
    let result = alloc(SexpType::Integer, strings.len())?;
    crate::heap().root(result);
    for (index, text) in strings.iter().enumerate() {
        values::ints_mut(result)[index] = text
            .as_deref()
            .and_then(|text| code_of.get(text).copied())
            .unwrap_or(NA_INTEGER);
    }
    let level_strings = alloc(SexpType::String, levels.len())?;
    crate::heap().root(level_strings);
    for (position, &index) in order.iter().enumerate() {
        values::set_elt(level_strings, position, level_elements[index]);
    }
    values::set_attr(result, symbols().levels, level_strings)?;
    let class = scalar_string("factor")?;
    values::set_attr(result, symbols().class, class)?;
    Ok(result)
}

// write.table.

/// `writetable(x, file, nrow, ncol, rnames, sep, eol, na, dec, quote, qmethod)`: the rows of a
/// data frame or matrix as delimited text, a port of GNU R's `writetable`. Numbers are written to
/// 15 significant digits, strings in the quoted columns between double quotes, with embedded
/// quotes escaped by a backslash when `qmethod` is `TRUE` and doubled otherwise. GNU R prints each
/// piece as it goes, so an error leaves the rows before it in the file; the buffered output is
/// written before the error is raised.
unsafe fn do_write_table(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let file = call.arg(1);
    let rows = values::as_integer_scalar(call.arg(2));
    let columns = values::as_integer_scalar(call.arg(3));
    let row_names = call.arg(4);
    let [separator, eol, na, decimal] = [5, 6, 7, 8].map(|index| call.arg(index));
    let escape = values::as_logical_scalar(call.arg(10));
    if rows == NA_INTEGER {
        return Err(error("invalid 'nr' argument"));
    }
    if columns == NA_INTEGER {
        return Err(error("invalid 'nc' argument"));
    }
    if row_names != nil() && row_names.kind() != SexpType::String {
        return Err(error("invalid 'rnames' argument"));
    }
    for (value, name) in [
        (separator, "sep"),
        (eol, "eol"),
        (na, "na"),
        (decimal, "dec"),
    ] {
        if value.kind() != SexpType::String {
            return Err(error(format!("invalid '{name}' argument")));
        }
    }
    if escape == NA_LOGICAL {
        return Err(error("invalid 'qmethod' argument"));
    }
    let first = |strings: Sexp| -> R<Vec<u8>> {
        if length(strings) == 0 {
            return Err(error("subscript out of bounds"));
        }
        let mut bytes = Vec::new();
        write_string(strings.element(0), false, false, &mut bytes)?;
        Ok(bytes)
    };
    let separator = first(separator)?;
    let eol = first(eol)?;
    let na = first(na)?;
    let decimal = first(decimal)?;
    let &[decimal] = decimal.as_slice() else {
        return Err(error("'dec' must be a single character"));
    };
    let rows = rows.max(0) as usize;
    let columns = columns.max(0) as usize;
    let quote = coerce_vector(call.arg(9), SexpType::Integer)?;
    let mut quote_row_names = false;
    let mut quote_columns = vec![false; columns];
    for &entry in values::ints(quote) {
        if entry == 0 {
            quote_row_names = true;
        }
        if entry > 0
            && let Some(slot) = quote_columns.get_mut(entry as usize - 1)
        {
            *slot = true;
        }
    }
    let settings = Settings {
        escape: escape == 1,
        decimal,
        scipen: crate::format::Settings::current_scipen(),
    };
    let is_frame = x.kind() == SexpType::List;
    let mut levels = Vec::with_capacity(columns);
    if is_frame {
        for column in 0..columns {
            let vector = x.element(column);
            if length(vector) != rows {
                return Err(error(format!(
                    "corrupt data frame -- length of column {} does not match nrows",
                    column + 1
                )));
            }
            levels.push(
                values::inherits(vector, "factor")
                    .then(|| values::get_attr(vector, symbols().levels)),
            );
        }
    } else if length(x) != rows * columns {
        return Err(error("corrupt matrix -- dims do not match length"));
    }
    let mut output = Vec::new();
    let written = (|| -> R<()> {
        for row in 0..rows {
            if row_names != nil() {
                write_element(row_names, row, quote_row_names, &settings, &mut output)?;
                output.extend_from_slice(&separator);
            }
            for (column, &quoted) in quote_columns.iter().enumerate() {
                if column > 0 {
                    output.extend_from_slice(&separator);
                }
                let (vector, index) = if is_frame {
                    (x.element(column), row)
                } else {
                    (x, row + column * rows)
                };
                if is_missing(vector, index) {
                    output.extend_from_slice(&na);
                    continue;
                }
                match levels.get(column).copied().flatten() {
                    Some(level_strings) => {
                        let code = match vector.kind() {
                            SexpType::Integer => values::ints(vector)[index] as i64 - 1,
                            SexpType::Real => (values::reals(vector)[index] - 1.0) as i64,
                            _ => {
                                return Err(error(format!(
                                    "column {} claims to be a factor but does not have numeric codes",
                                    column + 1
                                )));
                            }
                        };
                        let code = usize::try_from(code).unwrap_or(usize::MAX);
                        write_element(level_strings, code, quoted, &settings, &mut output)?;
                    }
                    None => write_element(vector, index, quoted, &settings, &mut output)?,
                }
            }
            output.extend_from_slice(&eol);
            if output.len() > 1 << 16 {
                crate::connections::print(file, &output)?;
                output.clear();
            }
        }
        Ok(())
    })();
    crate::connections::print(file, &output)?;
    written?;
    Ok(nil())
}

/// How `write.table` spells the elements it writes.
struct Settings {
    escape: bool,
    decimal: u8,
    scipen: i32,
}

/// GNU R's `isna` in io.c: which elements `write.table` writes as `na`. A double is missing when
/// it is any NaN, a complex number when either part is.
unsafe fn is_missing(vector: Sexp, index: usize) -> bool {
    if index >= length(vector) {
        return false;
    }
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(vector)[index] == NA_INTEGER,
        SexpType::Real => values::reals(vector)[index].is_nan(),
        SexpType::String => vector.element(index) == na_string(),
        SexpType::Complex => {
            let value = values::complexes(vector)[index];
            value.r.is_nan() || value.i.is_nan()
        }
        _ => false,
    }
}

/// GNU R's `EncodeElement2`: element `index` of `vector`, a string between double quotes when
/// `quoted`.
unsafe fn write_element(
    vector: Sexp,
    index: usize,
    quoted: bool,
    settings: &Settings,
    output: &mut Vec<u8>,
) -> R<()> {
    if index >= length(vector) {
        return Err(error("index out of range"));
    }
    let with_decimal = |text: String| -> Vec<u8> {
        text.into_bytes()
            .into_iter()
            .map(|byte| if byte == b'.' { settings.decimal } else { byte })
            .collect()
    };
    let text: Vec<u8> = match vector.kind() {
        SexpType::String => {
            return write_string(vector.element(index), quoted, settings.escape, output);
        }
        SexpType::Logical => match values::ints(vector)[index] {
            NA_LOGICAL => b"NA".to_vec(),
            0 => b"FALSE".to_vec(),
            _ => b"TRUE".to_vec(),
        },
        SexpType::Integer => match values::ints(vector)[index] {
            NA_INTEGER => b"NA".to_vec(),
            value => value.to_string().into_bytes(),
        },
        SexpType::Real => with_decimal(format_real_alone_scipen(
            values::reals(vector)[index],
            15,
            settings.scipen,
        )),
        SexpType::Complex => with_decimal(format_complex_alone(
            values::complexes(vector)[index],
            15,
            0,
        )),
        SexpType::Raw => format!("{:02x}", values::raws(vector)[index]).into_bytes(),
        _ => {
            let element = crate::subset::list_or_vector_element(vector, index)?;
            let text = coerce_vector(element, SexpType::String)?;
            values::first_string(text)
                .map_or_else(|| b"NA".to_vec(), |text| text.into_owned().into_bytes())
        }
    };
    output.extend_from_slice(&text);
    Ok(())
}

/// A string as `translateChar` gives it to `write.table`: Latin-1 text in UTF-8, other text as
/// its bytes up to the first NUL, and `NA` for the missing string. Non-ASCII text declared as
/// bytes cannot be translated.
unsafe fn write_string(charsxp: Sexp, quoted: bool, escape: bool, output: &mut Vec<u8>) -> R<()> {
    let raw = charsxp.bytes();
    let raw = raw
        .iter()
        .position(|&byte| byte == 0)
        .map_or(raw, |end| &raw[..end]);
    let latin1;
    let bytes: &[u8] = if charsxp == na_string() {
        b"NA"
    } else if raw.is_ascii() {
        raw
    } else if charsxp.has_gp(gp::BYTES) {
        return Err(error(
            "translating strings with \"bytes\" encoding is not allowed",
        ));
    } else if charsxp.has_gp(gp::LATIN1) {
        latin1 = values::char_str(charsxp);
        latin1.as_bytes()
    } else {
        raw
    };
    if !quoted {
        output.extend_from_slice(bytes);
        return Ok(());
    }
    output.push(b'"');
    for &byte in bytes {
        if byte == b'"' {
            output.push(if escape { b'\\' } else { b'"' });
        }
        output.push(byte);
    }
    output.push(b'"');
    Ok(())
}

// Checksums and sizes.

/// `crc64(x)`: the CRC-64 of the first string, in lower-case hexadecimal.
unsafe fn do_crc64(call: &NativeCall<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("input must be a character string"));
    }
    let bytes = values::first_string(text)
        .map(|text| text.into_owned())
        .unwrap_or_default();
    scalar_string(&crate::system::crc64_hex(bytes.as_bytes()))
}

/// `objectSize(x)`: the bytes GNU R would use for `x` on a 64-bit platform, which is what
/// `object.size` reports.
unsafe fn do_object_size(call: &NativeCall<'_>) -> R<Sexp> {
    values::scalar_real(object_size(call.arg(0)) as f64)
}

const NODE_SIZE: usize = 56;
const VECTOR_HEADER: usize = 48;

/// The data part of a vector of `bytes`, as GNU R allocates it: small vectors come in classes
/// of 8, 16, 32, 48, 64 and 128 bytes, larger ones in multiples of 8.
fn vector_data_size(bytes: usize) -> usize {
    match bytes {
        0 => 0,
        1..=8 => 8,
        9..=16 => 16,
        17..=32 => 32,
        33..=48 => 48,
        49..=64 => 64,
        65..=128 => 128,
        _ => bytes.div_ceil(8) * 8,
    }
}

unsafe fn object_size(object: Sexp) -> usize {
    let size = match object.kind() {
        SexpType::Nil => return 0,
        SexpType::Symbol
        | SexpType::Environment
        | SexpType::Builtin
        | SexpType::Special
        | SexpType::ExternalPtr
        | SexpType::WeakRef
        | SexpType::Bytecode
        | SexpType::Object => NODE_SIZE,
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            NODE_SIZE
                + object_size(object.tag())
                + object_size(object.car())
                + object_size(object.cdr())
        }
        SexpType::Closure => {
            NODE_SIZE
                + object_size(object.car())
                + object_size(crate::bytecode::closure_body(object))
        }
        SexpType::Promise => NODE_SIZE + object_size(object.car()) + object_size(object.cdr()),
        SexpType::Char => VECTOR_HEADER + vector_data_size(object.bytes().len() + 1),
        SexpType::String => {
            let count = length(object);
            let mut seen = FxHashSet::default();
            let mut strings = 0;
            for index in 0..count {
                let charsxp = object.element(index);
                if charsxp != na_string() && seen.insert(charsxp.as_ptr() as usize) {
                    strings += object_size(charsxp);
                }
            }
            VECTOR_HEADER + vector_data_size(count * 8) + strings
        }
        SexpType::List | SexpType::Expression => {
            let count = length(object);
            let elements: usize = (0..count)
                .map(|index| object_size(object.element(index)))
                .sum();
            VECTOR_HEADER + vector_data_size(count * 8) + elements
        }
        kind => {
            let element = kind.element_size().unwrap_or(8);
            VECTOR_HEADER + vector_data_size(length(object) * element)
        }
    };
    size + object_size(object.attrib())
}

/// `octsize(size)`: the low 11 octal digits of the size, as the raw bytes of a tar header
/// field. A missing size leaves the field zeroed.
unsafe fn do_octal_size(call: &NativeCall<'_>) -> R<Sexp> {
    let size = values::as_real_scalar(call.arg(0));
    let result = alloc(SexpType::Raw, 11)?;
    if !size.is_nan() {
        let bits = size as i64;
        for (index, byte) in values::raws_mut(result).iter_mut().enumerate() {
            *byte = b'0' + ((bits >> (3 * (10 - index))) & 7) as u8;
        }
    }
    Ok(result)
}

/// `charClass(x, class)`: whether each character of `x` (a string, or code points) belongs to
/// the named character class.
unsafe fn do_char_class(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let characters: Vec<Option<char>> = match x.kind() {
        SexpType::String if length(x) == 1 => values::string_at(x, 0)
            .map(|text| text.chars().map(Some).collect())
            .unwrap_or_default(),
        SexpType::Integer | SexpType::Real => {
            let codes = coerce_vector(x, SexpType::Integer)?;
            values::ints(codes)
                .iter()
                .map(|code| u32::try_from(*code).ok().and_then(char::from_u32))
                .collect()
        }
        _ => {
            return Err(error(
                "argument 'x' must be a length-1 character vector or an integer vector",
            ));
        }
    };
    let class = values::require_str(call.arg(1), "class")?;
    let test: fn(char) -> bool = match class.as_ref() {
        "alnum" => char::is_alphanumeric,
        "alpha" => char::is_alphabetic,
        "blank" => |character| character == ' ' || character == '\t',
        "cntrl" => char::is_control,
        "digit" => |character| character.is_ascii_digit(),
        "graph" => |character| !character.is_control() && !character.is_whitespace(),
        "lower" => char::is_lowercase,
        "print" => |character| !character.is_control(),
        "punct" => |character| {
            !character.is_control() && !character.is_whitespace() && !character.is_alphanumeric()
        },
        "space" => char::is_whitespace,
        "upper" => char::is_uppercase,
        "xdigit" => |character| character.is_ascii_hexdigit(),
        _ => {
            return Err(error(
                "character class \"{class}\" is invalid".replace("{class}", &class),
            ));
        }
    };
    let result = alloc(SexpType::Logical, characters.len())?;
    for (index, character) in characters.iter().enumerate() {
        values::ints_mut(result)[index] =
            character.map_or(NA_LOGICAL, |character| test(character) as i32);
    }
    Ok(result)
}

// The console, the network and profiling.

unsafe fn do_flush_console(_: &NativeCall<'_>) -> R<Sexp> {
    std::io::stdout()
        .flush()
        .map_err(|failure| error(format!("cannot flush the console: {failure}")))?;
    Ok(nil())
}

/// `nsl(hostname)`: the host's IPv4 address, or `NULL` with a warning when it does not resolve.
unsafe fn do_nsl(call: &NativeCall<'_>) -> R<Sexp> {
    use std::net::ToSocketAddrs;
    let host = values::require_str(call.arg(0), "hostname")?;
    let address = (host.as_ref(), 0)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.find(std::net::SocketAddr::is_ipv4));
    match address {
        Some(address) => scalar_string(&address.ip().to_string()),
        None => {
            warning(format!("nsl() was unable to resolve host '{host}'"))?;
            Ok(nil())
        }
    }
}

/// GNU R's profiling timer cannot sample more often than this, so it raises shorter intervals
/// to it with a warning.
const MINIMUM_PROFILE_MICROS: u64 = 10_000;

struct Profiler {
    output: BufWriter<File>,
    stop: Arc<AtomicBool>,
    timer: std::thread::JoinHandle<()>,
}

thread_local! {
    static PROFILER: RefCell<Option<Profiler>> = const { RefCell::new(None) };
}

/// `Rprof(filename, append, interval, ...)`: starts writing call-stack samples to `filename`
/// in GNU R's format, which `summaryRprof` reads, or stops profiling for an empty or `NULL`
/// filename. Only the call stack is sampled; memory, GC and line profiling are not supported.
unsafe fn do_rprof(call: &NativeCall<'_>) -> R<Sexp> {
    stop_profiler()?;
    let Some(filename) = values::first_string(call.arg(0)).filter(|name| !name.is_empty()) else {
        return Ok(nil());
    };
    let append = values::as_logical_scalar(call.arg(1)) == 1;
    let interval = values::as_real_scalar(call.arg(2));
    let interval = if interval.is_nan() { 0.02 } else { interval };
    let micros = if interval * 1e6 < MINIMUM_PROFILE_MICROS as f64 {
        warning(format!(
            "interval too short for this platform, using '{:.6}'",
            MINIMUM_PROFILE_MICROS as f64 / 1e6
        ))?;
        MINIMUM_PROFILE_MICROS
    } else {
        (interval * 1e6).round().min(u32::MAX as f64) as u64
    };
    let path = crate::connections::expand_path(&filename);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(&path)
        .map_err(|failure| {
            error(format!(
                "Rprof: cannot open profile file '{filename}': {failure}"
            ))
        })?;
    let mut output = BufWriter::new(file);
    writeln!(output, "sample.interval={micros}").map_err(profile_write_error)?;
    let stop = Arc::new(AtomicBool::new(false));
    let timer_stop = stop.clone();
    let timer = std::thread::Builder::new()
        .name("rprof".into())
        .spawn(move || {
            while !timer_stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_micros(micros));
                set_profile_tick(true);
            }
        })
        .map_err(|failure| {
            error(format!(
                "Rprof: cannot start the profiling timer: {failure}"
            ))
        })?;
    PROFILER.with_borrow_mut(|profiler| {
        *profiler = Some(Profiler {
            output,
            stop,
            timer,
        })
    });
    Ok(nil())
}

/// Stops a running profile and flushes its file. The session calls it on exit too, because the
/// profiler lives in a thread local, whose destructor does not run for the main thread.
pub unsafe fn stop_profiler() -> R<()> {
    let Some(mut profiler) = PROFILER.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    profiler.stop.store(true, Ordering::Relaxed);
    if profiler.timer.join().is_err() {
        return Err(error("Rprof: the profiling timer panicked"));
    }
    set_profile_tick(false);
    profiler.output.flush().map_err(profile_write_error)
}

/// Writes one sample: the names of the calls on the stack, innermost first, after `innermost`
/// (the builtin being called, if any).
pub unsafe fn record_profile_sample(innermost: Option<&str>) -> R<()> {
    set_profile_tick(false);
    let mut line = String::new();
    if let Some(name) = innermost {
        line.push_str(&format!("\"{name}\" "));
    }
    for frame in crate::interp().frames.iter().rev() {
        line.push_str(&format!("\"{}\" ", profile_name(frame.call.car())));
    }
    if line.is_empty() {
        return Ok(());
    }
    line.push('\n');
    let written = PROFILER.with_borrow_mut(|profiler| match profiler {
        Some(profiler) => profiler.output.write_all(line.as_bytes()),
        None => Ok(()),
    });
    // A failed sample must not fail the user's call, so profiling stops with a warning instead.
    if let Err(failure) = written {
        match stop_profiler() {
            // A failed flush fails for the reason the warning already reports.
            Ok(()) | Err(_) => {}
        }
        warning(format!(
            "Rprof: cannot write the profile file, so profiling stopped: {failure}"
        ))?;
    }
    Ok(())
}

/// The name a call's function gets in a profile, as in GNU R: the symbol it was called by,
/// `pkg::name` for a namespace-qualified call, and `<Anonymous>` otherwise.
unsafe fn profile_name(head: Sexp) -> String {
    if head.kind() == SexpType::Symbol {
        return values::symbol_name(head).into_owned();
    }
    if head.kind() == SexpType::Language
        && head.car().kind() == SexpType::Symbol
        && length(head) == 3
        && matches!(&*values::symbol_name(head.car()), "::" | ":::")
        && head.cdr().car().kind() == SexpType::Symbol
        && head.cdr().cdr().car().kind() == SexpType::Symbol
    {
        return format!(
            "{}{}{}",
            values::symbol_name(head.cdr().car()),
            values::symbol_name(head.car()),
            values::symbol_name(head.cdr().cdr().car())
        );
    }
    "<Anonymous>".to_owned()
}

fn profile_write_error(failure: std::io::Error) -> Unwind {
    error(format!("Rprof: cannot write the profile file: {failure}"))
}

/// `Rprofmem(filename, append, threshold)`: logs every vector allocation larger than
/// `threshold` bytes, or stops logging for an empty or `NULL` filename. Like GNU R, only vectors
/// too large for the small-object pages are logged; ry has no pages, so it writes no
/// `new page:` lines.
unsafe fn do_rprofmem(call: &NativeCall<'_>) -> R<Sexp> {
    stop_memory_profiler()?;
    let Some(filename) = values::first_string(call.arg(0)).filter(|name| !name.is_empty()) else {
        return Ok(nil());
    };
    let append = values::as_logical_scalar(call.arg(1)) == 1;
    let threshold = values::as_real_scalar(call.arg(2));
    let threshold = if threshold.is_finite() && threshold > 0.0 {
        threshold as usize
    } else {
        0
    };
    let path = crate::connections::expand_path(&filename);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(&path)
        .map_err(|_| error(format!("Rprofmem: cannot open output file '{filename}'")))?;
    MEMORY_PROFILE.with_borrow_mut(|output| *output = Some(BufWriter::new(file)));
    crate::heap().report_allocations(Some(record_allocation), threshold);
    Ok(nil())
}

thread_local! {
    static MEMORY_PROFILE: RefCell<Option<BufWriter<File>>> = const { RefCell::new(None) };
}

/// Stops a running memory profile and flushes its file; the session calls it on exit as well.
pub unsafe fn stop_memory_profiler() -> R<()> {
    crate::heap().report_allocations(None, 0);
    let Some(mut output) = MEMORY_PROFILE.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    output
        .flush()
        .map_err(|failure| error(format!("Rprofmem: cannot write the output file: {failure}")))
}

/// The heap calls this inside an allocation, so it must not touch the heap: it writes the size
/// and the names of the calls on the stack, innermost first, as GNU R does. It returns false
/// to end the profile.
fn record_allocation(size: usize) -> bool {
    let mut line = format!("{size} :");
    for frame in crate::interp().frames.iter().rev() {
        line.push_str(&format!("\"{}\" ", unsafe {
            profile_name(frame.call.car())
        }));
    }
    line.push('\n');
    let written = MEMORY_PROFILE.with_borrow_mut(|output| match output {
        Some(output) => output.write_all(line.as_bytes()),
        None => Ok(()),
    });
    // The allocation itself must still succeed, so a write failure ends the profile instead.
    if written.is_err() {
        MEMORY_PROFILE.with_borrow_mut(Option::take);
        return false;
    }
    true
}

// Hash tables.

/// A hash table is a list of class `hashtab` holding one external pointer, as in GNU R. The
/// pointer's protected value holds the table: its type, the keys, the values, the key hashes
/// and the entry count. Entries live in insertion order, and a lookup compares hashes before
/// running `identical()` on the candidates.
const HASHTAB_MARKER: &str = "ry hashtab";

struct Table {
    pointer: Sexp,
    state: Sexp,
}

const STATE_TYPE: usize = 0;
const STATE_KEYS: usize = 1;
const STATE_VALUES: usize = 2;
const STATE_HASHES: usize = 3;
const STATE_COUNT: usize = 4;

/// `hashtab_Ext(type, K)`: a table for `2^K` entries before it grows.
unsafe fn do_hashtab(call: &NativeCall<'_>) -> R<Sexp> {
    let kind = values::require_str(call.arg(0), "type")?;
    if kind != "identical" && kind != "address" {
        return Err(error(format!("bad hash table type: {kind}")));
    }
    let exponent = values::as_integer_scalar(call.arg(1)).clamp(0, 24);
    let capacity = 1usize << exponent;
    let state = new_state(&kind, capacity)?;
    let marker = values::install(HASHTAB_MARKER)?;
    let pointer = crate::heap().alloc_external_ptr(std::ptr::null_mut(), marker, state)?;
    crate::heap().root(pointer);
    let table = values::list_vector(&[pointer])?;
    let class = scalar_string("hashtab")?;
    values::set_attr(table, symbols().class, class)?;
    Ok(table)
}

unsafe fn new_state(kind: &str, capacity: usize) -> R<Sexp> {
    let kind = scalar_string(kind)?;
    let keys = alloc(SexpType::List, capacity)?;
    let entries = alloc(SexpType::List, capacity)?;
    let hashes = alloc(SexpType::Real, capacity)?;
    let count = scalar_integer(0)?;
    values::list_vector(&[kind, keys, entries, hashes, count])
}

unsafe fn table_of(object: Sexp) -> Option<Table> {
    if object.kind() != SexpType::List
        || length(object) != 1
        || !values::inherits(object, "hashtab")
    {
        return None;
    }
    let pointer = object.element(0);
    if pointer.kind() != SexpType::ExternalPtr {
        return None;
    }
    let tag = pointer.external_tag();
    if tag.kind() != SexpType::Symbol || values::symbol_name(tag) != HASHTAB_MARKER {
        return None;
    }
    Some(Table {
        pointer,
        state: pointer.external_protected(),
    })
}

unsafe fn require_table(object: Sexp) -> R<Table> {
    table_of(object).ok_or_else(|| error("not a proper hash table object"))
}

impl Table {
    unsafe fn by_address(&self) -> bool {
        values::first_string(self.state.element(STATE_TYPE)).as_deref() == Some("address")
    }

    unsafe fn count(&self) -> usize {
        values::as_integer_scalar(self.state.element(STATE_COUNT)) as usize
    }

    unsafe fn set_count(&self, count: usize) {
        values::ints_mut(self.state.element(STATE_COUNT))[0] = count as i32;
    }

    unsafe fn key(&self, index: usize) -> Sexp {
        self.state.element(STATE_KEYS).element(index)
    }

    unsafe fn value(&self, index: usize) -> Sexp {
        self.state.element(STATE_VALUES).element(index)
    }

    unsafe fn hash_at(&self, index: usize) -> u64 {
        values::reals(self.state.element(STATE_HASHES))[index].to_bits()
    }

    unsafe fn hash(&self, key: Sexp) -> u64 {
        if self.by_address() {
            key.as_ptr() as u64
        } else {
            structural_hash(key, 3)
        }
    }

    unsafe fn find(&self, key: Sexp) -> Option<usize> {
        let hash = self.hash(key);
        let by_address = self.by_address();
        let flags = crate::sort::IdenticalFlags::default();
        (0..self.count()).find(|&index| {
            self.hash_at(index) == hash
                && if by_address {
                    self.key(index) == key
                } else {
                    crate::sort::identical(self.key(index), key, &flags)
                }
        })
    }

    unsafe fn insert(&self, key: Sexp, value: Sexp) -> R<()> {
        if let Some(index) = self.find(key) {
            values::set_elt(self.state.element(STATE_VALUES), index, value);
            return Ok(());
        }
        let count = self.count();
        let capacity = length(self.state.element(STATE_KEYS));
        if count == capacity {
            let kind = values::first_string(self.state.element(STATE_TYPE))
                .map(|text| text.into_owned())
                .unwrap_or_default();
            let grown = new_state(&kind, (capacity * 2).max(8))?;
            for index in 0..count {
                values::set_elt(grown.element(STATE_KEYS), index, self.key(index));
                values::set_elt(grown.element(STATE_VALUES), index, self.value(index));
                values::reals_mut(grown.element(STATE_HASHES))[index] =
                    values::reals(self.state.element(STATE_HASHES))[index];
            }
            crate::heap().set_external_protected(self.pointer, grown);
            let table = Table {
                pointer: self.pointer,
                state: grown,
            };
            table.set_count(count);
            return table.insert(key, value);
        }
        values::set_elt(self.state.element(STATE_KEYS), count, key);
        values::set_elt(self.state.element(STATE_VALUES), count, value);
        values::reals_mut(self.state.element(STATE_HASHES))[count] = f64::from_bits(self.hash(key));
        self.set_count(count + 1);
        Ok(())
    }

    unsafe fn remove(&self, index: usize) {
        let last = self.count() - 1;
        for slot in [STATE_KEYS, STATE_VALUES] {
            let vector = self.state.element(slot);
            values::set_elt(vector, index, vector.element(last));
            values::set_elt(vector, last, nil());
        }
        let hashes = values::reals_mut(self.state.element(STATE_HASHES));
        hashes[index] = hashes[last];
        self.set_count(last);
    }
}

/// A hash of `object` that agrees with `identical()`: equal objects hash equally, so only
/// the contents of vectors (to `depth` levels of lists) are mixed in.
unsafe fn structural_hash(object: Sexp, depth: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    let kind = object.kind();
    kind.code().hash(&mut hasher);
    match kind {
        SexpType::Logical | SexpType::Integer => values::ints(object).hash(&mut hasher),
        SexpType::Real => {
            for value in values::reals(object) {
                // Zero's sign and NaN payloads other than NA's do not matter to identical().
                let canonical = if *value == 0.0 {
                    0
                } else if value.is_nan() {
                    u64::from(values::is_na(*value))
                } else {
                    value.to_bits()
                };
                canonical.hash(&mut hasher);
            }
        }
        SexpType::String => {
            for index in 0..length(object) {
                object.element(index).bytes().hash(&mut hasher);
            }
        }
        SexpType::Raw => values::raws(object).hash(&mut hasher),
        SexpType::Symbol => (object.as_ptr() as usize).hash(&mut hasher),
        SexpType::List | SexpType::Expression if depth > 0 => {
            for index in 0..length(object) {
                structural_hash(object.element(index), depth - 1).hash(&mut hasher);
            }
        }
        _ => length(object).hash(&mut hasher),
    }
    hasher.finish()
}

/// `gethash_Ext(h, key, nomatch)`.
unsafe fn do_gethash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    Ok(table
        .find(call.arg(1))
        .map_or_else(|| call.arg(2), |index| table.value(index)))
}

/// `sethash_Ext(h, key, value)`: the value.
unsafe fn do_sethash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    table.insert(call.arg(1), call.arg(2))?;
    Ok(call.arg(2))
}

/// `remhash_Ext(h, key)`: whether there was an entry to remove.
unsafe fn do_remhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    let found = table.find(call.arg(1));
    if let Some(index) = found {
        table.remove(index);
    }
    scalar_bool(found.is_some())
}

unsafe fn do_numhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    scalar_integer(table.count() as i32)
}

unsafe fn do_typhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    Ok(table.state.element(STATE_TYPE))
}

/// `maphash_Ext(h, FUN)`: calls `FUN(key, value)` for every entry, newest first.
unsafe fn do_maphash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    let function = call.arg(1);
    if !values::is_function(function) {
        return Err(error("FUN must be a function"));
    }
    let entries: Vec<(Sexp, Sexp)> = (0..table.count())
        .rev()
        .map(|index| (table.key(index), table.value(index)))
        .collect();
    for (key, value) in entries {
        let args = [
            Arg {
                tag: nil(),
                value: key,
            },
            Arg { tag: nil(), value },
        ];
        crate::eval::apply(nil(), function, &args, call.env()?)?;
    }
    Ok(nil())
}

unsafe fn do_clrhash(call: &NativeCall<'_>) -> R<Sexp> {
    let table = require_table(call.arg(0))?;
    for index in 0..table.count() {
        values::set_elt(table.state.element(STATE_KEYS), index, nil());
        values::set_elt(table.state.element(STATE_VALUES), index, nil());
    }
    table.set_count(0);
    Ok(nil())
}

unsafe fn do_is_hashtab(call: &NativeCall<'_>) -> R<Sexp> {
    scalar_bool(table_of(call.arg(0)).is_some())
}

// unzip.

/// `download.file(method = "internal")`: `.External(C_download, url, destfile, quiet, mode,
/// headers, cacheOK)`. Since R 4.2 the internal method only copies `file://` URLs; http and ftp
/// are defunct for it and every other scheme is refused, with the messages GNU R gives.
unsafe fn do_download(call: &NativeCall<'_>) -> R<Sexp> {
    let urls = call.arg(0);
    if urls.kind() != SexpType::String || length(urls) == 0 {
        return Err(error("invalid 'url' argument"));
    }
    if length(urls) > 1 {
        warning("only first element of 'url' argument used")?;
    }
    let url = values::string_at(urls, 0).ok_or_else(|| error("invalid 'url' argument"))?;
    let destination = call.arg(1);
    if destination.kind() != SexpType::String || length(destination) == 0 {
        return Err(error("invalid 'destfile' argument"));
    }
    if length(destination) > 1 {
        warning("only first element of 'destfile' argument used")?;
    }
    let destination =
        values::string_at(destination, 0).ok_or_else(|| error("invalid 'destfile' argument"))?;
    let mode = values::require_str(call.arg(3), "mode")?;
    let Some(path) = url.strip_prefix("file://") else {
        let scheme = ["http", "ftp"]
            .into_iter()
            .find(|scheme| url.starts_with(&format!("{scheme}://")));
        return Err(error(match scheme {
            Some(scheme) => format!("the 'internal' method for {scheme}:// URLs is defunct"),
            None => format!("scheme not supported in URL '{url}'"),
        }));
    };
    let contents = std::fs::read(crate::connections::expand_path(path)).map_err(|failure| {
        error(format!(
            "cannot open URL '{url}', reason '{}'",
            crate::connections::os_message(&failure)
        ))
    })?;
    let destination = crate::connections::expand_path(&destination);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(mode.starts_with('a'))
        .truncate(!mode.starts_with('a'))
        .open(&destination)
        .map_err(|failure| {
            error(format!(
                "cannot open destfile '{destination}', reason '{}'",
                crate::connections::os_message(&failure)
            ))
        })?;
    file.write_all(&contents)
        .map_err(|failure| error(crate::connections::os_message(&failure)))?;
    scalar_integer(0)
}

/// `unzip(zipfile, files, exdir, list, overwrite, junkpaths, setTimes)`: lists or extracts the
/// members of a zip archive (stored or deflated). Listing returns the names, sizes and
/// modification times; extracting returns 0 with the extracted paths as `extracted`.
unsafe fn do_unzip(call: &NativeCall<'_>) -> R<Sexp> {
    let path = values::require_str(call.arg(0), "zipfile")?;
    let path = crate::connections::expand_path(&path);
    let wanted = call.arg(1);
    let wanted: Option<Vec<String>> =
        (wanted != nil()).then(|| values::strings(wanted).into_iter().flatten().collect());
    let directory = values::require_str(call.arg(2), "exdir")?;
    let list = values::as_logical_scalar(call.arg(3)) == 1;
    let overwrite = values::as_logical_scalar(call.arg(4)) == 1;
    let junk_paths = values::as_logical_scalar(call.arg(5)) == 1;
    let archive =
        std::fs::read(&path).map_err(|_| error(format!("zip file '{path}' cannot be opened")))?;
    let entries = zip_entries(&archive)
        .ok_or_else(|| error(format!("zip file '{path}' cannot be opened")))?;
    if list {
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        let sizes: Vec<f64> = entries.iter().map(|entry| entry.size as f64).collect();
        let dates: Vec<String> = entries.iter().map(|entry| entry.date.clone()).collect();
        return values::list_vector(&[
            string_vector(&names)?,
            values::real_vector(&sizes)?,
            string_vector(&dates)?,
        ]);
    }
    let mut extracted = Vec::new();
    for entry in &entries {
        if wanted
            .as_ref()
            .is_some_and(|wanted| !wanted.contains(&entry.name))
        {
            continue;
        }
        let relative = if junk_paths {
            entry.name.rsplit('/').next().unwrap_or(&entry.name)
        } else {
            &entry.name
        };
        if relative.is_empty() {
            continue;
        }
        let target =
            std::path::Path::new(&crate::connections::expand_path(&directory)).join(relative);
        if entry.name.ends_with('/') {
            std::fs::create_dir_all(&target).map_err(|failure| {
                error(format!(
                    "cannot create directory '{}': {failure}",
                    target.display()
                ))
            })?;
            continue;
        }
        if target.exists() && !overwrite {
            warning(format!("not overwriting file '{}'", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|failure| {
                error(format!(
                    "cannot create directory '{}': {failure}",
                    parent.display()
                ))
            })?;
        }
        let data = zip_member(&archive, entry)
            .ok_or_else(|| error("error 1 in extracting from zip file"))?;
        std::fs::write(&target, data).map_err(|failure| {
            error(format!(
                "cannot open file '{}': {failure}",
                target.display()
            ))
        })?;
        extracted.push(target.display().to_string());
    }
    if let Some(wanted) = &wanted
        && wanted
            .iter()
            .any(|name| !entries.iter().any(|entry| &entry.name == name))
    {
        warning("requested file not found in the zip file")?;
    }
    let result = scalar_integer(0)?;
    let paths = string_vector(&extracted)?;
    values::set_attr(result, values::install("extracted")?, paths)?;
    Ok(result)
}

struct ZipEntry {
    name: String,
    size: u64,
    compressed_size: u64,
    method: u16,
    local_header: usize,
    /// `%Y-%m-%d %H:%M`.
    date: String,
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The central directory of a zip archive.
fn zip_entries(archive: &[u8]) -> Option<Vec<ZipEntry>> {
    const END_SIGNATURE: u32 = 0x0605_4b50;
    const ENTRY_SIGNATURE: u32 = 0x0201_4b50;
    let end = (0..archive.len().saturating_sub(21))
        .rev()
        .find(|&at| read_u32(archive, at) == Some(END_SIGNATURE))?;
    let count = read_u16(archive, end + 10)? as usize;
    let mut at = read_u32(archive, end + 16)? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if read_u32(archive, at)? != ENTRY_SIGNATURE {
            return None;
        }
        let method = read_u16(archive, at + 10)?;
        let time = read_u16(archive, at + 12)?;
        let day = read_u16(archive, at + 14)?;
        let compressed_size = read_u32(archive, at + 20)? as u64;
        let size = read_u32(archive, at + 24)? as u64;
        let name_length = read_u16(archive, at + 28)? as usize;
        let extra_length = read_u16(archive, at + 30)? as usize;
        let comment_length = read_u16(archive, at + 32)? as usize;
        let local_header = read_u32(archive, at + 42)? as usize;
        let name =
            String::from_utf8_lossy(archive.get(at + 46..at + 46 + name_length)?).into_owned();
        let date = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            1980 + (day >> 9),
            (day >> 5) & 0xF,
            day & 0x1F,
            time >> 11,
            (time >> 5) & 0x3F
        );
        entries.push(ZipEntry {
            name,
            size,
            compressed_size,
            method,
            local_header,
            date,
        });
        at += 46 + name_length + extra_length + comment_length;
    }
    Some(entries)
}

/// The uncompressed bytes of the member called `name`, which `unz()` connections read.
pub(crate) fn zip_member_named(archive: &[u8], name: &str) -> Option<Vec<u8>> {
    let entries = zip_entries(archive)?;
    let entry = entries.iter().find(|entry| entry.name == name)?;
    zip_member(archive, entry)
}

/// The uncompressed bytes of one member.
fn zip_member(archive: &[u8], entry: &ZipEntry) -> Option<Vec<u8>> {
    use std::io::Read;
    let at = entry.local_header;
    if read_u32(archive, at)? != 0x0403_4b50 {
        return None;
    }
    let name_length = read_u16(archive, at + 26)? as usize;
    let extra_length = read_u16(archive, at + 28)? as usize;
    let start = at + 30 + name_length + extra_length;
    let data = archive.get(start..start + entry.compressed_size as usize)?;
    match entry.method {
        0 => Some(data.to_vec()),
        8 => {
            let mut output = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(data)
                .read_to_end(&mut output)
                .ok()?;
            Some(output)
        }
        _ => None,
    }
}
