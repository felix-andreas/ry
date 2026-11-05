//! Code images: the compiled code of an installed package's closures, written beside each
//! lazy-load database when the package is installed (`R/<package>.ryc` beside
//! `R/<package>.rdb`), so that a closure fetched from the database runs compiled code from its
//! first call instead of compiling at its second.
//!
//! An image is build output, never a cache. It is stamped with the runtime build that wrote it,
//! and a different build ignores it, so nothing ever checks whether its code is still current.
//! Each record carries the length and hash of the database record it was compiled from, so an
//! image that does not match its database (a package reinstalled by GNU R over ry's install, say)
//! is ignored record by record rather than trusted.
//!
//! A record's closures are named by the order the stream reader makes them
//! ([`serialize::unserialize_with_closures`]), which depends on the record's bytes alone, and a
//! code object is encoded against its closure's body ([`serialize::encode_code`]), so the image
//! never holds a second copy of the package's source.
//!
//! An install that trains the image (`R CMD INSTALL --ry-train=...`) first runs the package's
//! tests, its examples or a given script, and then writes each code object with the type feedback,
//! entry and back-edge counters and optimizing-tier state the run left, so a body that was hot in
//! training is compiled by the optimizing tier at its first calls in a new process.
//!
//! The file layout, all integers little-endian:
//!
//! | Field | Content |
//! |---|---|
//! | magic | `RYCODE\0\x01` |
//! | build | the runtime's identity, as a length-prefixed blob |
//! | flags | `u32`, bit 0 set when the image was trained |
//! | database length | `u64`, the size of the `.rdb` file the image was written for |
//! | record count | `u32` |
//! | records | per record: database offset `u64`, record length `u32`, record hash `u64`, payload offset `u64`, payload length `u32`, payload hash `u64`; sorted by database offset |
//! | native code | offset `u64` and length `u64` of a section reserved for install-time native code; both 0 |
//! | payloads | per record: the closure count, then per closure the length of its code (0 for none) and the code |

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        bytecode, interp,
        image::ImageFile,
        serialize::{
            self, CodeInput, SourceNumbers, Unencodable, decode_code, encode_code, walk_source,
            write_varint,
        },
        unwind::{R, Unwind, error},
        values::{self, length, nil},
    },
    heap::{
        image::{Reader, Writer},
        object::{Sexp, SexpType},
    },
    rustc_hash::FxHashMap,
    std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryCodeImageRecord", 1, do_record).invisible(),
    internal("ryCodeImageWrite", 1, do_write),
];

const MAGIC: &[u8; 8] = b"RYCODE\x00\x01";

/// The bytes of one record table entry.
const ENTRY_SIZE: usize = 40;

/// The flag bit of an image written after a training run.
const TRAINED: u32 = 1;

/// The code image of the lazy-load database `database`: its path with `.rdb` replaced by `.ryc`.
pub fn path(database: &str) -> PathBuf {
    Path::new(database.strip_suffix(".rdb").unwrap_or(database)).with_extension("ryc")
}

/// The code images this process opened, by database path; `None` for a database without a usable
/// image, so its absence is looked up once.
#[derive(Default)]
pub struct State {
    images: FxHashMap<String, Option<Image>>,
    /// The fetches an install records while it loads the package it is writing images for.
    recording: Option<Recording>,
    /// The runtime build images are stamped with; empty when the host named none, which turns
    /// images off.
    pub build: &'static str,
}

struct Image {
    file: ImageFile,
    entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub length: u32,
    pub hash: u64,
    pub payload_offset: u64,
    pub payload_length: u32,
    pub payload_hash: u64,
}

struct Recording {
    /// Only databases under this directory are recorded: the package being installed, not its
    /// imports.
    directory: String,
    /// Per database, per record offset: the record's length and hash, and a list holding each
    /// closure the record made followed by the body it was read with.
    records: BTreeMap<String, BTreeMap<u64, Recorded>>,
}

struct Recorded {
    length: u32,
    hash: u64,
    closures: Sexp,
}

