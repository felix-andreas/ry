//! Connections: files (plain and gzip-compressed), text connections and the standard streams.
//! A connection is an integer with class `connection`, as in GNU R, indexing the session's table.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    interp,
    unwind::{R, error},
    values::{self, alloc, length, nil, scalar_bool, scalar_integer, string_vector, symbols},
    write_console,
};
use flate2::read::MultiGzDecoder;
use heap::object::{Sexp, SexpType};
use std::io::{Read, Write};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("file", 6, |call| unsafe {
        open_connection(call, Class::File)
    }),
    internal("gzfile", 4, |call| unsafe {
        open_connection(call, Class::Gzfile)
    }),
    internal("bzfile", 4, |call| unsafe {
        open_connection(call, Class::Gzfile)
    }),
    internal("xzfile", 4, |call| unsafe {
        open_connection(call, Class::Gzfile)
    }),
    internal("url", 6, |call| unsafe {
        Err(call.error("url connections are not supported"))
    }),
    internal("textConnection", 5, do_text_connection),
    internal("textConnectionValue", 1, do_text_connection_value),
    internal("open", 3, do_open).invisible(),
    internal("close", 2, do_close).invisible(),
    internal("isOpen", 2, do_is_open),
    internal("stdin", 0, |_| unsafe { connection_object(0) }),
    internal("stdout", 0, |_| unsafe { connection_object(1) }),
    internal("stderr", 0, |_| unsafe { connection_object(2) }),
    internal("readLines", 6, do_read_lines),
    internal("readDCF", 3, do_read_dcf),
    internal("writeLines", 4, do_write_lines).invisible(),
    // R 4.3 passes `con, nchars, useBytes`; later versions add `allowNUL`.
    internal("readChar", -1, do_read_char),
    internal("writeChar", 5, |call| unsafe {
        Err(call.error("writeChar is not supported"))
    }),
    internal("readBin", 6, do_read_bin),
    internal("writeBin", 5, |call| unsafe {
        Err(call.error("writeBin is not supported"))
    }),
    internal("cat", 6, do_cat).invisible(),
    internal("summary.connection", 1, do_summary_connection),
    internal("getAllConnections", 0, do_get_all_connections),
    internal("getConnection", 1, |call| unsafe {
        connection_object(values::as_integer_scalar(call.arg(0)))
    }),
    internal("flush", 1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("isIncomplete", 1, |_| unsafe { scalar_bool(false) }),
    internal("isSeekable", 1, |_| unsafe { scalar_bool(false) }),
    internal("pushBack", 4, do_push_back).invisible(),
    internal("pushBackLength", 1, do_push_back_length),
    internal("clearPushBack", 1, do_clear_push_back).invisible(),
    internal("sink", 4, do_sink).invisible(),
    internal("sink.number", 1, do_sink_number),
    internal("readline", 1, |_| unsafe { values::scalar_string("") }),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    File,
    Gzfile,
    Text,
    Terminal,
}

pub struct Connection {
    class: Class,
    description: String,
    mode: String,
    open: bool,
    /// Unread input, for reading connections.
    input: Vec<u8>,
    position: usize,
    /// How many bytes at `position` were pushed back, which `pushBackLength` counts in lines.
    pushed: usize,
    /// Pending output, written to the file on close.
    output: Vec<u8>,
    /// For an output text connection: the variable it appends to, and where.
    text_target: Option<(Sexp, Sexp)>,
    text_lines: Vec<String>,
    partial_line: String,
}

#[derive(Default)]
pub struct Table {
    connections: Vec<Option<Connection>>,
    /// The output diversions `sink` set up, innermost last.
    sinks: Vec<Sink>,
}

struct Sink {
    connection: i32,
    close_on_exit: bool,
    /// Whether output also still goes to the console.
    split: bool,
}

