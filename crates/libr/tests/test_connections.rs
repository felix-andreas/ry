//! Connections: the R functions over ry's built-in kinds and over custom connections a package
//! creates with `R_new_custom_connection`, the C readers and writers packages use, and the
//! struct layout. Behavior is compared against GNU R through `connection_scenarios`.

mod connection_scenarios;
mod harness;

use {
    connection_scenarios::*,
    harness::*,
    std::{ffi::c_int, path::PathBuf, sync::OnceLock},
    syntax::testing::{SplitMix64, fuzz_iterations},
};

fn ryconn(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("ryconn", Headers::Ry));
    r.load(library);
}

fn connection_count(r: &R) -> c_int {
    unsafe {
        let count = *r
            .api()
            .library
            .get::<unsafe extern "C" fn() -> c_int>(b"ry_connection_count\0")
            .expect("ry_connection_count");
        count()
    }
}

fn logical(r: &R, value: bool) -> Sexp {
    unsafe {
        let scalar = *r
            .api()
            .library
            .get::<unsafe extern "C" fn(c_int) -> Sexp>(b"Rf_ScalarLogical\0")
            .expect("Rf_ScalarLogical");
        r.keep(scalar(value as c_int))
    }
}

#[test]
fn scenarios_print_what_gnu_r_prints() {
    let r = r_with_base();
    ryconn(&r);
    let expected = std::fs::read_to_string(expectation_path()).expect("the recorded GNU R output");
    let actual = run_in_ry(&r, "ry");
    assert_same_transcript(&expected, &actual, "ry against the recorded GNU R output");
    assert_eq!(
        connection_count(&r),
        3,
        "every scenario connection was closed"
    );
}

/// The same scenarios with a collection at every allocation, which finds any object the
/// connection code allocates while holding another one unrooted.
#[test]
fn scenarios_survive_collection_stress() {
    let r = r_with_base();
    ryconn(&r);
    let expected = std::fs::read_to_string(expectation_path()).expect("the recorded GNU R output");
    r.stress(true);
    let actual = run_in_ry(&r, "ry-stress");
    r.stress(false);
    assert_same_transcript(&expected, &actual, "ry under collection stress");
}

/// Re-runs the scenarios in GNU R and checks the recorded output is current. With
/// `RY_BLESS=1` it rewrites the recording instead.
#[test]
fn recorded_gnu_r_output_is_current() {
    if !gnu_r_available() {
        return;
    }
    let library = build_package("ryconn", Headers::GnuR);
    let actual = run_in_gnu_r(&library);
    if std::env::var_os("RY_BLESS").is_some() {
        std::fs::write(expectation_path(), &actual).expect("write the recording");
        return;
    }
    let expected = std::fs::read_to_string(expectation_path()).expect("the recorded GNU R output");
    assert_same_transcript(&expected, &actual, "GNU R against its recording");
}

/// The struct layout seen through ry's header, compared with the layout GNU R's header gives,
/// which `layout.expected` records.
#[test]
fn the_header_lays_out_rconn_like_gnu_r() {
    let probe = package_sources("ryconn")
        .parent()
        .expect("the package directory")
        .join("layout_probe.c");
    let expected = std::fs::read_to_string(probe.with_file_name("layout.expected"))
        .expect("the recorded layout");
    let run_probe = |include: &std::path::Path, tag: &str| {
        let binary =
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("layout-probe-{tag}"));
        let built = std::process::Command::new("cc")
            .arg("-I")
            .arg(include)
            .arg(&probe)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("run cc");
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let output = std::process::Command::new(&binary)
            .output()
            .expect("run the probe");
        String::from_utf8(output.stdout).expect("the probe prints text")
    };
    let ry_include = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    assert_eq!(run_probe(&ry_include, "ry"), expected);
    let gnu_include = std::path::Path::new("/usr/share/R/include");
    if gnu_r_available() && gnu_include.join("R_ext/Connections.h").exists() {
        assert_eq!(run_probe(gnu_include, "gnu"), expected);
    }
}

#[test]
fn closing_destroys_a_custom_connection_once() {
    let r = r_with_base();
    ryconn(&r);
    let destroyed =
        |r: &R| r.integers(r.call("ryconn_destroyed", &[]).expect("ryconn_destroyed"))[0];
    let before = destroyed(&r);
    let contents = r.keep(unsafe { (r.api().Rf_allocVector)(RAWSXP, 0) });
    let con = r
        .call(
            "ryconn_new",
            &[
                r.string("mem"),
                r.string("w"),
                contents,
                r.integer(0),
                r.integer(0),
            ],
        )
        .expect("ryconn_new");
    assert_eq!(connection_count(&r), 4);
    r.eval("close", &[(None, con)]).expect("close");
    assert_eq!(destroyed(&r), before + 1);
    assert_eq!(connection_count(&r), 3);
    let error = r.eval("close", &[(None, con)]).expect_err("closing twice");
    assert_eq!(error_message(&error), "invalid connection");
    assert_eq!(destroyed(&r), before + 1);
}

