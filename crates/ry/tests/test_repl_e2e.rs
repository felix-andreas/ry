//! End-to-end tests for `ry repl`, driving the real binary through a
//! pseudo-terminal: type input, watch R evaluate. They need a local R
//! installation and a dynamically linked `ry` to load it into, so they SKIP
//! (loudly, but green) otherwise — CI has no R, and by decision these run
//! locally before REPL-touching changes:
//!
//! ```sh
//! cargo test -p ry-lang --test test_repl_e2e -- --nocapture
//! ```
//!
//! Runs wherever the console itself runs (Unix ptys and Windows ConPTY,
//! through the same `portable-pty` seam); on platforms with no embedding
//! recipe `ry repl` refuses at startup, so an R on `PATH` would only
//! produce false failures there.

#![cfg(any(unix, windows))]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn r_available() -> bool {
    if cfg!(target_feature = "crt-static") {
        eprintln!("skipped: a statically linked ry cannot load R");
        return false;
    }
    let available = std::process::Command::new("R")
        .arg("RHOME")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped: no R installation on this machine");
    }
    available
}

/// One interactive session at a time: concurrent full R sessions on a
/// loaded machine make the pty timing flaky, while serial runs are stable —
/// the lock rides in the session so its scope is exactly the session's.
static SESSION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ReplSession {
    _lock: std::sync::MutexGuard<'static, ()>,
    _pair: portable_pty::PtyPair,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    seen: String,
    /// Raw tail kept across chunks so a terminal query split over a read
    /// boundary is still recognized.
    raw_carry: Vec<u8>,
}

impl ReplSession {
    fn start() -> ReplSession {
        let lock = SESSION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_ry"));
        command.arg("repl");
        let child = pair.slave.spawn_command(command).expect("spawn repl");
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        // The pty read is blocking with no timeout, so it lives on its own
        // thread; `expect` then waits on the channel with a deadline — a
        // session that goes silent fails loudly instead of hanging the test.
        let (sender, chunks) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if sender.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        ReplSession {
            _lock: lock,
            _pair: pair,
            child,
            chunks,
            writer,
            seen: String::new(),
            raw_carry: Vec::new(),
        }
    }

    fn send(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).expect("pty write");
        self.writer.flush().expect("pty flush");
    }

    /// Waits up to `wait` for one output chunk, answering the terminal
    /// queries a real terminal would answer (the editor asks for the cursor
    /// position before drawing its prompt and blocks until the reply comes).
    fn drain(&mut self, wait: Duration) {
        let Ok(chunk) = self.chunks.recv_timeout(wait) else {
            return;
        };
        self.raw_carry.extend_from_slice(&chunk);
        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
        let queries = self
            .raw_carry
            .windows(CURSOR_QUERY.len())
            .filter(|window| *window == CURSOR_QUERY)
            .count();
        for _ in 0..queries {
            self.writer.write_all(b"\x1b[1;1R").expect("query reply");
        }
        if queries > 0 {
            self.writer.flush().expect("query flush");
        }
        let keep = self.raw_carry.len().saturating_sub(CURSOR_QUERY.len() - 1);
        self.raw_carry.drain(..keep);
        self.seen
            .push_str(&strip_ansi(&String::from_utf8_lossy(&chunk)));
    }

    /// Keeps servicing the session for a fixed duration — used where a test
    /// must let evaluation *start* (a bare sleep would leave the editor's
    /// terminal handshake unanswered, so the input would still be queued).
    fn settle(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.drain(Duration::from_millis(50));
        }
    }

    /// Reads until the accumulated (ANSI-stripped) output contains `needle`.
    /// Panics with everything seen so far on timeout, so a failure names
    /// what the session actually did.
    fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.seen.contains(needle) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; output so far:\n{}",
                self.seen
            );
            self.drain(Duration::from_millis(100));
        }
    }

    fn quit(mut self) {
        self.send("q()\r");
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.child.try_wait().expect("child wait").is_some() {
                return;
            }
            self.drain(Duration::from_millis(50));
        }
        self.child.kill().ok();
        panic!("the session did not exit after q(); output:\n{}", self.seen);
    }
}

