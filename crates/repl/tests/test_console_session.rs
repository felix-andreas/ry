//! A seeded session driver: each seed builds a random console session (assignments, printing,
//! `invisible()`, warnings, errors, syntax errors, commands continued over several lines,
//! `readline()` answers, `browser()`, lines longer than R's input buffer) and pipes it to the
//! console running GNU R, then to GNU R's own console. The console must be transparent: both
//! must print the same output and the same errors, once GNU R's echo of each input line is
//! removed. Skips without GNU R.
//!
//! R must own the main thread, so this file has no test harness: its `main` drives the
//! sessions, and runs the console itself when the driver starts it as a child.

use {
    repl::RunOptions,
    std::{
        env,
        fmt::Write as _,
        io::Write as _,
        path::Path,
        process::{Command, ExitCode, Stdio},
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

const CHILD: &str = "RY_CONSOLE_SESSION_CHILD";

fn main() -> ExitCode {
    if env::var_os(CHILD).is_some() {
        return match repl::run(RunOptions::default()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(2)
            }
        };
    }
    if !gnu_r_available() {
        eprintln!("skipped: no GNU R installation on this machine");
        return ExitCode::SUCCESS;
    }
    // Pinned: the console showed no prompt for `readline()` on a pipe, which GNU R shows.
    let readline = "name <- readline(\"Name: \")\nBob\nprint(name)\n";
    let console = run_console(readline);
    assert_eq!(console.stdout, "Name: [1] \"Bob\"\n", "{}", console.stderr);
    check_session(&Session {
        console: readline.to_owned(),
        reference: readline.to_owned(),
    });
    for_each_seed(6, |seed| {
        check_session(&random_session(&mut SplitMix64(seed)))
    });
    println!("console sessions agree with GNU R");
    ExitCode::SUCCESS
}

/// The same commands for each console. They differ only where GNU R's console cannot read a
/// line longer than its 4096-byte buffer, which ry's console reads whole: there GNU R gets a
/// short command that prints the same value.
struct Session {
    console: String,
    reference: String,
}

fn check_session(session: &Session) {
    let console = run_console(&session.console);
    let reference = run_gnu_r(&session.reference);
    let expected_output = remove_echo(&reference.stdout, &session.reference);
    assert!(
        console.stdout == expected_output && console.stderr == reference.stderr,
        "the console and GNU R disagree on this session:\n{}\n\
         console stdout:\n{}\nGNU R stdout (echo removed):\n{}\n\
         console stderr:\n{}\nGNU R stderr:\n{}",
        session.reference,
        console.stdout,
        expected_output,
        console.stderr,
        reference.stderr,
    );
    assert!(console.success, "the console failed:\n{}", console.stderr);
}

/// Commands chosen so no output line can be mistaken for an input line, which keeps removing
/// GNU R's echo exact: printed values start with `[1]`, `cat()` prints `out` and a number.
fn random_session(rng: &mut SplitMix64) -> Session {
    let mut console = String::new();
    let mut reference = String::new();
    let mut defined: Vec<usize> = Vec::new();
    for number in 1..=(8 + rng.below(16)) {
        let command = match rng.below(17) {
            0 => {
                defined.push(number);
                format!("x{number} <- {number}L\n")
            }
            1 => match defined.get(rng.below(defined.len().max(1))) {
                Some(variable) => format!("x{variable}\n"),
                None => format!("{number} * 2\n"),
            },
            2 => format!("invisible({number})\n"),
            3 => format!("warning(\"w{number}\")\n"),
            4 => format!("stop(\"e{number}\")\n"),
            5 => format!("f{number} <- function(a) {{\n  a +\n    {number}\n}}\nf{number}(1)\n"),
            6 => format!("{number} +\n  {number}\n"),
            7 => format!("{number} +* 2\n"),
            8 => format!("message(\"m{number}\")\n"),
            9 => format!("cat(\"out{number}\\n\")\n"),
            10 => format!(
                "y{number} <- readline(\"ask{number}: \")\nanswer{number}\nprint(y{number})\n"
            ),
            11 => "browser()\nc\n".to_owned(),
            // Longer than R's 4096-byte console buffer, so R reads it in pieces.
            12 => {
                let length = 4100 + number;
                console.push_str(&format!("nchar(\"{}\")\n", "a".repeat(length)));
                reference.push_str(&format!("nchar(strrep(\"a\", {length}))\n"));
                continue;
            }
            13 => format!("a{number} <- 1; a{number} + {number}\n"),
            14 => format!("g{number} <- function() warning(\"inner{number}\")\ng{number}()\n"),
            15 => format!("s{number} <- \"first{number}\nsecond{number}\"\nnchar(s{number})\n"),
            _ => match rng.below(3) {
                0 => "\n".to_owned(),
                1 => "# a comment\n".to_owned(),
                _ => format!(
                    "tryCatch(stop(\"t{number}\"), error = function(e) cat(\"out{number}\\n\"))\n"
                ),
            },
        };
        console.push_str(&command);
        reference.push_str(&command);
    }
    Session { console, reference }
}

struct Transcript {
    stdout: String,
    stderr: String,
    success: bool,
}

fn run_console(input: &str) -> Transcript {
    let executable = env::current_exe().expect("the test executable");
    let mut command = Command::new(executable);
    command
        .env(CHILD, "1")
        // The console keeps its history under the data directory.
        .env("XDG_DATA_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")))
        .env("NO_COLOR", "1");
    run(command, input)
}

fn run_gnu_r(input: &str) -> Transcript {
    let mut command = Command::new("R");
    command.args([
        "--interactive",
        "--no-echo",
        "--no-readline",
        "--no-save",
        "--no-restore-data",
    ]);
    run(command, input)
}

fn run(mut command: Command, input: &str) -> Transcript {
    let mut child = command
        .env("R_PROFILE_USER", "/dev/null")
        .env("R_ENVIRON_USER", "/dev/null")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the session");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let bytes = input.as_bytes().to_vec();
    // A session that stops reading must not block the writer, and so the driver.
    let writer = std::thread::spawn(move || stdin.write_all(&bytes).ok());
    let output = child.wait_with_output().expect("the session ends");
    writer.join().ok();
    Transcript {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    }
}

/// GNU R's console echoes each line it reads from a pipe; removes those echoes in order. An echo
/// starts a line, or follows the prompt of `readline()`, which ends in `: `.
fn remove_echo(output: &str, input: &str) -> String {
    let mut rest = output;
    let mut kept = String::new();
    for line in input.lines() {
        let echo = format!("{line}\n");
        let found = rest
            .match_indices(&echo)
            .map(|(position, _)| position)
            .find(|&position| {
                let before = &rest[..position];
                before.is_empty() || before.ends_with('\n') || before.ends_with(": ")
            });
        match found {
            Some(position) => {
                kept.push_str(&rest[..position]);
                rest = &rest[position + echo.len()..];
            }
            None => {
                writeln!(kept, "<GNU R did not echo {line:?}>").ok();
            }
        }
    }
    kept.push_str(rest);
    kept
}

fn gnu_r_available() -> bool {
    Command::new("R")
        .arg("RHOME")
        .output()
        .is_ok_and(|output| output.status.success())
}