/// Forgets the image of `database`, which `lazyLoadDBflush` calls when the database changes.
pub fn forget(database: &str) {
    interp().code_image.images.remove(database);
}

/// A lazy-load record as the image identifies it: where it starts in its database, and the length
/// and hash of its bytes as stored.
#[derive(Clone, Copy)]
pub struct Record {
    offset: u64,
    length: usize,
    hash: u64,
}

impl Record {
    pub fn of(offset: u64, bytes: &[u8]) -> Record {
        Record {
            offset,
            length: bytes.len(),
            hash: hash(bytes),
        }
    }
}

/// Whether a fetch from `database` goes through [`read_record`]: the database has an image this
/// build can use, or an install is recording it. Every other fetch skips hashing its record.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn wants(database: &str) -> R<bool> {
    let state = &mut interp().code_image;
    if state.build.is_empty() {
        return Ok(false);
    }
    if state
        .recording
        .as_ref()
        .is_some_and(|recording| database.starts_with(&recording.directory))
    {
        return Ok(true);
    }
    if !state.images.contains_key(database) {
        let image = open(database, state.build)?;
        state.images.insert(database.to_owned(), image);
    }
    Ok(matches!(state.images.get(database), Some(Some(_))))
}

/// Reads one lazy-load record of `database` that [`wants`] accepted: `data` is the decompressed
/// stream of `record`. The closures the record makes get their compiled code from the database's
/// image, when it has code for exactly this record.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn read_record(database: &str, record: Record, data: &[u8], hook: Sexp) -> R<Sexp> {
    let state = &mut interp().code_image;
    if let Some(recording) = &state.recording
        && database.starts_with(&recording.directory)
    {
        let (value, closures) = serialize::unserialize_with_closures(data, hook)?;
        crate::heap().root(value);
        record_fetch(database, record, &closures)?;
        return Ok(value);
    }
    let Some(payload) = payload(database, record)? else {
        return serialize::unserialize(data, hook);
    };
    let (value, closures) = serialize::unserialize_with_closures(data, hook)?;
    crate::heap().root(value);
    attach(&payload, &closures).map_err(|failure| {
        error(format!(
            "the code image {} is corrupt ({}); reinstall the package",
            path(database).display(),
            match failure {
                Unwind::Error(failure) => failure.message,
                _ => "interrupted".to_owned(),
            }
        ))
    })?;
    Ok(value)
}

/// A copy of the payload the image of `database` holds for `record`, when it was written for
/// exactly this record. The copy is small, and the image's map stays valid only until the image is
/// forgotten, which the record's own hook could do.
unsafe fn payload(database: &str, record: Record) -> R<Option<Vec<u8>>> {
    let Some(Some(image)) = interp().code_image.images.get(database) else {
        return Ok(None);
    };
    let Ok(position) = image
        .entries
        .binary_search_by_key(&record.offset, |entry| entry.offset)
    else {
        return Ok(None);
    };
    let entry = image.entries[position];
    if entry.length as usize != record.length || entry.hash != record.hash {
        return Ok(None);
    }
    let corrupt = || {
        error(format!(
            "the code image {} is corrupt; reinstall the package",
            path(database).display()
        ))
    };
    let start = usize::try_from(entry.payload_offset).map_err(|_| corrupt())?;
    let bytes = image
        .file
        .bytes()
        .get(start..start.saturating_add(entry.payload_length as usize))
        .ok_or_else(corrupt)?;
    if hash(bytes) != entry.payload_hash {
        return Err(corrupt());
    }
    Ok(Some(bytes.to_vec()))
}

/// Opens the image of `database`. `None` when there is none, `RY_CODE_IMAGE=0` turns images off,
/// or another build wrote it.
fn open(database: &str, build: &str) -> R<Option<Image>> {
    if std::env::var_os("RY_CODE_IMAGE").is_some_and(|value| value == "0") {
        return Ok(None);
    }
    let image_path = path(database);
    let file = match ImageFile::open(&image_path, false) {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(failure) => {
            return Err(error(format!(
                "cannot read {}: {failure}",
                image_path.display()
            )));
        }
    };
    let database_length = std::fs::metadata(database)
        .map_err(|failure| error(format!("cannot read {database}: {failure}")))?
        .len();
    match parse(file.bytes(), build, database_length) {
        Ok(Some(entries)) => Ok(Some(Image { file, entries })),
        Ok(None) => Ok(None),
        Err(message) => Err(error(format!(
            "the code image {} is corrupt ({message}); reinstall the package",
            image_path.display()
        ))),
    }
}

