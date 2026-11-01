//! Connections: R's streams, as the C API exposes them through `struct Rconn` and as base R's
//! `file()`, `readLines()`, `writeBin()`, `sink()` and friends use them.
//!
//! A connection lives in a slot of a fixed table of [`NCONNECTIONS`], as in GNU R: slots 0, 1
//! and 2 are stdin, stdout and stderr, and the R-level connection object is an integer vector
//! holding the slot number, classed `c(<class>, "connection")`. Every connection but the
//! standard three also carries a `conn_id` attribute, an external pointer whose address is the
//! slot's `Rconn`. Closing a connection frees its slot and clears that address, so an R object
//! that outlives its connection reads as an invalid connection instead of dangling.
//!
//! Every connection is driven through the method pointers of its `Rconn`, because packages fill
//! them in for custom connections (curl, processx) and call them directly on any connection
//! (vroom). ry's built-in kinds install Rust methods whose state sits behind `private`. Calling
//! a method from Rust goes through the host's native trampoline ([`Hooks::connection_method`])
//! unless the pointer is ry's own method, which then runs directly: package code is never
//! called from a Rust frame, and the common path pays no context switch.
//!
//! [`Hooks::connection_method`]: crate::Hooks

use crate::{
    builtins::{BuiltinDef, Call, internal},
    interp,
    unwind::{R, Unwind, error},
    values::{self, nil, scalar_bool, scalar_integer, symbols},
    write_terminal,
};
use bzip2::{read::MultiBzDecoder, write::BzEncoder};
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use lzma_rust2::{XzOptions, XzReader, XzWriter};
use heap::{
    object::{NA_INTEGER, Rcomplex, Sexp, SexpType},
    strings::Encoding,
};
use std::{
    borrow::Cow,
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    ptr::null_mut,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("file", 6, do_file),
    internal("gzfile", 4, |call| unsafe { compressed_file(call, Compressed::Gzip) }),
    internal("bzfile", 4, |call| unsafe { compressed_file(call, Compressed::Bzip2) }),
    internal("xzfile", 4, |call| unsafe { compressed_file(call, Compressed::Xz) }),
    internal("fifo", 4, |_| Err(unsupported("fifo"))),
    internal("url", 6, |_| Err(error("ry does not support URL connections yet"))),
    internal("pipe", 3, do_pipe),
    internal("rawConnection", 3, do_raw_connection),
    internal("rawConnectionValue", 1, |call| unsafe { raw_connection_value(call.arg(0)) }),
    internal("textConnection", 5, do_text_connection),
    internal("textConnectionValue", 1, |call| unsafe { text_connection_value(call.arg(0)) }),
    internal("open", 3, do_open).invisible(),
    internal("close", 2, |call| unsafe { close(call.arg(0)) }).invisible(),
    internal("isOpen", 2, do_is_open),
    internal("isIncomplete", 1, |call| unsafe {
        scalar_bool((*resolve_con(call.arg(0))?.1).incomplete != 0)
    }),
    internal("isSeekable", 1, |call| unsafe {
        scalar_bool((*resolve_con(call.arg(0))?.1).canseek != 0)
    }),
    internal("stdin", 0, |_| unsafe { connection_object(0) }),
    internal("stdout", 0, |_| unsafe { connection_object(1) }),
    internal("stderr", 0, |_| unsafe { connection_object(2) }),
    internal("readLines", 6, do_read_lines),
    internal("readDCF", 3, do_read_dcf),
    internal("writeLines", 4, do_write_lines).invisible(),
    internal("readChar", 3, do_read_char),
    internal("writeChar", 5, do_write_char).keep_visibility(),
    internal("readBin", 6, do_read_bin),
    internal("writeBin", 5, do_write_bin).keep_visibility(),
    internal("seek", 4, do_seek),
    internal("truncate", 1, do_truncate).invisible(),
    internal("flush", 1, do_flush).invisible(),
    internal("summary.connection", 1, |call| unsafe { summary(call.arg(0)) }),
    internal("pushBack", 4, do_push_back).invisible(),
    internal("pushBackLength", 1, |call| unsafe {
        scalar_integer((*resolve_con(call.arg(0))?.1).nPushBack)
    }),
    internal("clearPushBack", 1, |call| unsafe {
        clear_push_back_lines(resolve_con(call.arg(0))?.1);
        Ok(nil())
    })
    .invisible(),
    internal("getConnection", 1, do_get_connection),
    internal("getAllConnections", 0, |_| unsafe { all_connections() }),
    internal("sink", 4, do_sink).invisible(),
    internal("sink.number", 1, do_sink_number),
    internal("readline", 1, |_| unsafe { values::scalar_string("") }),
    internal("cat", 6, do_cat).invisible(),
];

/// The version of the C interface in `R_ext/Connections.h`.
pub const R_CONNECTIONS_VERSION: c_int = 1;
/// How many connections can exist at once, the three standard ones included.
pub const NCONNECTIONS: usize = 128;

/// `Rboolean` is a C enum, so it has the size of `int`.
pub type Rboolean = c_int;

/// GNU R's `struct Rconn`, member for member.
#[repr(C)]
// The members keep the C names, which packages written against the header use.
#[allow(non_snake_case)]
pub struct Rconn {
    pub class: *mut c_char,
    pub description: *mut c_char,
    pub enc: c_int,
    pub mode: [c_char; 5],
    pub text: Rboolean,
    pub isopen: Rboolean,
    pub incomplete: Rboolean,
    pub canread: Rboolean,
    pub canwrite: Rboolean,
    pub canseek: Rboolean,
    pub blocking: Rboolean,
    pub isGzcon: Rboolean,
    pub open: Option<OpenMethod>,
    pub close: Option<VoidMethod>,
    pub destroy: Option<VoidMethod>,
    /// Takes a `va_list`, whose representation differs between platforms, so Rust never calls
    /// it and keeps it opaque; the host's native code casts it back.
    pub vfprintf: Option<OpaqueMethod>,
    pub fgetc: Option<IntMethod>,
    pub fgetc_internal: Option<IntMethod>,
    pub seek: Option<SeekMethod>,
    pub truncate: Option<VoidMethod>,
    pub fflush: Option<IntMethod>,
    pub read: Option<ReadMethod>,
    pub write: Option<WriteMethod>,
    pub nPushBack: c_int,
    pub posPushBack: c_int,
    pub PushBack: *mut *mut c_char,
    pub save: c_int,
    pub save2: c_int,
    pub encname: [c_char; 101],
    pub inconv: *mut c_void,
    pub outconv: *mut c_void,
    pub iconvbuff: [c_char; 25],
    pub oconvbuff: [c_char; 50],
    pub next: *mut c_char,
    pub init_out: [c_char; 25],
    pub navail: i16,
    pub inavail: i16,
    pub EOF_signalled: Rboolean,
    pub UTF8out: Rboolean,
    pub id: *mut c_void,
    pub ex_ptr: *mut c_void,
    pub private: *mut c_void,
    pub status: c_int,
    pub buff: *mut u8,
    pub buff_len: usize,
    pub buff_stored_len: usize,
    pub buff_pos: usize,
}

pub type Rconnection = *mut Rconn;
pub type OpenMethod = unsafe extern "C" fn(Rconnection) -> Rboolean;
pub type VoidMethod = unsafe extern "C" fn(Rconnection);
pub type OpaqueMethod = unsafe extern "C" fn();
pub type IntMethod = unsafe extern "C" fn(Rconnection) -> c_int;
pub type SeekMethod = unsafe extern "C" fn(Rconnection, f64, c_int, c_int) -> f64;
pub type ReadMethod = unsafe extern "C" fn(*mut c_void, usize, usize, Rconnection) -> usize;
pub type WriteMethod = unsafe extern "C" fn(*const c_void, usize, usize, Rconnection) -> usize;

/// A call of a method a package installed, which the host runs inside a native context so the
/// method's `Rf_error` comes back as an `Err`. Each variant carries the method, its arguments
/// beyond the connection, and where its result goes.
pub enum NativeMethod<'a> {
    Open {
        method: OpenMethod,
        result: &'a mut Rboolean,
    },
    Void(VoidMethod),
    Int {
        method: IntMethod,
        result: &'a mut c_int,
    },
    Seek {
        method: SeekMethod,
        position: f64,
        origin: c_int,
        rw: c_int,
        result: &'a mut f64,
    },
    Read {
        method: ReadMethod,
        buffer: &'a mut [u8],
        result: &'a mut usize,
    },
    Write {
        method: WriteMethod,
        bytes: &'a [u8],
        result: &'a mut usize,
    },
    /// `vfprintf`, given the text already formatted.
    Print {
        method: OpaqueMethod,
        text: &'a CStr,
        result: &'a mut c_int,
    },
}

/// The session's connections and the output diversions `sink` set up.
pub struct Table {
    entries: Vec<Option<Entry>>,
    /// Innermost last.
    sinks: Vec<Sink>,
}

struct Entry {
    con: Rconnection,
    /// Whether `private` holds a ry `Device`. A custom connection's `private` is the package's.
    builtin: bool,
}

struct Sink {
    slot: usize,
    /// `sink(closeOnExit = TRUE)`: removing the diversion closes and destroys the connection.
    close_on_exit: bool,
    /// The diversion opened the connection, so removing it closes the connection again.
    opened: bool,
    /// Output also still goes to the console.
    split: bool,
}

/// GNU R's limit on nested output diversions.
const MAX_SINKS: usize = 21;

impl Table {
    /// The table with the three standard connections. `vfprintf` is the host's default
    /// `vfprintf` method, which every connection starts with.
    pub(crate) fn new(vfprintf: Option<OpaqueMethod>) -> Table {
        let mut entries: Vec<Option<Entry>> = (0..NCONNECTIONS).map(|_| None).collect();
        for (index, (stream, description, mode)) in [
            (Standard::Input, "stdin", "r"),
            (Standard::Output, "stdout", "w"),
            (Standard::Error, "stderr", "w"),
        ]
        .into_iter()
        .enumerate()
        {
            // SAFETY: allocating a fresh struct touches no R object.
            let con = unsafe {
                let con = allocate_rconn(
                    b"terminal",
                    description.as_bytes(),
                    mode.as_bytes(),
                    &BUILTIN_METHODS,
                    vfprintf,
                );
                (*con).private = Box::into_raw(Box::new(Device::Standard(stream))).cast();
                (*con).isopen = 1;
                (*con).canread = (stream == Standard::Input) as Rboolean;
                (*con).canwrite = (stream != Standard::Input) as Rboolean;
                con
            };
            if let Some(slot) = entries.get_mut(index) {
                *slot = Some(Entry { con, builtin: true });
            }
        }
        Table {
            entries,
            sinks: Vec::new(),
        }
    }

    /// Marks the variables text output connections write to.
    pub(crate) unsafe fn trace(&self, marker: &mut heap::Marker<'_>) {
        for entry in self.entries.iter().flatten() {
            if !entry.builtin {
                continue;
            }
            let device = (*entry.con).private as *const Device;
            if let Some(Device::TextOutput(TextOutput {
                target: Some(target),
                ..
            })) = device.as_ref()
            {
                marker.mark(target.symbol);
                marker.mark(target.env);
            }
        }
    }
}

// The interface the rest of the interpreter and the host use.

/// The connection an R object names, after checking it is live. `not_connection` is the message
/// for an object that is not classed as a connection at all.
pub unsafe fn resolve(object: Sexp, not_connection: &str) -> R<(usize, Rconnection)> {
    if !values::inherits(object, "connection") {
        return Err(error(not_connection));
    }
    let invalid = || error("invalid connection");
    let slot = match object.kind() {
        SexpType::Integer if object.length() >= 1 => {
            *object.slice::<i32>().first().ok_or_else(invalid)?
        }
        SexpType::Real if object.length() >= 1 => {
            let number = *object.slice::<f64>().first().ok_or_else(invalid)?;
            // `as` turns NaN into 0, which would name stdin.
            if number.fract() != 0.0 || !(0.0..=i32::MAX as f64).contains(&number) {
                return Err(invalid());
            }
            number as i32
        }
        _ => return Err(invalid()),
    };
    let entry = usize::try_from(slot)
        .ok()
        .and_then(entry)
        .ok_or_else(invalid)?;
    let id = values::get_attr(object, values::install("conn_id")?);
    if id.kind() == SexpType::ExternalPtr && id.external_address() != entry.con.cast() {
        return Err(invalid());
    }
    Ok((slot as usize, entry.con))
}

/// Registers a connection a package created with `R_new_custom_connection`.
pub unsafe fn new_custom(description: &[u8], mode: &[u8], class: &[u8]) -> R<(Sexp, Rconnection)> {
    if mode.len() > 4 {
        return Err(error("invalid 'mode' argument"));
    }
    let con = allocate_rconn(class, description, mode, &CUSTOM_METHODS, default_vfprintf());
    (*con).blocking = 0;
    let object = register(con, false)?;
    Ok((object, con))
}

/// Reads bytes as `R_ReadConnection` does: straight from the device, past any pushback.
pub unsafe fn read_raw(con: Rconnection, buffer: &mut [u8]) -> R<usize> {
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if (*con).canread == 0 {
        return Err(error("cannot read from this connection"));
    }
    method_read(con, buffer)
}

/// Writes bytes as `R_WriteConnection` does.
pub unsafe fn write_raw(con: Rconnection, bytes: &[u8]) -> R<usize> {
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if (*con).canwrite == 0 {
        return Err(error("cannot write to this connection"));
    }
    method_write(con, bytes)
}

/// How many slots are in use.
pub fn count() -> usize {
    interp().connections.entries.iter().flatten().count()
}