/// Drops ESC-introduced control sequences so assertions match what a human
/// reads, not the editor's redraw traffic.
fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            stripped.push(character);
            continue;
        }
        match characters.peek() {
            // CSI: parameters then a final byte in @..~
            Some('[') => {
                characters.next();
                for terminator in characters.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&terminator) {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ESC \
            Some(']') => {
                characters.next();
                while let Some(inner) = characters.next() {
                    if inner == '\u{7}' {
                        break;
                    }
                    if inner == '\u{1b}' && characters.peek() == Some(&'\\') {
                        characters.next();
                        break;
                    }
                }
            }
            _ => {
                characters.next();
            }
        }
    }
    stripped
}

#[test]
fn evaluates_and_autoprints() {
    if !r_available() {
        return;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("1 + 1\r");
    session.expect("[1] 2");
    session.quit();
}

#[test]
fn multiline_input_continues_and_defines() {
    if !r_available() {
        return;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("f <- function(x) {\r");
    session.send("x * 2\r");
    session.send("}\r");
    session.send("f(21)\r");
    session.expect("[1] 42");
    session.quit();
}

#[test]
fn errors_come_back_on_the_error_stream() {
    if !r_available() {
        return;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("stop(\"boom\")\r");
    session.expect("boom");
    session.send("1 + 1\r");
    session.expect("[1] 2");
    session.quit();
}

#[test]
fn tab_completes_with_analysis_signatures() {
    if !r_available() {
        return;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("seq_l\t");
    // The completion menu renders the analysis-backed signature next to the
    // candidate — the stub corpus reaching the console. (Session-binding
    // completion is pinned by the completer's own unit tests; driving the
    // menu through several interactions over a pty is too stateful to
    // assert reliably.)
    session.expect("seq_len");
    session.expect("fn(length.out: Any) -> integer[]");
    // Enter accepts the highlighted candidate and closes the menu; Ctrl-C
    // then clears the line for a clean quit.
    session.send("\r");
    session.settle(Duration::from_millis(300));
    session.send("\u{3}");
    session.settle(Duration::from_millis(300));
    session.quit();
}

#[test]
fn ctrl_c_interrupts_evaluation() {
    if !r_available() {
        return;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("Sys.sleep(60)\r");
    // The editor echoes the line once it has consumed it; give evaluation a
    // moment to actually start (terminal back in cooked mode) so Ctrl-C
    // arrives as SIGINT, not as an editor keystroke.
    session.expect("Sys.sleep(60)");
    session.settle(Duration::from_millis(1500));
    session.send("\u{3}");
    // The session must come back alive well before the sleep could finish.
    session.send("40 + 2\r");
    session.expect("[1] 42");
    session.quit();
}

// Batch mode (`ry run`) needs no terminal: plain process spawns.

#[test]
fn the_terminal_width_reaches_r() {
    if !r_available() {
        return;
    }
    // R's own default is 80 columns whatever the terminal is, and it exports
    // no setter — so without the console feeding `options(width = …)` in,
    // every wide table wraps on a terminal that had room for it.
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send("cat(\"WIDTH:\", getOption(\"width\"), \"\\n\")\r");
    session.expect("WIDTH: 120");
    session.quit();
}

#[test]
fn a_table_that_fits_is_printed_on_one_line() {
    if !r_available() {
        return;
    }
    // Three columns whose header is 92 characters: over R's default width, so
    // this is the shape that used to split after the second column, and well
    // under the 120 the session's terminal actually has.
    let mut session = ReplSession::start();
    session.expect("ry R console");
    session.send(
        "print(data.frame(a_reasonably_long_column=1L, another_long_column_name=2L, third_column_name_here=3L))\r",
    );
    session.expect("a_reasonably_long_column another_long_column_name third_column_name_here");
    session.quit();
}

#[test]
fn run_executes_a_file_and_exits_zero() {
    if !r_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    let script = directory.path().join("ok.R");
    std::fs::write(&script, "x <- 40 + 2\nprint(x)\n").expect("write script");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("run")
        .arg(&script)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("[1] 42"), "script output missing: {stdout}");
}

#[test]
fn run_error_exits_nonzero() {
    if !r_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    let script = directory.path().join("bad.R");
    std::fs::write(&script, "stop(\"boom\")\nprint(\"unreached\")\n").expect("write script");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("run")
        .arg(&script)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a top-level error must exit 1\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("unreached"),
        "the error must halt the script: {stdout}"
    );
}
