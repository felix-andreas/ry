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
    internal("file", 6, |call| unsafe { open_connection(call, Class::File) }),
    internal("gzfile", 4, |call| unsafe { open_connection(call, Class::Gzfile) }),
    internal("bzfile", 4, |call| unsafe { open_connection(call, Class::Gzfile) }),
    internal("xzfile", 4, |call| unsafe { open_connection(call, Class::Gzfile) }),
    internal("url", 5, |call| unsafe { Err(call.error("url connections are not supported")) }),
    internal("textConnection", 5, do_text_connection),
    internal("textConnectionValue", 1, do_text_connection_value),
    internal("open", 3, do_open).invisible(),
    internal("close", 2, do_close).invisible(),
    internal("isOpen", 2, do_is_open),
    internal("stdin", 0, |_| unsafe { connection_object(0) }),
    internal("stdout", 0, |_| unsafe { connection_object(1) }),
    internal("stderr", 0, |_| unsafe { connection_object(2) }),
    internal("readLines", 6, do_read_lines),
    internal("writeLines", 4, do_write_lines).invisible(),
    internal("readChar", 4, do_read_char),
    internal("writeChar", 5, |call| unsafe { Err(call.error("writeChar is not supported")) }),
    internal("readBin", 6, do_read_bin),
    internal("writeBin", 5, |call| unsafe { Err(call.error("writeBin is not supported")) }),
    internal("cat", 6, do_cat).invisible(),
    internal("summary.connection", 1, do_summary_connection),
    internal("getAllConnections", 0, do_get_all_connections),
    internal("getConnection", 1, |call| unsafe { connection_object(values::as_integer_scalar(call.arg(0))) }),
    internal("flush", 1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("isIncomplete", 1, |_| unsafe { scalar_bool(false) }),
    internal("isSeekable", 1, |_| unsafe { scalar_bool(false) }),
    internal("pushBack", 4, |_| unsafe { Ok(nil()) }).invisible(),
    internal("pushBackLength", 1, |_| unsafe { scalar_integer(0) }),
    internal("sink", 4, |_| unsafe { Ok(nil()) }).invisible(),
    internal("sink.number", 1, |_| unsafe { scalar_integer(0) }),
    internal("readline", 1, |_| unsafe { values::scalar_string("") }),
    internal("scan", 19, |call| unsafe { Err(call.error("scan is not supported")) }),
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
        match table.connections.get(index.max(0) as usize).and_then(Option::as_ref) {
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
        error(format!("cannot open file '{path}': {}", io_reason(&failure)))
    })?;
    if gzip && bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoded = Vec::new();
        MultiGzDecoder::new(&bytes[..])
            .read_to_end(&mut decoded)
            .map_err(|failure| error(format!("cannot read compressed file '{path}': {failure}")))?;
        return Ok(decoded);
    }
    Ok(bytes)
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
    let mode = values::first_string(call.arg(1)).map(|text| text.into_owned()).unwrap_or_default();
    let mut connection = Connection {
        class,
        description: description.clone(),
        mode: mode.clone(),
        open: false,
        input: Vec::new(),
        position: 0,
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
    let mode = if connection.mode.is_empty() { "r".to_owned() } else { connection.mode.clone() };
    connection.mode = mode.clone();
    if mode.starts_with('r') {
        connection.input = read_file_bytes(&connection.description, true)?;
        connection.position = 0;
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
            let file = std::fs::File::create(&path)
                .map_err(|failure| error(format!("cannot open file '{path}': {}", io_reason(&failure))))?;
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::new(6));
            encoder
                .write_all(&connection.output)
                .and_then(|()| encoder.finish().map(drop))
                .map_err(|failure| error(format!("cannot write '{path}': {failure}")))?;
        } else {
            std::fs::write(&path, &connection.output)
                .map_err(|failure| error(format!("cannot write '{path}': {}", io_reason(&failure))))?;
        }
    }
    Ok(())
}