/// Writes standard output: to the console, or to where `sink` diverts it.
pub unsafe fn write_output(bytes: &[u8]) {
    let Some(sink) = interp().connections.sinks.last() else {
        write_terminal(bytes, false);
        return;
    };
    let (slot, split) = (sink.slot, sink.split);
    if split || slot == 1 {
        write_terminal(bytes, false);
    }
    if slot == 1 {
        return;
    }
    let written = match entry(slot) {
        Some(entry) => method_print(entry.con, bytes),
        None => Err(error("invalid connection")),
    };
    if let Err(unwind) = written {
        // Output has no caller to report a failed diversion to, so the console shows it.
        let message = match unwind {
            Unwind::Error(failure) => failure.message,
            _ => "the sink connection could not be written".to_owned(),
        };
        write_terminal(
            format!("Error writing to the sink connection: {message}\n").as_bytes(),
            true,
        );
        write_terminal(bytes, false);
    }
}

/// Runs `read` over the unread text of the connection `file`, which advances the position it
/// is given. A closed connection is opened for the call and closed after it, as GNU R's readers
/// do; on an open one, the text `read` left is pushed back for the next reader.
pub unsafe fn with_input<T>(file: Sexp, read: impl FnOnce(&[u8], &mut usize) -> T) -> R<T> {
    let con = resolve_con(file)?.1;
    let was_open = (*con).isopen != 0;
    let text = with_open(con, "rt", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        read_text_rest(con)
    })?;
    let mut position = 0;
    let result = read(&text, &mut position);
    if was_open && let Some(rest) = text.get(position..).filter(|rest| !rest.is_empty()) {
        push_back_bytes(con, rest);
    }
    Ok(result)
}

/// The whole text of a file path (decompressed when it is gzip data) or of a connection.
pub unsafe fn read_all_text(file: Sexp) -> R<String> {
    if file.kind() == SexpType::String {
        let path = expand_path(&values::require_string(file, "file")?);
        let bytes = std::fs::read(&path).map_err(|failure| {
            error(format!("cannot open file '{path}': {}", os_message(&failure)))
        })?;
        let bytes = decompress(&bytes)
            .map_err(|failure| error(format!("cannot read compressed file '{path}': {failure}")))?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut text = String::new();
    with_input(file, |input, position| {
        text = String::from_utf8_lossy(input).into_owned();
        *position = input.len();
    })?;
    Ok(text)
}

/// Every byte left on a connection, as `unserialize` reads them.
pub unsafe fn read_binary(file: Sexp) -> R<Vec<u8>> {
    let con = resolve_con(file)?.1;
    with_open(con, "rb", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        let mut bytes = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            let count = method_read(con, &mut chunk)?;
            if count == 0 {
                return Ok(bytes);
            }
            bytes.extend_from_slice(chunk.get(..count).unwrap_or_default());
        }
    })
}

/// Writes bytes to a connection opened for binary output, as `serialize` does.
pub unsafe fn write_binary(file: Sexp, bytes: &[u8]) -> R<()> {
    let con = resolve_con(file)?.1;
    with_open(con, "wb", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        write_all(con, bytes)
    })
}

/// Writes text to a connection the way `cat` and `write.table` do.
pub unsafe fn print(file: Sexp, text: &[u8]) -> R<()> {
    let con = resolve_con(file)?.1;
    with_open(con, "wt", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        method_print(con, text)
    })
}

/// Writes text to a connection, or a file path, or the console for `""`.
pub unsafe fn write_text(file: Sexp, text: &str) -> R<()> {
    if file.kind() == SexpType::String {
        let path = values::require_string(file, "file")?;
        if path.is_empty() {
            crate::write_console(text.as_bytes(), false);
            return Ok(());
        }
        return std::fs::write(expand_path(&path), text).map_err(|failure| {
            error(format!("cannot open file '{path}': {}", os_message(&failure)))
        });
    }
    print(file, text.as_bytes())
}

/// The description of a connection, or the path itself.
pub unsafe fn description(file: Sexp) -> Option<String> {
    if file.kind() == SexpType::String {
        return values::first_string(file).map(|text| text.into_owned());
    }
    let con = resolve_con(file).ok()?.1;
    Some(description_of(con))
}

/// `~` expanded to the home directory.
pub fn expand_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/'))
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}{rest}");
    }
    path.to_owned()
}

// The internals.

/// `file(description, open, blocking, encoding, method, raw)`. An empty description is an
/// anonymous file, opened `"w+"` unless `open` says otherwise, and a file opened for reading
/// that holds compressed data reads decompressed with class `gzfile`, `bzfile` or `xzfile`.
unsafe fn do_file(call: &Call<'_>) -> R<Sexp> {
    let description = values::require_string(call.arg(0), "description")?;
    let open = values::require_string(call.arg(1), "open")?;
    let blocking = values::require_flag(call.arg(2), "blocking")?;
    let raw = values::require_flag(call.arg(5), "raw")?;
    if description.contains("://") {
        return Err(error("ry does not support URL connections yet"));
    }
    let (path, temporary, open) = if description.is_empty() {
        let open = if open.is_empty() { "w+".to_owned() } else { open };
        (temporary_path(), true, open)
    } else {
        (PathBuf::from(expand_path(&description)), false, open)
    };
    let compression = if !raw && parse_mode(open.as_bytes()).is_some_and(|mode| mode.read && !mode.write && !mode.binary) {
        file_compression(&path).ok().flatten()
    } else {
        None
    };
    let device = Device::File(FileDevice {
        path,
        temporary,
        encoding: None,
        raw,
        compression: 6,
        stream: None,
    });
    let object = new_builtin("file", description.as_bytes(), &open, "r", device, |con| {
        (*con).blocking = blocking as Rboolean;
    })?;
    // The object keeps the class it was created with; only the struct says what it became.
    if let Some(compression) = compression {
        let class: &[u8] = match compression {
            Compressed::Gzip => b"gzfile",
            Compressed::Bzip2 => b"bzfile",
            Compressed::Xz => b"xzfile",
        };
        let con = resolve_con(object)?.1;
        libc::free((*con).class.cast());
        (*con).class = malloc_string(class);
    }
    Ok(object)
}

/// `gzfile`, `bzfile` and `xzfile(description, open, encoding, compression)`: reads decompress
/// gzip, bzip2 or xz data, whichever the magic bytes announce, and pass anything else through,
/// as GNU R's do. Writes use the connection's own format.
unsafe fn compressed_file(call: &Call<'_>, encoding: Compressed) -> R<Sexp> {
    let description = values::require_string(call.arg(0), "description")?;
    let open = values::require_string(call.arg(1), "open")?;
    let compression = values::as_integer_scalar(call.arg(3));
    // xzfile's negative levels select the slower "extreme" presets, which ry treats as the
    // matching ordinary preset.
    let valid = match encoding {
        Compressed::Xz => (-9..=9).contains(&compression),
        Compressed::Gzip | Compressed::Bzip2 => (0..=9).contains(&compression),
    };
    if !valid {
        return Err(error("invalid 'compress' argument"));
    }
    let class = match encoding {
        Compressed::Gzip => "gzfile",
        Compressed::Bzip2 => "bzfile",
        Compressed::Xz => "xzfile",
    };
    let device = Device::File(FileDevice {
        path: PathBuf::from(expand_path(&description)),
        temporary: false,
        encoding: Some(encoding),
        raw: false,
        compression: compression.unsigned_abs(),
        stream: None,
    });
    new_builtin(class, description.as_bytes(), &open, "rb", device, |_| {})
}

fn unsupported(class: &str) -> Unwind {
    error(format!("ry does not support {class} connections yet"))
}

/// `pipe(description, open, encoding)`: the command's standard output to read, or its input to
/// write.
unsafe fn do_pipe(call: &Call<'_>) -> R<Sexp> {
    let command = values::require_string(call.arg(0), "description")?;
    let open = values::require_string(call.arg(1), "open")?;
    let command_text =
        CString::new(command.as_str()).map_err(|_| error("embedded nul in a pipe command"))?;
    let device = Device::Pipe(PipeDevice {
        command: command_text,
        stream: null_mut(),
    });
    // A pipe's reader may be waiting on the command, so an unfinished last line is kept back.
    new_builtin("pipe", command.as_bytes(), &open, "r", device, |con| {
        (*con).blocking = 0;
    })
}

/// `rawConnection(description, object, open)`: reads from or writes into a copy of a raw vector.
unsafe fn do_raw_connection(call: &Call<'_>) -> R<Sexp> {
    let description = values::require_string(call.arg(0), "description")?;
    let object = call.arg(1);
    if object.kind() != SexpType::Raw {
        return Err(error("invalid 'raw' argument"));
    }
    let open = values::require_string(call.arg(2), "open")?;
    let open = if open.is_empty() { "r".to_owned() } else { open };
    let mode = parse_mode(open.as_bytes()).ok_or_else(|| error("unsupported mode"))?;
    let mut data = values::raws(object).to_vec();
    if mode.truncate {
        data.clear();
    }
    let position = if mode.append { data.len() } else { 0 };
    let device = Device::Raw(RawDevice { data, position });
    new_builtin("rawConnection", description.as_bytes(), &open, &open, device, |con| {
        (*con).canseek = 1;
    })
}

/// `textConnection(nm, object, open, env, type)`: reads the lines of a character vector, or
/// collects written lines, into the variable `object` names in `env` when it names one.
unsafe fn do_text_connection(call: &Call<'_>) -> R<Sexp> {
    let description = values::require_string(call.arg(0), "description")?;
    let object = call.arg(1);
    let open = values::require_string(call.arg(2), "open")?;
    let env = call.arg(3);
    let device = match open.as_str() {
        "" | "r" => {
            if object.kind() != SexpType::String {
                return Err(error("invalid 'text' argument"));
            }
            let mut data = Vec::new();
            for line in values::strings(object) {
                data.extend_from_slice(line.as_deref().unwrap_or("NA").as_bytes());
                data.push(b'\n');
            }
            Device::TextInput(TextInput { data, position: 0 })
        }
        "w" | "a" => {
            let target = if object == nil() {
                None
            } else {
                let name = values::require_string(object, "object")?;
                Some(TextTarget {
                    symbol: values::install(&name)?,
                    env,
                })
            };
            let mut lines = Vec::new();
            if let Some(target) = target
                && open == "a"
            {
                let existing = crate::eval::find_var(target.symbol, target.env)?;
                if existing.kind() == SexpType::String {
                    for line in values::strings(existing) {
                        lines.push(line.unwrap_or_else(|| "NA".into()).into_bytes());
                    }
                }
            }
            let output = TextOutput {
                lines,
                partial: Vec::new(),
                target,
            };
            update_text_target(&output)?;
            Device::TextOutput(output)
        }
        _ => return Err(error("unsupported mode")),
    };
    let open = if open.is_empty() { "r" } else { open.as_str() };
    new_builtin("textConnection", description.as_bytes(), open, open, device, |_| {})
}

/// `open(con, open, blocking)`. Opening an open connection only warns, as in GNU R.
unsafe fn do_open(call: &Call<'_>) -> R<Sexp> {
    let (slot, con) = resolve_con(call.arg(0))?;
    let mode = values::require_string(call.arg(1), "open")?;
    let blocking = values::require_flag(call.arg(2), "blocking")?;
    if slot <= 2 {
        return Err(error("cannot open standard connections"));
    }
    if (*con).isopen != 0 {
        crate::conditions::warning("connection is already open")?;
        return Ok(nil());
    }
    if !mode.is_empty() {
        set_mode(con, mode.as_bytes());
    }
    (*con).blocking = blocking as Rboolean;
    open_con(con)?;
    Ok(nil())
}

/// `close(con, type)`: closes the connection and frees its slot. Returns the close status where
/// the kind has one (a pipe's exit status), `NULL` otherwise.
unsafe fn close(con: Sexp) -> R<Sexp> {
    let (slot, _) = resolve_con(con)?;
    if slot <= 2 {
        return Err(error("cannot close standard connections"));
    }
    let status = close_slot(slot)?;
    if status == NA_INTEGER {
        Ok(nil())
    } else {
        scalar_integer(status)
    }
}

/// `isOpen(con, rw)`, with `rw` 0 for either, 1 for reading and 2 for writing.
unsafe fn do_is_open(call: &Call<'_>) -> R<Sexp> {
    let con = resolve_con(call.arg(0))?.1;
    let open = (*con).isopen != 0;
    scalar_bool(match values::as_integer_scalar(call.arg(1)) {
        1 => open && (*con).canread != 0,
        2 => open && (*con).canwrite != 0,
        _ => open,
    })
}

/// `readLines(con, n, ok, warn, encoding, skipNul)`. `n < 0` reads every line.
unsafe fn do_read_lines(call: &Call<'_>) -> R<Sexp> {
    let con = resolve_con(call.arg(0))?.1;
    let n = match values::as_integer_scalar(call.arg(1)) {
        NA_INTEGER => -1,
        n => n as i64,
    };
    let ok = values::require_flag(call.arg(2), "ok")?;
    let warn_incomplete = values::require_flag(call.arg(3), "warn")?;
    let encoding = match values::first_string(call.arg(4)).as_deref() {
        Some("UTF-8" | "utf8") => Encoding::Utf8,
        Some("latin1") => Encoding::Latin1,
        Some("bytes") => Encoding::Bytes,
        _ => Encoding::Native,
    };
    let skip_nul = values::require_flag(call.arg(5), "skipNul")?;
    with_open(con, "rt", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        (*con).incomplete = 0;
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut line = Vec::new();
        while n < 0 || (lines.len() as i64) < n {
            line.clear();
            let mut nul = false;
            let character = loop {
                let character = read_char(con)?;
                if character == EOF || character == b'\n' as c_int {
                    break character;
                }
                if character == 0 {
                    if !skip_nul {
                        nul = true;
                    }
                    continue;
                }
                if !nul {
                    line.push(character as u8);
                }
            };
            if nul {
                crate::conditions::warning(format!(
                    "line {} appears to contain an embedded nul",
                    lines.len() + 1
                ))?;
            }
            if character == EOF {
                if !line.is_empty() || nul {
                    if (*con).text != 0 && (*con).blocking == 0 && !matches!(class_of(con).as_ref(), "gzfile" | "bzfile" | "xzfile") {
                        push_back_bytes(con, &line);
                        (*con).incomplete = 1;
                    } else {
                        lines.push(std::mem::take(&mut line));
                        if warn_incomplete {
                            crate::conditions::warning(format!(
                                "incomplete final line found on '{}'",
                                description_of(con)
                            ))?;
                        }
                    }
                }
                break;
            }
            lines.push(line.clone());
        }
        if n > 0 && (lines.len() as i64) < n && !ok {
            return Err(error("too few lines read in readLines"));
        }
        strings_vector(&lines, encoding)
    })
}

