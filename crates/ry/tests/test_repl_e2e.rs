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
        ReplSession::start_with(directory, &[])
    }

    fn start_with(directory: Option<&std::path::Path>, arguments: &[&str]) -> ReplSession {
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
        command.args(arguments);
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
fn reloads_survive_a_custom_prompt_and_skip_readline_answers() {
    if !r_available() {
        return;
    }
    let package = std::env::temp_dir().join(format!("ry-repl-prompt-{}", std::process::id()));
    fs::create_dir_all(package.join("R")).expect("create package");
    fs::write(
        package.join("DESCRIPTION"),
        "Package: promptpkg\nVersion: 1.0\n",
    )
    .expect("write");
    fs::write(package.join("R/a.R"), "level <- function() \"one\"\n").expect("write");
    let mut session = ReplSession::start_in(Some(&package));
    session.expect("Loaded promptpkg");
    session.send("options(prompt = \"R> \")\r");
    session.settle(Duration::from_millis(300));
    fs::write(package.join("R/a.R"), "level <- function() \"level two\"\n").expect("write");
    session.send("answer <- readline(\"answer: \")\r");
    session.expect("answer: ");
    session.send("typed\r");
    session.send("cat(\"got\", answer, level(), \"\\n\")\r");
    session.expect("got typed level two");
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

/// ry's runtime is the `libR` built beside the binary, which `cargo test -p ry-lang` does not
/// build, so these tests skip without it: `cargo build -p libr` first.
fn ry_runtime_available() -> bool {
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_ry"));
    let library = if cfg!(target_os = "macos") {
        "libR.dylib"
    } else {
        "libR.so"
    };
    let available = std::env::var_os("RY_LIBR").is_some()
        || binary
            .parent()
            .is_some_and(|directory| directory.join(library).exists());
    if !available {
        eprintln!("skipped: ry's runtime library is not built");
    }
    available
}

/// Runs `files[0]` with `ry run`, in a project made of `files` (name, text), on `runtime`.
fn run_project(runtime: &str, files: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let directory = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        fs::write(directory.path().join(name), text).expect("write project file");
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("run")
        .arg("--runtime")
        .arg(runtime)
        .arg(files[0].0)
        .current_dir(directory.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run the binary");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The runtimes available here: GNU R when installed, ry's when built.
fn runtimes() -> Vec<&'static str> {
    let mut runtimes = Vec::new();
    if r_available() {
        runtimes.push("gnu-r");
    }
    if ry_runtime_available() {
        runtimes.push("ry");
    }
    runtimes
}

const DISCOUNT: &str = "apply_discount <- function(price: double, rate: double = 0.2) -> double {\n  price * (1 - rate)\n}\ncount: integer <- 3L\nprint(apply_discount(100))\nprint(count)\n";

#[test]
fn run_executes_annotated_code_on_both_runtimes() {
    for runtime in runtimes() {
        let (status, stdout, stderr) = run_project(runtime, &[("script.R", DISCOUNT)]);
        assert_eq!(status, Some(0), "{runtime}: {stderr}");
        assert!(
            stdout.contains("[1] 80") && stdout.contains("[1] 3"),
            "{runtime}: {stdout}"
        );
        assert!(
            stderr.is_empty(),
            "{runtime}: a satisfied annotation prints nothing: {stderr}"
        );
    }
}

#[test]
fn ry_runtime_admits_a_frame_for_a_frame_annotation() {
    if !ry_runtime_available() {
        return;
    }
    let script = "total <- function(sales: frame{team: character, amount: double}) -> double sum(sales$amount)\n\
        print(total(frame(team = \"a\", amount = 2)))\n";
    let (status, stdout, stderr) = run_project("ry", &[("script.R", script)]);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(stdout.contains("[1] 2"), "{stdout}");
}

#[test]
fn ry_runtime_checks_arguments_and_returns_at_the_boundary() {
    if !ry_runtime_available() {
        return;
    }
    let script = "f <- function(x: integer) -> integer x\n\
        print(tryCatch(f(\"a\"), error = conditionMessage))\n\
        g <- function() -> character { 1 }\n\
        print(tryCatch(g(), error = conditionMessage))\n\
        lazy <- function(x: integer, y: integer) x\n\
        print(lazy(1L, stop(\"never forced\")))\n";
    let (status, stdout, stderr) = run_project("ry", &[("script.R", script)]);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(
        stdout.contains("argument `x` must be `integer`, not `character`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("the function must return `character`, not `double`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("[1] 1"),
        "an unforced parameter stays unchecked: {stdout}"
    );
}

#[test]
fn ry_runtime_checks_follow_the_definition_and_the_promise() {
    if !ry_runtime_available() {
        return;
    }
    let script = r#"typed <- function(x: integer) -> integer x
untyped <- eval(call("function", formals(typed), quote(x * 1.5)))
print(untyped(2L))
inner <- function(...: double) ..1
outer <- function(...: integer) inner(...)
print(tryCatch(outer(2.5), error = conditionMessage))
caught <- function(x: integer) { try(x, silent = TRUE); x }
print(tryCatch(caught("a"), error = conditionMessage))
apply1 <- function(fun, v) fun(v)
named <- function(x: integer) apply1(function(z) x, 0)
print(tryCatch(apply1(named, "bad"), error = conditionMessage))
"#;
    let (status, stdout, stderr) = run_project("ry", &[("script.R", script)]);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(
        stdout.contains("[1] 3"),
        "a closure built from typed formals is untyped: {stdout}"
    );
    assert!(
        stdout.contains("every `...` argument must be `integer`, but one is `double`"),
        "a forwarded promise is checked by every typed frame: {stdout}"
    );
    assert_eq!(
        stdout
            .matches("argument `x` must be `integer`, not `character`")
            .count(),
        2,
        "a caught failure leaves the value checked, and the error names the formal: {stdout}"
    );
}

#[test]
fn run_warns_when_a_comment_and_an_inline_annotation_disagree() {
    let script = "#: fn(x: character) -> character\nf <- function(x: integer) x\nprint(f(2L))\n";
    for runtime in runtimes() {
        let (status, stdout, stderr) = run_project(runtime, &[("script.R", script)]);
        assert_eq!(status, Some(0), "{runtime}: {stderr}");
        assert!(stdout.contains("[1] 2"), "{runtime}: {stdout}");
        assert!(
            stderr.contains("annotation-mismatch"),
            "{runtime}: {stderr}"
        );
    }
}

#[test]
fn run_with_inline_annotations_off_warns_and_skips_the_checks() {
    let config = "[check]\ninline-annotations = false\n";
    let script = "f <- function(x: integer) is.character(x)\nprint(f(\"a\"))\n";
    for runtime in runtimes() {
        let (status, stdout, stderr) =
            run_project(runtime, &[("script.R", script), ("ry.toml", config)]);
        assert_eq!(status, Some(0), "{runtime}: {stderr}");
        assert!(
            stdout.contains("[1] TRUE"),
            "{runtime}: nothing checks `x`: {stdout}"
        );
        assert!(
            stderr.contains("inline type annotations are turned off"),
            "{runtime}: {stderr}"
        );
    }
}

#[test]
fn ry_runtime_refuses_class_definitions_when_object_systems_are_off() {
    if !ry_runtime_available() {
        return;
    }
    let config = "[check]\nobject-systems = false\n";
    for (code, function) in [
        ("setClass(\"P\", representation(x = \"numeric\"))\n", "setClass"),
        ("methods::setRefClass(\"A\")\n", "setRefClass"),
    ] {
        let script = format!("cat(\"before\\n\")\n{code}cat(\"after\\n\")\n");
        let (status, stdout, stderr) =
            run_project("ry", &[("script.R", &script), ("ry.toml", config)]);
        assert_eq!(status, Some(1), "{function}: {stderr}");
        assert!(stdout.contains("before") && !stdout.contains("after"), "{stdout}");
        assert!(
            stderr.contains(&format!(
                "(`[check] object-systems = false`), so `{function}` cannot define"
            )),
            "{function}: {stderr}"
        );
    }
    let (status, stdout, stderr) = run_project(
        "ry",
        &[("script.R", "setClass(\"P\", representation(x = \"numeric\"))\ncat(\"defined\\n\")\n")],
    );
    assert_eq!(status, Some(0), "{stderr}");
    assert!(stdout.contains("defined"), "{stdout}");
}

#[test]
fn the_ry_console_refuses_class_definitions_when_object_systems_are_off() {
    if !ry_runtime_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    fs::write(
        directory.path().join("ry.toml"),
        "[check]\nobject-systems = false\n",
    )
    .expect("write config");
    let mut session =
        ReplSession::start_with(Some(directory.path()), &["--no-load", "--runtime", "ry"]);
    session.expect("console");
    session.settle(Duration::from_millis(400));
    session.send("setRefClass(\"A\")\r");
    session.expect("`setRefClass` cannot define a Reference Class");
    session.send("1 + 1\r");
    session.expect("[1] 2");
    session.quit();
}

#[test]
fn piped_input_is_evaluated_and_autoprinted_on_both_runtimes() {
    use std::io::Write;
    let directory = tempfile::tempdir().expect("temp dir");
    for runtime in runtimes() {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
            .args(["repl", "--no-load", "--runtime", runtime])
            .current_dir(directory.path())
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("start the console");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(b"1 + 1\nf <- function(a) {\n  a * 2\n}\nf(21)\n")
            .expect("write the commands");
        let output = child.wait_with_output().expect("console exits at end of input");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{runtime}: {output:?}");
        assert!(stdout.contains("[1] 2\n"), "{runtime}: {stdout}");
        assert!(stdout.contains("[1] 42\n"), "{runtime}: {stdout}");
    }
}

#[test]
fn run_in_strict_mode_refuses_untyped_code() {
    let config = "[check]\ntyping = true\nstrict = true\n";
    let untyped = "describe <- function(x) cat(\"a value\\n\")\ndescribe(1)\n";
    let typed = "describe <- function(x: double) cat(\"a value\\n\")\ndescribe(1)\n";
    for runtime in runtimes() {
        let (status, stdout, stderr) =
            run_project(runtime, &[("script.R", untyped), ("ry.toml", config)]);
        assert_eq!(status, Some(1), "{runtime}: {stderr}");
        assert!(
            !stdout.contains("a value"),
            "{runtime}: refused code must not run: {stdout}"
        );
        assert!(
            stderr.contains("strict mode found 1 error"),
            "{runtime}: {stderr}"
        );
        let (status, stdout, stderr) =
            run_project(runtime, &[("script.R", typed), ("ry.toml", config)]);
        assert_eq!(status, Some(0), "{runtime}: {stderr}");
        assert!(stdout.contains("a value"), "{runtime}: {stdout}");
    }
}

#[test]
fn the_console_holds_a_multiline_command_and_refuses_it_in_strict_mode() {
    let directory = tempfile::tempdir().expect("temp dir");
    fs::write(
        directory.path().join("ry.toml"),
        "[check]\ntyping = true\nstrict = true\n",
    )
    .expect("write config");
    for runtime in runtimes() {
        let mut session =
            ReplSession::start_with(Some(directory.path()), &["--no-load", "--runtime", runtime]);
        session.expect("console");
        // Input typed before the editor takes the terminal would arrive as one paste.
        for line in [
            "f <- function(x: integer) {\r",
            "x * 2L\r",
            "}\r",
            "f(21L)\r",
        ] {
            session.settle(Duration::from_millis(400));
            session.send(line);
        }
        session.expect("[1] 42");
        session.send("g <- function(y) cat(\"hi\\n\")\r");
        session.expect("strict mode found 1 error");
        session.send("exists(\"g\")\r");
        session.expect("[1] FALSE");
        session.quit();
    }
}

/// Each query's type as the checker infers it, which must also be its schema at runtime.
const FRAME_TYPES: &[(&str, &str)] = &[
    ("sales", "frame{team: character[], amount: double[], units: integer[]}"),
    ("sales[amount > 1]", "frame{team: character[], amount: double[], units: integer[]}"),
    (
        "sales[, .(total = sum(amount), n = .N), by = team]",
        "frame{team: character[], total: double[], n: integer[]}",
    ),
    ("sales[, .(team, double = units * 2)]", "frame{team: character[], double: double[]}"),
    (
        "sales[labels, on = \"team\"]",
        "frame{team: character[], amount: double[], units: integer[], label: character[]}",
    ),
];

#[test]
fn frame_types_agree_between_the_checker_and_the_runtime() {
    if !ry_runtime_available() {
        return;
    }
    let mut script = String::from(
        "sales <- frame(team = c(\"a\", \"b\", \"a\"), amount = c(1, 2, 3), units = c(4L, 5L, 6L))\n\
         labels <- frame(team = c(\"a\", \"b\"), label = c(\"A\", \"B\"))\n",
    );
    for (index, (expression, ty)) in FRAME_TYPES.iter().enumerate() {
        script.push_str(&format!("q{index}: {ty} <- {expression}\nprint(frame_type(q{index}))\n"));
    }
    let directory = tempfile::tempdir().expect("temp dir");
    fs::write(directory.path().join("script.R"), &script).expect("write the script");
    let check = std::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .args(["check", "script.R"])
        .current_dir(directory.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run the checker");
    assert!(
        check.status.success(),
        "the checker must infer every annotated type:\n{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let (status, stdout, stderr) = run_project("ry", &[("script.R", script.as_str())]);
    assert_eq!(status, Some(0), "{stderr}");
    for (_, ty) in FRAME_TYPES {
        assert!(stdout.contains(&format!("[1] \"{ty}\"")), "the runtime schema differs from `{ty}`:\n{stdout}");
    }
}
