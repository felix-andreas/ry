//! The serialization stream API as native code uses it: `serialtest` writes and reads through
//! `R_InitOutPStream`/`R_InitInPStream` callbacks over memory and through the `FILE` streams,
//! with persistence hooks, and the stream structs have GNU R's layout.

mod harness;

use {
    harness::*,
    std::{
        ffi::c_int,
        path::{Path, PathBuf},
        process::Command,
        sync::OnceLock,
    },
};

fn serialtest(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("serialtest", Headers::Ry));
    r.load(library);
}

fn raw(r: &R, bytes: &[u8]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(RAWSXP, bytes.len() as isize) });
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (r.api().RAW)(vector), bytes.len());
    }
    vector
}

fn bytes(r: &R, vector: Sexp) -> Vec<u8> {
    assert_eq!(r.type_of(vector), RAWSXP);
    unsafe { std::slice::from_raw_parts((r.api().RAW)(vector), r.length(vector)).to_vec() }
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

/// The writer version a stream's header records, as `R_Version` encodes it: the version of the
/// base R the session runs, as in GNU R.
fn session_writer_version() -> i32 {
    interp::system::r_version().code()
}

/// `stream` with the writer version in its header replaced by the session's, so a stream
/// recorded by one GNU R release is what another release writes.
fn with_session_writer_version(stream: &[u8]) -> Vec<u8> {
    let writer = session_writer_version();
    let mut output = stream.to_vec();
    match stream.first() {
        Some(b'X') if stream.len() >= 10 => output[6..10].copy_from_slice(&writer.to_be_bytes()),
        Some(b'B') if stream.len() >= 10 => output[6..10].copy_from_slice(&writer.to_le_bytes()),
        Some(b'A') => {
            let lines: Vec<&[u8]> = stream.splitn(4, |byte| *byte == b'\n').collect();
            if let [format, version, _, rest] = lines[..] {
                output = [format, version, writer.to_string().as_bytes(), rest].join(&b'\n');
            }
        }
        _ => {}
    }
    output
}

fn format_code(name: &str) -> c_int {
    match name {
        "ascii" => 1,
        "binary" => 2,
        "xdr" => 3,
        "asciihex" => 4,
        other => panic!("unknown format {other}"),
    }
}

/// Every recorded GNU R stream reads through `R_Unserialize` and writes back through
/// `R_Serialize` as the stream ry must write, under collection stress for a sample.
#[test]
fn recorded_streams_round_trip_through_native_streams() {
    let r = r();
    serialtest(&r);
    let cases = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/serialize/cases.tsv"),
    )
    .expect("cases.tsv");
    let mut failures = Vec::new();
    for (line_number, line) in cases.lines().enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (name, format, version) = (fields[0], fields[1], fields[2]);
        if fields[4] == "-" {
            continue;
        }
        let stream = unhex(fields[3]);
        // Base R's ALTREP objects survive a version-3 round trip as themselves.
        let expected = with_session_writer_version(&if fields[4].is_empty()
            || (version == "3" && name.starts_with("altrep"))
        {
            stream.clone()
        } else {
            unhex(fields[4])
        });
        r.stress(line_number % 10 == 0);
        let mark = r.protected.get();
        let input = raw(&r, &stream);
        let format_value = r.integer(format_code(format));
        let any = r.integer(0);
        let result = r
            .call("serialtest_unserialize", &[input, any])
            .and_then(|object| {
                let version = r.integer(version.parse().expect("a version"));
                // Quoted, so `.Call` passes a symbol or call as itself instead of its value.
                let object = r.quote(object);
                r.call("serialtest_serialize", &[object, format_value, version])
            });
        r.stress(false);
        match result {
            Ok(written) if bytes(&r, written) == expected => {}
            Ok(_) => failures.push(format!("{name} {format} v{version}: different bytes")),
            Err(error) => failures.push(format!("{name} {format} v{version}: {error}")),
        }
        r.release_to(mark);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn input_streams_report_format_and_encoding() {
    let r = r();
    serialtest(&r);
    let value = r.real(2.5);
    for (format, expected) in [(3, "3"), (2, "2"), (1, "1")] {
        let format = r.integer(format);
        let version = r.integer(3);
        let written = r
            .call("serialtest_serialize", &[value, format, version])
            .expect("serialize");
        let info = r
            .call("serialtest_stream_info", &[written])
            .expect("stream info");
        assert_eq!(
            r.strings(info),
            vec![Some(expected.into()), Some("UTF-8".into())]
        );
    }
    let ascii = r.integer(1);
    let version = r.integer(2);
    let written = r
        .call("serialtest_serialize", &[value, ascii, version])
        .expect("serialize");
    let header = format!("A\n2\n{}\n131840\n", session_writer_version());
    assert!(bytes(&r, written).starts_with(header.as_bytes()));
    let xdr = r.integer(3);
    let error = r
        .call("serialtest_unserialize", &[written, xdr])
        .expect_err("an ASCII stream is not XDR");
    assert!(
        error.contains("input format does not match specified format"),
        "{error}"
    );
}

#[test]
fn persistence_hooks_run_as_native_code() {
    let r = r();
    serialtest(&r);
    r.stress(true);
    let restored = r.call("serialtest_hooks", &[]).expect("hooks");
    r.stress(false);
    let counts = unsafe { (r.api().Rf_getAttrib)(restored, r.symbol("counts")) };
    assert_eq!(
        r.integers(counts),
        vec![3, 3],
        "every occurrence is named, as in GNU R"
    );
    let first = r.element(restored, 0);
    assert_eq!(r.type_of(first), EXTPTRSXP);
    assert_ne!(first, r.element(restored, 2));

    let pointer_list = r.call("serialtest_hooks", &[]).expect("hooks again");
    assert_eq!(r.length(pointer_list), 3);

    let error = r
        .call("serialtest_failing_hook", &[pointer_list])
        .expect_err("the hook raises an error");
    assert_eq!(error, "Error: the hook refused externalptr\n");
    assert_eq!(r.runtime_protect_depth(), 0);
}

#[test]
fn file_streams_round_trip() {
    let r = r();
    serialtest(&r);
    let path = temporary_file("serialtest.bin");
    let path_value = r.string(&path.to_string_lossy());
    let value = r.call_expression("f", &[(Some("x"), r.integer(7))]);
    let quoted = r.quote(value);
    for format in [1, 2, 3, 4] {
        let format = r.integer(format);
        let back = r
            .call("serialtest_file", &[quoted, path_value, format])
            .expect("a file round trip");
        let same = unsafe { (r.api().R_compute_identical)(value, back, 16) };
        assert_eq!(same, 1);
    }
    let truncated = raw(&r, b"X\n\0\0\0\x03");
    let any = r.integer(0);
    let error = r
        .call("serialtest_unserialize", &[truncated, any])
        .expect_err("a truncated stream");
    assert_eq!(
        error,
        "Error: serialtest: read past the end of the buffer\n"
    );
}

/// The stream structs and format codes have GNU R's layout, compared by compiling one probe
/// against each set of headers.
#[test]
fn stream_structs_match_gnu_r_layout() {
    if !gnu_r_available() {
        return;
    }
    let probe =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages/serialtest/probe/offsets.c");
    let ry_include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let gnu_include = Command::new("R")
        .args(["RHOME"])
        .output()
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("include"))
        .expect("R RHOME");
    let gnu_include = if gnu_include.join("Rinternals.h").exists() {
        gnu_include
    } else {
        PathBuf::from("/usr/share/R/include")
    };
    let layout = |include: &Path, name: &str| {
        let binary = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let status = Command::new("cc")
            .arg("-I")
            .arg(include)
            .arg(&probe)
            .arg("-o")
            .arg(&binary)
            .status()
            .expect("cc runs");
        assert!(status.success());
        let output = Command::new(&binary).output().expect("the probe runs");
        String::from_utf8(output.stdout).expect("text")
    };
    let ry = layout(&ry_include, "offsets-ry");
    let gnu = layout(&gnu_include, "offsets-gnu");
    assert!(ry.contains("R_inpstream_st.native_encoding"));
    assert_eq!(ry, gnu);
}