/// `writeLines(text, con, sep, useBytes)`.
unsafe fn do_write_lines(call: &Call<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("can only write character objects"));
    }
    let con = resolve_con(call.arg(1))?.1;
    let separator = values::require_string(call.arg(2), "sep")?;
    with_open(con, "wt", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        let mut line = Vec::new();
        for element in values::strings(text) {
            line.clear();
            line.extend_from_slice(element.as_deref().unwrap_or("NA").as_bytes());
            line.extend_from_slice(separator.as_bytes());
            method_print(con, &line)?;
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `read.dcf()`: one row per record of a Debian control file (records are separated by blank
/// lines), one column per field. Values of the fields not in `keep.white` are folded: every line
/// is trimmed and the lines are joined with newlines. A continuation line holding only `.`
/// stands for an empty line.
unsafe fn do_read_dcf(call: &Call<'_>) -> R<Sexp> {
    let fields = call.arg(1);
    let keep_white = values::strings(call.arg(2));
    let text = with_input(call.arg(0), |bytes, position| {
        let text = String::from_utf8_lossy(&bytes[*position..]).into_owned();
        *position = bytes.len();
        text
    })?;
    let mut records: Vec<Vec<(String, String)>> = Vec::new();
    let mut record: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !record.is_empty() {
                records.push(std::mem::take(&mut record));
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            let Some((tag, value)) = record.last_mut() else {
                let start: String = line.chars().take(20).collect();
                return Err(call.error(format!(
                    "Found continuation line starting '{start} ...' at begin of record."
                )));
            };
            let line = if line.trim() == "." { "" } else { line };
            let keep = keep_white
                .iter()
                .any(|kept| kept.as_deref() == Some(tag.as_str()));
            value.push('\n');
            value.push_str(if keep { line } else { line.trim() });
            continue;
        }
        let Some((tag, value)) = line.split_once(':').filter(|(tag, _)| !tag.is_empty()) else {
            let start: String = line.chars().take(20).collect();
            return Err(call.error(format!("Line starting '{start} ...' is malformed!")));
        };
        let keep = keep_white.iter().any(|kept| kept.as_deref() == Some(tag));
        let value = if keep {
            value.trim_start()
        } else {
            value.trim()
        }
        .to_owned();
        match record.iter_mut().find(|(seen, _)| seen == tag) {
            Some(entry) => entry.1 = value,
            None => record.push((tag.to_owned(), value)),
        }
    }
    if !record.is_empty() {
        records.push(record);
    }
    let columns: Vec<String> = if fields == nil() {
        let mut columns: Vec<String> = Vec::new();
        for (tag, _) in records.iter().flatten() {
            if !columns.contains(tag) {
                columns.push(tag.clone());
            }
        }
        columns
    } else {
        values::strings(fields)
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect()
    };
    let mut cells = Vec::with_capacity(records.len() * columns.len());
    for column in &columns {
        let keep = keep_white
            .iter()
            .any(|kept| kept.as_deref() == Some(column.as_str()));
        for record in &records {
            cells.push(
                record
                    .iter()
                    .find(|(tag, _)| tag == column)
                    .map(|(_, value)| {
                        if keep {
                            value.clone()
                        } else {
                            value.trim().to_owned()
                        }
                    }),
            );
        }
    }
    let matrix = values::string_vector_na(&cells)?;
    let dim = values::integer_vector(&[records.len() as i32, columns.len() as i32])?;
    values::set_attr(matrix, symbols().dim, dim)?;
    let dimnames = values::alloc(SexpType::List, 2)?;
    if !columns.is_empty() {
        values::set_elt(dimnames, 1, values::string_vector(&columns)?);
    }
    values::set_attr(matrix, symbols().dimnames, dimnames)?;
    Ok(matrix)
}

/// `readChar(con, nchars, useBytes)`: one string per count, stopping at the end of the data.
/// `con` may be a raw vector, which is read from memory.
unsafe fn do_read_char(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    let counts = values::ints(call.arg(1)).to_vec();
    let use_bytes = values::require_flag(call.arg(2), "useBytes")?;
    if source.kind() == SexpType::Raw {
        let bytes = values::raws(source);
        let mut position = 0;
        return read_char_strings(&counts, use_bytes, &mut |buffer| {
            let available = bytes.get(position..).unwrap_or_default();
            let count = available.len().min(buffer.len());
            buffer[..count].copy_from_slice(&available[..count]);
            position += count;
            Ok(count)
        });
    }
    let con = resolve_con(source)?.1;
    with_open(con, "rb", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        if (*con).text != 0 {
            crate::conditions::warning(
                "text connection used with readChar(), results may be incorrect",
            )?;
        }
        read_char_strings(&counts, use_bytes, &mut |buffer| method_read(con, buffer))
    })
}

/// `writeChar(object, con, nchars, eos, useBytes)`: each string cut or zero-padded to its count,
/// then `eos` and a nul unless `eos` is `NULL`. A raw vector `con` gets the bytes as a new raw
/// vector instead.
unsafe fn do_write_char(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() != SexpType::String {
        return Err(error("can only write character objects"));
    }
    let target = call.arg(1);
    let counts = values::ints(call.arg(2)).to_vec();
    let terminator = match call.arg(3) {
        eos if eos == nil() => None,
        eos => Some(values::require_string(eos, "eos")?),
    };
    let use_bytes = values::require_flag(call.arg(4), "useBytes")?;
    let strings = values::strings(object);
    if counts.len() < strings.len() {
        return Err(error("invalid 'nchars' argument"));
    }
    let mut bytes = Vec::new();
    for (text, &count) in strings.iter().zip(&counts) {
        let text = text.as_deref().unwrap_or("NA");
        if count == NA_INTEGER || count < 0 {
            return Err(error("invalid 'nchars' argument"));
        }
        let wanted = count as usize;
        let available = if use_bytes { text.len() } else { text.chars().count() };
        if wanted > available {
            crate::conditions::warning(
                "writeChar: more characters requested than are in the string - will zero-pad",
            )?;
            bytes.extend_from_slice(text.as_bytes());
            bytes.resize(bytes.len() + (wanted - available), 0);
        } else if use_bytes {
            bytes.extend_from_slice(text.as_bytes().get(..wanted).unwrap_or_default());
        } else {
            let end = text.char_indices().nth(wanted).map_or(text.len(), |(index, _)| index);
            bytes.extend_from_slice(text.as_bytes().get(..end).unwrap_or_default());
        }
        if let Some(terminator) = &terminator {
            bytes.extend_from_slice(terminator.as_bytes());
            bytes.push(0);
        }
    }
    if target.kind() == SexpType::Raw {
        interp().visible = true;
        return raw_vector(&bytes);
    }
    let con = resolve_con(target)?.1;
    with_open(con, "wb", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        write_all(con, &bytes)
    })?;
    interp().visible = false;
    Ok(nil())
}

/// `readBin(con, what, n, size, signed, swap)`, from a connection or from a raw vector.
unsafe fn do_read_bin(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    let kind = match values::require_string(call.arg(1), "what")?.as_str() {
        "numeric" | "double" => BinaryType::Double,
        "integer" | "int" => BinaryType::Integer,
        "logical" => BinaryType::Logical,
        "complex" => BinaryType::Complex,
        "raw" => BinaryType::Raw,
        "character" => BinaryType::Character,
        _ => return Err(error("invalid 'what' argument")),
    };
    let count = values::as_real_scalar(call.arg(2));
    if count.is_nan() || count < 0.0 {
        return Err(error("invalid 'n' argument"));
    }
    let count = count as usize;
    let size = size_argument(call.arg(3))?;
    let signed = values::require_flag(call.arg(4), "signed")?;
    let swap = values::require_flag(call.arg(5), "swap")?;
    if source.kind() == SexpType::Raw {
        return read_bin_bytes(values::raws(source), kind, count, size, signed, swap);
    }
    let con = resolve_con(source)?.1;
    with_open(con, "rb", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        if (*con).text != 0 {
            return Err(error("can only read from a binary connection"));
        }
        if kind == BinaryType::Character {
            let mut strings = Vec::new();
            'strings: for _ in 0..count {
                let mut text = Vec::new();
                loop {
                    let mut byte = [0u8];
                    if method_read(con, &mut byte)? == 0 {
                        break 'strings;
                    }
                    if byte[0] == 0 {
                        break;
                    }
                    text.push(byte[0]);
                }
                strings.push(text);
            }
            return strings_vector(&strings, Encoding::Native);
        }
        let size = binary_size(kind, size)?;
        let total = count.checked_mul(size).ok_or_else(|| error("too many items"))?;
        let mut bytes = vec![0u8; total];
        let got = read_all(con, &mut bytes)? / size;
        decode_binary(bytes.get(..got * size).unwrap_or_default(), kind, size, signed, swap)
    })
}

/// `writeBin(object, con, size, swap, useBytes)`. A raw vector `con` gets the bytes as a new raw
/// vector instead.
unsafe fn do_write_bin(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let target = call.arg(1);
    let size = size_argument(call.arg(2))?;
    let swap = values::require_flag(call.arg(3), "swap")?;
    let bytes = encode_binary(object, size, swap)?;
    if target.kind() == SexpType::Raw {
        interp().visible = true;
        return raw_vector(&bytes);
    }
    let con = resolve_con(target)?.1;
    with_open(con, "wb", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        if (*con).text != 0 {
            return Err(error("can only write to a binary connection"));
        }
        write_all(con, &bytes)
    })?;
    interp().visible = false;
    Ok(nil())
}

unsafe fn size_argument(value: Sexp) -> R<Option<usize>> {
    match values::as_integer_scalar(value) {
        NA_INTEGER => Ok(None),
        size if size <= 0 => Err(error(format!("size {size} is unknown on this machine"))),
        size => Ok(Some(size as usize)),
    }
}

/// `seek(con, where, origin, rw)`: the position before the move; a NaN `where` only queries.
/// `origin` is 1, 2 or 3 for the start, the current position and the end, and `rw` is 0 for
/// the last used position, 1 for reading and 2 for writing. Moving discards pushback.
unsafe fn do_seek(call: &Call<'_>) -> R<Sexp> {
    let con = resolve_con(call.arg(0))?.1;
    let position = values::as_real_scalar(call.arg(1));
    let origin = match values::as_integer_scalar(call.arg(2)) {
        2 => SeekOrigin::Current,
        3 => SeekOrigin::End,
        _ => SeekOrigin::Start,
    };
    let side = seek_side(values::as_integer_scalar(call.arg(3)));
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if !position.is_nan() {
        clear_push_back_lines(con);
        (*con).save = NO_SAVE;
        (*con).save2 = NO_SAVE;
    }
    values::scalar_real(method_seek(con, position, origin, side)?)
}

/// `truncate(con)`: cuts the file at the current position.
unsafe fn do_truncate(call: &Call<'_>) -> R<Sexp> {
    let con = resolve_con(call.arg(0))?.1;
    if (*con).isopen == 0 {
        return Err(error("can only truncate an open connection"));
    }
    method_truncate(con)?;
    Ok(nil())
}

unsafe fn do_flush(call: &Call<'_>) -> R<Sexp> {
    let con = resolve_con(call.arg(0))?.1;
    if (*con).canwrite != 0 {
        method_fflush(con)?;
    }
    Ok(nil())
}

/// `summary.connection(object)`: description, class, mode, text, opened, can read, can write.
unsafe fn summary(con: Sexp) -> R<Sexp> {
    let con = resolve(con, "invalid connection")?.1;
    let yes_no = |flag: Rboolean| if flag != 0 { "yes" } else { "no" };
    let description = values::scalar_string(&description_of(con))?;
    let class = values::scalar_string(&class_of(con))?;
    let mode = values::scalar_string(&String::from_utf8_lossy(&mode_of(con)))?;
    let text = values::scalar_string(if (*con).text != 0 { "text" } else { "binary" })?;
    let opened = values::scalar_string(if (*con).isopen != 0 { "opened" } else { "closed" })?;
    let can_read = values::scalar_string(yes_no((*con).canread))?;
    let can_write = values::scalar_string(yes_no((*con).canwrite))?;
    values::named_list(&[
        ("description", description),
        ("class", class),
        ("mode", mode),
        ("text", text),
        ("opened", opened),
        ("can read", can_read),
        ("can write", can_write),
    ])
}