impl Table {
    fn ensure_standard(&mut self) {
        if self.connections.is_empty() {
            for (name, mode) in [("stdin", "r"), ("stdout", "w"), ("stderr", "w")] {
                self.connections.push(Some(Connection {
                    class: Class::Terminal,
                    description: name.into(),
                    mode: mode.into(),
                    open: true,
                    input: Vec::new(),
                    position: 0,
                    pushed: 0,
                    output: Vec::new(),
                    text_target: None,
                    text_lines: Vec::new(),
                    partial_line: String::new(),
                }));
            }
        }
    }

    fn add(&mut self, connection: Connection) -> i32 {
        self.ensure_standard();
        if let Some(index) = self.connections.iter().skip(3).position(Option::is_none) {
            self.connections[index + 3] = Some(connection);
            return index as i32 + 3;
        }
        self.connections.push(Some(connection));
        self.connections.len() as i32 - 1
    }

    fn get(&mut self, index: i32) -> R<&mut Connection> {
        self.ensure_standard();
        self.connections
            .get_mut(index.max(0) as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| error("invalid connection"))
    }

    pub(crate) unsafe fn trace(&self, marker: &mut heap::Marker<'_>) {
        for connection in self.connections.iter().flatten() {
            if let Some((symbol, env)) = connection.text_target {
                marker.mark(symbol);
                marker.mark(env);
            }
        }
    }
}

unsafe fn connection_object(index: i32) -> R<Sexp> {
    let object = values::scalar_integer(index)?;
    let class_name = {
        let table = &mut interp().connections;
        table.ensure_standard();
        match table
            .connections
            .get(index.max(0) as usize)
            .and_then(Option::as_ref)
        {
            Some(connection) => match connection.class {
                Class::File => "file",
                Class::Gzfile => "gzfile",
                Class::Text => "textConnection",
                Class::Terminal => "terminal",
            },
            None => "connection",
        }
    };
    let class = string_vector(&[class_name, "connection"])?;
    values::set_attr(object, symbols().class, class)?;
    let pointer = values::install("conn_id")?;
    let id = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
    crate::heap().root(id);
    values::set_attr(object, pointer, id)?;
    Ok(object)
}

use crate::heap;

/// The connection number of a connection object.
pub unsafe fn connection_index(object: Sexp) -> R<i32> {
    if !matches!(object.kind(), SexpType::Integer | SexpType::Real) || length(object) != 1 {
        return Err(error("'con' is not a connection"));
    }
    Ok(values::as_integer_scalar(object))
}

fn read_file_bytes(path: &str, gzip: bool) -> R<Vec<u8>> {
    let path = expand_path(path);
    let bytes = std::fs::read(&path).map_err(|failure| {
        error(format!(
            "cannot open file '{path}': {}",
            io_reason(&failure)
        ))
    })?;
    if !gzip {
        return Ok(bytes);
    }
    // `gzfile()` reads all three of R's compressions, as GNU R's does, because `save()` and
    // `usethis::use_data()` write bzip2 or xz `.rda` files through it.
    let mut decoded = Vec::new();
    let result = if bytes.starts_with(&[0x1f, 0x8b]) {
        MultiGzDecoder::new(&bytes[..])
            .read_to_end(&mut decoded)
            .map(drop)
    } else if bytes.starts_with(b"BZh") {
        bzip2_rs::DecoderReader::new(&bytes[..])
            .read_to_end(&mut decoded)
            .map(drop)
    } else if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        lzma_rs::xz_decompress(&mut &bytes[..], &mut decoded)
            .map_err(|failure| std::io::Error::other(failure.to_string()))
    } else {
        return Ok(bytes);
    };
    result.map_err(|failure| error(format!("cannot read compressed file '{path}': {failure}")))?;
    Ok(decoded)
}

fn io_reason(failure: &std::io::Error) -> &'static str {
    match failure.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory",
        std::io::ErrorKind::PermissionDenied => "Permission denied",
        _ => "I/O error",
    }
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

