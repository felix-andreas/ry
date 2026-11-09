//! The interpreter's fuzz contracts, shared by the in-tree harness (`tests/test_fuzz.rs`) and the
//! coverage-guided `fuzz/` targets so both check exactly the same properties. Each one runs
//! without base R or libR, on an interpreter it installs on first use.

use {
    crate::{
        bytecode, code_image, deparse, eval,
        graphics_render::{
            Affine, Clip, Definition, Drawing, Extend, Fill, FillRule, Format, Item, Mark,
            Operator, Outline, Page, Pattern, Rect, Segment, Stop, Stroke, circle_outline,
            render_files,
        },
        lower, serialize,
        unwind::Unwind,
        values,
    },
    heap::{
        Heap,
        object::{Sexp, SexpType},
    },
    std::sync::{Mutex, Once},
    syntax::testing::Entropy,
};

/// Runs `body` with the interpreter installed. The interpreter is single-threaded, so callers on
/// different threads take turns.
pub fn with_interpreter<T>(body: impl FnOnce() -> T) -> T {
    static INSTALL: Once = Once::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    INSTALL.call_once(|| {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        // SAFETY: the heap lives for the rest of the process, and the lock above keeps every
        // use of the interpreter on one thread at a time.
        unsafe {
            crate::install(heap, crate::default_hooks());
            crate::interp().show_error_messages = false;
        }
    });
    body()
}

/// Lowering any text must not panic: it either fails with a parse error or produces expressions,
/// and every expression it produces must deparse without panicking.
pub fn check_lowering_input(text: &str) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            if let Ok(expressions) = lower::parse_text(text, "<fuzz>") {
                for index in 0..values::length(expressions) {
                    deparse::deparse_lines(
                        expressions.element(index),
                        60,
                        deparse::options::KEEP_INTEGER,
                    );
                }
            }
        }
        crate::heap().release_handles(mark);
    });
}

/// Reading any byte stream as a serialized object must fail with an R error or succeed, never
/// panic.
pub fn check_unserialize_input(bytes: &[u8]) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            drop(serialize::unserialize(bytes, values::nil()));
        }
        crate::truncate_frames(crate::interp(), 0);
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
    });
}

