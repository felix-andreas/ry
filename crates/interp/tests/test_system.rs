//! `.Internal(system())` in base R's Windows calling convention, which `system()` and `system2()`
//! use there and which `R CMD build` depends on through `tools:::.system_with_capture`: output
//! redirected to files, captured into the result, read from an input file, and the statuses of a
//! failing, missing or timed-out command. The convention is chosen by the argument count, so it
//! runs on every platform; only how the command line reaches the program differs on Windows.
//!
//! The interpreter runs here without base R, so the cases call the internals directly. The
//! interpreter is global to the process, so everything runs as one test.

use {
    heap::{Heap, object::Sexp},
    interp::{eval, lower, unwind::Unwind, values},
    std::{fs, path::Path},
};

/// Each case's code and the deparsed value it must give. `DIR` stands for a scratch directory
/// holding `script.sh`, which prints `out` to stdout, then `err` to stderr, and exits with 3, and
/// `input.txt`, which holds two lines.
const CASES: &[(&str, &str)] = &[
    // `system2(stdout = f, stderr = f)`: both streams go to one file, in the order written.
    (
        r#"c(.Internal(system("sh DIR/script.sh", 21L, "", "DIR/both", "DIR/both", 0L, TRUE)), readLines("DIR/both"))"#,
        r#"c("3", "out", "err")"#,
    ),
    // `tools:::.system_with_capture`: separate files, created even when nothing is written.
    (
        r#"c(.Internal(system("sh DIR/script.sh", 21L, "", "DIR/out", "DIR/err", 0L, TRUE)), readLines("DIR/out"), readLines("DIR/err"))"#,
        r#"c("3", "out", "err")"#,
    ),
    (
        r#".Internal(system("true", 21L, "", "DIR/empty", "DIR/empty", 0L, TRUE)) + length(readLines("DIR/empty"))"#,
        "0",
    ),
    // `system(intern = TRUE)`: stdout captured, the status attached.
    (
        r#".Internal(system("sh DIR/script.sh", 23L, "", TRUE, FALSE, 0L, TRUE))"#,
        r#"structure("out", status = 3L)"#,
    ),
    // Both streams captured share one pipe.
    (
        r#".Internal(system("sh DIR/script.sh", 23L, "", TRUE, TRUE, 0L, TRUE))"#,
        r#"structure(c("out", "err"), status = 3L)"#,
    ),
    // `system2(input = ...)` and `stdin = file`: input read from the file.
    (
        r#".Internal(system("cat", 23L, "DIR/input.txt", TRUE, FALSE, 0L, TRUE))"#,
        r#"c("first", "second")"#,
    ),
    // `ignore.stdout = TRUE`.
    (
        r#".Internal(system("sh DIR/script.sh", 21L, "", FALSE, FALSE, 0L, TRUE))"#,
        "3L",
    ),
    // The program is the first word of the line, which may be quoted, and nothing in the rest is
    // read by a shell.
    (
        r#".Internal(system("\"sh\" DIR/script.sh", 23L, "", TRUE, FALSE, 0L, TRUE))"#,
        r#"structure("out", status = 3L)"#,
    ),
    // A command that cannot be found has GNU R's status 127 and a warning, or is an error when
    // its output is wanted.
    (
        r#".Internal(system("ry-no-such-program x", 21L, "", "", "", 0L, TRUE))"#,
        "127L",
    ),
    (
        r#".Internal(system("ry-no-such-program x", 23L, "", TRUE, FALSE, 0L, TRUE))"#,
        "'ry-no-such-program' not found",
    ),
    (
        r#".Internal(system("\"sh DIR/script.sh", 21L, "", "", "", 0L, TRUE))"#,
        "127L",
    ),
    // A command still running at its timeout is stopped and has status 124.
    (
        r#".Internal(system("sleep 30", 21L, "", "", "", 1L, TRUE))"#,
        "124L",
    ),
    // The argument checks of GNU R's `do_system`.
    (
        r#".Internal(system("true", 23L, "", FALSE, FALSE, 0L, TRUE))"#,
        "invalid flag argument",
    ),
    (
        r#".Internal(system("true", 20L, "", "", "", 5L, TRUE))"#,
        "Timeout with background running processes is not supported.",
    ),
    (
        r#".Internal(system("true", 21L, NULL, "", "", 0L, TRUE))"#,
        "character string expected as third argument",
    ),
];

const SETUP: &str = r#"
readLines <- function(path) .Internal(readLines(.Internal(file(path, "r", TRUE, "", "", FALSE)), -1L, TRUE, FALSE, "unknown", FALSE))
length <- function(x) .Primitive("length")(x)
deparse <- function(expr) .Internal(deparse(expr, 500L, TRUE, 1093L, -1L))
"#;

#[test]
fn windows_system_redirects_captures_and_reports_status() {
    let scratch = std::env::temp_dir().join(format!("ry-test-system-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create the scratch directory");
    fs::write(
        scratch.join("script.sh"),
        "echo out\necho err >&2\nexit 3\n",
    )
    .expect("write the script");
    fs::write(scratch.join("input.txt"), "first\r\nsecond\n").expect("write the input");
    let directory = scratch.to_string_lossy().replace('\\', "/");
    // SAFETY: the heap is leaked, so it outlives the interpreter, and this is the only test in
    // its process that uses the interpreter.
    let failures = unsafe {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        interp::install(heap, interp::default_hooks());
        interp::interp().show_error_messages = false;
        evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
        let mut failures = Vec::new();
        for (code, expected) in CASES {
            let code = code.replace("DIR", &directory);
            let shown = evaluate(&format!("deparse({code})")).map(|value| {
                values::string_at(value, 0).map_or_else(String::new, |text| text.into_owned())
            });
            let shown = match shown {
                Ok(text) | Err(text) => text,
            };
            if !shown.contains(expected) {
                failures.push(format!(
                    "`{code}`\n  gave     {shown:?}\n  expected {expected:?}"
                ));
            }
        }
        failures
    };
    remove_scratch(&scratch);
    assert!(
        failures.is_empty(),
        "{} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn remove_scratch(scratch: &Path) {
    fs::remove_dir_all(scratch).expect("remove the scratch directory");
}

/// Evaluates `text` in the global environment and returns the last value, or the error message.
unsafe fn evaluate(text: &str) -> Result<Sexp, String> {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let expressions = lower::parse_text(text, "<test>").map_err(message)?;
        let mut value = values::nil();
        for index in 0..values::length(expressions) {
            interp::interp().error_message.clear();
            value = eval::eval(expressions.element(index), values::global_env()).map_err(
                |failure| format!("{}{}", message(failure), interp::interp().error_message),
            )?;
        }
        Ok(value)
    }
}

fn message(unwind: Unwind) -> String {
    match unwind {
        Unwind::Error(failure) => failure.message,
        _ => String::new(),
    }
}