/// The record table of an image, or `None` when it was written by another build or for another
/// version of the database.
pub fn parse(bytes: &[u8], build: &str, database_length: u64) -> Result<Option<Vec<Entry>>, String> {
    let mut input = Reader { bytes };
    let failed = |failure: heap::ImageError| failure.to_string();
    if input.take(MAGIC.len()).map_err(failed)? != MAGIC {
        // Another layout version is another build's output.
        return Ok(None);
    }
    if input.blob().map_err(failed)? != build.as_bytes() {
        return Ok(None);
    }
    let _flags = input.u32().map_err(failed)?;
    if input.u64().map_err(failed)? != database_length {
        return Ok(None);
    }
    let count = input.u32().map_err(failed)? as usize;
    if count > bytes.len() / ENTRY_SIZE {
        return Err("the record table is longer than the file".to_owned());
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = Entry {
            offset: input.u64().map_err(failed)?,
            length: input.u32().map_err(failed)?,
            hash: input.u64().map_err(failed)?,
            payload_offset: input.u64().map_err(failed)?,
            payload_length: input.u32().map_err(failed)?,
            payload_hash: input.u64().map_err(failed)?,
        };
        if entries
            .last()
            .is_some_and(|previous: &Entry| previous.offset >= entry.offset)
        {
            return Err("the record table is not sorted".to_owned());
        }
        let end = entry
            .payload_offset
            .checked_add(u64::from(entry.payload_length));
        if end.is_none_or(|end| end > bytes.len() as u64) {
            return Err("a payload lies outside the file".to_owned());
        }
        entries.push(entry);
    }
    let _native_offset = input.u64().map_err(failed)?;
    let _native_length = input.u64().map_err(failed)?;
    Ok(Some(entries))
}

/// Gives each closure in `closures` the code the payload holds for it.
unsafe fn attach(payload: &[u8], closures: &[Sexp]) -> R<()> {
    let mut input = CodeInput {
        bytes: payload,
        position: 0,
    };
    if input.count()? != closures.len() {
        return Err(error("its closures do not match the database's"));
    }
    let heap = crate::heap();
    for &closure in closures {
        let size = input.count()?;
        if size == 0 {
            continue;
        }
        let end = input
            .position
            .checked_add(size)
            .filter(|&end| end <= payload.len())
            .ok_or_else(|| error("a code object runs past its record"))?;
        let body = closure.cdr();
        let mut code_input = CodeInput {
            bytes: payload.get(..end).ok_or_else(|| error("a code object runs past its record"))?,
            position: input.position,
        };
        input.position = end;
        if body.kind() != SexpType::Language {
            return Err(error("it holds code for a closure whose body is not a call"));
        }
        let code = decode_code(&mut code_input, &walk_source(body))?;
        if code_input.position != end || bytecode::source_of(code) != body {
            return Err(error("a code object does not match its closure"));
        }
        heap.set_cdr(closure, code);
    }
    if input.position != payload.len() {
        return Err(error("it holds more than its closures"));
    }
    Ok(())
}

/// The FNV-1a hash of `bytes`, the same in every build.
fn hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

unsafe fn record_fetch(database: &str, record: Record, closures: &[Sexp]) -> R<()> {
    let heap = crate::heap();
    let Some(recording) = &mut interp().code_image.recording else {
        return Ok(());
    };
    let records = recording.records.entry(database.to_owned()).or_default();
    if records.contains_key(&record.offset) {
        return Ok(());
    }
    let list = values::alloc(SexpType::List, closures.len() * 2)?;
    for (index, &closure) in closures.iter().enumerate() {
        heap.set_element(list, index * 2, closure);
        heap.set_element(list, index * 2 + 1, closure.cdr());
    }
    heap.add_global_root(list);
    let length = u32::try_from(record.length)
        .map_err(|_| error(format!("a record of {database} is larger than 4GB")))?;
    records.insert(
        record.offset,
        Recorded {
            length,
            hash: record.hash,
            closures: list,
        },
    );
    Ok(())
}