/// `pushBack(data, connection, newLine, type)`: the lines are read before anything else, in
/// order, and before any pushback already waiting.
unsafe fn do_push_back(call: &Call<'_>) -> R<Sexp> {
    let data = call.arg(0);
    if data.kind() != SexpType::String {
        return Err(error("invalid 'data' argument"));
    }
    let con = resolve_con(call.arg(1))?.1;
    let newline = values::require_flag(call.arg(2), "newLine")?;
    let lines: Vec<Vec<u8>> = values::strings(data)
        .into_iter()
        .map(|line| {
            let mut line = line.unwrap_or_else(|| "NA".into()).into_bytes();
            if newline {
                line.push(b'\n');
            }
            line
        })
        .collect();
    for line in lines.iter().rev() {
        push_back_bytes(con, line);
    }
    Ok(nil())
}

/// `rawConnectionValue(con)`: everything written so far.
unsafe fn raw_connection_value(con: Sexp) -> R<Sexp> {
    let (slot, con) = resolve_con(con)?;
    let builtin = entry(slot).is_some_and(|entry| entry.builtin);
    match device(con) {
        Ok(Device::Raw(raw)) if builtin => {
            if (*con).canwrite == 0 {
                return Err(error("'con' is not an output rawConnection"));
            }
            raw_vector(&raw.data)
        }
        _ => Err(error("'con' is not a rawConnection")),
    }
}

/// `textConnectionValue(con)`: the completed lines written so far.
unsafe fn text_connection_value(con: Sexp) -> R<Sexp> {
    let (slot, con) = resolve_con(con)?;
    let builtin = entry(slot).is_some_and(|entry| entry.builtin);
    match device(con) {
        Ok(Device::TextOutput(output)) if builtin => {
            strings_vector(&output.lines, Encoding::Native)
        }
        Ok(Device::TextInput(_)) if builtin => Err(error("'con' is not an output textConnection")),
        _ => Err(error("'con' is not a textConnection")),
    }
}

/// `getConnection(what)`.
unsafe fn do_get_connection(call: &Call<'_>) -> R<Sexp> {
    let slot = values::as_integer_scalar(call.arg(0));
    match usize::try_from(slot).ok().filter(|&slot| entry(slot).is_some()) {
        Some(slot) => connection_object(slot),
        None => Err(error(format!("there is no connection {slot}"))),
    }
}

/// `getAllConnections()`: the slots in use, ascending.
unsafe fn all_connections() -> R<Sexp> {
    let slots: Vec<i32> = (0..NCONNECTIONS)
        .filter(|&slot| entry(slot).is_some())
        .map(|slot| slot as i32)
        .collect();
    values::integer_vector(&slots)
}

/// `sink(con, closeOnExit, type, split)`: diverts output to connection `con`, or ends the
/// innermost diversion for `con = -1`. Diverting messages is accepted and ignored.
unsafe fn do_sink(call: &Call<'_>) -> R<Sexp> {
    let target = call.arg(0);
    let close_on_exit = values::require_flag(call.arg(1), "closeOnExit")?;
    let messages = values::require_flag(call.arg(2), "type")?;
    let split = values::require_flag(call.arg(3), "split")?;
    if messages {
        return Ok(nil());
    }
    if !values::inherits(target, "connection") && values::as_integer_scalar(target) < 0 {
        let Some(sink) = interp().connections.sinks.pop() else {
            crate::conditions::warning_call(call.call, "no sink to remove")?;
            return Ok(nil());
        };
        let con = entry(sink.slot).ok_or_else(|| error("invalid connection"))?.con;
        if sink.close_on_exit {
            close_slot(sink.slot)?;
        } else if sink.opened {
            method_close(con)?;
        }
        return Ok(nil());
    }
    let (slot, con) = resolve_con(target)?;
    if interp().connections.sinks.len() >= MAX_SINKS {
        return Err(error("sink stack is full"));
    }
    let opened = (*con).isopen == 0;
    if opened {
        let saved = mode_of(con);
        set_mode(con, b"wt");
        let result = open_con(con);
        set_mode(con, &saved);
        result?;
    }
    if (*con).canwrite == 0 {
        return Err(error("cannot write to this connection"));
    }
    interp().connections.sinks.push(Sink {
        slot,
        close_on_exit,
        opened,
        split,
    });
    Ok(nil())
}

/// `sink.number(type != "message")`.
unsafe fn do_sink_number(call: &Call<'_>) -> R<Sexp> {
    if values::as_logical_scalar(call.arg(0)) == 1 {
        scalar_integer(interp().connections.sinks.len() as i32)
    } else {
        scalar_integer(2)
    }
}

/// `cat(list(...), file, sep, fill, labels, append)`.
unsafe fn do_cat(call: &Call<'_>) -> R<Sexp> {
    let items = call.arg(0);
    let file = call.arg(1);
    let separators = call.arg(2);
    let fill = call.arg(3);
    let fill_width = if fill.kind() == SexpType::Logical {
        if values::as_logical_scalar(fill) == 1 {
            crate::format::width_option()
        } else {
            0
        }
    } else {
        values::as_integer_scalar(fill).max(0) as usize
    };
    let separator_count = values::length(separators);
    let separator_at = |index: usize| -> String {
        if separator_count == 0 {
            String::new()
        } else {
            values::string_at(separators, index % separator_count)
                .map_or_else(|| "NA".into(), |text| text.into_owned())
        }
    };
    // With `fill`, every line starts with the next of `labels` and a space.
    let labels = call.arg(4);
    let label_count = if fill_width > 0 && labels.kind() == SexpType::String {
        values::length(labels)
    } else {
        0
    };
    let mut line_index = 0usize;
    let mut output = String::new();
    let mut line_width = 0usize;
    let mut start_line = |output: &mut String, line_width: &mut usize| {
        if label_count > 0 {
            let label = values::string_at(labels, line_index % label_count)
                .map_or_else(|| "NA".into(), |text| text.into_owned());
            output.push_str(&label);
            output.push(' ');
            *line_width = label.len() + 1;
        }
        line_index += 1;
    };
    start_line(&mut output, &mut line_width);
    let mut separator_index = 0usize;
    for item_index in 0..values::length(items) {
        let item = items.element(item_index);
        let texts: Vec<String> = match item.kind() {
            SexpType::Nil => continue,
            SexpType::Symbol => vec![values::symbol_name(item).into_owned()],
            SexpType::String => (0..values::length(item))
                .map(|index| {
                    values::string_at(item, index).map_or_else(|| "NA".into(), |text| text.into_owned())
                })
                .collect(),
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Raw => {
                let item = values::materialize(item)?;
                (0..values::length(item)).map(|index| cat_element(item, index)).collect()
            }
            SexpType::List
                if (0..values::length(item)).all(|index| {
                    let element = item.element(index);
                    element.kind().is_atomic() && values::length(element) == 1
                }) =>
            {
                (0..values::length(item)).map(|index| cat_element(item.element(index), 0)).collect()
            }
            SexpType::Language => vec![crate::deparse::deparse_one_line(item)],
            kind => {
                return Err(call.error(format!(
                    "argument {} (type '{}') cannot be handled by 'cat'",
                    item_index + 1,
                    kind.name()
                )));
            }
        };
        // Every object after the first, even an empty one, is preceded by a separator, but a
        // NULL object is skipped without one.
        if item_index > 0 && texts.is_empty() {
            output.push_str(&separator_at(separator_index));
            separator_index += 1;
        }
        for (position, text) in texts.into_iter().enumerate() {
            // The separator goes before the line break, and a piece only fits on the line if
            // the separator after it does too.
            if position > 0 || item_index > 0 {
                let separator = separator_at(separator_index);
                separator_index += 1;
                output.push_str(&separator);
                line_width += separator.len();
                if let Some(position) = separator.rfind('\n') {
                    line_width = separator.len() - position - 1;
                }
                let next_separator = separator_at(separator_index).len();
                if fill_width > 0
                    && line_width > 0
                    && line_width + text.len() + next_separator > fill_width
                {
                    output.push('\n');
                    line_width = 0;
                    start_line(&mut output, &mut line_width);
                }
            }
            line_width += text.len();
            if let Some(position) = text.rfind('\n') {
                line_width = text.len() - position - 1;
            }
            output.push_str(&text);
        }
    }
    // A separator holding a newline makes the separators terminators, so the output always
    // ends with one, even when there was nothing to print.
    let newline_separator = (0..separator_count).any(|index| separator_at(index).contains('\n'));
    if newline_separator || (fill_width > 0 && !output.ends_with('\n') && !output.is_empty()) {
        output.push('\n');
    }
    print(file, output.as_bytes())?;
    Ok(nil())
}

/// One element as `cat` prints it: numbers with up to 7 significant digits.
unsafe fn cat_element(vector: Sexp, index: usize) -> String {
    match vector.kind() {
        SexpType::Real => {
            let value = values::reals(vector)[index];
            if values::is_na(value) {
                "NA".into()
            } else {
                let digits = crate::format::Settings::current().digits;
                crate::format::format_real_alone_scipen(value, digits)
            }
        }
        SexpType::String => {
            values::string_at(vector, index).map_or_else(|| "NA".into(), |text| text.into_owned())
        }
        SexpType::Complex => crate::format::format_complex_alone(values::complexes(vector)[index], 7),
        _ => crate::coerce::element_as_charsxp(vector, index)
            .map(|charsxp| {
                if charsxp == values::na_string() {
                    "NA".to_owned()
                } else {
                    values::char_str(charsxp).into_owned()
                }
            })
            .unwrap_or_default(),
    }
}

// The table and the connection objects.

/// `save` and `save2` hold this when they hold no character.
const NO_SAVE: c_int = -1000;
const EOF: c_int = -1;
const READ_CHUNK: usize = 64 * 1024;

/// Which position `seek` reports and moves, as `seek(rw = )` chooses it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeekSide {
    LastUsed = 0,
    Read = 1,
    Write = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeekOrigin {
    Start = 1,
    Current = 2,
    End = 3,
}

/// A text connection's target variable: each completed line updates it.
#[derive(Clone, Copy)]
struct TextTarget {
    symbol: Sexp,
    env: Sexp,
}

/// The element types `readBin` and `writeBin` move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BinaryType {
    Integer,
    Double,
    Logical,
    Complex,
    Character,
    Raw,
}

fn entries() -> &'static mut Vec<Option<Entry>> {
    &mut interp().connections.entries
}

fn entry(slot: usize) -> Option<&'static Entry> {
    entries().get(slot).and_then(Option::as_ref)
}

unsafe fn resolve_con(object: Sexp) -> R<(usize, Rconnection)> {
    resolve(object, "'con' is not a connection")
}

fn default_vfprintf() -> Option<OpaqueMethod> {
    interp().hooks.connection_vfprintf
}

/// Takes a free slot for `con`, failing as GNU R does when the table is full.
fn claim_slot(con: Rconnection, builtin: bool) -> R<usize> {
    let entries = entries();
    let slot = (3..NCONNECTIONS)
        .find(|&slot| entries.get(slot).is_some_and(Option::is_none))
        .ok_or_else(|| error("all connections are in use"))?;
    entries[slot] = Some(Entry { con, builtin });
    Ok(slot)
}

struct Methods {
    open: OpenMethod,
    close: VoidMethod,
    destroy: VoidMethod,
    fgetc: IntMethod,
    fgetc_internal: IntMethod,
    seek: SeekMethod,
    truncate: VoidMethod,
    fflush: IntMethod,
    read: ReadMethod,
    write: WriteMethod,
}

static BUILTIN_METHODS: Methods = Methods {
    open: builtin_open,
    close: builtin_close,
    destroy: builtin_destroy,
    fgetc: builtin_fgetc,
    fgetc_internal: builtin_fgetc,
    seek: builtin_seek,
    truncate: builtin_truncate,
    fflush: builtin_fflush,
    read: builtin_read,
    write: builtin_write,
};

/// What `R_new_custom_connection` installs: each method reports that it is not enabled, except
/// that `fgetc` reads through `fgetc_internal`, the method packages actually supply.
static CUSTOM_METHODS: Methods = Methods {
    open: null_open,
    close: null_close,
    destroy: null_destroy,
    fgetc: custom_fgetc,
    fgetc_internal: null_fgetc,
    seek: null_seek,
    truncate: null_truncate,
    fflush: null_fflush,
    read: null_read,
    write: null_write,
};

/// A zeroed struct with the given names and methods, closed, reading and writing allowed, as
/// GNU R initializes one before its class opens it.
unsafe fn allocate_rconn(
    class: &[u8],
    description: &[u8],
    mode: &[u8],
    methods: &Methods,
    vfprintf: Option<OpaqueMethod>,
) -> Rconnection {
    let con = Box::into_raw(Box::<Rconn>::new_zeroed().assume_init());
    (*con).class = malloc_string(class);
    (*con).description = malloc_string(description);
    set_mode(con, mode);
    (*con).text = 1;
    (*con).canread = 1;
    (*con).canwrite = 1;
    (*con).blocking = 1;
    (*con).open = Some(methods.open);
    (*con).close = Some(methods.close);
    (*con).destroy = Some(methods.destroy);
    (*con).vfprintf = vfprintf;
    (*con).fgetc = Some(methods.fgetc);
    (*con).fgetc_internal = Some(methods.fgetc_internal);
    (*con).seek = Some(methods.seek);
    (*con).truncate = Some(methods.truncate);
    (*con).fflush = Some(methods.fflush);
    (*con).read = Some(methods.read);
    (*con).write = Some(methods.write);
    (*con).save = NO_SAVE;
    (*con).save2 = NO_SAVE;
    (*con).status = NA_INTEGER;
    for (target, &byte) in (*con).encname.iter_mut().zip(b"native.enc") {
        *target = byte as c_char;
    }
    con
}

/// The class and description are `malloc`ed, as in GNU R, because a package may replace one
/// with its own `malloc`ed string and the runtime frees whatever is there.
unsafe fn malloc_string(bytes: &[u8]) -> *mut c_char {
    let text = libc::malloc(bytes.len() + 1) as *mut c_char;
    if text.is_null() {
        panic!("out of memory allocating a connection string");
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), text.cast(), bytes.len());
    *text.add(bytes.len()) = 0;
    text
}