#[test]
fn an_unreachable_connection_is_closed_with_a_warning() {
    let r = r_with_base();
    let path = temporary_file("unreachable.txt");
    let path_value = r.string(&path.to_string_lossy());
    let mark = r.protected.get();
    let con = r
        .eval("file", &[(None, path_value), (None, r.string("w"))])
        .expect("file");
    r.eval("writeLines", &[(None, r.string("kept")), (None, con)])
        .expect("writeLines");
    r.release_to(mark);
    let before = connection_count(&r);
    unsafe { (r.api().ry_collect)(1) };
    // Finalizers run when the next native call returns.
    r.eval("getAllConnections", &[]).expect("getAllConnections");
    assert_eq!(connection_count(&r), before - 1);
    let warnings = r.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("closing unused connection 3 (")),
        "{warnings:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("the file"), "kept\n");
}

#[test]
fn standard_output_goes_to_the_console() {
    let r = r_with_base();
    let stdout = r.eval("stdout", &[]).expect("stdout");
    r.eval(
        "writeLines",
        &[(None, r.string("to the console")), (None, stdout)],
    )
    .expect("writeLines");
    let stderr = r.eval("stderr", &[]).expect("stderr");
    r.eval(
        "writeLines",
        &[(None, r.string("and to errors")), (None, stderr)],
    )
    .expect("writeLines");
    let console = CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(
        String::from_utf8_lossy(&console),
        "to the console\nand to errors\n"
    );
}

#[test]
fn a_pipe_writes_to_a_command() {
    let r = r_with_base();
    let path = temporary_file("pipe-output.txt");
    let command = r.string(&format!("cat > '{}'", path.display()));
    let con = r
        .eval("pipe", &[(None, command), (None, r.string("w"))])
        .expect("pipe");
    r.eval(
        "writeLines",
        &[(None, r.string("through a pipe")), (None, con)],
    )
    .expect("writeLines");
    let status = r.eval("close", &[(None, con)]).expect("close");
    assert_eq!(r.integers(status), vec![0]);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file"),
        "through a pipe\n"
    );
}

#[test]
fn large_files_round_trip_through_lines_and_bytes() {
    let r = r_with_base();
    ryconn(&r);
    let path = temporary_file("large.txt");
    let lines: Vec<String> = (0..20_000)
        .map(|index| format!("line {index} {}", "x".repeat(index % 97)))
        .collect();
    let text = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, lines.len() as isize) });
    for (index, line) in lines.iter().enumerate() {
        unsafe {
            let element = (r.api().Rf_mkCharLenCE)(line.as_ptr().cast(), line.len() as c_int, 0);
            (r.api().SET_STRING_ELT)(text, index as isize, element);
        }
    }
    let path_value = r.string(&path.to_string_lossy());
    r.eval("writeLines", &[(None, text), (None, path_value)])
        .expect("writeLines");
    let read = r
        .eval("readLines", &[(None, path_value)])
        .expect("readLines");
    assert_eq!(r.strings(read).len(), lines.len());
    assert!(
        r.strings(read)
            .iter()
            .zip(&lines)
            .all(|(got, want)| got.as_deref() == Some(want.as_str()))
    );
    let con = r
        .eval("file", &[(None, path_value), (None, r.string("rb"))])
        .expect("file");
    let all = r
        .call("ryconn_read_all", &[con, r.integer(4093)])
        .expect("read all");
    let expected: String = lines.iter().map(|line| format!("{line}\n")).collect();
    assert_eq!(
        unsafe { std::slice::from_raw_parts((r.api().RAW)(all), r.length(all)) },
        expected.as_bytes()
    );
    r.eval("close", &[(None, con)]).expect("close");
}

/// What a connection should hold: bytes and a read position, with pushed-back lines that text
/// reads see first and byte reads skip.
struct Model {
    bytes: Vec<u8>,
    position: usize,
    pushed: Vec<Vec<u8>>,
    /// Whether an unfinished last line is pushed back instead of returned, as a text-mode,
    /// non-blocking connection does.
    keeps_partial_lines: bool,
}

impl Model {
    fn read_bytes(&mut self, count: usize) -> Vec<u8> {
        let end = (self.position + count).min(self.bytes.len());
        let out = self.bytes[self.position..end].to_vec();
        self.position = end;
        out
    }

    /// The lines one `readLines(con, n = 1)` returns.
    fn read_line(&mut self) -> Vec<Vec<u8>> {
        if let Some(line) = self.pushed.pop() {
            if let Some(text) = line.strip_suffix(b"\n") {
                return vec![text.to_vec()];
            }
            self.pushed.push(line);
            return Vec::new();
        }
        let rest = &self.bytes[self.position..];
        if rest.is_empty() {
            return Vec::new();
        }
        match rest.iter().position(|&byte| byte == b'\n') {
            Some(end) => {
                let line = rest[..end].to_vec();
                self.position += end + 1;
                vec![line]
            }
            None => {
                let line = rest.to_vec();
                self.position = self.bytes.len();
                if self.keeps_partial_lines {
                    self.pushed.push(line);
                    Vec::new()
                } else {
                    vec![line]
                }
            }
        }
    }
}

