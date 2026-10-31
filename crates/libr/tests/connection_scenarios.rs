//! Connection scenarios that run in GNU R and in ry and must print the same thing.
//!
//! Each scenario is a list of R calls written once, here, as data. The GNU R side renders them
//! as an R script, runs it with `Rscript`, and records the printed results in
//! `tests/packages/ryconn/gnu_r.expected`, so the comparison needs no R installation. The ry
//! side builds the same calls as language objects and evaluates them through the C API. Both
//! sides print a value the same canonical way, errors and warnings by their message only, and
//! file paths with the scratch directory replaced by `<dir>`.

#![allow(dead_code)]

use crate::harness::*;
use std::{
    ffi::{CStr, c_char, c_int},
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub enum Arg {
    Str(&'static str),
    Strs(&'static [&'static str]),
    Int(i32),
    Ints(&'static [i32]),
    Real(f64),
    Reals(&'static [f64]),
    Bool(bool),
    /// A logical vector; `None` is `NA`.
    Logicals(&'static [Option<bool>]),
    Raw(&'static [u8]),
    Var(&'static str),
    /// A file in the scenario's scratch directory.
    Path(&'static str),
    Null,
    Call(Call),
}

#[derive(Clone, Debug)]
pub struct Call {
    pub function: &'static str,
    pub arguments: Vec<(Option<&'static str>, Arg)>,
}

#[derive(Clone, Debug)]
pub enum Step {
    /// Evaluates a call and prints its result, binding it to a variable when named.
    Eval(Option<&'static str>, Call),
    /// Binds a variable without printing.
    Define(&'static str, Arg),
    /// Writes a file in the scratch directory without printing.
    WriteFile(&'static str, &'static [u8]),
}

pub struct Scenario {
    pub name: &'static str,
    pub steps: Vec<Step>,
}

pub fn call(function: &'static str, arguments: Vec<(Option<&'static str>, Arg)>) -> Call {
    Call {
        function,
        arguments,
    }
}

pub fn eval(function: &'static str, arguments: Vec<Arg>) -> Step {
    Step::Eval(
        None,
        call(
            function,
            arguments.into_iter().map(|arg| (None, arg)).collect(),
        ),
    )
}

pub fn eval_named(function: &'static str, arguments: Vec<(Option<&'static str>, Arg)>) -> Step {
    Step::Eval(None, call(function, arguments))
}

pub fn assign(name: &'static str, function: &'static str, arguments: Vec<Arg>) -> Step {
    Step::Eval(
        Some(name),
        call(
            function,
            arguments.into_iter().map(|arg| (None, arg)).collect(),
        ),
    )
}

pub fn dot_call(routine: &'static str, mut arguments: Vec<Arg>) -> Step {
    arguments.insert(0, Arg::Str(routine));
    eval(".Call", arguments)
}

pub fn assign_dot_call(name: &'static str, routine: &'static str, mut arguments: Vec<Arg>) -> Step {
    arguments.insert(0, Arg::Str(routine));
    assign(name, ".Call", arguments)
}

fn var(name: &'static str) -> Arg {
    Arg::Var(name)
}

fn nested(function: &'static str) -> Arg {
    Arg::Call(call(function, Vec::new()))
}

pub fn scenarios() -> Vec<Scenario> {
    use Arg::*;
    vec![
        Scenario {
            name: "file_reading",
            steps: vec![
                Step::WriteFile("a.txt", b"a\nb\n"),
                assign("con", "file", vec![Path("a.txt")]),
                eval("summary.connection", vec![var("con")]),
                eval("isOpen", vec![var("con")]),
                eval("readLines", vec![var("con")]),
                eval("isOpen", vec![var("con")]),
                eval("open", vec![var("con"), Str("r")]),
                eval("summary.connection", vec![var("con")]),
                eval("seek", vec![var("con")]),
                eval_named("readLines", vec![(None, var("con")), (Some("n"), Int(1))]),
                eval("seek", vec![var("con")]),
                eval("readLines", vec![var("con")]),
                eval("readLines", vec![var("con")]),
                eval("open", vec![var("con"), Str("r")]),
                eval("seek", vec![var("con"), Real(0.0)]),
                eval("readLines", vec![var("con")]),
                eval("close", vec![var("con")]),
                eval("readLines", vec![var("con")]),
                eval("summary.connection", vec![var("con")]),
            ],
        },
        Scenario {
            name: "file_writing",
            steps: vec![
                assign("con", "file", vec![Path("b.txt"), Str("w")]),
                eval_named(
                    "writeLines",
                    vec![
                        (None, Strs(&["x", "y"])),
                        (None, var("con")),
                        (Some("sep"), Str("|")),
                    ],
                ),
                eval("isOpen", vec![var("con"), Str("write")]),
                eval("isOpen", vec![var("con"), Str("read")]),
                eval("readLines", vec![var("con")]),
                eval("flush", vec![var("con")]),
                eval("close", vec![var("con")]),
                eval("readLines", vec![Path("b.txt")]),
                assign("con", "file", vec![Path("b.txt"), Str("a")]),
                eval("summary.connection", vec![var("con")]),
                eval("writeLines", vec![Str("z"), var("con")]),
                eval("close", vec![var("con")]),
                eval("readChar", vec![Path("b.txt"), Int(100)]),
                eval("writeLines", vec![Strs(&["one", "two"]), Path("c.txt")]),
                eval("readLines", vec![Path("c.txt")]),
                assign("con", "file", vec![Path("b.txt")]),
                eval("open", vec![var("con"), Str("r+")]),
                eval_named("readLines", vec![(None, var("con")), (Some("n"), Int(1))]),
                eval("writeLines", vec![Str("Q"), var("con")]),
                eval_named("seek", vec![(None, var("con")), (Some("rw"), Str("write"))]),
                eval_named("seek", vec![(None, var("con")), (Some("rw"), Str("read"))]),
                eval("close", vec![var("con")]),
                eval("readChar", vec![Path("b.txt"), Int(100)]),
                assign("con", "file", vec![Path("b.txt"), Str("r+")]),
                eval("seek", vec![var("con"), Real(2.0)]),
                eval("truncate", vec![var("con")]),
                eval("close", vec![var("con")]),
                eval("readChar", vec![Path("b.txt"), Int(100)]),
            ],
        },
        Scenario {
            name: "anonymous_file",
            steps: vec![
                assign("con", "file", vec![Str("")]),
                eval("summary.connection", vec![var("con")]),
                eval("writeLines", vec![Strs(&["zz", "yy"]), var("con")]),
                eval("seek", vec![var("con"), Real(0.0)]),
                eval("readLines", vec![var("con")]),
                eval("close", vec![var("con")]),
            ],
        },
        Scenario {
            name: "file_errors",
            steps: vec![
                eval("file", vec![Path("missing/x"), Str("r")]),
                eval("readLines", vec![Path("missing/x")]),
                assign("con", "file", vec![Path("missing/x")]),
                eval("open", vec![var("con"), Str("q")]),
                eval("summary.connection", vec![var("con")]),
                eval("readLines", vec![Int(5)]),
                eval("getConnection", vec![Int(99)]),
                eval("close", vec![nested("stdout")]),
                eval("seek", vec![var("con")]),
                eval("close", vec![var("con")]),
                eval("close", vec![var("con")]),
            ],
        },
        Scenario {
            name: "line_endings_and_nul",
            steps: vec![
                Step::WriteFile("d.txt", b"\r\nA\rB\r\nC"),
                eval("readLines", vec![Path("d.txt")]),
                eval_named(
                    "readLines",
                    vec![(None, Path("d.txt")), (Some("warn"), Bool(false))],
                ),
                eval_named(
                    "readLines",
                    vec![
                        (None, Path("d.txt")),
                        (Some("n"), Int(5)),
                        (Some("ok"), Bool(false)),
                    ],
                ),
                eval_named(
                    "readLines",
                    vec![(None, Path("d.txt")), (Some("n"), Int(2))],
                ),
                Step::WriteFile("e.txt", b"a\0b\nc\n"),
                eval("readLines", vec![Path("e.txt")]),
                eval_named(
                    "readLines",
                    vec![(None, Path("e.txt")), (Some("skipNul"), Bool(true))],
                ),
                Step::WriteFile("empty.txt", b""),
                eval("readLines", vec![Path("empty.txt")]),
            ],
        },
        Scenario {
            name: "push_back",
            steps: vec![
                Step::WriteFile("a.txt", b"a\nb\n"),
                assign("con", "file", vec![Path("a.txt"), Str("r")]),
                eval("pushBack", vec![Strs(&["one", "two"]), var("con")]),
                eval("pushBackLength", vec![var("con")]),
                eval_named("readLines", vec![(None, var("con")), (Some("n"), Int(1))]),
                eval("pushBackLength", vec![var("con")]),
                eval_named(
                    "pushBack",
                    vec![
                        (None, Str("pp")),
                        (None, var("con")),
                        (Some("newLine"), Bool(false)),
                    ],
                ),
                eval("readLines", vec![var("con")]),
                eval("pushBack", vec![Strs(&["gone"]), var("con")]),
                eval("clearPushBack", vec![var("con")]),
                eval("pushBackLength", vec![var("con")]),
                eval("readLines", vec![var("con")]),
                eval("close", vec![var("con")]),
            ],
        },
        Scenario {
            name: "raw_connections",
            steps: vec![
                Step::Define("bytes", Raw(&[1, 2, 3, 4, 5])),
                Step::Define("nothing", Raw(&[])),
                assign("r", "rawConnection", vec![var("bytes")]),
                eval("summary.connection", vec![var("r")]),
                eval("readBin", vec![var("r"), Str("raw"), Int(2)]),
                eval("seek", vec![var("r")]),
                eval_named(
                    "seek",
                    vec![
                        (None, var("r")),
                        (None, Real(-1.0)),
                        (Some("origin"), Str("end")),
                    ],
                ),
                eval("readBin", vec![var("r"), Str("raw"), Int(10)]),
                eval("seek", vec![var("r"), Real(9.0)]),
                eval("rawConnectionValue", vec![var("r")]),
                eval("writeBin", vec![Raw(&[1]), var("r")]),
                eval("close", vec![var("r")]),
                assign("w", "rawConnection", vec![var("nothing"), Str("w")]),
                eval("summary.connection", vec![var("w")]),
                eval("writeBin", vec![Reals(&[1.5, -2.0]), var("w")]),
                eval("writeBin", vec![Ints(&[1, 2]), var("w")]),
                eval("writeBin", vec![Logicals(&[Some(true), None]), var("w")]),
                eval("writeBin", vec![Strs(&["ab", "c"]), var("w")]),
                eval_named(
                    "writeBin",
                    vec![
                        (None, Int(1)),
                        (None, var("w")),
                        (Some("size"), Int(2)),
                        (Some("endian"), Str("big")),
                    ],
                ),
                assign("v", "rawConnectionValue", vec![var("w")]),
                eval("readLines", vec![var("w")]),
                eval("close", vec![var("w")]),
                assign("r", "rawConnection", vec![var("v")]),
                eval("readBin", vec![var("r"), Str("double"), Int(2)]),
                eval("readBin", vec![var("r"), Str("integer"), Int(2)]),
                eval("readBin", vec![var("r"), Str("logical"), Int(2)]),
                eval("readBin", vec![var("r"), Str("character"), Int(2)]),
                eval_named(
                    "readBin",
                    vec![
                        (None, var("r")),
                        (None, Str("integer")),
                        (None, Int(1)),
                        (Some("size"), Int(2)),
                        (Some("endian"), Str("big")),
                    ],
                ),
                eval("readBin", vec![var("r"), Str("integer"), Int(1)]),
                eval("close", vec![var("r")]),
                assign("a", "rawConnection", vec![var("bytes"), Str("a")]),
                eval("writeBin", vec![Raw(&[9]), var("a")]),
                eval("rawConnectionValue", vec![var("a")]),
                eval("seek", vec![var("a")]),
                eval("close", vec![var("a")]),
                Step::Define("signed", Raw(&[255, 255, 1, 2, 3, 4])),
                assign("s", "rawConnection", vec![var("signed")]),
                eval_named(
                    "readBin",
                    vec![
                        (None, var("s")),
                        (None, Str("integer")),
                        (None, Int(2)),
                        (Some("size"), Int(1)),
                        (Some("signed"), Bool(false)),
                    ],
                ),
                eval_named(
                    "readBin",
                    vec![
                        (None, var("s")),
                        (None, Str("int")),
                        (None, Int(5)),
                        (Some("size"), Int(1)),
                    ],
                ),
                eval("close", vec![var("s")]),
                eval_named(
                    "readBin",
                    vec![
                        (None, var("signed")),
                        (None, Str("integer")),
                        (None, Int(3)),
                        (Some("size"), Int(2)),
                    ],
                ),
                eval_named(
                    "readBin",
                    vec![
                        (None, var("signed")),
                        (None, Str("integer")),
                        (None, Int(1)),
                        (Some("size"), Int(3)),
                    ],
                ),
                eval("writeBin", vec![Ints(&[7, 8]), var("nothing")]),
                Step::Define("hello", Raw(b"hello world")),
                assign("h", "rawConnection", vec![var("hello")]),
                eval("readChar", vec![var("h"), Ints(&[2, 3, 100, 4])]),
                eval("close", vec![var("h")]),
                assign("h", "rawConnection", vec![var("hello")]),
                eval("readLines", vec![var("h")]),
                eval("close", vec![var("h")]),
            ],
        },
        Scenario {
            name: "text_connections",
            steps: vec![
                Step::Define("lines", Strs(&["x", "y", "z"])),
                assign("t", "textConnection", vec![var("lines")]),
                eval("summary.connection", vec![var("t")]),
                eval("seek", vec![var("t")]),
                eval("seek", vec![var("t"), Real(1.0)]),
                eval("writeLines", vec![Str("a"), var("t")]),
                eval_named("readLines", vec![(None, var("t")), (Some("n"), Int(2))]),
                eval("textConnectionValue", vec![var("t")]),
                eval("readLines", vec![var("t")]),
                eval("readLines", vec![var("t")]),
                eval("close", vec![var("t")]),
                assign("out", "textConnection", vec![Null, Str("w")]),
                eval("summary.connection", vec![var("out")]),
                eval("writeLines", vec![Strs(&["p", "q"]), var("out")]),
                eval_named(
                    "writeLines",
                    vec![
                        (None, Str("part")),
                        (None, var("out")),
                        (Some("sep"), Str("")),
                    ],
                ),
                eval("textConnectionValue", vec![var("out")]),
                eval("close", vec![var("out")]),
                assign("named", "textConnection", vec![Str("captured"), Str("w")]),
                eval("identity", vec![var("captured")]),
                eval("writeLines", vec![Strs(&["l1", "l2"]), var("named")]),
                eval("identity", vec![var("captured")]),
                eval_named(
                    "writeLines",
                    vec![
                        (None, Str("tail")),
                        (None, var("named")),
                        (Some("sep"), Str("")),
                    ],
                ),
                eval("close", vec![var("named")]),
                eval("identity", vec![var("captured")]),
            ],
        },
        Scenario {
            name: "standard_connections",
            steps: vec![
                eval("summary.connection", vec![nested("stdin")]),
                eval("summary.connection", vec![nested("stdout")]),
                eval("summary.connection", vec![nested("stderr")]),
                eval("stdout", vec![]),
                eval("getAllConnections", vec![]),
                eval("isIncomplete", vec![nested("stdin")]),
                eval("seek", vec![nested("stdin")]),
                eval("writeLines", vec![Str("x"), nested("stdin")]),
                eval("textConnectionValue", vec![nested("stdout")]),
                eval("rawConnectionValue", vec![nested("stdout")]),
                eval("getConnection", vec![Int(2)]),
                eval("open", vec![nested("stdin")]),
            ],
        },
        Scenario {
            name: "slots",
            steps: vec![
                Step::Define("lines", Strs(&["x"])),
                assign("first", "textConnection", vec![var("lines")]),
                assign("second", "textConnection", vec![var("lines")]),
                assign("third", "textConnection", vec![var("lines")]),
                eval("getAllConnections", vec![]),
                eval("close", vec![var("second")]),
                eval("getAllConnections", vec![]),
                assign("again", "textConnection", vec![var("lines")]),
                eval("getConnection", vec![Int(4)]),
                eval("readLines", vec![var("again")]),
                eval("closeAllConnections", vec![]),
                eval("getAllConnections", vec![]),
            ],
        },
        Scenario {
            name: "gzip",
            steps: vec![
                assign("z", "gzfile", vec![Path("g.gz"), Str("w")]),
                eval("summary.connection", vec![var("z")]),
                eval("writeLines", vec![Strs(&["gz", "line"]), var("z")]),
                eval("close", vec![var("z")]),
                assign("z", "gzfile", vec![Path("g.gz")]),
                eval("summary.connection", vec![var("z")]),
                eval("readLines", vec![var("z")]),
                eval("open", vec![var("z")]),
                eval("readChar", vec![var("z"), Int(3)]),
                eval("seek", vec![var("z")]),
                eval("seek", vec![var("z"), Real(1.0)]),
                eval("readLines", vec![var("z")]),
                eval("close", vec![var("z")]),
                eval("readLines", vec![Path("g.gz")]),
                assign("f", "file", vec![Path("g.gz"), Str("r")]),
                eval("summary.connection", vec![var("f")]),
                eval("close", vec![var("f")]),
                Step::WriteFile("plain.txt", b"not compressed\n"),
                eval(
                    "readLines",
                    vec![Call(call("gzfile", vec![(None, Path("plain.txt"))]))],
                ),
            ],
        },
        Scenario {
            name: "pipes",
            steps: vec![
                assign("p", "pipe", vec![Str("echo hi; echo there")]),
                eval("summary.connection", vec![var("p")]),
                eval("readLines", vec![var("p")]),
                eval("close", vec![var("p")]),
                assign("q", "pipe", vec![Str("printf 'a\\nb'; exit 3"), Str("r")]),
                eval("readLines", vec![var("q")]),
                eval("close", vec![var("q")]),
            ],
        },
        Scenario {
            name: "custom_defaults",
            steps: vec![
                assign_dot_call("b", "ryconn_bare", vec![Str("r")]),
                eval("summary.connection", vec![var("b")]),
                dot_call("ryconn_fields", vec![var("b")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("open")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("close")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("fgetc")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("fgetc_internal")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("seek")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("truncate")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("fflush")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("read")]),
                dot_call("ryconn_call_method", vec![var("b"), Str("write")]),
                eval("open", vec![var("b")]),
                eval("readLines", vec![var("b")]),
                dot_call("ryconn_mark_open", vec![var("b")]),
                eval("readLines", vec![var("b")]),
                eval("writeLines", vec![Str("x"), var("b")]),
                dot_call("ryconn_read", vec![var("b"), Int(4)]),
                dot_call("ryconn_write", vec![var("b"), Raw(&[1, 2, 3]), Int(2)]),
                dot_call("ryconn_printf", vec![var("b"), Str("k"), Int(1)]),
                eval("close", vec![var("b")]),
                eval("summary.connection", vec![var("b")]),
            ],
        },
        Scenario {
            name: "custom_memory",
            steps: vec![
                assign_dot_call(
                    "m",
                    "ryconn_new",
                    vec![
                        Str("mem"),
                        Str("r"),
                        Raw(b"one\ntwo\nthree"),
                        Int(2),
                        Int(0),
                    ],
                ),
                dot_call("ryconn_fields", vec![var("m")]),
                eval("readLines", vec![var("m")]),
                eval("isIncomplete", vec![var("m")]),
                eval("pushBackLength", vec![var("m")]),
                dot_call("ryconn_fields", vec![var("m")]),
                eval("open", vec![var("m")]),
                eval_named("readLines", vec![(None, var("m")), (Some("n"), Int(1))]),
                dot_call("ryconn_read_all", vec![var("m"), Int(3)]),
                dot_call("ryconn_read", vec![var("m"), Int(3)]),
                eval("close", vec![var("m")]),
                assign_dot_call(
                    "w",
                    "ryconn_new",
                    vec![Str("mem"), Str("w"), Raw(&[]), Int(0), Int(0)],
                ),
                eval("open", vec![var("w"), Str("w")]),
                eval("writeLines", vec![Strs(&["a", "b"]), var("w")]),
                dot_call("ryconn_write", vec![var("w"), Raw(b"xyz"), Int(2)]),
                dot_call("ryconn_printf", vec![var("w"), Str("k"), Int(7)]),
                eval("writeBin", vec![Raw(b"!"), var("w")]),
                dot_call("ryconn_contents", vec![var("w")]),
                dot_call("ryconn_read", vec![var("w"), Int(3)]),
                eval("close", vec![var("w")]),
                assign_dot_call(
                    "c5",
                    "ryconn_new",
                    vec![Str("mem"), Str("r"), Raw(b"ab\ncd"), Int(0), Int(0)],
                ),
                eval("open", vec![var("c5")]),
                dot_call("ryconn_fgetc", vec![var("c5"), Int(6)]),
                eval("close", vec![var("c5")]),
            ],
        },
        Scenario {
            name: "custom_errors",
            steps: vec![
                assign_dot_call(
                    "f",
                    "ryconn_new",
                    vec![Str("fail"), Str("r"), Raw(b"x"), Int(0), Int(1)],
                ),
                eval("open", vec![var("f")]),
                eval("readLines", vec![var("f")]),
                dot_call("ryconn_read", vec![var("f"), Int(1)]),
                eval("readBin", vec![var("f"), Str("raw"), Int(1)]),
                eval("close", vec![var("f")]),
                assign_dot_call(
                    "g",
                    "ryconn_new",
                    vec![Str("fail"), Str("r"), Raw(b"x"), Int(0), Int(2)],
                ),
                eval("open", vec![var("g")]),
                eval("readLines", vec![var("g")]),
                eval("isOpen", vec![var("g")]),
                eval("close", vec![var("g")]),
                assign_dot_call(
                    "h",
                    "ryconn_new",
                    vec![Str("fail"), Str("w"), Raw(&[]), Int(0), Int(3)],
                ),
                eval("writeLines", vec![Str("x"), var("h")]),
                eval("isOpen", vec![var("h")]),
                eval("close", vec![var("h")]),
            ],
        },
        Scenario {
            name: "c_readers_and_writers",
            steps: vec![
                Step::WriteFile("hw.txt", b"hello\nworld\n"),
                assign("f", "file", vec![Path("hw.txt"), Str("rb")]),
                dot_call("ryconn_fgetc", vec![var("f"), Int(3)]),
                dot_call("ryconn_seek", vec![var("f"), Real(-1.0)]),
                dot_call("ryconn_read_all", vec![var("f"), Int(4)]),
                dot_call("ryconn_seek", vec![var("f"), Real(1.0)]),
                dot_call("ryconn_read", vec![var("f"), Int(3)]),
                dot_call("ryconn_fields", vec![var("f")]),
                eval("close", vec![var("f")]),
                assign("f", "file", vec![Path("hw.txt")]),
                dot_call("ryconn_read", vec![var("f"), Int(3)]),
                eval("close", vec![var("f")]),
                assign("f", "file", vec![Path("hw.txt"), Str("w")]),
                dot_call("ryconn_write", vec![var("f"), Raw(b"abcdef"), Int(4)]),
                dot_call("ryconn_printf", vec![var("f"), Str("n"), Int(3)]),
                eval("close", vec![var("f")]),
                eval("readLines", vec![Path("hw.txt")]),
                assign("f", "file", vec![Path("hw.txt"), Str("r")]),
                dot_call("ryconn_write", vec![var("f"), Raw(&[1]), Int(1)]),
                eval("close", vec![var("f")]),
                dot_call("ryconn_fields", vec![Int(3)]),
                assign("f", "file", vec![Path("hw.txt")]),
                eval("close", vec![var("f")]),
                dot_call("ryconn_fields", vec![var("f")]),
                Step::Define("bytes", Raw(b"0123456789")),
                assign("r", "rawConnection", vec![var("bytes")]),
                dot_call("ryconn_read_all", vec![var("r"), Int(3)]),
                eval("close", vec![var("r")]),
                assign("out", "textConnection", vec![Null, Str("w")]),
                dot_call("ryconn_write", vec![var("out"), Raw(b"ab\ncd\nef"), Int(3)]),
                eval("textConnectionValue", vec![var("out")]),
                eval("close", vec![var("out")]),
                assign("f", "file", vec![Path("hw.txt"), Str("r")]),
                eval("pushBack", vec![Str("pushed"), var("f")]),
                dot_call("ryconn_read", vec![var("f"), Int(100)]),
                eval("readLines", vec![var("f")]),
                eval("close", vec![var("f")]),
            ],
        },
    ]
}

fn r_string(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn r_real(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn r_vector(elements: Vec<String>) -> String {
    format!("c({})", elements.join(", "))
}

pub fn r_code(arg: &Arg) -> String {
    match arg {
        Arg::Str(text) => r_string(text),
        Arg::Strs(texts) => r_vector(texts.iter().map(|text| r_string(text)).collect()),
        Arg::Int(value) => format!("{value}L"),
        Arg::Ints(values) => r_vector(values.iter().map(|value| format!("{value}L")).collect()),
        Arg::Real(value) => r_real(*value),
        Arg::Reals(values) => r_vector(values.iter().map(|value| r_real(*value)).collect()),
        Arg::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_owned(),
        Arg::Logicals(values) => r_vector(
            values
                .iter()
                .map(|value| match value {
                    Some(true) => "TRUE".to_owned(),
                    Some(false) => "FALSE".to_owned(),
                    None => "NA".to_owned(),
                })
                .collect(),
        ),
        Arg::Raw([]) => "raw(0)".to_owned(),
        Arg::Raw(bytes) => format!(
            "as.raw({})",
            r_vector(bytes.iter().map(|byte| format!("0x{byte:02x}")).collect())
        ),
        Arg::Var(name) => (*name).to_owned(),
        Arg::Path(name) => format!("file.path(.dir, {})", r_string(name)),
        Arg::Null => "NULL".to_owned(),
        Arg::Call(inner) => r_call(inner),
    }
}

pub fn r_call(call: &Call) -> String {
    let arguments: Vec<String> = call
        .arguments
        .iter()
        .map(|(name, arg)| match name {
            Some(name) => format!("{name} = {}", r_code(arg)),
            None => r_code(arg),
        })
        .collect();
    format!("{}({})", call.function, arguments.join(", "))
}

const R_PRELUDE: &str = r#"
arguments <- commandArgs(TRUE)
dyn.load(arguments[1])
.dir <- arguments[2]
.out <- arguments[3]
.escape <- function(s) {
  s <- gsub("\\", "\\\\", s, fixed = TRUE)
  s <- gsub("\"", "\\\"", s, fixed = TRUE)
  s <- gsub("\n", "\\n", s, fixed = TRUE)
  s <- gsub("\r", "\\r", s, fixed = TRUE)
  s <- gsub("\t", "\\t", s, fixed = TRUE)
  paste0("\"", s, "\"")
}
.render <- function(x, indent = "") {
  if (is.null(x)) return(paste0(indent, "NULL"))
  head <- paste0(indent, if (is.list(x)) "list" else typeof(x), "[", length(x), "]")
  classes <- attr(x, "class", exact = TRUE)
  if (!is.null(classes)) head <- paste0(head, " class=", paste(classes, collapse = ","))
  if (is.list(x)) {
    names <- names(x)
    lines <- head
    for (i in seq_along(x)) {
      lines <- c(lines, paste0(indent, "  $", if (is.null(names)) i else names[i]),
                 .render(x[[i]], paste0(indent, "    ")))
    }
    return(lines)
  }
  x <- unclass(x)
  attributes(x) <- NULL
  values <- if (is.character(x)) ifelse(is.na(x), "NA", .escape(x))
            else if (is.raw(x)) as.character(x)
            else ifelse(is.na(x), "NA", as.character(x))
  paste(c(head, values), collapse = " ")
}
.emit <- function(lines) cat(gsub(.dir, "<dir>", lines, fixed = TRUE), file = .out, sep = "\n", append = TRUE)
.step <- function(code, name) {
  warnings <- character()
  result <- withCallingHandlers(
    tryCatch(list(value = eval(parse(text = code)[[1]], globalenv())),
             error = function(e) list(error = conditionMessage(e))),
    warning = function(w) {
      warnings <<- c(warnings, conditionMessage(w))
      invokeRestart("muffleWarning")
    })
  lines <- paste(">", if (is.null(name)) code else paste(name, "<-", code))
  if (!is.null(result$error)) {
    lines <- c(lines, paste("error:", result$error))
  } else {
    lines <- c(lines, .render(result$value))
    if (!is.null(name)) assign(name, result$value, envir = globalenv())
  }
  if (length(warnings)) lines <- c(lines, paste("warning:", warnings))
  .emit(lines)
}
"#;

/// The scenarios as an R script for `Rscript <script> <library> <dir> <output>`.
pub fn r_script() -> String {
    let mut script = String::from(R_PRELUDE);
    for scenario in scenarios() {
        writeln!(
            script,
            ".emit({})",
            r_string(&format!("# {}", scenario.name))
        )
        .ok();
        for step in &scenario.steps {
            match step {
                Step::Eval(name, call) => {
                    let name = name.map_or("NULL".to_owned(), r_string);
                    writeln!(script, ".step({}, {name})", r_string(&r_call(call))).ok();
                }
                Step::Define(name, arg) => {
                    writeln!(script, "{name} <- {}", r_code(arg)).ok();
                }
                Step::WriteFile(file, bytes) => {
                    writeln!(
                        script,
                        "writeBin({}, file.path(.dir, {}))",
                        r_code(&Arg::Raw(bytes)),
                        r_string(file)
                    )
                    .ok();
                }
            }
        }
        writeln!(script, "invisible(closeAllConnections())").ok();
    }
    script
}

/// Runs the scenarios in GNU R against the package built by `R CMD SHLIB`.
pub fn run_in_gnu_r(library: &Path) -> String {
    let directory = scratch_directory("gnu");
    let script_path = directory.join("scenarios.R");
    std::fs::write(&script_path, r_script()).expect("write the scenario script");
    let output = directory.join("output.txt");
    let data = directory.join("data");
    std::fs::create_dir_all(&data).expect("a data directory");
    let result = Command::new("Rscript")
        .arg("--vanilla")
        .arg(&script_path)
        .arg(library)
        .arg(&data)
        .arg(&output)
        .output()
        .expect("run Rscript");
    assert!(
        result.status.success(),
        "Rscript failed:\n{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read_to_string(&output).expect("the scenario output")
}

fn scratch_directory(tag: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("connections-{tag}"));
    drop(std::fs::remove_dir_all(&directory));
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    directory
}

pub fn expectation_path() -> PathBuf {
    package_sources("ryconn")
        .parent()
        .expect("the package directory")
        .join("gnu_r.expected")
}

struct Runner<'a> {
    r: &'a R<'a>,
    data: PathBuf,
    define: unsafe extern "C" fn(Sexp, Sexp, Sexp),
    scalar_logical: unsafe extern "C" fn(c_int) -> Sexp,
    output: String,
}

/// Runs the scenarios in ry, which must already have the ryconn package loaded.
pub fn run_in_ry(r: &R, tag: &str) -> String {
    let directory = scratch_directory(tag);
    let data = directory.join("data");
    std::fs::create_dir_all(&data).expect("a data directory");
    let mut runner = unsafe {
        Runner {
            r,
            data,
            define: *r
                .api()
                .library
                .get::<unsafe extern "C" fn(Sexp, Sexp, Sexp)>(b"Rf_defineVar\0")
                .expect("Rf_defineVar"),
            scalar_logical: *r
                .api()
                .library
                .get::<unsafe extern "C" fn(c_int) -> Sexp>(b"Rf_ScalarLogical\0")
                .expect("Rf_ScalarLogical"),
            output: String::new(),
        }
    };
    // Connections other tests left open would shift the slot numbers the transcript shows.
    let close_all = runner.build_call(&call("closeAllConnections", Vec::new()));
    r.eval_expression(close_all).expect("closeAllConnections");
    for scenario in scenarios() {
        runner.emit(&[format!("# {}", scenario.name)]);
        for step in &scenario.steps {
            runner.step(step);
        }
        let close_all = runner.build_call(&call("closeAllConnections", Vec::new()));
        r.eval_expression(close_all).expect("closeAllConnections");
    }
    runner.output
}

impl Runner<'_> {
    fn emit(&mut self, lines: &[String]) {
        let directory = self.data.to_string_lossy().into_owned();
        for line in lines {
            self.output.push_str(&line.replace(&directory, "<dir>"));
            self.output.push('\n');
        }
    }

    fn step(&mut self, step: &Step) {
        let r = self.r;
        let mark = r.protected.get();
        match step {
            Step::Define(name, arg) => {
                let value = self.build(arg);
                unsafe { (self.define)(r.symbol(name), value, r.data("R_GlobalEnv")) };
            }
            Step::WriteFile(file, bytes) => {
                std::fs::write(self.data.join(file), bytes).expect("write a scenario file");
            }
            Step::Eval(name, call) => {
                unsafe { (r.api().ry_clear_warnings)() };
                let expression = self.build_call(call);
                let shown = match name {
                    Some(name) => format!("{name} <- {}", r_call(call)),
                    None => r_call(call),
                };
                let mut lines = vec![format!("> {shown}")];
                match r.eval_expression(expression) {
                    Ok(value) => {
                        lines.extend(render(r, value, ""));
                        if let Some(name) = name {
                            unsafe { (self.define)(r.symbol(name), value, r.data("R_GlobalEnv")) };
                        }
                    }
                    Err(message) => lines.push(format!("error: {}", error_message(&message))),
                }
                lines.extend(
                    r.warnings()
                        .into_iter()
                        .map(|warning| format!("warning: {warning}")),
                );
                self.emit(&lines);
            }
        }
        r.release_to(mark);
    }

    fn build(&self, arg: &Arg) -> Sexp {
        let r = self.r;
        let api = r.api();
        unsafe {
            match arg {
                Arg::Str(text) => self.string_vector(&[text]),
                Arg::Strs(texts) => self.string_vector(texts),
                Arg::Int(value) => r.integer(*value),
                Arg::Ints(values) => {
                    let vector = r.keep((api.Rf_allocVector)(INTSXP, values.len() as isize));
                    std::slice::from_raw_parts_mut((api.INTEGER)(vector), values.len())
                        .copy_from_slice(values);
                    vector
                }
                Arg::Real(value) => r.real(*value),
                Arg::Reals(values) => {
                    let vector = r.keep((api.Rf_allocVector)(REALSXP, values.len() as isize));
                    std::slice::from_raw_parts_mut((api.REAL)(vector), values.len())
                        .copy_from_slice(values);
                    vector
                }
                Arg::Bool(value) => r.keep((self.scalar_logical)(*value as c_int)),
                Arg::Logicals(values) => {
                    let vector = r.keep((api.Rf_allocVector)(LGLSXP, values.len() as isize));
                    let slots = std::slice::from_raw_parts_mut((api.INTEGER)(vector), values.len());
                    for (slot, value) in slots.iter_mut().zip(values.iter()) {
                        *slot = value.map_or(c_int::MIN, |flag| flag as c_int);
                    }
                    vector
                }
                Arg::Raw(bytes) => {
                    let vector = r.keep((api.Rf_allocVector)(RAWSXP, bytes.len() as isize));
                    if !bytes.is_empty() {
                        std::slice::from_raw_parts_mut((api.RAW)(vector), bytes.len())
                            .copy_from_slice(bytes);
                    }
                    vector
                }
                Arg::Var(name) => r.symbol(name),
                Arg::Path(name) => r.string(&self.data.join(name).to_string_lossy()),
                Arg::Null => r.nil(),
                Arg::Call(inner) => self.build_call(inner),
            }
        }
    }

    fn string_vector(&self, texts: &[&str]) -> Sexp {
        let r = self.r;
        let api = r.api();
        unsafe {
            let vector = r.keep((api.Rf_allocVector)(STRSXP, texts.len() as isize));
            for (index, text) in texts.iter().enumerate() {
                let element =
                    (api.Rf_mkCharLenCE)(text.as_ptr() as *const c_char, text.len() as c_int, 0);
                (api.SET_STRING_ELT)(vector, index as isize, element);
            }
            vector
        }
    }

    fn build_call(&self, call: &Call) -> Sexp {
        let arguments: Vec<(Option<&str>, Sexp)> = call
            .arguments
            .iter()
            .map(|(name, arg)| (*name, self.build(arg)))
            .collect();
        self.r.call_expression(call.function, &arguments)
    }
}

/// The message of an error as `R_curErrorBuf` formats it, without the call.
pub fn error_message(text: &str) -> String {
    let text = text.trim_end_matches('\n');
    let body = if let Some(rest) = text.strip_prefix("Error: ") {
        rest
    } else if let Some(rest) = text.strip_prefix("Error in ") {
        rest.split_once(" : ").map_or(rest, |(_, message)| message)
    } else {
        text
    };
    body.trim_start_matches(['\n', ' ']).to_owned()
}

fn render(r: &R, value: Sexp, indent: &str) -> Vec<String> {
    let api = r.api();
    if value == r.nil() {
        return vec![format!("{indent}NULL")];
    }
    let kind = r.type_of(value);
    let type_name = match kind {
        LGLSXP => "logical",
        INTSXP => "integer",
        REALSXP => "double",
        STRSXP => "character",
        VECSXP => "list",
        RAWSXP => "raw",
        15 => "complex",
        other => panic!("cannot render type {other}"),
    };
    let length = r.length(value);
    let mut head = format!("{indent}{type_name}[{length}]");
    let classes = unsafe { (api.Rf_getAttrib)(value, r.data("R_ClassSymbol")) };
    if classes != r.nil() {
        let names: Vec<String> = r
            .strings(classes)
            .into_iter()
            .map(|name| name.unwrap_or_default())
            .collect();
        head.push_str(&format!(" class={}", names.join(",")));
    }
    if kind == VECSXP {
        let names = unsafe { (api.Rf_getAttrib)(value, r.data("R_NamesSymbol")) };
        let names: Option<Vec<Option<String>>> = (names != r.nil()).then(|| r.strings(names));
        let mut lines = vec![head];
        for index in 0..length {
            let label = match &names {
                Some(names) => names[index].clone().unwrap_or_default(),
                None => (index + 1).to_string(),
            };
            lines.push(format!("{indent}  ${label}"));
            lines.extend(render(r, r.element(value, index), &format!("{indent}    ")));
        }
        return lines;
    }
    let values: Vec<String> = match kind {
        LGLSXP => r
            .integers(value)
            .into_iter()
            .map(|flag| match flag {
                c_int::MIN => "NA".to_owned(),
                0 => "FALSE".to_owned(),
                _ => "TRUE".to_owned(),
            })
            .collect(),
        INTSXP => r
            .integers(value)
            .into_iter()
            .map(|number| {
                if number == c_int::MIN {
                    "NA".to_owned()
                } else {
                    number.to_string()
                }
            })
            .collect(),
        REALSXP => r
            .reals(value)
            .into_iter()
            .map(|number| {
                if number.is_nan() {
                    "NA".to_owned()
                } else {
                    r_real(number)
                }
            })
            .collect(),
        STRSXP => r
            .strings(value)
            .into_iter()
            .map(|text| text.map_or("NA".to_owned(), |text| r_string(&text)))
            .collect(),
        RAWSXP => unsafe { std::slice::from_raw_parts((api.RAW)(value), length) }
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        _ => panic!("cannot render the elements of type {kind}"),
    };
    let mut line = head;
    for element in values {
        line.push(' ');
        line.push_str(&element);
    }
    vec![line]
}

/// Compares two scenario transcripts, reporting the first differing line with context.
pub fn assert_same_transcript(expected: &str, actual: &str, what: &str) {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let first_difference = (0..expected_lines.len().max(actual_lines.len()))
        .find(|&index| expected_lines.get(index) != actual_lines.get(index));
    if let Some(index) = first_difference {
        let start = index.saturating_sub(6);
        let context =
            |lines: &[&str]| lines[start.min(lines.len())..(index + 4).min(lines.len())].join("\n");
        panic!(
            "{what}: transcripts differ at line {}\n--- expected\n{}\n--- actual\n{}",
            index + 1,
            context(&expected_lines),
            context(&actual_lines)
        );
    }
}

pub fn c_string(text: *const c_char) -> String {
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}