unsafe fn set_mode(con: Rconnection, mode: &[u8]) {
    (*con).mode = [0; 5];
    for (target, &byte) in (*con).mode.iter_mut().zip(mode.iter().take(4)) {
        *target = byte as c_char;
    }
}

unsafe fn mode_of(con: Rconnection) -> Vec<u8> {
    CStr::from_ptr((*con).mode.as_ptr()).to_bytes().to_vec()
}

unsafe fn description_of(con: Rconnection) -> String {
    String::from_utf8_lossy(c_text((*con).description)).into_owned()
}

unsafe fn class_of(con: Rconnection) -> String {
    String::from_utf8_lossy(c_text((*con).class)).into_owned()
}

unsafe fn c_text<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        &[]
    } else {
        CStr::from_ptr(text).to_bytes()
    }
}

/// The R object for a slot: the slot number, its classes, and the `conn_id` pointer.
unsafe fn connection_object(slot: usize) -> R<Sexp> {
    let con = entry(slot)
        .ok_or_else(|| error(format!("there is no connection {slot}")))?
        .con;
    let object = scalar_integer(slot as c_int)?;
    let classes = values::alloc(SexpType::String, 2)?;
    for (index, name) in [c_text((*con).class), b"connection"].into_iter().enumerate() {
        let charsxp = values::mkchar_bytes(name, Encoding::Native)?;
        crate::heap().set_element(classes, index, charsxp);
    }
    values::set_attr(object, symbols().class, classes)?;
    if let Some(id) = Sexp::from_raw((*con).ex_ptr.cast()) {
        values::set_attr(object, values::install("conn_id")?, id)?;
    }
    Ok(object)
}

/// Registers a new connection and gives it its `conn_id`. A collection that finds the pointer
/// unreachable closes the connection, with GNU R's warning.
unsafe fn register(con: Rconnection, builtin: bool) -> R<Sexp> {
    let slot = match claim_slot(con, builtin) {
        Ok(slot) => slot,
        Err(unwind) => {
            free_rconn(con, builtin);
            return Err(unwind);
        }
    };
    let heap = crate::heap();
    let tag = values::install("connection")?;
    let id = heap.alloc_external_ptr(con.cast(), tag, nil())?;
    heap.root(id);
    heap.alloc_weak_ref(id, nil(), nil(), connection_finalizer as *const () as usize, false)?;
    (*con).ex_ptr = id.as_ptr().cast();
    (*con).id = con.cast();
    connection_object(slot)
}

/// Runs when a connection's `conn_id` becomes unreachable while the connection is open.
unsafe extern "C" fn connection_finalizer(id: Sexp) {
    from_native(|| {
        let address = id.external_address();
        if address.is_null() {
            return Ok(());
        }
        let found = (3..NCONNECTIONS)
            .find(|&slot| entry(slot).is_some_and(|entry| entry.con.cast() == address));
        if let Some(slot) = found {
            let con = address as Rconnection;
            crate::conditions::warning(format!(
                "closing unused connection {slot} ({})",
                description_of(con)
            ))?;
            close_slot(slot)?;
        }
        Ok(())
    })
}

/// Closes and destroys the connection in `slot`, freeing the slot. Returns the close status.
unsafe fn close_slot(slot: usize) -> R<c_int> {
    let Some(Entry { con, builtin }) = entries().get_mut(slot).and_then(Option::take) else {
        return Err(error("invalid connection"));
    };
    let closed = if (*con).isopen != 0 { method_close(con) } else { Ok(()) };
    let status = (*con).status;
    let destroyed = method_destroy(con);
    if let Some(id) = Sexp::from_raw((*con).ex_ptr.cast()) {
        id.set_external_address(null_mut());
    }
    free_rconn(con, builtin);
    closed?;
    destroyed?;
    Ok(status)
}

unsafe fn free_rconn(con: Rconnection, builtin: bool) {
    clear_push_back_lines(con);
    if builtin && !(*con).private.is_null() {
        drop(Box::from_raw((*con).private as *mut Device));
    }
    libc::free((*con).class.cast());
    libc::free((*con).description.cast());
    drop(Box::from_raw(con));
}

/// A built-in connection, registered and, when `open` is not empty, opened. `configure` sets
/// the struct's flags before the connection opens.
unsafe fn new_builtin(
    class: &str,
    description: &[u8],
    open: &str,
    default_mode: &str,
    device: Device,
    configure: impl FnOnce(Rconnection),
) -> R<Sexp> {
    let mode = if open.is_empty() { default_mode } else { open };
    let con = allocate_rconn(
        class.as_bytes(),
        description,
        mode.as_bytes(),
        &BUILTIN_METHODS,
        default_vfprintf(),
    );
    (*con).private = Box::into_raw(Box::new(device)).cast();
    let object = register(con, true)?;
    configure(con);
    if !open.is_empty() {
        let slot = resolve_con(object)?.0;
        if let Err(unwind) = open_con(con) {
            close_slot(slot)?;
            return Err(unwind);
        }
    }
    Ok(object)
}

fn temporary_path() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    std::env::temp_dir().join(format!("ry-connection-{}-{count}", std::process::id()))
}

unsafe fn open_con(con: Rconnection) -> R<()> {
    if !method_open(con)? {
        return Err(error("cannot open the connection"));
    }
    Ok(())
}

/// Runs `body` with the connection open in `mode`, opening it first and closing it afterwards
/// when it was closed, as R's readers and writers do for an unopened connection.
unsafe fn with_open<T>(con: Rconnection, mode: &str, body: impl FnOnce() -> R<T>) -> R<T> {
    if (*con).isopen != 0 {
        return body();
    }
    let saved = mode_of(con);
    set_mode(con, mode.as_bytes());
    let opened = open_con(con);
    set_mode(con, &saved);
    opened?;
    let result = body();
    let closed = method_close(con);
    match (result, closed) {
        (Err(unwind), _) => Err(unwind),
        (Ok(_), Err(unwind)) => Err(unwind),
        (Ok(value), Ok(())) => Ok(value),
    }
}

// Text reads, pushback, and binary data.

/// One character as text reads see it: pushback first, then the look-ahead, then the device,
/// with `\r` and `\r\n` both read as `\n`.
unsafe fn read_char(con: Rconnection) -> R<c_int> {
    if (*con).save2 != NO_SAVE {
        let character = (*con).save2;
        (*con).save2 = NO_SAVE;
        return Ok(character);
    }
    while (*con).nPushBack > 0 {
        let top_index = (*con).nPushBack as usize - 1;
        let top = *(*con).PushBack.add(top_index);
        let position = (*con).posPushBack as usize;
        let character = *top.add(position) as u8;
        if character == 0 {
            pop_push_back(con);
            continue;
        }
        (*con).posPushBack += 1;
        if *top.add(position + 1) == 0 {
            pop_push_back(con);
        }
        return Ok(character as c_int);
    }
    if (*con).save != NO_SAVE {
        let character = (*con).save;
        (*con).save = NO_SAVE;
        return Ok(character);
    }
    let character = method_fgetc(con)?;
    if character == b'\r' as c_int {
        let next = method_fgetc(con)?;
        if next != b'\n' as c_int {
            (*con).save = if next != b'\r' as c_int { next } else { b'\n' as c_int };
        }
        return Ok(b'\n' as c_int);
    }
    Ok(character)
}

/// Everything a text read would still see, as [`read_char`] would deliver it. A built-in
/// device is read in bulk once the pushback and look-ahead are used up.
unsafe fn read_text_rest(con: Rconnection) -> R<Vec<u8>> {
    let mut text = Vec::new();
    while (*con).save2 != NO_SAVE || (*con).nPushBack > 0 || (*con).save != NO_SAVE {
        let character = read_char(con)?;
        if character == EOF {
            return Ok(text);
        }
        text.push(character as u8);
    }
    let builtin = (*con)
        .fgetc
        .is_some_and(|method| is_method(method, builtin_fgetc as IntMethod));
    if !builtin {
        loop {
            let character = read_char(con)?;
            if character == EOF {
                return Ok(text);
            }
            text.push(character as u8);
        }
    }
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut after_return = false;
    loop {
        let count = device_read(con, &mut chunk)?;
        if count == 0 {
            return Ok(text);
        }
        for &byte in chunk.get(..count).unwrap_or_default() {
            match (after_return, byte) {
                (true, b'\n') => after_return = false,
                // The second `\r` reads as `\n` without starting a pair of its own.
                (true, b'\r') => {
                    after_return = false;
                    text.push(b'\n');
                }
                (_, b'\r') => {
                    after_return = true;
                    text.push(b'\n');
                }
                (_, byte) => {
                    after_return = false;
                    text.push(byte);
                }
            }
        }
    }
}

unsafe fn pop_push_back(con: Rconnection) {
    let count = (*con).nPushBack as usize;
    libc::free((*(*con).PushBack.add(count - 1)).cast());
    (*con).nPushBack -= 1;
    (*con).posPushBack = 0;
    if (*con).nPushBack == 0 {
        libc::free((*con).PushBack.cast());
        (*con).PushBack = null_mut();
    }
}

unsafe fn clear_push_back_lines(con: Rconnection) {
    for _ in 0..(*con).nPushBack {
        pop_push_back(con);
    }
}

/// Pushes one line on top of the pushback stack. A partly read top line is first cut to its
/// unread rest, so `posPushBack` can apply to the new top.
unsafe fn push_back_bytes(con: Rconnection, line: &[u8]) {
    if (*con).nPushBack > 0 && (*con).posPushBack > 0 {
        let top_slot = (*con).PushBack.add((*con).nPushBack as usize - 1);
        let rest = c_text((*top_slot).add((*con).posPushBack as usize)).to_vec();
        libc::free((*top_slot).cast());
        *top_slot = malloc_string(&rest);
        (*con).posPushBack = 0;
    }
    let count = (*con).nPushBack as usize + 1;
    let lines =
        libc::realloc((*con).PushBack.cast(), count * size_of::<*mut c_char>()) as *mut *mut c_char;
    if lines.is_null() {
        panic!("out of memory pushing back onto a connection");
    }
    *lines.add(count - 1) = malloc_string(line);
    (*con).PushBack = lines;
    (*con).nPushBack = count as c_int;
}

/// `readChar`'s strings, one per count, from `read`, which fills a buffer and returns how much
/// it filled.
unsafe fn read_char_strings(
    counts: &[i32],
    use_bytes: bool,
    read: &mut dyn FnMut(&mut [u8]) -> R<usize>,
) -> R<Sexp> {
    let mut strings = Vec::new();
    for &count in counts {
        if count == NA_INTEGER || count < 0 {
            return Err(error("invalid 'nchars' argument"));
        }
        let mut text = Vec::new();
        let mut wanted = count as usize;
        while wanted > 0 {
            let mut lead = [0u8];
            if read(&mut lead)? == 0 {
                break;
            }
            text.push(lead[0]);
            if !use_bytes {
                let mut rest = vec![0u8; utf8_continuation_bytes(lead[0])];
                let mut filled = 0;
                while filled < rest.len() {
                    let got = read(&mut rest[filled..])?;
                    if got == 0 {
                        break;
                    }
                    filled += got;
                }
                text.extend_from_slice(&rest[..filled]);
            }
            wanted -= 1;
        }
        if text.is_empty() && count > 0 {
            break;
        }
        if let Some(end) = text.iter().position(|&byte| byte == 0) {
            text.truncate(end);
        }
        strings.push(text);
    }
    strings_vector(&strings, Encoding::Native)
}

fn utf8_continuation_bytes(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        _ => 0,
    }
}

