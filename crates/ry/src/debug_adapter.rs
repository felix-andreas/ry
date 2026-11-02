//! `ry debug-adapter`: the Debug Adapter Protocol on stdio. The adapter itself runs inside ry's
//! runtime (`interp::debugger`); this side only frames its messages. A reader thread parses the
//! client's `Content-Length` frames from stdin, because the runtime polls for a `pause` between
//! statements and must not block on stdin to do so.

use {
    repl::{ReplError, RuntimeFlags},
    std::{
        ffi::{c_char, c_int, c_void},
        io::{self, BufRead, Read, Write},
        sync::mpsc::{self, Receiver},
    },
};

/// Serves one debug session on stdin and stdout and returns the program's exit status.
pub fn run(flags: RuntimeFlags) -> Result<i32, ReplError> {
    let output = protocol_output()
        .map_err(|error| ReplError(format!("cannot set up the protocol's output: {error}")))?;
    let (sender, messages) = mpsc::channel();
    std::thread::Builder::new()
        .name("ry-debug-reader".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            loop {
                match read_message(&mut stdin) {
                    Ok(Some(message)) => {
                        if sender.send(message).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        eprintln!("ry: cannot read from the debug client: {error}");
                        break;
                    }
                }
            }
        })
        .map_err(|error| ReplError(format!("cannot start the protocol reader: {error}")))?;
    let mut wire = Wire {
        messages,
        current: Vec::new(),
        output,
    };
    // SAFETY: `wire` outlives the call, and the callbacks keep the contracts `repl` states.
    unsafe {
        repl::debug_on_ry_runtime(
            receive,
            send,
            (&mut wire as *mut Wire).cast::<c_void>(),
            flags,
        )
    }
}

/// Reads one `Content-Length`-framed message's body, or `None` at the end of the input before a
/// message starts. Blank lines between messages are skipped, header names ignore case, and
/// headers other than `Content-Length` are ignored.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut started = false;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_HEADER_LINE as u64)
            .read_line(&mut line)?;
        if read == MAX_HEADER_LINE && !line.ends_with('\n') {
            return Err(invalid(format!(
                "a header line is longer than the {MAX_HEADER_LINE} bytes ry accepts"
            )));
        }
        if read == 0 {
            return match started {
                false => Ok(None),
                true => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the input ended inside a message's headers",
                )),
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if started {
                break;
            }
            continue;
        }
        started = true;
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let value = value.trim().parse::<usize>().map_err(|_| {
                invalid(format!("`{}` is not a valid Content-Length", value.trim()))
            })?;
            length = Some(value);
        }
    }
    let length = length.ok_or_else(|| invalid("a message has no Content-Length".to_owned()))?;
    if length > MAX_MESSAGE {
        return Err(invalid(format!(
            "a message of {length} bytes is larger than the {MAX_MESSAGE} ry accepts"
        )));
    }
    // The body grows with the bytes that arrive, not with the length a header claims, so a
    // corrupt header costs no more memory than the input it came with.
    let mut body = Vec::new();
    reader.take(length as u64).read_to_end(&mut body)?;
    if body.len() < length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the input ended inside a message's body",
        ));
    }
    Ok(Some(body))
}

pub fn write_message(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// A larger length is a corrupt header rather than a real request, and allocating it would
/// abort the process.
pub const MAX_MESSAGE: usize = 64 << 20;

/// A header line is a name and a number; a longer one is garbage, and reading it whole would let
/// input without line breaks grow the line without bound.
pub const MAX_HEADER_LINE: usize = 1024;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Wire {
    messages: Receiver<Vec<u8>>,
    /// The message `receive` last returned, which must stay alive until its next call.
    current: Vec<u8>,
    output: Box<dyn Write>,
}

unsafe extern "C" fn receive(wait: c_int, length: *mut usize, data: *mut c_void) -> *const c_char {
    // SAFETY: `data` is the `Wire` that `run` passed, alive until the runtime returns.
    let wire = unsafe { &mut *data.cast::<Wire>() };
    let next = match wait != 0 {
        true => wire.messages.recv().ok(),
        false => wire.messages.try_recv().ok(),
    };
    match next {
        Some(message) => {
            wire.current = message;
            // SAFETY: the runtime passes a valid pointer for the length.
            unsafe { *length = wire.current.len() };
            wire.current.as_ptr().cast()
        }
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn send(text: *const c_char, length: usize, data: *mut c_void) {
    // SAFETY: as for `receive`; the runtime passes `length` readable bytes at `text`.
    let (wire, body) = unsafe {
        (
            &mut *data.cast::<Wire>(),
            std::slice::from_raw_parts(text.cast::<u8>(), length),
        )
    };
    // A client that stopped reading is gone; the reader thread sees its end of the input, and
    // the session ends when the runtime next waits for a message.
    if let Err(error) = write_message(&mut wire.output, body) {
        eprintln!("ry: cannot write to the debug client: {error}");
    }
}

/// The protocol owns stdout, so anything else the process writes there (native code calling
/// `printf`, say) would corrupt it. On Unix the protocol gets its own copy of stdout, and stdout
/// itself is pointed at stderr.
#[cfg(unix)]
fn protocol_output() -> io::Result<Box<dyn Write>> {
    use std::os::fd::FromRawFd;
    io::stdout().flush()?;
    // SAFETY: duplicating the process's own standard descriptors has no other preconditions.
    let duplicate = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `duplicate` is a fresh descriptor this file now owns.
    Ok(Box::new(unsafe { std::fs::File::from_raw_fd(duplicate) }))
}

#[cfg(not(unix))]
fn protocol_output() -> io::Result<Box<dyn Write>> {
    Ok(Box::new(io::stdout()))
}