/// Generates a program over `a` and `b` and checks that the tier-0 bytecode agrees with the AST
/// evaluator on it: the program's value evaluated directly must equal its value as a closure body
/// on each of the first three calls, the second and later of which run compiled bytecode.
pub fn check_bytecode_agreement(entropy: &mut impl Entropy) {
    let body = program(entropy);
    let arguments = [
        "3L",
        "-2.5",
        "NA",
        "TRUE",
        "2147483647L",
        "0",
        "NA_integer_",
        "[3L]",
        "[-2.5, 1]",
        "[TRUE]",
    ];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

unsafe fn outcomes(body: &str, a: &str, b: &str) -> (String, Vec<String>) {
    unsafe {
        // The direct run assigns its locals globally, which the closure would then read, so
        // every run starts from the same globals.
        let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
        run(&reset);
        let direct = run(&format!("local_value <- {{ {body} }}\nlocal_value"));
        run(&format!("f <- function(a, b) {{ {body} }}"));
        let compiled = (0..3)
            .map(|_| {
                run(&reset);
                run(&format!("f({a}, {b})"))
            })
            .collect();
        (direct, compiled)
    }
}

/// Bodies that put into a code object what the arithmetic generator does not: strings, nested
/// functions, named arguments and calls the compiler leaves to the evaluator.
const IMAGE_BODIES: &[&str] = &[
    "g <- function(x, y = 2L) x * y + a; g(b) + g(a, y = 3L)",
    "v <- c(first = a, second = b); if (length(v) > 1L) v[[\"second\"]] else \"none\"",
    "h <- function(n) { total <- 0; for (i in seq_len(n)) total <- total + i; total }; h(3L) + a",
    "s <- list(x = a, y = \"text\", z = NULL); is.null(s$z) && !is.null(s$x)",
    "k <- 0L; repeat { k <- k + 1L; if (k >= 3L) break }; k * b",
];

/// A compiled closure survives the code image's encoding: decoding its code against the same
/// body gives an object that encodes to the same bytes and computes what the AST evaluator
/// computes, trained or not, and every mutation of the encoding decodes or fails with an R error.
pub fn check_code_image_round_trip(entropy: &mut impl Entropy) {
    let body = if entropy.below(3) == 0 {
        IMAGE_BODIES[entropy.below(IMAGE_BODIES.len())].to_owned()
    } else {
        program(entropy)
    };
    let arguments = ["3L", "-2.5", "NA", "TRUE", "0", "[3L]"];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    let mutations: Vec<(usize, u8)> = (0..8)
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every object it reads is rooted by the
        // global environment or the handle scope.
        unsafe {
            let (direct, _) = outcomes(&body, a, b);
            let mark = crate::heap().handle_mark();
            let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
            let global = values::global_env();
            let compiled = (|| -> Result<Option<(Sexp, Sexp, Sexp)>, Unwind> {
                let function = eval::find_var(values::install("f")?, global)?;
                let code = bytecode::compiled_body(function)?;
                if code.kind() != SexpType::Bytecode {
                    return Ok(None);
                }
                Ok(Some((function, code, bytecode::source_body(function))))
            })();
            let Ok(Some((function, code, source))) = compiled else {
                crate::heap().release_handles(mark);
                return;
            };
            for trained in [false, true] {
                let Some(bytes) = code_image::encode(code, source, trained) else {
                    continue;
                };
                let decoded = code_image::decode_untrusted(&bytes, source)
                    .unwrap_or_else(|failure| {
                        panic!("`{body}`: an encoded code object must decode: {failure:?}")
                    });
                crate::heap().root(decoded);
                assert_eq!(
                    code_image::encode(decoded, source, trained).as_deref(),
                    Some(&bytes[..]),
                    "`{body}`: decoding then encoding must give the same bytes (trained: {trained})"
                );
                let copy = eval::make_closure(function.car(), source, values::nil(), global)
                    .unwrap_or_else(|_| panic!("cannot make a closure"));
                crate::heap().set_cdr(copy, decoded);
                eval::define(
                    values::install("g_image").unwrap_or_else(|_| panic!("cannot install")),
                    copy,
                    global,
                )
                .unwrap_or_else(|_| panic!("cannot define the decoded closure"));
                run(&reset);
                let outcome = run(&format!("g_image({a}, {b})"));
                assert_eq!(
                    direct, outcome,
                    "`{body}` with a = {a}, b = {b}: the decoded code must compute what the \
                     evaluator does (trained: {trained})"
                );
                for &(position, value) in &mutations {
                    let mut mutated = bytes.clone();
                    let at = position % mutated.len().max(1);
                    match mutated.get_mut(at) {
                        Some(byte) if value % 4 != 0 => *byte ^= value,
                        _ => mutated.truncate(at),
                    }
                    // A mutation may decode to some other valid object; it must only never panic.
                    if let Ok(object) = code_image::decode_untrusted(&mutated, source) {
                        crate::heap().root(object);
                    }
                }
            }
            crate::heap().release_handles(mark);
        }
    });
}

/// An image file's record table reads back as it was written, an image of another build or
/// database reads as absent, and a corrupted file reads as absent or fails with a message, never
/// panics or names a payload outside the file.
pub fn check_code_image_file(entropy: &mut impl Entropy) {
    let count = entropy.below(6);
    let mut offset = 0u64;
    let mut records = Vec::new();
    let mut payloads = Vec::new();
    for _ in 0..count {
        offset += 1 + entropy.below(1000) as u64;
        records.push((offset, entropy.below(5000) as u32, entropy.next_u64()));
        payloads.push((0..entropy.below(40)).map(|_| entropy.below(256) as u8).collect::<Vec<u8>>());
    }
    let database_length = offset + entropy.below(100) as u64;
    let build = "fuzz-build";
    let trained = entropy.below(2) == 0;
    let mutations: Vec<(usize, u8)> = (0..1 + entropy.below(4))
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    with_interpreter(|| {
        let bytes = code_image::image_bytes(build, trained, database_length, &records, &payloads, None)
            .unwrap_or_else(|_| panic!("a small image must write"));
        let entries = code_image::parse(&bytes, build, database_length)
            .unwrap_or_else(|message| panic!("a written image must parse: {message}"))
            .unwrap_or_else(|| panic!("a written image must belong to its build"))
            .entries;
        assert_eq!(entries.len(), records.len());
        for ((entry, record), payload) in entries.iter().zip(&records).zip(&payloads) {
            assert_eq!((entry.offset, entry.length, entry.hash), *record);
            let start = entry.payload_offset as usize;
            assert_eq!(
                bytes.get(start..start + entry.payload_length as usize),
                Some(&payload[..])
            );
        }
        assert_eq!(code_image::parse(&bytes, "another-build", database_length), Ok(None));
        assert_eq!(code_image::parse(&bytes, build, database_length + 1), Ok(None));
        let mut mutated = bytes.clone();
        for &(position, value) in &mutations {
            let at = position % mutated.len().max(1);
            match mutated.get_mut(at) {
                Some(byte) if value % 4 != 0 => *byte ^= value,
                _ => mutated.truncate(at),
            }
        }
        if let Ok(Some(parsed)) = code_image::parse(&mutated, build, database_length) {
            assert!(
                parsed.native_offset + parsed.native_length <= mutated.len() as u64,
                "a parsed image must not name a native section outside the file"
            );
            for entry in parsed.entries {
                assert!(
                    entry.payload_offset + u64::from(entry.payload_length) <= mutated.len() as u64,
                    "a parsed image must not name a payload outside the file"
                );
            }
        }
    });
}