/// Reads until `buffer` is full or a read returns nothing.
unsafe fn read_all(con: Rconnection, buffer: &mut [u8]) -> R<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let count = method_read(con, &mut buffer[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    Ok(filled)
}

/// Writes every byte, as `R_WriteConnection` callers expect.
unsafe fn write_all(con: Rconnection, bytes: &[u8]) -> R<()> {
    if !bytes.is_empty() {
        method_write(con, bytes)?;
    }
    Ok(())
}

unsafe fn strings_vector(lines: &[Vec<u8>], encoding: Encoding) -> R<Sexp> {
    let vector = values::alloc(SexpType::String, lines.len())?;
    for (index, line) in lines.iter().enumerate() {
        let charsxp = values::mkchar_bytes(line, encoding)?;
        crate::heap().set_element(vector, index, charsxp);
    }
    Ok(vector)
}

unsafe fn raw_vector(bytes: &[u8]) -> R<Sexp> {
    let vector = values::alloc(SexpType::Raw, bytes.len())?;
    values::raws_mut(vector).copy_from_slice(bytes);
    Ok(vector)
}

/// The size each `readBin`/`writeBin` element type takes by default, and the sizes it allows.
fn binary_size(kind: BinaryType, size: Option<usize>) -> R<usize> {
    let (default, allowed): (usize, &[usize]) = match kind {
        BinaryType::Integer => (4, &[1, 2, 4, 8]),
        BinaryType::Logical => (4, &[1, 2, 4, 8]),
        BinaryType::Double => (8, &[4, 8]),
        BinaryType::Complex => (16, &[16]),
        BinaryType::Raw => (1, &[1]),
        BinaryType::Character => (1, &[1]),
    };
    match size {
        None => Ok(default),
        Some(size) if allowed.contains(&size) => Ok(size),
        Some(size) => Err(error(format!("size {size} is unknown on this machine"))),
    }
}

/// `readBin` on bytes in memory, as for a raw vector argument.
unsafe fn read_bin_bytes(
    bytes: &[u8],
    kind: BinaryType,
    n: usize,
    size: Option<usize>,
    signed: bool,
    swap: bool,
) -> R<Sexp> {
    if kind == BinaryType::Character {
        let strings: Vec<Vec<u8>> = bytes
            .split_inclusive(|&byte| byte == 0)
            .filter(|piece| piece.last() == Some(&0))
            .take(n)
            .map(|piece| piece[..piece.len() - 1].to_vec())
            .collect();
        return strings_vector(&strings, Encoding::Native);
    }
    let size = binary_size(kind, size)?;
    let count = (bytes.len() / size).min(n);
    decode_binary(&bytes[..count * size], kind, size, signed, swap)
}

unsafe fn decode_binary(
    bytes: &[u8],
    kind: BinaryType,
    size: usize,
    signed: bool,
    swap: bool,
) -> R<Sexp> {
    let count = bytes.len() / size;
    let element = |index: usize| -> [u8; 16] {
        let mut value = [0u8; 16];
        value[..size].copy_from_slice(&bytes[index * size..(index + 1) * size]);
        if swap {
            value[..size].reverse();
        }
        value
    };
    match kind {
        BinaryType::Integer | BinaryType::Logical => {
            let vector = values::alloc(
                if kind == BinaryType::Integer { SexpType::Integer } else { SexpType::Logical },
                count,
            )?;
            for (index, target) in vector.slice_mut::<i32>().iter_mut().enumerate() {
                let raw = element(index);
                *target = match (size, signed) {
                    (1, true) => raw[0] as i8 as i32,
                    (1, false) => raw[0] as i32,
                    (2, true) => i16::from_ne_bytes([raw[0], raw[1]]) as i32,
                    (2, false) => u16::from_ne_bytes([raw[0], raw[1]]) as i32,
                    (8, _) => i64::from_ne_bytes([
                        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                    ]) as i32,
                    _ => i32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]),
                };
            }
            Ok(vector)
        }
        BinaryType::Double => {
            let vector = values::alloc(SexpType::Real, count)?;
            for (index, target) in vector.slice_mut::<f64>().iter_mut().enumerate() {
                let raw = element(index);
                *target = if size == 4 {
                    f32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64
                } else {
                    f64::from_ne_bytes([
                        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                    ])
                };
            }
            Ok(vector)
        }
        BinaryType::Complex => {
            let vector = values::alloc(SexpType::Complex, count)?;
            for (index, target) in vector.slice_mut::<Rcomplex>().iter_mut().enumerate() {
                let mut raw = [0u8; 16];
                raw.copy_from_slice(&bytes[index * 16..(index + 1) * 16]);
                if swap {
                    raw[..8].reverse();
                    raw[8..].reverse();
                }
                let mut half = [0u8; 8];
                half.copy_from_slice(&raw[..8]);
                target.r = f64::from_ne_bytes(half);
                half.copy_from_slice(&raw[8..]);
                target.i = f64::from_ne_bytes(half);
            }
            Ok(vector)
        }
        BinaryType::Raw => raw_vector(bytes),
        BinaryType::Character => Err(error("character data has no fixed size")),
    }
}

/// The bytes `writeBin(object, size, endian)` writes. `object` must not be ALTREP.
unsafe fn encode_binary(object: Sexp, size: Option<usize>, swap: bool) -> R<Vec<u8>> {
    let kind = match object.kind() {
        SexpType::Integer => BinaryType::Integer,
        SexpType::Logical => BinaryType::Logical,
        SexpType::Real => BinaryType::Double,
        SexpType::Complex => BinaryType::Complex,
        SexpType::Raw => return Ok(values::raws(object).to_vec()),
        SexpType::String => {
            let mut bytes = Vec::new();
            for element in values::strings(object) {
                bytes.extend_from_slice(element.as_deref().unwrap_or("NA").as_bytes());
                bytes.push(0);
            }
            return Ok(bytes);
        }
        _ => return Err(error("can only write vector objects")),
    };
    let size = binary_size(kind, size)?;
    let mut bytes = Vec::with_capacity(object.length() as usize * size);
    let mut push = |mut element: Vec<u8>| {
        if swap {
            element.reverse();
        }
        bytes.extend_from_slice(&element);
    };
    match kind {
        BinaryType::Integer | BinaryType::Logical => {
            for &value in object.slice::<i32>() {
                push(match size {
                    1 => vec![value as u8],
                    2 => (value as i16).to_ne_bytes().to_vec(),
                    8 => (value as i64).to_ne_bytes().to_vec(),
                    _ => value.to_ne_bytes().to_vec(),
                });
            }
        }
        BinaryType::Double => {
            for &value in object.slice::<f64>() {
                push(if size == 4 {
                    (value as f32).to_ne_bytes().to_vec()
                } else {
                    value.to_ne_bytes().to_vec()
                });
            }
        }
        BinaryType::Complex => {
            for value in object.slice::<Rcomplex>() {
                push(value.r.to_ne_bytes().to_vec());
                push(value.i.to_ne_bytes().to_vec());
            }
        }
        BinaryType::Raw | BinaryType::Character => {}
    }
    Ok(bytes)
}

// Method dispatch. A method pointer that is ry's own runs its device directly; any other is
// package code and runs through the host.

fn is_method<F: Copy>(method: F, own: F) -> bool {
    // SAFETY: both are function pointers of the same type, compared by address.
    unsafe {
        std::mem::transmute_copy::<F, usize>(&method) == std::mem::transmute_copy::<F, usize>(&own)
    }
}

fn missing(what: &str) -> Unwind {
    error(format!("'{what}' not enabled for this connection"))
}

unsafe fn call_native(con: Rconnection, method: NativeMethod<'_>) -> R<()> {
    (interp().hooks.connection_method)(con, method)
}

/// Runs the body of a Rust method that native code called, through the host, which turns an
/// `Err` into the jump native code expects.
unsafe fn from_native<T: Default>(body: impl FnOnce() -> R<T>) -> T {
    let mut body = Some(body);
    let mut value = T::default();
    (interp().hooks.enter_from_native)(&mut || {
        if let Some(body) = body.take() {
            value = body()?;
        }
        Ok(())
    });
    value
}

unsafe fn method_open(con: Rconnection) -> R<bool> {
    let method = (*con).open.ok_or_else(|| error("open not enabled for this connection"))?;
    if is_method(method, builtin_open as OpenMethod) {
        return device_open(con);
    }
    let mut result = 0;
    call_native(con, NativeMethod::Open { method, result: &mut result })?;
    Ok(result != 0)
}

unsafe fn method_close(con: Rconnection) -> R<()> {
    match (*con).close {
        Some(method) if is_method(method, builtin_close as VoidMethod) => device_close(con),
        Some(method) => call_native(con, NativeMethod::Void(method)),
        None => {
            (*con).isopen = 0;
            Ok(())
        }
    }
}

unsafe fn method_destroy(con: Rconnection) -> R<()> {
    match (*con).destroy {
        Some(method) if is_method(method, builtin_destroy as VoidMethod) => Ok(()),
        Some(method) => call_native(con, NativeMethod::Void(method)),
        None => Ok(()),
    }
}

unsafe fn method_fgetc(con: Rconnection) -> R<c_int> {
    let method = (*con).fgetc.ok_or_else(|| missing("getc"))?;
    if is_method(method, builtin_fgetc as IntMethod) {
        return device_fgetc(con);
    }
    if is_method(method, custom_fgetc as IntMethod) {
        return method_fgetc_internal(con);
    }
    let mut result = 0;
    call_native(con, NativeMethod::Int { method, result: &mut result })?;
    Ok(result)
}

unsafe fn method_fgetc_internal(con: Rconnection) -> R<c_int> {
    let method = (*con).fgetc_internal.ok_or_else(|| missing("getc"))?;
    if is_method(method, builtin_fgetc as IntMethod) {
        return device_fgetc(con);
    }
    let mut result = 0;
    call_native(con, NativeMethod::Int { method, result: &mut result })?;
    Ok(result)
}

unsafe fn method_seek(con: Rconnection, position: f64, origin: SeekOrigin, side: SeekSide) -> R<f64> {
    let method = (*con).seek.ok_or_else(|| missing("seek"))?;
    if is_method(method, builtin_seek as SeekMethod) {
        return device_seek(con, position, origin, side);
    }
    let mut result = 0.0;
    call_native(
        con,
        NativeMethod::Seek {
            method,
            position,
            origin: origin as c_int,
            rw: side as c_int,
            result: &mut result,
        },
    )?;
    Ok(result)
}

unsafe fn method_truncate(con: Rconnection) -> R<()> {
    let method = (*con)
        .truncate
        .ok_or_else(|| error("truncation not enabled for this connection"))?;
    if is_method(method, builtin_truncate as VoidMethod) {
        return device_truncate(con);
    }
    call_native(con, NativeMethod::Void(method))
}

unsafe fn method_fflush(con: Rconnection) -> R<c_int> {
    let Some(method) = (*con).fflush else {
        return Ok(0);
    };
    if is_method(method, builtin_fflush as IntMethod) {
        return device_flush(con).map(|()| 0);
    }
    let mut result = 0;
    call_native(con, NativeMethod::Int { method, result: &mut result })?;
    Ok(result)
}

unsafe fn method_read(con: Rconnection, buffer: &mut [u8]) -> R<usize> {
    let method = (*con).read.ok_or_else(|| missing("read"))?;
    if is_method(method, builtin_read as ReadMethod) {
        return device_read(con, buffer);
    }
    let length = buffer.len();
    let mut result = 0;
    call_native(con, NativeMethod::Read { method, buffer, result: &mut result })?;
    // A method that claims more than it was given would make every caller read past the end.
    Ok(result.min(length))
}

unsafe fn method_write(con: Rconnection, bytes: &[u8]) -> R<usize> {
    let method = (*con).write.ok_or_else(|| missing("write"))?;
    if is_method(method, builtin_write as WriteMethod) {
        return device_write(con, bytes, false);
    }
    let mut result = 0;
    call_native(con, NativeMethod::Write { method, bytes, result: &mut result })?;
    Ok(result)
}

/// Writes text the way `Rconn_printf` does: through `vfprintf`, which by default formats and
/// calls `write`, so the default goes straight to `write`. ry's own connections print through
/// the device, because a text output connection takes text but has no `write` method.
unsafe fn method_print(con: Rconnection, text: &[u8]) -> R<()> {
    let default = default_vfprintf();
    match (*con).vfprintf {
        Some(method) if default.is_none_or(|default| !is_method(method, default)) => {
            let text = CString::new(text)
                .map_err(|_| error("embedded nul in a string written to a connection"))?;
            let mut result = 0;
            call_native(con, NativeMethod::Print { method, text: &text, result: &mut result })
        }
        _ => match (*con).write {
            Some(method) if is_method(method, builtin_write as WriteMethod) => {
                device_write(con, text, true).map(drop)
            }
            _ => method_write(con, text).map(drop),
        },
    }
}

unsafe extern "C" fn builtin_open(con: Rconnection) -> Rboolean {
    from_native(|| device_open(con).map(Rboolean::from))
}

unsafe extern "C" fn builtin_close(con: Rconnection) {
    from_native(|| device_close(con))
}

/// The device itself is dropped when the slot is freed, so destroy has nothing left to do.
unsafe extern "C" fn builtin_destroy(_con: Rconnection) {}

unsafe extern "C" fn builtin_fgetc(con: Rconnection) -> c_int {
    from_native(|| device_fgetc(con))
}

unsafe extern "C" fn builtin_seek(con: Rconnection, position: f64, origin: c_int, rw: c_int) -> f64 {
    let origin = match origin {
        2 => SeekOrigin::Current,
        3 => SeekOrigin::End,
        _ => SeekOrigin::Start,
    };
    from_native(|| device_seek(con, position, origin, seek_side(rw)))
}

unsafe extern "C" fn builtin_truncate(con: Rconnection) {
    from_native(|| device_truncate(con))
}

unsafe extern "C" fn builtin_fflush(con: Rconnection) -> c_int {
    from_native(|| device_flush(con).map(|()| 0))
}

unsafe extern "C" fn builtin_read(
    buffer: *mut c_void,
    size: usize,
    count: usize,
    con: Rconnection,
) -> usize {
    from_native(|| {
        let bytes = size.checked_mul(count).ok_or_else(|| error("read request too large"))?;
        if bytes == 0 {
            return Ok(0);
        }
        let slice = std::slice::from_raw_parts_mut(buffer as *mut u8, bytes);
        Ok(device_read(con, slice)? / size)
    })
}

unsafe extern "C" fn builtin_write(
    buffer: *const c_void,
    size: usize,
    count: usize,
    con: Rconnection,
) -> usize {
    from_native(|| {
        let bytes = size.checked_mul(count).ok_or_else(|| error("write request too large"))?;
        if bytes == 0 {
            return Ok(0);
        }
        let slice = std::slice::from_raw_parts(buffer as *const u8, bytes);
        Ok(device_write(con, slice, false)? / size)
    })
}

fn seek_side(code: c_int) -> SeekSide {
    match code {
        1 => SeekSide::Read,
        2 => SeekSide::Write,
        _ => SeekSide::LastUsed,
    }
}

unsafe extern "C" fn null_open(_con: Rconnection) -> Rboolean {
    from_native(|| Err::<Rboolean, _>(error("open not enabled for this connection")))
}

unsafe extern "C" fn null_close(con: Rconnection) {
    (*con).isopen = 0;
}

unsafe extern "C" fn null_destroy(_con: Rconnection) {}

unsafe extern "C" fn custom_fgetc(con: Rconnection) -> c_int {
    from_native(|| method_fgetc_internal(con))
}

unsafe extern "C" fn null_fgetc(_con: Rconnection) -> c_int {
    from_native(|| Err::<c_int, _>(missing("getc")))
}