/// `ryCodeImageRecord(directory)`: from now on, records every lazy-load record fetched from a
/// database under `directory` with the closures it made, for [`do_write`].
unsafe fn do_record(call: &Call<'_>) -> R<Sexp> {
    let directory = values::require_string(call.arg(0), "directory")?;
    let directory = std::path::absolute(&directory)
        .map_err(|failure| error(format!("cannot resolve {directory}: {failure}")))?;
    interp().code_image.recording = Some(Recording {
        directory: directory.display().to_string(),
        records: BTreeMap::new(),
    });
    Ok(nil())
}

/// `ryCodeImageWrite(trained)`: compiles every recorded closure and writes each database's
/// image, keeping the run's feedback when `trained`. Returns how many closures were recorded and
/// written, and how many were left out for each reason.
unsafe fn do_write(call: &Call<'_>) -> R<Sexp> {
    let trained = values::as_logical_scalar(call.arg(0)) == 1;
    let state = &mut interp().code_image;
    let Some(recording) = state.recording.take() else {
        return Err(error("no fetches were recorded"));
    };
    if state.build.is_empty() {
        return Err(error("this runtime has no build identity to stamp images with"));
    }
    let build = state.build;
    let mut summary = Summary::default();
    for (database, records) in &recording.records {
        write_image(database, records, build, trained, &mut summary)?;
    }
    let mut items = vec![
        ("closures", summary.closures),
        ("written", summary.written),
    ];
    for (reason, count) in &summary.skipped {
        items.push((reason, *count));
    }
    let names: Vec<&str> = items.iter().map(|(name, _)| *name).collect();
    let counts: Vec<i32> = items
        .iter()
        .map(|(_, count)| i32::try_from(*count).unwrap_or(i32::MAX))
        .collect();
    let counts = values::integer_vector(&counts)?;
    crate::heap().root(counts);
    values::set_attr(counts, values::symbols().names, values::string_vector(&names)?)?;
    Ok(counts)
}

#[derive(Default)]
struct Summary {
    closures: usize,
    written: usize,
    skipped: BTreeMap<&'static str, usize>,
}

unsafe fn write_image(
    database: &str,
    records: &BTreeMap<u64, Recorded>,
    build: &str,
    trained: bool,
    summary: &mut Summary,
) -> R<()> {
    let database_length = std::fs::metadata(database)
        .map_err(|failure| error(format!("cannot read {database}: {failure}")))?
        .len();
    let mut payloads = Vec::with_capacity(records.len());
    for recorded in records.values() {
        let mut payload = Vec::new();
        let count = length(recorded.closures) / 2;
        write_varint(&mut payload, count as u64);
        for index in 0..count {
            let closure = recorded.closures.element(index * 2);
            let body = recorded.closures.element(index * 2 + 1);
            summary.closures += 1;
            match compiled_code(closure, body, trained) {
                Ok(code) => {
                    summary.written += 1;
                    write_varint(&mut payload, code.len() as u64);
                    payload.extend_from_slice(&code);
                }
                Err(Unencodable(reason)) => {
                    *summary.skipped.entry(reason).or_default() += 1;
                    write_varint(&mut payload, 0);
                }
            }
        }
        payloads.push(payload);
    }
    let table: Vec<(u64, u32, u64)> = records
        .iter()
        .map(|(offset, recorded)| (*offset, recorded.length, recorded.hash))
        .collect();
    let bytes = image_bytes(build, trained, database_length, &table, &payloads)?;
    let target = path(database);
    let staged = target.with_extension("ryc.partial");
    let failed = |failure: std::io::Error| {
        error(format!("cannot write {}: {failure}", target.display()))
    };
    std::fs::write(&staged, &bytes).map_err(failed)?;
    std::fs::rename(&staged, &target).map_err(failed)?;
    forget(database);
    Ok(())
}