unsafe fn open_connection(call: &Call<'_>, class: Class) -> R<Sexp> {
    let description = values::require_string(call.arg(0), "description")?;
    let mode = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let mut connection = Connection {
        class,
        description: description.clone(),
        mode: mode.clone(),
        open: false,
        input: Vec::new(),
        position: 0,
        pushed: 0,
        output: Vec::new(),
        text_target: None,
        text_lines: Vec::new(),
        partial_line: String::new(),
    };
    if description.is_empty() && class == Class::File {
        connection.mode = "w+".into();
        connection.open = true;
    } else if !mode.is_empty() {
        open(&mut connection)?;
    }
    let index = interp().connections.add(connection);
    connection_object(index)
}

fn open(connection: &mut Connection) -> R<()> {
    let mode = if connection.mode.is_empty() {
        "r".to_owned()
    } else {
        connection.mode.clone()
    };
    connection.mode = mode.clone();
    if mode.starts_with('r') {
        connection.input = read_file_bytes(&connection.description, true)?;
        connection.position = 0;
        connection.pushed = 0;
    } else if mode.starts_with('a') {
        connection.output = std::fs::read(expand_path(&connection.description)).unwrap_or_default();
    } else {
        connection.output.clear();
    }
    connection.open = true;
    Ok(())
}

fn flush_to_file(connection: &mut Connection) -> R<()> {
    if connection.mode.starts_with('w') || connection.mode.starts_with('a') {
        let path = expand_path(&connection.description);
        if connection.class == Class::Gzfile {
            let file = std::fs::File::create(&path).map_err(|failure| {
                error(format!(
                    "cannot open file '{path}': {}",
                    io_reason(&failure)
                ))
            })?;
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::new(6));
            encoder
                .write_all(&connection.output)
                .and_then(|()| encoder.finish().map(drop))
                .map_err(|failure| error(format!("cannot write '{path}': {failure}")))?;
        } else {
            std::fs::write(&path, &connection.output).map_err(|failure| {
                error(format!("cannot write '{path}': {}", io_reason(&failure)))
            })?;
        }
    }
    Ok(())
}

unsafe fn do_open(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let mode = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let connection = interp().connections.get(index)?;
    if !mode.is_empty() {
        connection.mode = mode;
    }
    open(connection)?;
    Ok(nil())
}

unsafe fn do_close(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    if index < 3 {
        return Err(call.error("cannot close standard connections"));
    }
    let table = &mut interp().connections;
    let connection = table.get(index)?;
    if connection.class == Class::Text {
        finish_text_output(connection)?;
    } else if connection.open {
        flush_to_file(connection)?;
    }
    table.connections[index as usize] = None;
    Ok(nil())
}

/// `sink(con, closeOnExit, type, split)`: diverts output to connection `con`, or ends the
/// innermost diversion for `con = -1`. Diverting messages is accepted and ignored.
unsafe fn do_sink(call: &Call<'_>) -> R<Sexp> {
    let index = values::as_integer_scalar(call.arg(0));
    let close_on_exit = values::as_logical_scalar(call.arg(1)) == 1;
    let messages = values::as_logical_scalar(call.arg(2)) == 1;
    let split = values::as_logical_scalar(call.arg(3)) == 1;
    if messages {
        return Ok(nil());
    }
    let table = &mut interp().connections;
    if index < 0 {
        let Some(sink) = table.sinks.pop() else {
            crate::conditions::warning_call(call.call, "no sink to remove")?;
            return Ok(nil());
        };
        if sink.close_on_exit && sink.connection >= 3 {
            let connection = table.get(sink.connection)?;
            if connection.class == Class::Text {
                finish_text_output(connection)?;
            } else if connection.open {
                flush_to_file(connection)?;
            }
            table.connections[sink.connection as usize] = None;
        }
        return Ok(nil());
    }
    let connection = table.get(index)?;
    if index >= 3 && connection.class != Class::Text && !connection.open {
        if connection.mode.is_empty() || connection.mode.starts_with('r') {
            connection.mode = "w".into();
        }
        open(connection)?;
    }
    table.sinks.push(Sink {
        connection: index,
        close_on_exit,
        split,
    });
    Ok(nil())
}

