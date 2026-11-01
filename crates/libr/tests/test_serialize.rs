//! Serialization through the Rust API: byte-for-byte conformance with the streams GNU R writes
//! (recorded in `data/serialize/cases.tsv` by `generate.R`), round trips of random objects in
//! every format, the lazy-load databases of installed packages, and a fuzz pass of corrupted
//! streams that must fail cleanly.
//!
//! The runtime is linked in as the crate's rlib, so these tests drive the same runtime the
//! shared library exports, in-process. It is one process-wide session, so every test takes the
//! session lock.

use R::{
    runtime,
    serialize::{
        Format, LazyLoadCompression, PersistHook, from_bytes, header_of, lazyload_fetch, read_rds,
        to_bytes,
    },
};
use heap::{
    GcStress,
    object::{NA_INTEGER, NA_REAL, Rcomplex, Sexp, SexpType, gp},
    strings::Encoding,
};
use std::{
    path::Path,
    process::Command,
    sync::{Mutex, MutexGuard},
};
use syntax::testing::{SplitMix64, fuzz_iterations};

static LOCK: Mutex<()> = Mutex::new(());

/// The session lock, with the handle scope the test's objects live in released when it ends.
struct Session {
    mark: usize,
    _lock: MutexGuard<'static, ()>,
}

fn session() -> Session {
    let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let heap = &mut runtime().heap;
    heap.policy_mut().stress = GcStress::Off;
    Session {
        mark: heap.handle_mark(),
        _lock: lock,
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let heap = &mut runtime().heap;
        heap.policy_mut().stress = GcStress::Off;
        heap.release_handles(self.mark);
    }
}

fn data_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/serialize"))
}

struct Case {
    name: String,
    format: Format,
    version: i32,
    stream: Vec<u8>,
    expected: Expected,
}

enum Expected {
    Same,
    Stream(Vec<u8>),
    /// ry reads the stream but writes an equally valid, different layout.
    Different,
}

fn cases() -> Vec<Case> {
    let text = std::fs::read_to_string(data_dir().join("cases.tsv")).expect("cases.tsv");
    text.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 5, "malformed case line: {line}");
            let format = match fields[1] {
                "xdr" => Format::Xdr,
                "binary" => Format::Binary,
                "ascii" => Format::Ascii,
                "asciihex" => Format::AsciiHex,
                other => panic!("unknown format {other}"),
            };
            let expected = match fields[4] {
                "" => Expected::Same,
                "-" => Expected::Different,
                hex => Expected::Stream(unhex(hex)),
            };
            Case {
                name: fields[0].to_owned(),
                format,
                version: fields[2].parse().expect("a version"),
                stream: unhex(fields[3]),
                expected,
            }
        })
        .collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