/// The bytes of an image for a database of `database_length` bytes: `records` holds each
/// record's offset, length and hash, sorted by offset, and `payloads` the payload of each.
pub fn image_bytes(
    build: &str,
    trained: bool,
    database_length: u64,
    records: &[(u64, u32, u64)],
    payloads: &[Vec<u8>],
) -> R<Vec<u8>> {
    let mut out = Writer::default();
    out.bytes.extend_from_slice(MAGIC);
    out.blob(build.as_bytes());
    out.u32(if trained { TRAINED } else { 0 });
    out.u64(database_length);
    out.u32(u32::try_from(records.len()).map_err(|_| error("an image holds too many records"))?);
    let mut payload_offset = (out.bytes.len() + records.len() * ENTRY_SIZE + 16) as u64;
    for (&(offset, length, hash_value), payload) in records.iter().zip(payloads) {
        out.u64(offset);
        out.u32(length);
        out.u64(hash_value);
        out.u64(payload_offset);
        out.u32(u32::try_from(payload.len()).map_err(|_| error("a payload is larger than 4GB"))?);
        out.u64(hash(payload));
        payload_offset += payload.len() as u64;
    }
    // The section reserved for install-time native code, empty until the native pipeline
    // writes one.
    out.u64(0);
    out.u64(0);
    for payload in payloads {
        out.bytes.extend_from_slice(payload);
    }
    Ok(out.bytes)
}

/// The encoded code of `closure`, compiling it now if it has not compiled yet.
unsafe fn compiled_code(closure: Sexp, body: Sexp, trained: bool) -> Result<Vec<u8>, Unencodable> {
    if closure.kind() != SexpType::Closure {
        return Err(Unencodable("not a closure"));
    }
    if bytecode::source_body(closure) != body {
        return Err(Unencodable("a body replaced after loading"));
    }
    if body.kind() != SexpType::Language {
        return Err(Unencodable("a body that is not a call"));
    }
    let code = match bytecode::compiled_body(closure) {
        Ok(code) if code.kind() == SexpType::Bytecode => code,
        _ => return Err(Unencodable("a body the compiler declines")),
    };
    if bytecode::compile_nested_functions(code, closure.tag()).is_err() {
        return Err(Unencodable("a nested function the compiler declines"));
    }
    let source: SourceNumbers = walk_source(body).numbers();
    let mut out = Vec::new();
    encode_code(code, &source, trained, &mut out)?;
    Ok(out)
}

/// The encoded form of `code`, compiled from `body`, or `None` when it holds something the image
/// cannot encode.
///
/// # Safety
/// `code` is a live code object compiled from `body`.
pub unsafe fn encode(code: Sexp, body: Sexp, trained: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    encode_code(code, &walk_source(body).numbers(), trained, &mut out).ok()?;
    Some(out)
}

/// Encodes `code` and decodes it again against the same body, for tests and the fuzz harness:
/// the decoded object must equal the original as the interpreter reads it.
///
/// # Safety
/// `code` is a live code object compiled from `body`.
pub unsafe fn round_trip(code: Sexp, body: Sexp, trained: bool) -> R<Option<Sexp>> {
    let source = walk_source(body);
    let mut out = Vec::new();
    if encode_code(code, &source.numbers(), trained, &mut out).is_err() {
        return Ok(None);
    }
    let mut input = CodeInput {
        bytes: &out,
        position: 0,
    };
    let decoded = decode_code(&mut input, &source)?;
    if input.position != out.len() {
        return Err(error("the decoder stopped before the end of the code"));
    }
    Ok(Some(decoded))
}

/// Decodes arbitrary bytes as a code object against `body`, for the fuzz harness: it must fail
/// with an error, never panic or read out of bounds.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn decode_untrusted(bytes: &[u8], body: Sexp) -> R<Sexp> {
    let mut input = CodeInput { bytes, position: 0 };
    decode_code(&mut input, &walk_source(body))
}