unsafe fn do_open(call: &Call<'_>) -> R<Sexp> {
    let index = connection_index(call.arg(0))?;
    let mode = values::first_string(call.arg(1)).map(|text| text.into_owned()).unwrap_or_default();
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

/// Appends bytes to a writable connection.
pub unsafe fn write_bytes(file: Sexp, bytes: &[u8]) -> R<()> {
    let index = connection_index(file)?;
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
        std::fs::write(expand_path(&path), text)
            .map_err(|failure| error(format!("cannot open file '{path}': {}", io_reason(&failure))))?;
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
    interp().connections.get(index).ok().map(|connection| connection.description.clone())
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
    let separator = values::first_string(call.arg(2)).map(|text| text.into_owned()).unwrap_or_else(|| "\n".into());
    let mut output = String::new();
    for index in 0..length(text) {
        output.push_str(&values::string_at(text, index).map_or_else(|| "NA".to_owned(), |line| line.into_owned()));
        output.push_str(&separator);
    }
    write_bytes(file, output.as_bytes())?;
    Ok(nil())
}

unsafe fn do_read_char(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    let counts = call.arg(1);
    let bytes = if source.kind() == SexpType::String {
        read_file_bytes(&values::require_string(source, "con")?, false)?
    } else {
        read_all_bytes(source)?
    };
    let mut offset = 0;
    let mut items = Vec::new();
    for index in 0..length(counts) {
        let count = values::as_integer_scalar(crate::subset::vector_element(counts, index)?).max(0) as usize;
        let end = (offset + count).min(bytes.len());
        items.push(String::from_utf8_lossy(&bytes[offset..end]).into_owned());
        offset = end;
    }
    string_vector(&items)
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
        if values::as_logical_scalar(fill) == 1 { crate::format::width_option() } else { 0 }
    } else {
        values::as_integer_scalar(fill).max(0) as usize
    };
    let separator_count = length(separators);
    let separator_at = |index: usize| -> String {
        if separator_count == 0 {
            String::new()
        } else {
            values::string_at(separators, index % separator_count).map_or_else(|| "NA".into(), |text| text.into_owned())
        }
    };
    let mut output = String::new();
    let mut piece_index = 0usize;
    let mut line_width = 0usize;
    for item_index in 0..length(items) {
        let item = items.element(item_index);
        let texts: Vec<String> = match item.kind() {
            SexpType::Nil => continue,
            SexpType::Symbol => vec![values::symbol_name(item).into_owned()],
            SexpType::String => (0..length(item))
                .map(|index| values::string_at(item, index).map_or_else(|| "NA".into(), |text| text.into_owned()))
                .collect(),
            SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex | SexpType::Raw => {
                let item = values::materialize(item)?;
                (0..length(item))
                    .map(|index| cat_element(item, index))
                    .collect()
            }
            SexpType::List if (0..length(item)).all(|index| {
                let element = item.element(index);
                element.kind().is_atomic() && length(element) == 1
            }) => (0..length(item))
                .map(|index| cat_element(item.element(index), 0))
                .collect(),
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
            if piece_index > 0 {
                let separator = separator_at(piece_index - 1);
                if fill_width > 0 && line_width + separator.len() + text.len() > fill_width {
                    output.push('\n');
                    line_width = 0;
                } else {
                    output.push_str(&separator);
                    line_width += separator.len();
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
    if fill_width > 0 && !output.ends_with('\n') && !output.is_empty() {
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
        SexpType::String => values::string_at(vector, index).map_or_else(|| "NA".into(), |text| text.into_owned()),
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
        output: Vec::new(),
        text_target: None,
        text_lines: Vec::new(),
        partial_line: String::new(),
    };
    if mode.starts_with('r') || mode.is_empty() {
        let mut text = String::new();
        for index in 0..length(object) {
            text.push_str(&values::string_at(object, index).map_or_else(|| "NA".into(), |line| line.into_owned()));
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
        if existing.kind() == SexpType::String && connection.mode.starts_with('a') && connection.text_lines.is_empty() {
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
    let text = values::scalar_string("text")?;
    let opened = values::scalar_string(if connection.open { "opened" } else { "closed" })?;
    let can_read = values::scalar_string(if connection.mode.starts_with('r') { "yes" } else { "no" })?;
    let can_write = values::scalar_string(if connection.mode.starts_with('w') || connection.mode.starts_with('a') { "yes" } else { "no" })?;
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