/// How [`check_code_image_load`] damages the image it loads from.
const LOAD_MODES: usize = 6;

/// A closure fetched from a lazy-load database through its image computes what the evaluator
/// computes, whatever the image holds: a damaged file, a record whose closure count or entry does
/// not match, or a closure that shares the fetched body under other formals all fall back to
/// compiling, and code the decoder accepts from a mutated but rehashed payload runs without
/// crashing.
pub fn check_code_image_load(entropy: &mut impl Entropy) {
    let body = if entropy.below(3) == 0 {
        IMAGE_BODIES[entropy.below(IMAGE_BODIES.len())].to_owned()
    } else {
        program(entropy)
    };
    let arguments = ["3L", "-2.5", "NA", "TRUE", "0", "[3L]"];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    let mode = entropy.below(LOAD_MODES);
    let trained = entropy.below(2) == 0;
    let mutations: Vec<(usize, u8)> = (0..1 + entropy.below(4))
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    let directory = std::env::temp_dir().join(format!(
        "ry-code-image-load-{}-{}",
        std::process::id(),
        entropy.next_u64()
    ));
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every object it reads is rooted by the
        // global environment or the handle scope.
        unsafe {
            let (direct, _) = outcomes(&body, a, b);
            let mark = crate::heap().handle_mark();
            let global = values::global_env();
            let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
            let function = eval::find_var(
                values::install("f").unwrap_or_else(|_| panic!("cannot install")),
                global,
            )
            .unwrap_or_else(|_| panic!("`f` must be defined"));
            let mut stream = b"X\n".to_vec();
            for word in [2, 0x0004_0300, 0x0002_0300] {
                stream.extend_from_slice(&i32::to_be_bytes(word));
            }
            // A closure is its flags, its environment, its formals and its body.
            for word in [3 | 1 << 10, 253] {
                stream.extend_from_slice(&i32::to_be_bytes(word));
            }
            let source = bytecode::source_body(function);
            if !(xdr_item(&mut stream, function.car()) && xdr_item(&mut stream, source)) {
                crate::heap().release_handles(mark);
                return;
            }
            // The training run of an install: the record's closure runs before its code is
            // encoded, so a trained image carries the run's feedback.
            let (fetched, closures) = serialize::unserialize_with_closures(&stream, values::nil())
                .unwrap_or_else(|_| panic!("`{body}`: a written closure must read back"));
            crate::heap().root(fetched);
            let define = |name: &str, value: Sexp| {
                eval::define(
                    values::install(name).unwrap_or_else(|_| panic!("cannot install")),
                    value,
                    global,
                )
                .unwrap_or_else(|_| panic!("cannot define `{name}`"));
            };
            define("g_train", fetched);
            if trained {
                for _ in 0..3 {
                    run(&reset);
                    run(&format!("g_train({a}, {b})"));
                }
            }
            let pairs: Vec<(Sexp, Sexp)> = closures
                .iter()
                .map(|&closure| (closure, bytecode::source_body(closure)))
                .collect();
            let mut summary = code_image::Summary::default();
            let mut payload = code_image::record_payload(&pairs, trained, &mut summary);
            let mut offset = 0u64;
            match mode {
                // The record names more closures than the stream makes.
                1 => {
                    payload[0] = payload[0].wrapping_add(1);
                }
                // The record's one closure has no code in the image.
                2 => payload = vec![1, 0],
                // No entry for the record that is fetched.
                3 => offset = 7,
                // The payload is mutated and then rehashed, so only the decoder and the
                // verifier stand between it and the interpreter.
                4 => {
                    for &(position, value) in &mutations {
                        let at = 1 + position % payload.len().max(1);
                        if let Some(byte) = payload.get_mut(at) {
                            *byte ^= value | 1;
                        }
                    }
                }
                _ => {}
            }
            let build = "fuzz-build";
            let mut image = code_image::image_bytes(
                build,
                trained,
                stream.len() as u64,
                &[(offset, stream.len() as u32, code_image::hash(&stream))],
                &[payload],
                None,
            )
            .unwrap_or_else(|_| panic!("a small image must write"));
            // Raw damage: the file no longer matches its own hashes or framing.
            if mode == 0 {
                for &(position, value) in &mutations {
                    let at = position % image.len().max(1);
                    match image.get_mut(at) {
                        Some(byte) if value % 4 != 0 => *byte ^= value,
                        _ => image.truncate(at),
                    }
                }
            }
            std::fs::create_dir_all(&directory)
                .unwrap_or_else(|failure| panic!("cannot create {}: {failure}", directory.display()));
            let database = directory.join("fuzz.rdb");
            let database = database.to_str().unwrap_or_else(|| panic!("a temporary path is text"));
            std::fs::write(database, vec![0u8; stream.len()])
                .unwrap_or_else(|failure| panic!("cannot write {database}: {failure}"));
            std::fs::write(code_image::path(database), &image)
                .unwrap_or_else(|failure| panic!("cannot write the image: {failure}"));
            let previous = std::mem::replace(&mut crate::interp().code_image.build, build);
            let show = std::mem::replace(&mut crate::interp().show_error_messages, false);
            let loaded = (|| -> Result<Option<Sexp>, Unwind> {
                if !code_image::wants(database)? {
                    return Ok(None);
                }
                let record = code_image::Record::of(0, &stream);
                let value = code_image::read_record(database, record, &stream, values::nil())?;
                Ok(Some(value))
            })();
            if let Ok(Some(value)) = loaded {
                crate::heap().root(value);
                assert_eq!(value.kind(), SexpType::Closure, "`{body}`: a record reads back");
                define("g_image", value);
                // Another closure over the fetched body must not take the code compiled for the
                // fetched closure's formals.
                let template = lower::parse_text("function(a, b, extra) NULL", "<fuzz>")
                    .and_then(|expressions| {
                        crate::heap().root(expressions);
                        eval::eval(expressions.element(0), global)
                    })
                    .unwrap_or_else(|_| panic!("cannot make a function"));
                crate::heap().root(template);
                let formals = template.car();
                let other = eval::make_closure(formals, value.cdr(), values::nil(), global)
                    .unwrap_or_else(|_| panic!("cannot make a closure"));
                crate::heap().root(other);
                assert_eq!(
                    code_image::take_pending(other, value.cdr()).ok().flatten(),
                    None,
                    "`{body}`: a closure with other formals took the fetched closure's code"
                );
                for call in 0..3 {
                    run(&reset);
                    let outcome = run(&format!("g_image({a}, {b})"));
                    if mode != 4 {
                        assert_eq!(
                            direct, outcome,
                            "`{body}` with a = {a}, b = {b}, mode {mode}, call {}: a closure \
                             fetched through its image must compute what the evaluator does \
                             (trained: {trained})",
                            call + 1
                        );
                    }
                }
            }
            code_image::forget(database);
            crate::interp().code_image.build = previous;
            crate::interp().show_error_messages = show;
            crate::truncate_frames(crate::interp(), 0);
            crate::interp().foreign_calls.clear();
            run("rm(g_image, g_train)");
            if let Err(failure) = std::fs::remove_dir_all(&directory) {
                panic!("cannot remove {}: {failure}", directory.display());
            }
            crate::heap().release_handles(mark);
        }
    });
}