unsafe fn do_sink_number(call: &Call<'_>) -> R<Sexp> {
    if values::as_logical_scalar(call.arg(0)) == 1 {
        scalar_integer(interp().connections.sinks.len() as i32)
    } else {
        scalar_integer(2)
    }
}

unsafe fn do_is_open(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let connection = interp().connections.get(index)?;
    scalar_bool(connection.open)
}

/// The whole content of `file` (a connection or a path) as text, for `parse`.
pub unsafe fn read_all_text(file: Sexp) -> R<String> {
    if file.kind() == SexpType::String {
        let path = values::require_string(file, "file")?;
        let bytes = read_file_bytes(&path, true)?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let index = connection_index(file)?;
    let connection = interp().connections.get(index)?;
    if !connection.open {
        open(connection)?;
    }
    let rest = connection.input[connection.position..].to_vec();
    connection.position = connection.input.len();
    Ok(String::from_utf8_lossy(&rest).into_owned())
}

/// The bytes still to read from a connection, consuming them.
pub unsafe fn read_all_bytes(file: Sexp) -> R<Vec<u8>> {
    let index = connection_index(file)?;
    let connection = interp().connections.get(index)?;
    if !connection.open {
        open(connection)?;
    }
    let rest = connection.input[connection.position..].to_vec();
    connection.position = connection.input.len();
    Ok(rest)
}

/// Runs `read` over the unread input of `file`, which advances the position it is given. A
/// closed connection is opened for the call and closed again after it, as GNU R does when
/// reading from one.
pub unsafe fn with_input<T>(file: Sexp, read: impl FnOnce(&[u8], &mut usize) -> T) -> R<T> {
    let index = connection_index(file)?;
    let connection = interp().connections.get(index)?;
    let was_open = connection.open;
    if !was_open {
        open(connection)?;
    }
    let mut position = connection.position;
    let result = read(&connection.input, &mut position);
    connection.pushed = connection
        .pushed
        .saturating_sub(position - connection.position);
    connection.position = position;
    if !was_open && connection.class != Class::Text {
        connection.open = false;
    }
    Ok(result)
}

/// `pushBack(data, connection, newLine, type)`: text read before the rest of the input.
unsafe fn do_push_back(call: &Call<'_>) -> R<Sexp> {
    let data = call.arg(0);
    let index = connection_index(call.arg(1))?;
    let new_line = values::as_logical_scalar(call.arg(2)) == 1;
    let mut text = Vec::new();
    for line in values::strings(data) {
        text.extend_from_slice(line.as_deref().unwrap_or("NA").as_bytes());
        if new_line {
            text.push(b'\n');
        }
    }
    let connection = interp().connections.get(index)?;
    if !connection.mode.starts_with('r') && connection.class != Class::Text {
        return Err(error("can only push back on open readable connections"));
    }
    let position = connection.position;
    connection.pushed += text.len();
    connection.input.splice(position..position, text);
    Ok(nil())
}

unsafe fn do_push_back_length(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let connection = interp().connections.get(index)?;
    let end = (connection.position + connection.pushed).min(connection.input.len());
    let pushed = &connection.input[connection.position..end];
    scalar_integer(pushed.iter().filter(|byte| **byte == b'\n').count() as i32)
}

unsafe fn do_clear_push_back(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let connection = interp().connections.get(index)?;
    let position = connection.position;
    let end = (position + connection.pushed).min(connection.input.len());
    connection.input.drain(position..end);
    connection.pushed = 0;
    Ok(nil())
}

/// Appends bytes to a writable connection.
pub unsafe fn write_bytes(file: Sexp, bytes: &[u8]) -> R<()> {
    write_to(connection_index(file)?, bytes)
}

/// Writes standard output, to the console or to where `sink` diverts it.
pub unsafe fn write_output(bytes: &[u8]) {
    let Some(sink) = interp().connections.sinks.last() else {
        crate::write_terminal(bytes, false);
        return;
    };
    let (connection, split) = (sink.connection, sink.split);
    if split || connection == 1 {
        crate::write_terminal(bytes, false);
    }
    if connection != 1
        && let Err(unwind) = write_to(connection, bytes)
    {
        // Output has no caller to report a failed diversion to, so the console shows it.
        let message = match unwind {
            crate::unwind::Unwind::Error(failure) => failure.message,
            _ => "the sink connection could not be written".to_owned(),
        };
        crate::write_terminal(
            format!("Error writing to the sink connection: {message}\n").as_bytes(),
            true,
        );
        crate::write_terminal(bytes, false);
    }
}

unsafe fn write_to(index: i32, bytes: &[u8]) -> R<()> {
    match index {
        1 => write_console(bytes, false),
        2 => write_console(bytes, true),
        _ => {
            let connection = interp().connections.get(index)?;
            if connection.class == Class::Text {
                text_output(connection, &String::from_utf8_lossy(bytes))?;
            } else {
                if !connection.open {
                    if connection.mode.is_empty() {
                        connection.mode = "w".into();
                    }
                    open(connection)?;
                    connection.output.extend_from_slice(bytes);
                    flush_to_file(connection)?;
                    connection.open = false;
                    return Ok(());
                }
                connection.output.extend_from_slice(bytes);
            }
        }
    }
    Ok(())
}

/// Writes text to a connection, or a file path, or the console for `""`.
pub unsafe fn write_text(file: Sexp, text: &str) -> R<()> {
    if file.kind() == SexpType::String {
        let path = values::require_string(file, "file")?;
        if path.is_empty() {
            write_console(text.as_bytes(), false);
            return Ok(());
        }
        std::fs::write(expand_path(&path), text).map_err(|failure| {
            error(format!(
                "cannot open file '{path}': {}",
                io_reason(&failure)
            ))
        })?;
        return Ok(());
    }
    write_bytes(file, text.as_bytes())
}

/// The description of a connection or the path itself.
pub unsafe fn description(file: Sexp) -> Option<String> {
    if file.kind() == SexpType::String {
        return values::first_string(file).map(|text| text.into_owned());
    }
    let index = connection_index(file).ok()?;
    interp()
        .connections
        .get(index)
        .ok()
        .map(|connection| connection.description.clone())
}

unsafe fn do_read_lines(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let limit = values::as_integer_scalar(call.arg(1));
    let connection = interp().connections.get(index)?;
    let was_open = connection.open;
    if !connection.open {
        open(connection)?;
    }
    let mut lines = Vec::new();
    while limit < 0 || (lines.len() as i32) < limit {
        if connection.position >= connection.input.len() {
            break;
        }
        let rest = &connection.input[connection.position..];
        let (line, consumed) = match rest.iter().position(|&byte| byte == b'\n') {
            Some(end) => (&rest[..end], end + 1),
            None => (rest, rest.len()),
        };
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        lines.push(String::from_utf8_lossy(line).into_owned());
        connection.position += consumed;
    }
    if !was_open && connection.class != Class::Text {
        connection.open = false;
    }
    string_vector(&lines)
}

unsafe fn do_write_lines(call: &Call<'_>) -> R<Sexp> {
    let text = call.arg(0);
    let file = call.arg(1);
    let separator = values::first_string(call.arg(2))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "\n".into());
    let mut output = String::new();
    for index in 0..length(text) {
        output.push_str(
            &values::string_at(text, index)
                .map_or_else(|| "NA".to_owned(), |line| line.into_owned()),
        );
        output.push_str(&separator);
    }
    write_bytes(file, output.as_bytes())?;
    Ok(nil())
}

