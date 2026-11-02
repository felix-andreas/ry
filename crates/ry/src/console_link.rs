//! The language server's link to a running `ry run` console in the same workspace, from which
//! hover shows a variable's live value and completion offers the console's variables and a data
//! frame's columns after `$`.
//!
//! A console announces itself with a registry file in `$TMPDIR/ry-<uid>` (see
//! `interp::console_server`). The link picks the newest console whose working directory is the
//! workspace or inside it, keeps one connection to it, and asks for `ryGlobals` with a short
//! timeout, so a console busy with a long command costs a hover at most that timeout and then
//! answers "busy", which the link treats as no data. Static analysis stays the source of truth:
//! the console only adds facts that analysis cannot know, such as a data frame read from a file.

use {
    serde_json::{Value, json},
    std::{
        collections::BTreeMap,
        io::{BufRead, BufReader, Write},
        path::{Path, PathBuf},
        time::{Duration, Instant},
    },
};

/// How long a request to the console may take before the editor gives up on it.
const TIMEOUT: Duration = Duration::from_millis(150);

/// How long an answer stays fresh, and how long the link waits after finding no console before
/// it looks again. Typing fires a hover or completion per keystroke, and each must not cost a
/// round trip that walks the whole global environment, nor a scan of the registry directory.
const FRESH: Duration = Duration::from_secs(2);

/// One variable of the console's global environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    pub kind: String,
    pub class: Option<String>,
    pub summary: Option<String>,
    /// For a data frame or a table: its column names and types.
    pub columns: Vec<(String, String)>,
}

pub struct ConsoleLink {
    workspace: Option<PathBuf>,
    connection: Option<Connection>,
    /// The last answer and when it came, or when the last lookup found nothing (`None`).
    last: Option<(Instant, Option<BTreeMap<String, Global>>)>,
}

impl ConsoleLink {
    /// A link for `workspace`, or a disabled one (`None`) that never looks for a console, as
    /// the simulator needs to stay deterministic.
    pub fn new(workspace: Option<&Path>) -> ConsoleLink {
        ConsoleLink {
            workspace: workspace.map(|path| path.canonicalize().unwrap_or(path.to_path_buf())),
            connection: None,
            last: None,
        }
    }

    /// The console's global variables by name, or `None` when no console runs in the workspace
    /// or it did not answer in time.
    pub fn globals(&mut self) -> Option<&BTreeMap<String, Global>> {
        if self
            .last
            .as_ref()
            .is_none_or(|(when, _)| when.elapsed() >= FRESH)
        {
            let globals = self.request("ryGlobals", json!({})).and_then(|body| {
                let globals = body.get("globals")?.as_array()?;
                Some(
                    globals
                        .iter()
                        .filter_map(global_of)
                        .map(|global| (global.name.clone(), global))
                        .collect(),
                )
            });
            self.last = Some((Instant::now(), globals));
        }
        self.last.as_ref()?.1.as_ref()
    }

    fn request(&mut self, command: &str, arguments: Value) -> Option<Value> {
        let workspace = self.workspace.as_deref()?;
        if self.connection.is_none() {
            self.connection = platform::connect(workspace);
        }
        let connection = self.connection.as_mut()?;
        let answer = connection.request(command, arguments);
        if answer.is_none() {
            // A console that died or stopped answering is looked up afresh next time.
            self.connection = None;
        }
        answer?.ok()
    }
}