unsafe extern "C" fn null_seek(_con: Rconnection, _position: f64, _origin: c_int, _rw: c_int) -> f64 {
    from_native(|| Err::<f64, _>(missing("seek")))
}

unsafe extern "C" fn null_truncate(_con: Rconnection) {
    from_native(|| Err::<(), _>(error("truncation not enabled for this connection")))
}

unsafe extern "C" fn null_fflush(_con: Rconnection) -> c_int {
    0
}

unsafe extern "C" fn null_read(
    _buffer: *mut c_void,
    _size: usize,
    _count: usize,
    _con: Rconnection,
) -> usize {
    from_native(|| Err::<usize, _>(missing("read")))
}

unsafe extern "C" fn null_write(
    _buffer: *const c_void,
    _size: usize,
    _count: usize,
    _con: Rconnection,
) -> usize {
    from_native(|| Err::<usize, _>(missing("write")))
}

// The built-in devices.

enum Device {
    Standard(Standard),
    File(FileDevice),
    Raw(RawDevice),
    TextInput(TextInput),
    TextOutput(TextOutput),
    Pipe(PipeDevice),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Standard {
    Input,
    Output,
    Error,
}

/// `file()` and `gzfile()`: a path, and the stream while open.
struct FileDevice {
    path: PathBuf,
    /// `file("")`: an anonymous file, removed when closed.
    temporary: bool,
    /// `gzfile()`, `bzfile()` and `xzfile()`: writes compress in this format; reads decompress
    /// whatever format the file is in.
    encoding: Option<Compressed>,
    /// `file(raw = TRUE)`: reads never decompress.
    raw: bool,
    compression: u32,
    stream: Option<Stream>,
}

enum Stream {
    Plain(PlainFile),
    Inflate(Inflate),
    Deflate { encoder: Encoder, position: u64 },
}

/// A compressing writer for each format the compressed file connections write.
enum Encoder {
    Gzip(GzEncoder<File>),
    Bzip2(BzEncoder<File>),
    Xz(XzWriter<File>),
}

impl Encoder {
    fn finish(self) -> std::io::Result<()> {
        match self {
            Encoder::Gzip(encoder) => encoder.finish().map(drop),
            Encoder::Bzip2(encoder) => encoder.finish().map(drop),
            Encoder::Xz(encoder) => encoder.finish().map(drop),
        }
    }
}

impl Write for Encoder {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Encoder::Gzip(encoder) => encoder.write(bytes),
            Encoder::Bzip2(encoder) => encoder.write(bytes),
            Encoder::Xz(encoder) => encoder.write(bytes),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Encoder::Gzip(encoder) => encoder.flush(),
            Encoder::Bzip2(encoder) => encoder.flush(),
            Encoder::Xz(encoder) => encoder.flush(),
        }
    }
}

/// A file read and written through separate positions, as GNU R's file connections are.
struct PlainFile {
    file: File,
    append: bool,
    read_position: u64,
    write_position: u64,
    /// Where the operating system's file offset is, so a seek happens only when needed.
    os_position: u64,
    last_was_write: bool,
    /// Bytes read ahead; `read_buffer[read_offset..]` start at `read_position`.
    read_buffer: Vec<u8>,
    read_offset: usize,
    /// Bytes written but not yet passed to the operating system; they end at `write_position`.
    write_buffer: Vec<u8>,
}

/// A file read through gzip decompression when it starts with gzip's magic bytes.
struct Inflate {
    reader: Box<dyn Read>,
    path: PathBuf,
    position: u64,
}

struct RawDevice {
    data: Vec<u8>,
    position: usize,
}

struct TextInput {
    data: Vec<u8>,
    position: usize,
}

struct TextOutput {
    lines: Vec<Vec<u8>>,
    partial: Vec<u8>,
    /// The variable each completed line updates, which [`Table::trace`] keeps alive.
    target: Option<TextTarget>,
}

struct PipeDevice {
    command: CString,
    stream: *mut libc::FILE,
}

impl Drop for PipeDevice {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: the stream came from popen and is closed once.
            unsafe { libc::pclose(self.stream) };
        }
    }
}

unsafe fn device<'a>(con: Rconnection) -> R<&'a mut Device> {
    let device = (*con).private as *mut Device;
    if device.is_null() {
        return Err(error("invalid connection"));
    }
    Ok(&mut *device)
}

/// The parts of an fopen-style mode string that matter.
#[derive(Clone, Copy, Debug)]
struct Mode {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    binary: bool,
}

fn parse_mode(mode: &[u8]) -> Option<Mode> {
    let plus = mode.contains(&b'+');
    let binary = mode.contains(&b'b');
    match mode.first()? {
        b'r' => Some(Mode { read: true, write: plus, append: false, truncate: false, binary }),
        b'w' => Some(Mode { read: plus, write: true, append: false, truncate: true, binary }),
        b'a' => Some(Mode { read: plus, write: true, append: true, truncate: false, binary }),
        _ => None,
    }
}

pub(crate) fn os_message(failure: &std::io::Error) -> String {
    match failure.raw_os_error() {
        // SAFETY: strerror returns a valid C string for any error number.
        Some(code) => unsafe {
            String::from_utf8_lossy(CStr::from_ptr(libc::strerror(code)).to_bytes()).into_owned()
        },
        None => failure.to_string(),
    }
}

fn io_error(failure: std::io::Error) -> Unwind {
    error(os_message(&failure))
}

/// Opens the device for the struct's current mode. A device that cannot open warns why and
/// returns false; the caller turns that into "cannot open the connection".
unsafe fn device_open(con: Rconnection) -> R<bool> {
    let mode_bytes = mode_of(con);
    let device = device(con)?;
    let Some(mode) = parse_mode(&mode_bytes) else {
        crate::conditions::warning(format!(
            "cannot open file '{}': Invalid argument",
            description_of(con)
        ))?;
        return Ok(false);
    };
    match device {
        Device::File(file) => {
            if file.stream.is_some() {
                return Ok(true);
            }
            match open_file(file, mode) {
                Ok(stream) => file.stream = Some(stream),
                Err(failure) => {
                    crate::conditions::warning(format!(
                        "cannot open file '{}': {}",
                        description_of(con),
                        os_message(&failure)
                    ))?;
                    return Ok(false);
                }
            }
            (*con).canseek = 1;
        }
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                let kind: &CStr = if mode.write { c"w" } else { c"r" };
                pipe.stream = libc::popen(pipe.command.as_ptr(), kind.as_ptr());
                if pipe.stream.is_null() {
                    let failure = std::io::Error::last_os_error();
                    crate::conditions::warning(format!(
                        "cannot open pipe() cmd '{}': {}",
                        pipe.command.to_string_lossy(),
                        os_message(&failure)
                    ))?;
                    return Ok(false);
                }
            }
            (*con).canseek = 0;
            (*con).isopen = 1;
            (*con).canread = (!mode.write) as Rboolean;
            (*con).canwrite = mode.write as Rboolean;
            (*con).text = (!mode.binary) as Rboolean;
            return Ok(true);
        }
        Device::Raw(_) => {
            (*con).isopen = 1;
            (*con).canread = mode.read as Rboolean;
            (*con).canwrite = mode.write as Rboolean;
            // A raw connection is always binary, whatever its mode says.
            (*con).text = 0;
            (*con).save = NO_SAVE;
            return Ok(true);
        }
        Device::Standard(_) | Device::TextInput(_) | Device::TextOutput(_) => {}
    }
    (*con).isopen = 1;
    (*con).canread = mode.read as Rboolean;
    (*con).canwrite = mode.write as Rboolean;
    (*con).text = (!mode.binary) as Rboolean;
    (*con).save = NO_SAVE;
    Ok(true)
}

fn open_file(device: &mut FileDevice, mode: Mode) -> std::io::Result<Stream> {
    if let Some(encoding) = device.encoding.filter(|_| mode.write) {
        if mode.read {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(!mode.append)
            .append(mode.append)
            .open(&device.path)?;
        let level = device.compression;
        let encoder = match encoding {
            Compressed::Gzip => Encoder::Gzip(GzEncoder::new(file, Compression::new(level))),
            Compressed::Bzip2 => {
                Encoder::Bzip2(BzEncoder::new(file, bzip2::Compression::new(level.clamp(1, 9))))
            }
            Compressed::Xz => Encoder::Xz(XzWriter::new(file, XzOptions::with_preset(level))?),
        };
        return Ok(Stream::Deflate { encoder, position: 0 });
    }
    // `file()` decompresses only in text mode, so `readBin()` sees a compressed file's bytes;
    // the compressed file connections decompress in either mode.
    let decodes = device.encoding.is_some() || !(mode.binary || device.raw);
    if mode.read && !mode.write && decodes && file_compression(&device.path)?.is_some() {
        return Ok(Stream::Inflate(open_inflate(&device.path)?));
    }
    let file = OpenOptions::new()
        .read(mode.read)
        .write(mode.write && !mode.append)
        .append(mode.append)
        .create(mode.write)
        .truncate(mode.truncate)
        .open(&device.path)?;
    let mut plain = PlainFile {
        file,
        append: mode.append,
        read_position: 0,
        write_position: 0,
        os_position: 0,
        last_was_write: false,
        read_buffer: Vec::new(),
        read_offset: 0,
        write_buffer: Vec::new(),
    };
    if mode.append {
        let end = plain.file.seek(SeekFrom::End(0))?;
        plain.os_position = end;
        plain.write_position = end;
    }
    Ok(Stream::Plain(plain))
}

/// The compressions a file opened for reading is decoded from, recognized by their magic bytes.
/// GNU R's `gzfile()` reads all three, because `save()` and `usethis::use_data()` write bzip2
/// and xz `.rda` files through it.
#[derive(Clone, Copy)]
enum Compressed {
    Gzip,
    Bzip2,
    Xz,
}

fn compression_of(magic: &[u8]) -> Option<Compressed> {
    if magic.starts_with(&[0x1f, 0x8b]) {
        Some(Compressed::Gzip)
    } else if magic.starts_with(b"BZh") {
        Some(Compressed::Bzip2)
    } else if magic.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Compressed::Xz)
    } else {
        None
    }
}

fn file_compression(path: &PathBuf) -> std::io::Result<Option<Compressed>> {
    let mut magic = [0u8; 6];
    let mut file = File::open(path)?;
    let mut filled = 0;
    while filled < magic.len() {
        let count = file.read(&mut magic[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    Ok(compression_of(magic.get(..filled).unwrap_or_default()))
}

/// `bytes` decoded when they start with gzip's, bzip2's or xz's magic bytes, else unchanged.
pub fn decompress(bytes: &[u8]) -> std::io::Result<Cow<'_, [u8]>> {
    let Some(compression) = compression_of(bytes) else {
        return Ok(Cow::Borrowed(bytes));
    };
    let mut decoded = Vec::new();
    decoder(compression, Box::new(bytes))?.read_to_end(&mut decoded)?;
    Ok(Cow::Owned(decoded))
}

fn decoder<'a>(compression: Compressed, input: Box<dyn Read + 'a>) -> std::io::Result<Box<dyn Read + 'a>> {
    Ok(match compression {
        Compressed::Gzip => Box::new(MultiGzDecoder::new(input)),
        Compressed::Bzip2 => Box::new(MultiBzDecoder::new(input)),
        Compressed::Xz => Box::new(XzReader::new(input, true)),
    })
}

/// A decompressing reader that reads an uncompressed file as it is, as GNU R's gzfile does.
fn open_inflate(path: &PathBuf) -> std::io::Result<Inflate> {
    let file = BufReader::new(File::open(path)?);
    let reader = match file_compression(path)? {
        Some(compression) => decoder(compression, Box::new(file))?,
        None => Box::new(file),
    };
    Ok(Inflate { reader, path: path.clone(), position: 0 })
}

unsafe fn device_close(con: Rconnection) -> R<()> {
    (*con).isopen = 0;
    match device(con)? {
        Device::File(file) => {
            let stream = file.stream.take();
            let finished = match stream {
                // GNU R reports a plain file's close status, but not a compressed stream's.
                Some(Stream::Plain(mut plain)) => {
                    (*con).status = 0;
                    flush_writes(&mut plain)
                }
                Some(Stream::Deflate { encoder, .. }) => encoder.finish(),
                Some(Stream::Inflate(_)) | None => Ok(()),
            };
            if file.temporary {
                // The file may already be gone, and an anonymous file has no one to tell.
                drop(std::fs::remove_file(&file.path));
            }
            finished.map_err(io_error)
        }
        Device::Pipe(pipe) => {
            if !pipe.stream.is_null() {
                let status = libc::pclose(pipe.stream);
                pipe.stream = null_mut();
                (*con).status = if status == -1 { NA_INTEGER } else { status };
            }
            Ok(())
        }
        Device::TextOutput(output) => {
            if !output.partial.is_empty() {
                let line = std::mem::take(&mut output.partial);
                output.lines.push(line);
                update_text_target(output)?;
            }
            Ok(())
        }
        Device::Standard(_) | Device::Raw(_) | Device::TextInput(_) => Ok(()),
    }
}

unsafe fn device_fgetc(con: Rconnection) -> R<c_int> {
    let mut byte = [0u8];
    Ok(if device_read(con, &mut byte)? == 1 { byte[0] as c_int } else { EOF })
}