/// Random byte reads, line reads, pushback and seeks against the model, on a raw connection, a
/// file, and a custom connection whose reads come back short. Every connection is closed at the
/// end, so the table must be back to the three standard slots.
#[test]
fn random_reads_and_pushback_follow_a_byte_model() {
    let r = r_with_base();
    ryconn(&r);
    let mut rng = SplitMix64(0x5eed_c0de);
    for iteration in 0..fuzz_iterations(60) {
        let mark = r.protected.get();
        let length = rng.below(200);
        let bytes: Vec<u8> = (0..length)
            .map(|_| {
                if rng.chance(1, 8) {
                    b'\n'
                } else {
                    b'a' + rng.below(26) as u8
                }
            })
            .collect();
        let raw = r.keep(unsafe { (r.api().Rf_allocVector)(RAWSXP, bytes.len() as isize) });
        if !bytes.is_empty() {
            unsafe { std::slice::from_raw_parts_mut((r.api().RAW)(raw), bytes.len()) }
                .copy_from_slice(&bytes);
        }
        let kind = iteration % 3;
        let blocking = rng.chance(1, 2);
        let con = match kind {
            0 => r
                .eval("rawConnection", &[(None, raw)])
                .expect("rawConnection"),
            1 => {
                let path = temporary_file(&format!("fuzz-{iteration}.bin"));
                std::fs::write(&path, &bytes).expect("write");
                let path_value = r.string(&path.to_string_lossy());
                r.eval("file", &[(None, path_value), (None, r.string("rb"))])
                    .expect("file")
            }
            _ => {
                let chunk = r.integer(1 + rng.below(5) as c_int);
                let con = r
                    .call(
                        "ryconn_new",
                        &[r.string("fuzz"), r.string("r"), raw, chunk, r.integer(0)],
                    )
                    .expect("ryconn_new");
                r.eval(
                    "open",
                    &[
                        (None, con),
                        (None, r.string("r")),
                        (None, logical(&r, blocking)),
                    ],
                )
                .expect("open");
                con
            }
        };
        let mut model = Model {
            bytes: bytes.clone(),
            position: 0,
            pushed: Vec::new(),
            keeps_partial_lines: kind == 2 && !blocking,
        };
        for _ in 0..rng.below(25) {
            match rng.below(5) {
                0 => {
                    let count = rng.below(17);
                    let got = r
                        .call("ryconn_read", &[con, r.integer(count as c_int)])
                        .expect("read");
                    let got =
                        unsafe { std::slice::from_raw_parts((r.api().RAW)(got), r.length(got)) }
                            .to_vec();
                    let available = model.bytes.len() - model.position;
                    assert!(
                        got.len() <= count && (!got.is_empty() || count == 0 || available == 0),
                        "iteration {iteration}: read {} of {count} with {available} left",
                        got.len()
                    );
                    if kind != 2 {
                        assert_eq!(got.len(), count.min(available));
                    }
                    assert_eq!(got, model.read_bytes(got.len()), "iteration {iteration}");
                }
                1 => {
                    let got = r
                        .eval("readLines", &[(None, con), (Some("n"), r.integer(1))])
                        .expect("readLines");
                    let want: Vec<Option<String>> = model
                        .read_line()
                        .into_iter()
                        .map(|line| Some(String::from_utf8(line).expect("ascii")))
                        .collect();
                    assert_eq!(r.strings(got), want, "iteration {iteration}: line read");
                }
                2 => {
                    let line: Vec<u8> = (0..rng.below(5))
                        .map(|_| b'A' + rng.below(26) as u8)
                        .collect();
                    let text = r.string(&String::from_utf8(line.clone()).expect("ascii"));
                    r.eval("pushBack", &[(None, text), (None, con)])
                        .expect("pushBack");
                    model.pushed.push([line, b"\n".to_vec()].concat());
                }
                3 if kind != 2 => {
                    let target = rng.below(bytes.len() + 1);
                    r.eval("seek", &[(None, con), (None, r.real(target as f64))])
                        .expect("seek");
                    model.position = target;
                    model.pushed.clear();
                }
                _ => {
                    let count = r
                        .eval("pushBackLength", &[(None, con)])
                        .expect("pushBackLength");
                    assert_eq!(r.integers(count), vec![model.pushed.len() as c_int]);
                }
            }
        }
        r.eval("close", &[(None, con)]).expect("close");
        r.release_to(mark);
        assert_eq!(
            connection_count(&r),
            3,
            "iteration {iteration}: a slot leaked"
        );
    }
}