fn show(bytes: &[u8], format: Format) -> String {
    match format {
        Format::Ascii | Format::AsciiHex => String::from_utf8_lossy(bytes).into_owned(),
        _ => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

#[test]
fn gnu_r_streams_read_back_and_write_identically() {
    let _session = session();
    let cases = cases();
    assert!(cases.len() > 200);
    let mut failures = Vec::new();
    for case in &cases {
        let label = format!("{} {:?} v{}", case.name, case.format, case.version);
        let object = match unsafe { from_bytes(&case.stream) } {
            Ok(object) => object,
            Err(error) => {
                failures.push(format!("{label}: cannot read: {error:?}"));
                continue;
            }
        };
        // A version-3 stream of base R's ALTREP classes reads back as the same ALTREP object,
        // so writing it reproduces GNU R's bytes; a version-2 stream holds the plain vector.
        let expected = match &case.expected {
            Expected::Same => &case.stream,
            Expected::Stream(_) if case.version == 3 && case.name.starts_with("altrep") => {
                &case.stream
            }
            Expected::Stream(stream) => stream,
            Expected::Different => continue,
        };
        match unsafe { to_bytes(object, case.format, case.version) } {
            Ok(written) if &written == expected => {}
            Ok(written) => failures.push(format!(
                "{label}: wrote\n{}\nexpected\n{}",
                show(&written, case.format),
                show(expected, case.format)
            )),
            Err(error) => failures.push(format!("{label}: cannot write: {error:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn case(name: &str, format: Format, version: i32) -> Vec<u8> {
    cases()
        .into_iter()
        .find(|case| case.name == name && case.format == format && case.version == version)
        .unwrap_or_else(|| panic!("no case {name}"))
        .stream
}

unsafe fn text(charsxp: Sexp) -> String {
    unsafe { String::from_utf8_lossy(charsxp.bytes()).into_owned() }
}

unsafe fn binding(env: Sexp, name: &str) -> Option<Sexp> {
    unsafe {
        let heap = &mut runtime().heap;
        let symbol = heap.install(name.as_bytes()).expect("a symbol");
        heap.frame_cell(env, symbol)
    }
}

#[test]
fn gnu_r_streams_read_into_the_expected_structures() {
    let _session = session();
    unsafe {
        let header = header_of(&case("integer", Format::Xdr, 3)).expect("a header");
        assert_eq!(header.format, Format::Xdr);
        assert_eq!(header.version, 3);
        assert_eq!(header.writer_version, 4 * 65536 + 6 * 256 + 2);
        assert_eq!(header.min_reader_version, 3 * 65536 + 5 * 256);
        assert_eq!(header.native_encoding.as_deref(), Some("UTF-8"));

        let strings = from_bytes(&case("strings", Format::Ascii, 3)).expect("strings");
        assert_eq!(strings.kind(), SexpType::String);
        let known = *runtime().heap.known();
        assert_eq!(strings.element(2), known.na_string);
        assert_eq!(text(strings.element(3)), "caf\u{e9}");
        assert_eq!(Encoding::of(strings.element(3)), Encoding::Utf8);
        assert_eq!(strings.element(4).bytes(), b"caf\xe9");
        assert_eq!(Encoding::of(strings.element(4)), Encoding::Latin1);
        assert_eq!(Encoding::of(strings.element(5)), Encoding::Bytes);
        assert_eq!(text(strings.element(7)), "q\"'?\\");

        let sequence = from_bytes(&case("altrep_intseq_down", Format::Xdr, 3)).expect("5:-2");
        assert_eq!(sequence.kind(), SexpType::Integer);
        assert!(sequence.is_altrep(), "a compact sequence stays compact");
        let values: Vec<i32> = (0..8)
            .map(|index| R::vectors::int_elt(sequence, index).expect("an element"))
            .collect();
        assert_eq!(values, [5, 4, 3, 2, 1, 0, -1, -2]);
        let deferred = from_bytes(&case("altrep_deferred", Format::Xdr, 3)).expect("strings");
        assert_eq!(
            text(R::vectors::sexp_elt(deferred, 2).expect("an element")),
            "300"
        );

        let compiled = from_bytes(&case("bytecode", Format::Xdr, 3)).expect("a closure");
        assert_eq!(compiled.kind(), SexpType::Closure);
        let body = compiled.cdr();
        assert_eq!(body.kind(), SexpType::Language);
        assert_eq!(text(body.car().car()), "{");

        let cyclic = from_bytes(&case("environment_cycle", Format::Xdr, 3)).expect("an env");
        assert_eq!(cyclic.kind(), SexpType::Environment);
        let own = binding(cyclic, "self").expect("self is bound");
        assert_eq!(own.cdr(), cyclic);
        assert_eq!(cyclic.enclosure(), known.global_env);

        let locked = from_bytes(&case("environment_locked", Format::Binary, 3)).expect("an env");
        assert!(locked.has_gp(gp::FRAME_LOCKED));
        assert!(binding(locked, "a").expect("a").has_gp(gp::BINDING_LOCKED));
        assert!(!binding(locked, "b").expect("b").has_gp(gp::BINDING_LOCKED));

        let active = from_bytes(&case("active_binding", Format::Xdr, 2)).expect("an env");
        let now = binding(active, "now").expect("now");
        assert!(now.has_gp(gp::ACTIVE_BINDING));
        assert_eq!(now.cdr().kind(), SexpType::Closure);

        let promised = from_bytes(&case("promise", Format::Xdr, 3)).expect("an env");
        let later = binding(promised, "later").expect("later").cdr();
        assert_eq!(later.kind(), SexpType::Promise);
        assert_eq!(later.car(), known.unbound);
        assert_eq!(later.tag(), known.global_env);

        let hashed = from_bytes(&case("hashed_env", Format::Xdr, 3)).expect("a list");
        let hashed = hashed.element(0);
        assert_eq!(hashed.enclosure(), known.empty_env);
        assert_eq!(
            binding(hashed, "alpha")
                .expect("alpha")
                .cdr()
                .slice::<i32>(),
            &[1]
        );

        let namespace = from_bytes(&case("namespace", Format::Xdr, 3)).expect("a namespace");
        let again = from_bytes(&case("namespace", Format::Ascii, 2)).expect("a namespace");
        assert_eq!(
            namespace, again,
            "a namespace resolves to one environment per session"
        );

        let pointer = from_bytes(&case("external_pointer", Format::Xdr, 3)).expect("a list");
        let pointer = pointer.element(0);
        assert_eq!(pointer.kind(), SexpType::ExternalPtr);
        assert!(pointer.external_address().is_null());

        let object = from_bytes(&case("s4", Format::Xdr, 3)).expect("an S4 object");
        assert_eq!(object.kind(), SexpType::Object);
        assert!(object.is_object());
        assert!(object.has_gp(gp::S4_OBJECT));
    }
}

/// The committed cases are what the generator writes, so they cannot drift from GNU R.
#[test]
fn recorded_cases_match_gnu_r() {
    if std::env::var_os("RY_SKIP_GNU_R").is_some()
        || Command::new("Rscript").arg("--version").output().is_err()
    {
        return;
    }
    let output = Command::new("Rscript")
        .arg("generate.R")
        .current_dir(data_dir())
        .env("LC_ALL", "C.UTF-8")
        .output()
        .expect("Rscript runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recorded = std::fs::read(data_dir().join("cases.tsv")).expect("cases.tsv");
    assert!(
        output.stdout == recorded,
        "GNU R's streams differ from cases.tsv; regenerate it with generate.R"
    );
}

// Random objects.

struct Generator {
    rng: SplitMix64,
    symbols: Vec<Sexp>,
    environments: Vec<Sexp>,
    /// Whether the object holds anything `identical` compares by address.
    by_address: bool,
}

impl Generator {
    fn new(seed: u64) -> Generator {
        let mut symbols = Vec::new();
        for name in ["x", "y", "names", "class", "a b", "\u{e9}t\u{e9}", "..."] {
            symbols.push(unsafe { runtime().heap.install(name.as_bytes()).expect("a symbol") });
        }
        Generator {
            rng: SplitMix64(seed),
            symbols,
            environments: Vec::new(),
            by_address: false,
        }
    }

    fn keep(&self, object: Sexp) -> Sexp {
        runtime().heap.root(object);
        object
    }

    fn symbol(&mut self) -> Sexp {
        self.symbols[self.rng.below(self.symbols.len())]
    }

    unsafe fn string(&mut self) -> Sexp {
        unsafe {
            let heap = &mut runtime().heap;
            match self.rng.below(8) {
                0 => heap.known().na_string,
                1 => heap.known().blank_string,
                2 => heap
                    .mk_char("caf\u{e9}".as_bytes(), Encoding::Utf8)
                    .expect("a string"),
                3 => heap
                    .mk_char(b"caf\xe9", Encoding::Latin1)
                    .expect("a string"),
                4 => heap
                    .mk_char(b"\xff\x80", Encoding::Bytes)
                    .expect("a string"),
                _ => {
                    let length = self.rng.below(12);
                    let bytes: Vec<u8> = (0..length)
                        .map(|_| match self.rng.below(4) {
                            0 => b"\n\t\\\"'? \x07\x7f"[self.rng.below(9)],
                            _ => b'a' + self.rng.below(26) as u8,
                        })
                        .collect();
                    heap.mk_char(&bytes, Encoding::Native).expect("a string")
                }
            }
        }
    }

    fn real(&mut self) -> f64 {
        match self.rng.below(8) {
            0 => NA_REAL,
            1 => f64::NAN,
            2 => f64::INFINITY,
            3 => -0.0,
            4 => 5e-324,
            5 => f64::from_bits(self.rng.next_u64()),
            _ => (self.rng.next_u64() % 100_000) as f64 / 7.0,
        }
    }

    unsafe fn vector(&mut self, depth: usize) -> Sexp {
        unsafe {
            let kinds = [
                SexpType::Logical,
                SexpType::Integer,
                SexpType::Real,
                SexpType::Complex,
                SexpType::Raw,
                SexpType::String,
                SexpType::List,
                SexpType::Expression,
            ];
            let kind = kinds[self.rng.below(if depth == 0 { 6 } else { kinds.len() })];
            let length = self.rng.below(6) as isize;
            let vector = self.keep(runtime().heap.alloc_vector(kind, length).expect("a vector"));
            for index in 0..length as usize {
                match kind {
                    SexpType::Logical => {
                        vector.slice_mut::<i32>()[index] = [0, 1, NA_INTEGER][self.rng.below(3)]
                    }
                    SexpType::Integer => {
                        vector.slice_mut::<i32>()[index] = self.rng.next_u64() as i32
                    }
                    SexpType::Real => vector.slice_mut::<f64>()[index] = self.real(),
                    SexpType::Complex => {
                        vector.slice_mut::<Rcomplex>()[index] = Rcomplex {
                            r: self.real(),
                            i: self.real(),
                        }
                    }
                    SexpType::Raw => vector.slice_mut::<u8>()[index] = self.rng.next_u64() as u8,
                    SexpType::String => {
                        let element = self.string();
                        runtime().heap.set_element(vector, index, element);
                    }
                    _ => {
                        let element = self.object(depth - 1);
                        runtime().heap.set_element(vector, index, element);
                    }
                }
            }
            vector
        }
    }

    unsafe fn pairlist(&mut self, kind: SexpType, depth: usize) -> Sexp {
        unsafe {
            let known = *runtime().heap.known();
            let length = 1 + self.rng.below(4);
            let mut list = known.nil;
            for position in (0..length).rev() {
                let value = self.object(depth.saturating_sub(1));
                let cell_kind = if position == 0 {
                    kind
                } else {
                    SexpType::Pairlist
                };
                list = self.keep(
                    runtime()
                        .heap
                        .alloc_triple(cell_kind, value, list, known.nil)
                        .expect("a cell"),
                );
                if self.rng.chance(1, 2) {
                    let tag = self.symbol();
                    runtime().heap.set_tag(list, tag);
                }
            }
            list
        }
    }

    unsafe fn environment(&mut self, depth: usize) -> Sexp {
        unsafe {
            let known = *runtime().heap.known();
            self.by_address = true;
            if !self.environments.is_empty() && self.rng.chance(1, 3) {
                return self.environments[self.rng.below(self.environments.len())];
            }
            let enclosure = [known.global_env, known.empty_env, known.base_env][self.rng.below(3)];
            let env = self.keep(runtime().heap.alloc_environment(enclosure).expect("an env"));
            self.environments.push(env);
            for _ in 0..self.rng.below(4) {
                let symbol = self.symbol();
                if runtime().heap.frame_cell(env, symbol).is_some() {
                    continue;
                }
                let value = if self.rng.chance(1, 4) {
                    env
                } else {
                    self.object(depth.saturating_sub(1))
                };
                runtime()
                    .heap
                    .define(env, symbol, value)
                    .expect("a binding");
                if self.rng.chance(1, 4) {
                    let cell = runtime().heap.frame_cell(env, symbol).expect("a cell");
                    cell.set_gp_bits(gp::BINDING_LOCKED, true);
                }
            }
            if self.rng.chance(1, 4) {
                env.set_gp_bits(gp::FRAME_LOCKED, true);
            }
            env
        }
    }

    unsafe fn object(&mut self, depth: usize) -> Sexp {
        unsafe {
            let known = *runtime().heap.known();
            let object = match self.rng.below(if depth == 0 { 4 } else { 12 }) {
                0 => known.nil,
                1 => self.symbol(),
                2 | 3 => self.vector(0),
                4 | 5 => self.vector(depth),
                6 => self.pairlist(SexpType::Pairlist, depth),
                7 => self.pairlist(SexpType::Language, depth),
                8 => self.environment(depth),
                9 => {
                    self.by_address = true;
                    let formals = self.pairlist(SexpType::Pairlist, 1);
                    let body = self.pairlist(SexpType::Language, depth - 1);
                    let env = self.environment(depth - 1);
                    self.keep(
                        runtime()
                            .heap
                            .alloc_triple(SexpType::Closure, formals, body, env)
                            .expect("a closure"),
                    )
                }
                10 => {
                    self.by_address = true;
                    let tag = self.object(depth - 1);
                    let protected = self.object(depth - 1);
                    self.keep(
                        runtime()
                            .heap
                            .alloc_external_ptr(std::ptr::null_mut(), tag, protected)
                            .expect("a pointer"),
                    )
                }
                _ => {
                    let object = self.keep(
                        runtime()
                            .heap
                            .alloc_triple(SexpType::Object, known.nil, known.nil, known.nil)
                            .expect("an S4 object"),
                    );
                    object.set_gp_bits(gp::S4_OBJECT, true);
                    object
                }
            };
            let is_special = object == known.nil
                || matches!(object.kind(), SexpType::Symbol | SexpType::Environment);
            if !is_special && self.rng.chance(1, 3) {
                let value = self.vector(0);
                let tag = self.symbol();
                let attributes = self.keep(
                    runtime()
                        .heap
                        .alloc_triple(SexpType::Pairlist, value, known.nil, tag)
                        .expect("an attribute"),
                );
                runtime().heap.set_attrib(object, attributes);
                object.set_object(self.rng.chance(1, 2));
            }
            object
        }
    }
}

const FORMATS: [(Format, i32); 6] = [
    (Format::Xdr, 2),
    (Format::Xdr, 3),
    (Format::Binary, 3),
    (Format::Ascii, 2),
    (Format::Ascii, 3),
    (Format::AsciiHex, 3),
];

/// Random objects round-trip in every format: reading what was written and writing it again
/// gives the same bytes (the stream fixes every serialized fact, back-references included), and
/// objects without environments or pointers read back `identical` from the exact formats.
#[test]
fn random_objects_round_trip() {
    let _session = session();
    let iterations = fuzz_iterations(300);
    for seed in 0..iterations as u64 {
        let mark = runtime().heap.handle_mark();
        let mut generator = Generator::new(seed);
        if seed % 16 == 15 {
            runtime().heap.policy_mut().stress = GcStress::EveryAllocation;
        }
        let object = unsafe { generator.object(4) };
        for (format, version) in FORMATS {
            let written = unsafe { to_bytes(object, format, version) }
                .unwrap_or_else(|error| panic!("seed {seed}: cannot write: {error:?}"));
            let read = unsafe { from_bytes(&written) }
                .unwrap_or_else(|error| panic!("seed {seed} {format:?}: cannot read: {error:?}"));
            let again = unsafe { to_bytes(read, format, version) }.expect("a second write");
            assert!(
                again == written,
                "seed {seed} {format:?} v{version}: the round trip changed the stream\n{}\n{}",
                show(&written, format),
                show(&again, format)
            );
            // `%.16g` cannot hold every double, so only the exact formats read back identical.
            if !generator.by_address && format != Format::Ascii {
                let same = unsafe { R::vectors::identical(object, read, 0) }.expect("identical");
                assert!(same, "seed {seed} {format:?}: the object changed");
            }
        }
        runtime().heap.policy_mut().stress = GcStress::Off;
        runtime().heap.release_handles(mark);
    }
}

#[test]
fn shared_symbols_and_environments_are_written_once() {
    let _session = session();
    unsafe {
        let heap = &mut runtime().heap;
        let known = *heap.known();
        let symbol = heap.install(b"shared").expect("a symbol");
        let env = heap.alloc_environment(known.global_env).expect("an env");
        heap.root(env);
        heap.define(env, symbol, env).expect("a cycle");
        let list = heap.alloc_vector(SexpType::List, 3).expect("a list");
        heap.root(list);
        heap.set_element(list, 0, symbol);
        heap.set_element(list, 1, env);
        heap.set_element(list, 2, symbol);
        let written = to_bytes(list, Format::Xdr, 2).expect("written");
        let read = from_bytes(&written).expect("read");
        assert_eq!(read.element(0), symbol);
        assert_eq!(read.element(2), symbol);
        let copy = read.element(1);
        assert_ne!(copy, env);
        let cell = runtime().heap.frame_cell(copy, symbol).expect("bound");
        assert_eq!(cell.cdr(), copy);
        // The name `shared` appears once: later uses are back-references.
        let occurrences = written.windows(6).filter(|w| *w == b"shared").count();
        assert_eq!(occurrences, 1);
    }
}

#[test]
fn persistence_hooks_replace_and_restore_objects() {
    let _session = session();
    unsafe {
        let heap = &mut runtime().heap;
        let known = *heap.known();
        let env = heap.alloc_environment(known.empty_env).expect("an env");
        heap.root(env);
        let list = heap.alloc_vector(SexpType::List, 2).expect("a list");
        heap.root(list);
        heap.set_element(list, 0, env);
        heap.set_element(list, 1, env);
        let mut named = 0;
        let mut name_env = |object: Sexp| {
            assert_eq!(object, env);
            named += 1;
            let heap = &mut runtime().heap;
            let name = heap.alloc_vector(SexpType::String, 1)?;
            let text = heap.mk_char(b"env::7", Encoding::Native)?;
            heap.set_element(name, 0, text);
            Ok(name)
        };
        let mut bytes = Vec::new();
        let mut sink = |chunk: &[u8]| {
            bytes.extend_from_slice(chunk);
            Ok(())
        };
        let hook: PersistHook = &mut name_env;
        R::serialize::serialize(list, Format::Xdr, 3, &mut sink, Some(hook), R::serialize::Chunking::Buffered).expect("written");
        assert_eq!(named, 2, "the hook names every occurrence, as in GNU R");
        assert!(from_bytes(&bytes).is_err(), "restoring needs a hook");
        let mut restored = 0;
        let replacement = runtime()
            .heap
            .alloc_environment(known.global_env)
            .expect("an env");
        runtime().heap.root(replacement);
        let mut restore = |name: Sexp| {
            assert_eq!(name.element(0).bytes(), b"env::7");
            restored += 1;
            Ok(replacement)
        };
        let mut source = R::serialize::SliceSource::new(&bytes);
        let (read, _) =
            R::serialize::unserialize(&mut source, Some(&mut restore)).expect("restored");
        assert_eq!(restored, 2);
        assert_eq!(read.element(0), replacement);
        assert_eq!(read.element(1), replacement);
    }
}

#[test]
fn rds_files_read_in_every_compression() {
    let _session = session();
    let path = Path::new("/usr/lib/R/library/stats/R/stats.rdx");
    if !path.exists() {
        return;
    }
    unsafe {
        let index = read_rds(path).expect("the index");
        assert_eq!(index.kind(), SexpType::List);
    }
    if Command::new("Rscript").arg("--version").output().is_err() {
        return;
    }
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"));
    for compression in ["gzip", "bzip2", "xz", "FALSE"] {
        let file = directory.join(format!("serialize-{compression}.rds"));
        let compress = if compression == "FALSE" {
            "FALSE".to_owned()
        } else {
            format!("\"{compression}\"")
        };
        let script = format!(
            "saveRDS(list(a = c(1.5, NA), b = letters[1:3]), {:?}, compress = {compress})",
            file.display().to_string()
        );
        let status = Command::new("Rscript")
            .args(["-e", &script])
            .status()
            .expect("Rscript runs");
        assert!(status.success());
        unsafe {
            let object = read_rds(&file).expect("an rds file");
            assert_eq!(object.kind(), SexpType::List, "{compression}");
            assert_eq!(object.element(1).length(), 3);
        }
    }
}

/// Every object in the lazy-load databases of a few installed packages reads, including the
/// compiled closures, whose bodies come back as expressions.
#[test]
fn installed_lazy_load_databases_read() {
    let _session = session();
    let library = Path::new("/usr/lib/R/library");
    if !library.exists() {
        return;
    }
    let mut closures = 0;
    for (package, directory, base) in [
        ("stats", "R", "stats"),
        ("utils", "R", "utils"),
        ("base", "R", "base"),
        ("datasets", "data", "Rdata"),
    ] {
        let index_path = library
            .join(package)
            .join(directory)
            .join(format!("{base}.rdx"));
        let database = index_path.with_extension("rdb");
        if !index_path.exists() {
            continue;
        }
        unsafe {
            let mark = runtime().heap.handle_mark();
            let index = read_rds(&index_path).expect("the index");
            let names = R::attributes::raw_attribute(index, R::globals::R_NamesSymbol.get());
            let element = |name: &str| {
                (0..index.length() as usize)
                    .find(|&at| names.element(at).bytes() == name.as_bytes())
                    .map(|at| index.element(at))
                    .expect("an index element")
            };
            let compressed = element("compressed");
            let compression = LazyLoadCompression::from_index_value(match compressed.kind() {
                SexpType::Logical | SexpType::Integer => compressed.slice::<i32>()[0],
                SexpType::Real => compressed.slice::<f64>()[0] as i32,
                kind => panic!("compressed is a {kind:?}"),
            });
            for group in ["variables", "references"] {
                let entries = element(group);
                for at in 0..entries.length() as usize {
                    let entry = entries.element(at);
                    let [offset, length] =
                        <[i32; 2]>::try_from(entry.slice::<i32>()).expect("an offset and a length");
                    let entry_mark = runtime().heap.handle_mark();
                    let mut restore = |_name: Sexp| {
                        let known = *runtime().heap.known();
                        Ok(runtime().heap.alloc_environment(known.global_env)?)
                    };
                    let object = lazyload_fetch(
                        &database,
                        offset as u64,
                        length as usize,
                        compression,
                        Some(&mut restore),
                    )
                    .unwrap_or_else(|error| panic!("{package} entry {at}: {error:?}"));
                    if object.kind() == SexpType::Closure {
                        closures += 1;
                        assert_ne!(object.cdr().kind(), SexpType::Bytecode);
                    }
                    runtime().heap.release_handles(entry_mark);
                }
            }
            runtime().heap.release_handles(mark);
        }
    }
    if library.join("stats").exists() {
        assert!(closures > 1000, "only {closures} closures read");
    }
}

/// Corrupted streams fail with an error: never a panic, a crash or a runaway allocation.
#[test]
fn corrupted_streams_fail_cleanly() {
    let _session = session();
    let cases = cases();
    let iterations = fuzz_iterations(3000);
    let mut rng = SplitMix64(0x5E81_A112);
    for iteration in 0..iterations {
        let source = &cases[rng.below(cases.len())].stream;
        let mut bytes = source.clone();
        for _ in 0..1 + rng.below(4) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            match rng.below(5) {
                0 => bytes[at] ^= 1 << rng.below(8),
                1 => bytes[at] = rng.next_u64() as u8,
                2 => bytes.truncate(at),
                3 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, rng.next_u64() as u8),
            }
        }
        let mark = runtime().heap.handle_mark();
        if iteration % 64 == 63 {
            runtime().heap.policy_mut().stress = GcStress::EveryAllocation;
        }
        let outcome = std::panic::catch_unwind(|| unsafe { from_bytes(&bytes) });
        runtime().heap.policy_mut().stress = GcStress::Off;
        runtime().heap.release_handles(mark);
        assert!(
            outcome.is_ok(),
            "iteration {iteration} panicked on {}",
            show(&bytes, Format::Xdr)
        );
    }
}