unsafe fn do_read_char(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    let counts = call.arg(1);
    let mut wanted = Vec::with_capacity(length(counts));
    for index in 0..length(counts) {
        let count = values::as_integer_scalar(crate::subset::vector_element(counts, index)?);
        wanted.push(count.max(0) as usize);
    }
    let take = |bytes: &[u8], offset: &mut usize| {
        let mut items = Vec::with_capacity(wanted.len());
        for &count in &wanted {
            let end = (*offset + count).min(bytes.len());
            items.push(String::from_utf8_lossy(&bytes[*offset..end]).into_owned());
            *offset = end;
        }
        items
    };
    // A connection gives up only what was read, so `load()` can read the magic number and
    // hand the connection on.
    let items = if source.kind() == SexpType::String {
        take(
            &read_file_bytes(&values::require_string(source, "con")?, false)?,
            &mut 0,
        )
    } else {
        with_input(source, take)?
    };
    string_vector(&items)
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
    let dimnames = alloc(SexpType::List, 2)?;
    if !columns.is_empty() {
        values::set_elt(dimnames, 1, string_vector(&columns)?);
    }
    values::set_attr(matrix, symbols().dimnames, dimnames)?;
    Ok(matrix)
}

unsafe fn do_read_bin(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    let what = values::require_string(call.arg(1), "what")?;
    let count = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let bytes = if source.kind() == SexpType::Raw {
        values::raws(source).to_vec()
    } else {
        read_all_bytes(source)?
    };
    match what.as_str() {
        "raw" => {
            let taken = &bytes[..count.min(bytes.len())];
            let result = alloc(SexpType::Raw, taken.len())?;
            values::raws_mut(result).copy_from_slice(taken);
            Ok(result)
        }
        _ => Err(call.error(format!("readBin of '{what}' is not supported"))),
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
    let separator_count = length(separators);
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
        length(labels)
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
    let mut piece_index = 0usize;
    for item_index in 0..length(items) {
        let item = items.element(item_index);
        let texts: Vec<String> = match item.kind() {
            SexpType::Nil => continue,
            SexpType::Symbol => vec![values::symbol_name(item).into_owned()],
            SexpType::String => (0..length(item))
                .map(|index| {
                    values::string_at(item, index)
                        .map_or_else(|| "NA".into(), |text| text.into_owned())
                })
                .collect(),
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Raw => {
                let item = values::materialize(item)?;
                (0..length(item))
                    .map(|index| cat_element(item, index))
                    .collect()
            }
            SexpType::List
                if (0..length(item)).all(|index| {
                    let element = item.element(index);
                    element.kind().is_atomic() && length(element) == 1
                }) =>
            {
                (0..length(item))
                    .map(|index| cat_element(item.element(index), 0))
                    .collect()
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
        for text in texts {
            // The separator goes before the line break, and a piece only fits on the line if
            // the separator after it does too.
            if piece_index > 0 {
                let separator = separator_at(piece_index - 1);
                output.push_str(&separator);
                line_width += separator.len();
                if let Some(position) = separator.rfind('\n') {
                    line_width = separator.len() - position - 1;
                }
                let next_separator = separator_at(piece_index).len();
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
            piece_index += 1;
        }
    }
    // A separator holding a newline makes the separators terminators, so the output always
    // ends with one, even when there was nothing to print.
    let newline_separator = (0..separator_count).any(|index| separator_at(index).contains('\n'));
    if newline_separator || (fill_width > 0 && !output.ends_with('\n') && !output.is_empty()) {
        output.push('\n');
    }
    write_bytes(file, output.as_bytes())?;
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
        SexpType::Complex => {
            crate::format::format_complex_alone(values::complexes(vector)[index], 7)
        }
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

// Text connections.

unsafe fn do_text_connection(call: &Call<'_>) -> R<Sexp> {
    let name = values::require_string(call.arg(0), "nm")?;
    let object = call.arg(1);
    let mode = values::require_string(call.arg(2), "open")?;
    let env = call.arg(3);
    let mut connection = Connection {
        class: Class::Text,
        description: name.clone(),
        mode: mode.clone(),
        open: true,
        input: Vec::new(),
        position: 0,
        pushed: 0,
        output: Vec::new(),
        text_target: None,
        text_lines: Vec::new(),
        partial_line: String::new(),
    };
    if mode.starts_with('r') || mode.is_empty() {
        let mut text = String::new();
        for index in 0..length(object) {
            text.push_str(
                &values::string_at(object, index)
                    .map_or_else(|| "NA".into(), |line| line.into_owned()),
            );
            text.push('\n');
        }
        connection.input = text.into_bytes();
    } else if object.kind() == SexpType::String && length(object) == 1 {
        let symbol = values::install(&values::require_string(object, "object")?)?;
        if mode.starts_with('w') {
            let empty = alloc(SexpType::String, 0)?;
            crate::eval::define(symbol, empty, env)?;
        }
        connection.text_target = Some((symbol, env));
    }
    let index = interp().connections.add(connection);
    connection_object(index)
}

unsafe fn text_output(connection: &mut Connection, text: &str) -> R<()> {
    connection.partial_line.push_str(text);
    while let Some(position) = connection.partial_line.find('\n') {
        let line = connection.partial_line[..position].to_owned();
        connection.partial_line.drain(..=position);
        connection.text_lines.push(line);
    }
    publish_text(connection)
}

unsafe fn publish_text(connection: &mut Connection) -> R<()> {
    if let Some((symbol, env)) = connection.text_target {
        let mut lines = Vec::new();
        let existing = crate::eval::find_var_in_frame(symbol, env)?;
        if existing.kind() == SexpType::String
            && connection.mode.starts_with('a')
            && connection.text_lines.is_empty()
        {
            return Ok(());
        }
        lines.extend(connection.text_lines.iter().cloned());
        let vector = string_vector(&lines)?;
        crate::eval::define(symbol, vector, env)?;
    }
    Ok(())
}

unsafe fn finish_text_output(connection: &mut Connection) -> R<()> {
    if !connection.partial_line.is_empty() {
        let line = std::mem::take(&mut connection.partial_line);
        connection.text_lines.push(line);
        publish_text(connection)?;
    }
    Ok(())
}

unsafe fn do_text_connection_value(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let connection = interp().connections.get(index)?;
    string_vector(&connection.text_lines)
}

unsafe fn do_summary_connection(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let connection = interp().connections.get(index)?;
    let class = match connection.class {
        Class::File => "file",
        Class::Gzfile => "gzfile",
        Class::Text => "textConnection",
        Class::Terminal => "terminal",
    };
    let description = values::scalar_string(&connection.description)?;
    let class = values::scalar_string(class)?;
    let mode = values::scalar_string(&connection.mode)?;
    let text = values::scalar_string(if connection.mode.contains('b') {
        "binary"
    } else {
        "text"
    })?;
    let opened = values::scalar_string(if connection.open { "opened" } else { "closed" })?;
    let can_read = values::scalar_string(if connection.mode.starts_with('r') {
        "yes"
    } else {
        "no"
    })?;
    let can_write = values::scalar_string(
        if connection.mode.starts_with('w') || connection.mode.starts_with('a') {
            "yes"
        } else {
            "no"
        },
    )?;
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

unsafe fn do_get_all_connections(_call: &Call<'_>) -> R<Sexp> {
    let table = &mut interp().connections;
    table.ensure_standard();
    let indices: Vec<i32> = table
        .connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| connection.is_some())
        .map(|(index, _)| index as i32)
        .collect();
    values::integer_vector(&indices)
}
