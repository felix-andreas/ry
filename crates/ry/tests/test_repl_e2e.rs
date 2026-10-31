//! Needs a local R and skips (loudly, but green) without one; CI has no R, so run
//! these locally: `cargo test -p ry-lang --test test_repl_e2e -- --nocapture`.
//! Other platforms have no embedding recipe, so `ry repl` refuses at startup there.

#![cfg(any(unix, windows))]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fs, thread};

fn r_available() -> bool {
    let available = std::process::Command::new("R")
        .arg("RHOME")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped: no R installation on this machine");
    }
    available
}

/// Concurrent full R sessions make pty timing flaky on a loaded machine.
static SESSION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ReplSession {
    _lock: std::sync::MutexGuard<'static, ()>,
    _pair: portable_pty::PtyPair,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    chunks: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    seen: String,
    /// So a terminal query split over a read boundary is still recognized.
    raw_carry: Vec<u8>,
}

impl ReplSession {
    fn start() -> ReplSession {
        ReplSession::start_in(None)
    }

    fn start_in(directory: Option<&std::path::Path>) -> ReplSession {
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
        if let Some(directory) = directory {
            command.cwd(directory);
        }
        let child = pair.slave.spawn_command(command).expect("spawn repl");
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        // The pty read blocks with no timeout, so it gets its own thread and a silent
        // session fails instead of hanging.
        let (sender, chunks) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
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

    /// Answers terminal queries: the editor asks for the cursor position before
    /// drawing its prompt and blocks until the reply comes.
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

    /// A bare sleep would leave the editor's terminal handshake unanswered, so
    /// queued input would never start evaluating.
    fn settle(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.drain(Duration::from_millis(50));
        }
    }

    /// Matches ANSI-stripped output; on timeout, panics with everything seen so far.
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

/// Starts a session at its banner, or skips the test when no R is installed.
fn start_session() -> Option<ReplSession> {
    if !r_available() {
        return None;
    }
    let mut session = ReplSession::start();
    session.expect("ry R console");
    Some(session)
}

/// Runs `body` as a script through `ry run`, or skips the test when no R is installed.
fn run_script(body: &str) -> Option<(std::process::Output, String)> {
    if !r_available() {
        return None;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    let script = directory.path().join("script.R");
    fs::write(&script, body).expect("write script");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("run")
        .arg(&script)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Some((output, stdout))
}

#[test]
fn the_package_in_the_working_directory_loads_and_edits_reload_before_the_next_command() {
    if !r_available() {
        return;
    }
    let package = std::env::temp_dir().join(format!("ry-repl-load-{}", std::process::id()));
    fs::create_dir_all(package.join("R")).expect("create package");
    fs::write(
        package.join("DESCRIPTION"),
        "Package: replpkg\nVersion: 1.0\n",
    )
    .expect("write");
    fs::write(
        package.join("R/a.R"),
        "greet <- function() \"first version\"\n",
    )
    .expect("write");
    let mut session = ReplSession::start_in(Some(&package));
    session.expect("Loaded replpkg");
    session.send("greet()\r");
    session.expect("first version");
    // Distinct sizes, so the change is visible even within one timestamp tick.
    fs::write(
        package.join("R/a.R"),
        "greet <- function() \"second, edited version\"\n",
    )
    .expect("write");
    session.send("greet()\r");
    session.expect("Reloaded replpkg: 1 function rebound");
    session.expect("second, edited version");
    session.quit();
    fs::remove_dir_all(&package).ok();
}

#[test]
fn evaluates_and_autoprints() {
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("1 + 1\r");
    session.expect("[1] 2");
    session.quit();
}

#[test]
fn multiline_input_continues_and_defines() {
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("f <- function(x) {\r");
    session.send("x * 2\r");
    session.send("}\r");
    session.send("f(21)\r");
    session.expect("[1] 42");
    session.quit();
}

#[test]
fn errors_come_back_on_the_error_stream() {
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("stop(\"boom\")\r");
    session.expect("boom");
    session.send("1 + 1\r");
    session.expect("[1] 2");
    session.quit();
}

#[test]
fn tab_completes_with_analysis_signatures() {
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("seq_l\t");
    // Session-binding completion is pinned by the completer's unit tests; menu
    // interaction over a pty is too stateful to assert reliably.
    session.expect("seq_len");
    session.expect("fn(length.out: Any) -> integer[]");
    session.send("\r");
    session.settle(Duration::from_millis(300));
    session.send("\u{3}");
    session.settle(Duration::from_millis(300));
    session.quit();
}

#[test]
fn ctrl_c_interrupts_evaluation() {
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("Sys.sleep(60)\r");
    // Let evaluation start (terminal back in cooked mode) so Ctrl-C arrives as
    // SIGINT, not as an editor keystroke.
    session.expect("Sys.sleep(60)");
    session.settle(Duration::from_millis(1500));
    session.send("\u{3}");
    session.send("40 + 2\r");
    session.expect("[1] 42");
    session.quit();
}

#[test]
fn the_terminal_width_reaches_r() {
    // R defaults to 80 columns regardless of the terminal and exports no setter, so
    // the console must feed `options(width = …)` in.
    let Some(mut session) = start_session() else {
        return;
    };
    session.send("cat(\"WIDTH:\", getOption(\"width\"), \"\\n\")\r");
    session.expect("WIDTH: 120");
    session.quit();
}

#[test]
fn a_table_that_fits_is_printed_on_one_line() {
    // A 92-character header: over R's default width, under the session terminal's 120.
    let Some(mut session) = start_session() else {
        return;
    };
    session.send(
        "print(data.frame(a_reasonably_long_column=1L, another_long_column_name=2L, third_column_name_here=3L))\r",
    );
    session.expect("a_reasonably_long_column another_long_column_name third_column_name_here");
    session.quit();
}

#[test]
fn run_executes_a_file_and_exits_zero() {
    let Some((output, stdout)) = run_script("x <- 40 + 2\nprint(x)\n") else {
        return;
    };
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
    let Some((output, stdout)) = run_script("stop(\"boom\")\nprint(\"unreached\")\n") else {
        return;
    };
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