/// Reads up to `buffer.len()` bytes, fewer only at the end of the data.
unsafe fn device_read(con: Rconnection, buffer: &mut [u8]) -> R<usize> {
    match device(con)? {
        Device::Standard(Standard::Input) => {
            read_fully(&mut std::io::stdin().lock(), buffer).map_err(io_error)
        }
        Device::Standard(_) => Err(error("cannot read from this connection")),
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => plain_read(plain, buffer).map_err(io_error),
            Some(Stream::Inflate(inflate)) => {
                let count = read_fully(&mut inflate.reader, buffer).map_err(io_error)?;
                inflate.position += count as u64;
                Ok(count)
            }
            Some(Stream::Deflate { .. }) => Err(error("cannot read from this connection")),
            None => Err(error("connection is not open")),
        },
        Device::Raw(RawDevice { data, position }) | Device::TextInput(TextInput { data, position }) => {
            let available = data.get(*position..).unwrap_or_default();
            let count = available.len().min(buffer.len());
            buffer[..count].copy_from_slice(&available[..count]);
            *position += count;
            Ok(count)
        }
        Device::TextOutput(_) => Err(error("cannot read from this connection")),
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                return Err(error("connection is not open"));
            }
            Ok(libc::fread(buffer.as_mut_ptr().cast(), 1, buffer.len(), pipe.stream))
        }
    }
}

fn read_fully(reader: &mut dyn Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(failure) if failure.kind() == std::io::ErrorKind::Interrupted => {}
            Err(failure) => return Err(failure),
        }
    }
    Ok(filled)
}

fn plain_read(plain: &mut PlainFile, buffer: &mut [u8]) -> std::io::Result<usize> {
    if plain.last_was_write {
        flush_writes(plain)?;
        plain.last_was_write = false;
    }
    let mut done = 0;
    while done < buffer.len() {
        if plain.read_offset == plain.read_buffer.len() {
            plain.read_buffer.clear();
            plain.read_offset = 0;
            seek_os(plain, plain.read_position)?;
            if buffer.len() - done >= READ_CHUNK {
                let count = read_fully(&mut plain.file, &mut buffer[done..])?;
                plain.os_position += count as u64;
                plain.read_position += count as u64;
                done += count;
                break;
            }
            plain.read_buffer.resize(READ_CHUNK, 0);
            let count = read_fully(&mut plain.file, &mut plain.read_buffer)?;
            plain.read_buffer.truncate(count);
            plain.os_position += count as u64;
            if count == 0 {
                break;
            }
        }
        let available = &plain.read_buffer[plain.read_offset..];
        let count = available.len().min(buffer.len() - done);
        buffer[done..done + count].copy_from_slice(&available[..count]);
        plain.read_offset += count;
        plain.read_position += count as u64;
        done += count;
    }
    Ok(done)
}

fn seek_os(plain: &mut PlainFile, position: u64) -> std::io::Result<()> {
    if plain.os_position != position {
        plain.file.seek(SeekFrom::Start(position))?;
        plain.os_position = position;
    }
    Ok(())
}

fn flush_writes(plain: &mut PlainFile) -> std::io::Result<()> {
    if plain.write_buffer.is_empty() {
        return Ok(());
    }
    let pending = std::mem::take(&mut plain.write_buffer);
    if plain.append {
        plain.file.write_all(&pending)?;
        plain.os_position = plain.file.stream_position()?;
        plain.write_position = plain.os_position;
    } else {
        seek_os(plain, plain.write_position - pending.len() as u64)?;
        plain.file.write_all(&pending)?;
        plain.os_position = plain.write_position;
    }
    Ok(())
}

/// Writes bytes, or printed text when `printing`, which is the only way into a text output
/// connection, as in GNU R. Standard output goes where `sink` diverts it, as GNU R's does.
unsafe fn device_write(con: Rconnection, bytes: &[u8], printing: bool) -> R<usize> {
    match device(con)? {
        Device::Standard(Standard::Input) => Err(error("cannot write to this connection")),
        Device::Standard(Standard::Output) => {
            crate::write_console(bytes, false);
            Ok(bytes.len())
        }
        Device::Standard(Standard::Error) => {
            write_terminal(bytes, true);
            Ok(bytes.len())
        }
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => {
                if !plain.last_was_write {
                    plain.read_buffer.clear();
                    plain.read_offset = 0;
                    plain.last_was_write = true;
                }
                plain.write_buffer.extend_from_slice(bytes);
                plain.write_position += bytes.len() as u64;
                if plain.write_buffer.len() >= READ_CHUNK {
                    flush_writes(plain).map_err(io_error)?;
                }
                Ok(bytes.len())
            }
            Some(Stream::Deflate { encoder, position }) => {
                encoder.write_all(bytes).map_err(io_error)?;
                *position += bytes.len() as u64;
                Ok(bytes.len())
            }
            Some(Stream::Inflate(_)) => Err(error("cannot write to this connection")),
            None => Err(error("connection is not open")),
        },
        Device::Raw(raw) => {
            let end = raw.position + bytes.len();
            if end > raw.data.len() {
                raw.data.resize(end, 0);
            }
            raw.data[raw.position..end].copy_from_slice(bytes);
            raw.position = end;
            Ok(bytes.len())
        }
        Device::TextInput(_) => Err(error("cannot write to this connection")),
        Device::TextOutput(_) if !printing => Err(missing("write")),
        Device::TextOutput(output) => {
            let mut completed = false;
            for &byte in bytes {
                if byte == b'\n' {
                    let line = std::mem::take(&mut output.partial);
                    output.lines.push(line);
                    completed = true;
                } else {
                    output.partial.push(byte);
                }
            }
            if completed {
                update_text_target(output)?;
            }
            Ok(bytes.len())
        }
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                return Err(error("connection is not open"));
            }
            Ok(libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), pipe.stream))
        }
    }
}

unsafe fn update_text_target(output: &TextOutput) -> R<()> {
    let Some(target) = output.target else {
        return Ok(());
    };
    let value = strings_vector(&output.lines, Encoding::Native)?;
    crate::eval::define(target.symbol, value, target.env)
}

unsafe fn device_seek(con: Rconnection, position: f64, origin: SeekOrigin, side: SeekSide) -> R<f64> {
    let query = position.is_nan();
    match device(con)? {
        Device::Standard(_) | Device::Pipe(_) => Err(missing("seek")),
        Device::TextInput(_) | Device::TextOutput(_) => {
            if !query && position >= 0.0 {
                return Err(error("seek is not relevant for text connection"));
            }
            Ok(0.0)
        }
        Device::Raw(raw) => {
            let current = raw.position as f64;
            if query {
                return Ok(current);
            }
            let target = match origin {
                SeekOrigin::Start => position,
                SeekOrigin::Current => current + position,
                SeekOrigin::End => raw.data.len() as f64 + position,
            };
            if target < 0.0 || target > raw.data.len() as f64 {
                return Err(error("attempt to seek outside the range of the raw connection"));
            }
            raw.position = target as usize;
            Ok(current)
        }
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => {
                flush_writes(plain).map_err(io_error)?;
                let use_write = match side {
                    SeekSide::Read => false,
                    SeekSide::Write => true,
                    SeekSide::LastUsed => plain.last_was_write,
                };
                let current = if use_write { plain.write_position } else { plain.read_position };
                if query {
                    return Ok(current as f64);
                }
                let base = match origin {
                    SeekOrigin::Start => 0.0,
                    SeekOrigin::Current => current as f64,
                    SeekOrigin::End => plain.file.metadata().map_err(io_error)?.len() as f64,
                };
                let target = base + position;
                if target < 0.0 {
                    return Err(error("seek to a negative position"));
                }
                if use_write {
                    plain.write_position = target as u64;
                } else {
                    plain.read_position = target as u64;
                    plain.read_buffer.clear();
                    plain.read_offset = 0;
                }
                Ok(current as f64)
            }
            Some(Stream::Inflate(inflate)) => {
                let current = inflate.position as f64;
                if query {
                    return Ok(current);
                }
                let target = match origin {
                    SeekOrigin::Start => position,
                    SeekOrigin::Current => current + position,
                    SeekOrigin::End => {
                        return Err(error(
                            "whence = \"end\" is not implemented for gzfile connections",
                        ));
                    }
                };
                if target < 0.0 {
                    return Err(error("seek to a negative position"));
                }
                let target = target as u64;
                if target < inflate.position {
                    *inflate = open_inflate(&inflate.path).map_err(io_error)?;
                }
                let skip = target - inflate.position;
                let skipped =
                    std::io::copy(&mut (&mut inflate.reader).take(skip), &mut std::io::sink())
                        .map_err(io_error)?;
                inflate.position += skipped;
                Ok(current)
            }
            Some(Stream::Deflate { position: current, .. }) => {
                if query {
                    return Ok(*current as f64);
                }
                Err(error("seek on a gzfile connection open for writing is not supported"))
            }
            None => Err(error("connection is not open")),
        },
    }
}

unsafe fn device_truncate(con: Rconnection) -> R<()> {
    if (*con).isopen == 0 || (*con).canwrite == 0 {
        return Err(error("can only truncate connections open for writing"));
    }
    match device(con)? {
        Device::File(FileDevice { stream: Some(Stream::Plain(plain)), .. }) => {
            flush_writes(plain).map_err(io_error)?;
            let position =
                if plain.last_was_write { plain.write_position } else { plain.read_position };
            plain.file.set_len(position).map_err(io_error)?;
            plain.read_buffer.clear();
            plain.read_offset = 0;
            Ok(())
        }
        Device::Raw(raw) => {
            raw.data.truncate(raw.position);
            Ok(())
        }
        _ => Err(error("truncation not enabled for this connection")),
    }
}

unsafe fn device_flush(con: Rconnection) -> R<()> {
    match device(con)? {
        Device::File(FileDevice { stream: Some(Stream::Plain(plain)), .. }) => {
            flush_writes(plain).map_err(io_error)?;
            plain.file.flush().map_err(io_error)
        }
        Device::File(FileDevice { stream: Some(Stream::Deflate { encoder, .. }), .. }) => {
            encoder.flush().map_err(io_error)
        }
        Device::Pipe(pipe) if !pipe.stream.is_null() => {
            libc::fflush(pipe.stream);
            Ok(())
        }
        Device::Standard(Standard::Output) => {
            // Console output that cannot be flushed has nowhere else to go, as in GNU R.
            drop(std::io::stdout().flush());
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The Rust mirror of `struct Rconn` must have GNU R's layout, which the ryconn test
    /// package's `layout.expected` records from GNU R's own header.
    #[test]
    fn the_struct_matches_gnu_r_layout() {
        let expected = include_str!("../../libr/tests/packages/ryconn/layout.expected");
        let actual: Vec<(&str, usize)> = vec![
            ("sizeof", size_of::<Rconn>()),
            ("class", offset_of!(Rconn, class)),
            ("description", offset_of!(Rconn, description)),
            ("enc", offset_of!(Rconn, enc)),
            ("mode", offset_of!(Rconn, mode)),
            ("text", offset_of!(Rconn, text)),
            ("isopen", offset_of!(Rconn, isopen)),
            ("incomplete", offset_of!(Rconn, incomplete)),
            ("canread", offset_of!(Rconn, canread)),
            ("canwrite", offset_of!(Rconn, canwrite)),
            ("canseek", offset_of!(Rconn, canseek)),
            ("blocking", offset_of!(Rconn, blocking)),
            ("isGzcon", offset_of!(Rconn, isGzcon)),
            ("open", offset_of!(Rconn, open)),
            ("close", offset_of!(Rconn, close)),
            ("destroy", offset_of!(Rconn, destroy)),
            ("vfprintf", offset_of!(Rconn, vfprintf)),
            ("fgetc", offset_of!(Rconn, fgetc)),
            ("fgetc_internal", offset_of!(Rconn, fgetc_internal)),
            ("seek", offset_of!(Rconn, seek)),
            ("truncate", offset_of!(Rconn, truncate)),
            ("fflush", offset_of!(Rconn, fflush)),
            ("read", offset_of!(Rconn, read)),
            ("write", offset_of!(Rconn, write)),
            ("nPushBack", offset_of!(Rconn, nPushBack)),
            ("posPushBack", offset_of!(Rconn, posPushBack)),
            ("PushBack", offset_of!(Rconn, PushBack)),
            ("save", offset_of!(Rconn, save)),
            ("save2", offset_of!(Rconn, save2)),
            ("encname", offset_of!(Rconn, encname)),
            ("inconv", offset_of!(Rconn, inconv)),
            ("outconv", offset_of!(Rconn, outconv)),
            ("iconvbuff", offset_of!(Rconn, iconvbuff)),
            ("oconvbuff", offset_of!(Rconn, oconvbuff)),
            ("next", offset_of!(Rconn, next)),
            ("init_out", offset_of!(Rconn, init_out)),
            ("navail", offset_of!(Rconn, navail)),
            ("inavail", offset_of!(Rconn, inavail)),
            ("EOF_signalled", offset_of!(Rconn, EOF_signalled)),
            ("UTF8out", offset_of!(Rconn, UTF8out)),
            ("id", offset_of!(Rconn, id)),
            ("ex_ptr", offset_of!(Rconn, ex_ptr)),
            ("private", offset_of!(Rconn, private)),
            ("status", offset_of!(Rconn, status)),
            ("buff", offset_of!(Rconn, buff)),
            ("buff_len", offset_of!(Rconn, buff_len)),
            ("buff_stored_len", offset_of!(Rconn, buff_stored_len)),
            ("buff_pos", offset_of!(Rconn, buff_pos)),
        ];
        for (name, offset) in actual {
            let line = expected
                .lines()
                .find(|line| line.split(' ').next() == Some(name))
                .unwrap_or_else(|| panic!("{name} is not in the expected layout"));
            let recorded: usize = line
                .split(' ')
                .nth(1)
                .and_then(|text| text.parse().ok())
                .unwrap_or_else(|| panic!("a malformed layout line: {line}"));
            assert_eq!(offset, recorded, "{name}");
        }
    }
}