fn global_of(value: &Value) -> Option<Global> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|column| {
            Some((
                column.get("name")?.as_str()?.to_owned(),
                column.get("type")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    Some(Global {
        name: text("name")?,
        kind: text("type").unwrap_or_default(),
        class: text("class"),
        summary: text("summary"),
        columns,
    })
}

struct Connection {
    reader: BufReader<platform::Stream>,
    seq: u64,
}

impl Connection {
    /// The response's body, `Err` for a failed response (such as "busy"), and `None` when the
    /// connection broke.
    fn request(&mut self, command: &str, arguments: Value) -> Option<Result<Value, String>> {
        self.seq += 1;
        let seq = self.seq;
        let body =
            json!({ "type": "request", "seq": seq, "command": command, "arguments": arguments })
                .to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.reader.get_mut().write_all(framed.as_bytes()).ok()?;
        loop {
            let message = read_message(&mut self.reader)?;
            // Events, and answers to requests that timed out earlier, are skipped.
            if message.get("type").and_then(Value::as_str) != Some("response")
                || message.get("request_seq").and_then(Value::as_u64) != Some(seq)
            {
                continue;
            }
            return Some(match message.get("success").and_then(Value::as_bool) {
                Some(true) => Ok(message.get("body").cloned().unwrap_or(Value::Null)),
                _ => Err(message
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()),
            });
        }
    }
}

/// One message from the console, or `None` once the connection broke, timed out, or sent
/// something that is not a framed JSON message. The console frames its messages as the debug
/// adapter's client does, so both read them the same way.
pub fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let body = crate::debug_adapter::read_message(reader).ok()??;
    serde_json::from_slice(&body).ok()
}

#[cfg(unix)]
mod platform {
    use {
        super::{Connection, TIMEOUT},
        serde_json::Value,
        std::{
            fs,
            io::{BufReader, ErrorKind},
            os::unix::fs::{MetadataExt, PermissionsExt},
            path::{Path, PathBuf},
        },
    };

    pub type Stream = std::os::unix::net::UnixStream;

    /// Connects to the newest console whose working directory is in `workspace`.
    pub fn connect(workspace: &Path) -> Option<Connection> {
        // SAFETY: `getuid` cannot fail.
        let uid = unsafe { libc::getuid() };
        let directory = std::env::temp_dir().join(format!("ry-{uid}"));
        let metadata = fs::symlink_metadata(&directory).ok()?;
        // Only a directory this user owns and no one else can write is trusted to name sockets.
        if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0
        {
            return None;
        }
        let mut consoles: Vec<(u64, PathBuf, PathBuf)> = fs::read_dir(&directory)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                if !(name.starts_with("console-") && name.ends_with(".json")) {
                    return None;
                }
                let registry: Value =
                    serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
                let cwd = PathBuf::from(registry.get("cwd")?.as_str()?);
                let cwd = cwd.canonicalize().unwrap_or(cwd);
                if !cwd.starts_with(workspace) {
                    return None;
                }
                let socket = PathBuf::from(registry.get("socket")?.as_str()?);
                let modified = entry
                    .metadata()
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                Some((modified, socket, path))
            })
            .collect();
        consoles.sort_by(|left, right| right.0.cmp(&left.0));
        for (_, socket, registry) in consoles {
            match Stream::connect(&socket) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
                    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
                    return Some(Connection {
                        reader: BufReader::new(stream),
                        seq: 0,
                    });
                }
                // The console that wrote the entry is gone without removing it.
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionRefused | ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&registry).ok();
                    fs::remove_file(&socket).ok();
                }
                Err(_) => {}
            }
        }
        None
    }
}

#[cfg(not(unix))]
mod platform {
    use {super::Connection, std::path::Path};

    pub type Stream = std::net::TcpStream;

    pub fn connect(_: &Path) -> Option<Connection> {
        None
    }
}

/// The identifier around `offset` in `text`, and the name before a `$` that directly precedes
/// it, if any: `df$co|` gives `("co", Some("df"))`.
pub fn identifier_at(text: &str, offset: usize) -> Option<(&str, Option<&str>)> {
    let is_part =
        |character: char| character.is_alphanumeric() || character == '.' || character == '_';
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) {
        return None;
    }
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, character)| is_part(character))
        .last()
        .map_or(offset, |(index, _)| index);
    let end = offset
        + text[offset..]
            .char_indices()
            .find(|&(_, character)| !is_part(character))
            .map_or(text.len() - offset, |(index, _)| index);
    let name = &text[start..end];
    let owner = text[..start].strip_suffix('$').and_then(|before| {
        let owner_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, character)| is_part(character))
            .last()
            .map(|(index, _)| index)?;
        Some(&before[owner_start..])
    });
    Some((name, owner))
}

#[cfg(test)]
mod tests {
    use super::identifier_at;

    #[test]
    fn identifiers_and_their_dollar_owner() {
        assert_eq!(identifier_at("df$col", 6), Some(("col", Some("df"))));
        assert_eq!(identifier_at("df$", 3), Some(("", Some("df"))));
        assert_eq!(identifier_at("x + total", 6), Some(("total", None)));
        assert_eq!(identifier_at("my.df", 2), Some(("my.df", None)));
    }
}