/// Writes `object` to `out` in R's XDR serialization, for the plain values a function's formals
/// and body hold. Returns `false`, leaving `out` partly written, for anything else.
unsafe fn xdr_item(out: &mut Vec<u8>, object: Sexp) -> bool {
    unsafe {
        let word = |out: &mut Vec<u8>, value: i32| out.extend_from_slice(&value.to_be_bytes());
        let string = |out: &mut Vec<u8>, charsxp: Sexp| {
            if charsxp == values::na_string() {
                word(out, 9);
                word(out, -1);
            } else {
                let bytes = charsxp.bytes();
                word(out, 9 | 1 << 3 << 12);
                word(out, bytes.len() as i32);
                out.extend_from_slice(bytes);
            }
        };
        if object == values::nil() {
            word(out, 254);
            return true;
        }
        if object == values::global_env() {
            word(out, 253);
            return true;
        }
        if object == crate::heap().known().missing_arg {
            word(out, 251);
            return true;
        }
        let kind = object.kind();
        if kind != SexpType::Symbol && object.attrib() != values::nil() {
            return false;
        }
        match kind {
            SexpType::Symbol => {
                word(out, 1);
                string(out, object.car());
                true
            }
            SexpType::Pairlist | SexpType::Language => {
                let code = if kind == SexpType::Pairlist { 2 } else { 6 };
                let tagged = object.tag() != values::nil();
                word(out, code | i32::from(tagged) << 10);
                (!tagged || xdr_item(out, object.tag()))
                    && xdr_item(out, object.car())
                    && xdr_item(out, object.cdr())
            }
            SexpType::Integer | SexpType::Logical | SexpType::Real | SexpType::String => {
                let code = match kind {
                    SexpType::Logical => 10,
                    SexpType::Integer => 13,
                    SexpType::Real => 14,
                    _ => 16,
                };
                word(out, code);
                let count = values::length(object);
                word(out, count as i32);
                for index in 0..count {
                    match kind {
                        SexpType::Real => {
                            let value = *(object.data() as *const f64).add(index);
                            out.extend_from_slice(&value.to_bits().to_be_bytes());
                        }
                        SexpType::String => string(out, values::string_elt(object, index)),
                        _ => word(out, *(object.data() as *const i32).add(index)),
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// The deparsed value of the last expression of `text` in the global environment, or `error`.
unsafe fn run(text: &str) -> String {
    unsafe {
        let mark = crate::heap().handle_mark();
        let outcome = (|| -> Result<Sexp, Unwind> {
            let expressions = lower::parse_text(text, "<fuzz>")?;
            let mut value = values::nil();
            for index in 0..values::length(expressions) {
                value = eval::eval(expressions.element(index), values::global_env())?;
            }
            Ok(value)
        })();
        let rendered = match outcome {
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(_) => "error".to_owned(),
        };
        crate::truncate_frames(crate::interp(), 0);
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
        rendered
    }
}

/// Assignments, a loop, and a final expression.
fn program(entropy: &mut impl Entropy) -> String {
    let mut statements = vec![format!("s <- {}", expression(entropy, 2))];
    match entropy.below(4) {
        0 => statements.push(format!(
            "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
            entropy.below(5) + 1,
            expression(entropy, 2),
            condition(entropy),
            condition(entropy)
        )),
        1 => statements.push(format!(
            "k <- 0L\nwhile (k < {}L) {{ k <- k + 1L; s <- {} }}",
            entropy.below(4),
            expression(entropy, 2)
        )),
        2 => statements.push(format!(
            "s <- if ({}) {} else {}",
            condition(entropy),
            expression(entropy, 2),
            expression(entropy, 2)
        )),
        _ => {}
    }
    statements.push(expression(entropy, 3));
    statements.join("\n")
}

fn expression(entropy: &mut impl Entropy, depth: usize) -> String {
    if depth == 0 || entropy.below(3) == 0 {
        return leaf(entropy);
    }
    match entropy.below(5) {
        0 => format!("-({})", expression(entropy, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(entropy),
            expression(entropy, depth - 1),
            expression(entropy, depth - 1)
        ),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "%/%", "%%", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                expression(entropy, depth - 1),
                operators[entropy.below(operators.len())],
                expression(entropy, depth - 1)
            )
        }
    }
}

fn condition(entropy: &mut impl Entropy) -> String {
    match entropy.below(4) {
        0 => format!("{} && {}", comparison(entropy), comparison(entropy)),
        1 => format!("{} || {}", comparison(entropy), comparison(entropy)),
        _ => comparison(entropy),
    }
}

fn comparison(entropy: &mut impl Entropy) -> String {
    let operators = ["<", ">", "==", "!=", "<=", ">="];
    format!(
        "{} {} {}",
        leaf(entropy),
        operators[entropy.below(operators.len())],
        leaf(entropy)
    )
}

const LEAVES: [&str; 12] = [
    "a", "b", "s", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L",
];

fn leaf(entropy: &mut impl Entropy) -> String {
    LEAVES[entropy.below(LEAVES.len())].to_owned()
}

/// Renders a random drawing to every format twice: rendering must not panic, must give the same
/// bytes both times, and must give one file per page except for PDF and single-file
/// PostScript. Drawings nest definitions, refer to definitions that are missing or of the wrong
/// kind, and carry degenerate geometry such as empty outlines, zero-size tiles, coincident
/// gradient ends and non-finite coordinates.
pub fn check_render_drawing(entropy: &mut impl Entropy) {
    let drawing = drawing(entropy);
    let size = [0.5 + entropy.below(4) as f64, 0.5 + entropy.below(4) as f64];
    let formats = [
        Format::Pdf,
        Format::Svg,
        Format::Png,
        Format::Bmp,
        Format::Tiff,
        Format::Jpeg,
        Format::Postscript {
            onefile: true,
            paper: None,
            landscape: false,
        },
        Format::Postscript {
            onefile: false,
            paper: None,
            landscape: true,
        },
    ];
    for format in formats {
        let render = || render_files(format, &drawing, size, 24.0, "fuzz", 75);
        let first = render();
        let second = render();
        match (&first, &second) {
            (Ok(first), Ok(second)) => {
                assert!(first == second, "rendering is not deterministic");
                let single = matches!(
                    format,
                    Format::Pdf | Format::Postscript { onefile: true, .. }
                );
                let expected = if single { 1 } else { drawing.pages.len() };
                assert_eq!(first.len(), expected, "one file per page");
                for file in first {
                    check_file_structure(format, file, drawing.pages.len());
                }
            }
            (Err(first), Err(second)) => assert_eq!(first.to_string(), second.to_string()),
            _ => panic!("rendering succeeded only once"),
        }
    }
}

/// Checks that `file` is a well-formed file of `format`, as a reader would parse it: a PDF's
/// cross-reference table points at its objects and its page tree counts every page, an SVG's
/// tags nest, a PNG decodes, and the other formats carry their framing and sizes.
fn check_file_structure(format: Format, file: &[u8], pages: usize) {
    let text = String::from_utf8_lossy(file);
    match format {
        Format::Pdf => {
            assert!(file.starts_with(b"%PDF-"), "a PDF starts with its header");
            assert!(text.trim_end().ends_with("%%EOF"), "a PDF ends with %%EOF");
            let start = text.rfind("startxref").expect("a PDF names its xref table");
            let offset: usize = text[start + "startxref".len()..]
                .split_whitespace()
                .next()
                .and_then(|offset| offset.parse().ok())
                .expect("startxref gives an offset");
            assert!(
                file.get(offset..)
                    .is_some_and(|rest| rest.starts_with(b"xref")),
                "startxref points at the xref table"
            );
            // The lossy text replaces binary stream bytes, so offsets index the bytes.
            let table = String::from_utf8_lossy(&file[offset..]);
            let mut lines = table.lines().skip(1);
            let header = lines.next().expect("the xref table has a subsection");
            let mut header = header
                .split_whitespace()
                .map(|number| number.parse::<usize>());
            let (Some(Ok(first)), Some(Ok(count))) = (header.next(), header.next()) else {
                panic!("the xref subsection header is two numbers");
            };
            for (number, entry) in (first..first + count).zip(lines) {
                let mut fields = entry.split_whitespace();
                let (Some(position), Some(_), Some(kind)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    panic!("xref entry {number} has three fields: {entry:?}");
                };
                if kind != "n" {
                    continue;
                }
                let position: usize = position.parse().expect("an xref offset is a number");
                let object = format!("{number} 0 obj");
                assert!(
                    file.get(position..)
                        .is_some_and(|rest| rest.starts_with(object.as_bytes())),
                    "xref entry {number} points at `{object}`"
                );
            }
            let counted = text
                .split("/Count ")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|count| count.parse::<usize>().ok());
            assert_eq!(counted, Some(pages), "the page tree counts every page");
        }
        Format::Svg => {
            assert!(text.contains("<svg"), "an SVG has an svg element");
            let mut open: Vec<&str> = Vec::new();
            let mut rest = text.as_ref();
            while let Some(start) = rest.find('<') {
                rest = &rest[start + 1..];
                let end = rest.find('>').expect("every tag closes");
                let tag = &rest[..end];
                rest = &rest[end + 1..];
                if tag.starts_with('?') || tag.starts_with('!') {
                    continue;
                }
                let name = |tag: &'_ str| {
                    tag.split(|c: char| c.is_whitespace() || c == '/')
                        .find(|part| !part.is_empty())
                        .unwrap_or("")
                        .to_owned()
                };
                if let Some(closing) = tag.strip_prefix('/') {
                    let opened = open.pop().expect("a closing tag has an opening one");
                    assert_eq!(name(opened), name(closing), "SVG tags nest");
                } else if !tag.ends_with('/') {
                    open.push(tag);
                }
            }
            assert!(open.is_empty(), "every SVG element closes: {open:?}");
            for (index, _) in text.match_indices('&') {
                let entity = &text[index..text.len().min(index + 8)];
                assert!(
                    ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;", "&#"]
                        .iter()
                        .any(|known| entity.starts_with(known)),
                    "an SVG escapes every ampersand: {entity:?}"
                );
            }
        }
        Format::Png => {
            let pixmap = tiny_skia::Pixmap::decode_png(file).expect("the PNG decodes");
            assert!(
                pixmap.width() > 0 && pixmap.height() > 0,
                "a PNG has pixels"
            );
        }
        Format::Bmp => {
            assert!(file.starts_with(b"BM"), "a BMP starts with BM");
            let field = |at: usize| {
                file.get(at..at + 4)
                    .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .expect("the BMP header is whole")
            };
            assert_eq!(
                field(2) as usize,
                file.len(),
                "a BMP's header gives its size"
            );
            assert!(
                (field(10) as usize) < file.len(),
                "a BMP's pixels start inside it"
            );
        }
        Format::Tiff => assert!(
            file.starts_with(b"II*\0") || file.starts_with(b"MM\0*"),
            "a TIFF starts with its byte order and magic number"
        ),
        Format::Jpeg => {
            assert!(file.starts_with(&[0xff, 0xd8]), "a JPEG starts with SOI");
            assert!(file.ends_with(&[0xff, 0xd9]), "a JPEG ends with EOI");
        }
        Format::Postscript { onefile, .. } => {
            assert!(
                file.starts_with(b"%!PS-Adobe-"),
                "PostScript starts with its header"
            );
            assert!(
                text.trim_end().ends_with("%%EOF"),
                "PostScript ends with %%EOF"
            );
            let showpages = text.matches("showpage").count();
            let expected = if onefile { pages } else { 1 };
            assert!(showpages >= expected, "every page is shown");
            if onefile {
                let declared = text
                    .lines()
                    .find_map(|line| line.strip_prefix("%%Pages: "))
                    .and_then(|count| count.trim().parse::<usize>().ok());
                assert_eq!(declared, Some(pages), "%%Pages counts every page");
            }
        }
    }
}

fn drawing(entropy: &mut impl Entropy) -> Drawing {
    let mut definitions = Vec::new();
    for _ in 0..entropy.below(6) {
        let definition = definition(entropy, definitions.len());
        definitions.push(definition);
    }
    let pages = (0..1 + entropy.below(2))
        .map(|_| Page {
            background: colour(entropy),
            items: items(entropy, definitions.len(), 6),
        })
        .collect();
    Drawing { pages, definitions }
}

/// A definition that refers only to the `defined` definitions before it, as the device builds
/// them, except that a reference is now and then out of range.
fn definition(entropy: &mut impl Entropy, defined: usize) -> Definition {
    match entropy.below(6) {
        0 => Definition::Pattern(Pattern::Linear {
            start: point(entropy),
            end: point(entropy),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        1 => Definition::Pattern(Pattern::Radial {
            start: point(entropy),
            start_radius: number(entropy).abs(),
            end: point(entropy),
            end_radius: number(entropy).abs(),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        2 => {
            let [x0, y0] = point(entropy);
            let [x1, y1] = point(entropy);
            Definition::Pattern(Pattern::Tiling {
                rect: Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                },
                items: items(entropy, defined, 3),
                extend: extend(entropy),
            })
        }
        3 => Definition::ClipPath {
            outline: outline(entropy),
            rule: rule(entropy),
        },
        4 => Definition::Mask {
            luminance: entropy.chance(1, 2),
            items: items(entropy, defined, 3),
        },
        _ => Definition::Group {
            destination: items(entropy, defined, 2),
            operator: Operator::from_code(1 + entropy.below(25) as i32).unwrap_or(Operator::Over),
            source: items(entropy, defined, 3),
        },
    }
}

fn items(entropy: &mut impl Entropy, defined: usize, most: usize) -> Vec<Item> {
    (0..entropy.below(most + 1))
        .map(|_| Item {
            clip: if entropy.chance(1, 3) {
                Clip::Path(reference(entropy, defined))
            } else {
                let [x0, y0] = point(entropy);
                let [x1, y1] = point(entropy);
                Clip::Rect(Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                })
            },
            mask: entropy.chance(1, 4).then(|| reference(entropy, defined)),
            mark: mark(entropy, defined),
        })
        .collect()
}

fn mark(entropy: &mut impl Entropy, defined: usize) -> Mark {
    match entropy.below(5) {
        0 => Mark::Text {
            origin: point(entropy),
            text: ["", "ry", "Wg ÄÖ", "a\u{301}"][entropy.below(4)].to_owned(),
            points: 1.0 + entropy.below(40) as f64,
            font: 1 + entropy.below(5) as i32,
            rotation: entropy.below(360) as f64,
            hadj: entropy.below(3) as f64 / 2.0,
            colour: colour(entropy),
        },
        4 => {
            let (columns, rows) = (1 + entropy.below(4), 1 + entropy.below(4));
            Mark::Image {
                pixels: (0..columns * rows).map(|_| colour(entropy)).collect(),
                columns,
                rows,
                placement: Affine([
                    number(entropy),
                    number(entropy) / 4.0,
                    number(entropy) / 4.0,
                    number(entropy),
                    number(entropy),
                    number(entropy),
                ]),
                interpolate: entropy.chance(1, 2),
            }
        }
        1 => Mark::Group {
            group: reference(entropy, defined),
            transform: Affine([
                number(entropy),
                number(entropy) / 4.0,
                number(entropy) / 4.0,
                number(entropy),
                number(entropy),
                number(entropy),
            ]),
        },
        _ => Mark::Path {
            outline: outline(entropy),
            fill: match entropy.below(3) {
                0 => None,
                1 => Some(Fill::Colour(colour(entropy))),
                _ => Some(Fill::Pattern(reference(entropy, defined))),
            },
            rule: rule(entropy),
            stroke: entropy.chance(1, 2).then(|| Stroke {
                colour: colour(entropy),
                lwd: entropy.below(8) as f64 / 2.0,
                lty: [0, 0x44, 0x1343, 0xffff_ffff][entropy.below(4)],
                lend: 1 + entropy.below(3) as i32,
                ljoin: 1 + entropy.below(3) as i32,
                lmitre: 1.0 + entropy.below(10) as f64,
            }),
        },
    }
}

fn outline(entropy: &mut impl Entropy) -> Outline {
    match entropy.below(4) {
        0 => circle_outline(point(entropy), number(entropy).abs()),
        _ => {
            let mut outline = vec![Segment::Move(point(entropy))];
            for _ in 0..entropy.below(6) {
                outline.push(match entropy.below(5) {
                    0 => Segment::Cubic(point(entropy), point(entropy), point(entropy)),
                    1 => Segment::Close,
                    2 => Segment::Move(point(entropy)),
                    _ => Segment::Line(point(entropy)),
                });
            }
            outline
        }
    }
}

fn stops(entropy: &mut impl Entropy) -> Vec<Stop> {
    let mut stops: Vec<Stop> = (0..entropy.below(4))
        .map(|_| Stop {
            offset: entropy.below(5) as f64 / 4.0,
            colour: colour(entropy),
        })
        .collect();
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    stops
}

/// A definition index, now and then one past the end or far beyond it.
fn reference(entropy: &mut impl Entropy, defined: usize) -> usize {
    match entropy.below(8) {
        0 => defined,
        1 => usize::MAX,
        _ => entropy.below(defined.max(1)),
    }
}

/// A coordinate in inches, usually on the page and now and then far off it or not finite.
fn point(entropy: &mut impl Entropy) -> [f64; 2] {
    [number(entropy), number(entropy)]
}

fn number(entropy: &mut impl Entropy) -> f64 {
    match entropy.below(20) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1e9,
        3 => 0.0,
        _ => entropy.below(400) as f64 / 100.0 - 0.5,
    }
}

fn colour(entropy: &mut impl Entropy) -> u32 {
    let alpha = [0, 0x80, 0xff][entropy.below(3)];
    (alpha << 24) | (entropy.next_u64() as u32 & 0x00ff_ffff)
}

fn extend(entropy: &mut impl Entropy) -> Extend {
    [Extend::Pad, Extend::Repeat, Extend::Reflect, Extend::None][entropy.below(4)]
}

fn rule(entropy: &mut impl Entropy) -> FillRule {
    if entropy.chance(1, 2) {
        FillRule::Winding
    } else {
        FillRule::EvenOdd
    }
}
