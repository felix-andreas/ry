//! Code images: the compiled code of an installed package's closures, written beside each
//! lazy-load database when the package is installed (`R/<package>.ryc` beside
//! `R/<package>.rdb`), so that a closure fetched from the database takes its code from the image
//! instead of compiling it, and a trained image brings the type feedback and counters of its
//! training run.
//!
//! A fetch does not decode anything: it notes where each closure's code lies in the image, and
//! the call that would compile the closure decodes it instead ([`take_pending`]). Decoding costs
//! about as much as compiling, so it pays only where compiling would happen: a namespace fetches
//! many closures it never calls (every S3 method it registers) and calls many only once.
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
//! tests, its examples or a given script, and then writes each code object with the type feedback
//! and the entry and back-edge counters the run left, so a body that was hot in training is
//! compiled by the optimizing tier at its first calls in a new process.
//!
//! The image only saves compiling, so a damaged one warns once and is ignored ([`give_up`]), and
//! decoded code passes [`bytecode::verify`] before tier 0 runs it without bounds checks.
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
//! | native code | offset `u64` and length `u64` of the native section, both 0 for none |
//! | payloads | per record: the closure count, then per closure the length of its code (0 for none) and the code |
//! | native section | install-time machine code (`ssa::section`), its code at a multiple of 16 KiB in the file |
//!
//! The native section holds machine code an install compiled for closures whose facts need no
//! run (see [`NativeHooks`]). The first fetch from a database maps it read-only from the file,
//! so every process that loads the package shares its pages, and each closure a record makes
//! that has native code is marked so that its calls try that code first.

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
        cell::Cell,
        collections::BTreeMap,
        path::{Path, PathBuf},
        rc::Rc,
        sync::OnceLock,
    },
};

/// What install-time machine code needs from the compiler, which this crate cannot depend on.
/// The host sets it once ([`set_native_hooks`]); without it, installs write no native code and
/// loads ignore what images hold.
#[derive(Clone, Copy)]
pub struct NativeHooks {
    /// Compiles what it can of a package's closures to machine code. With `explain`, it prints
    /// what kept each closure in the interpreter.
    pub compile: unsafe fn(&[NativeCandidate], bool) -> Option<NativeBuild>,
    /// Maps the native section `bytes`, found at `offset` of the image file `path`, and returns
    /// a handle for [`NativeHooks::attach`]; `None` when this processor cannot run it.
    pub load: unsafe fn(&Path, u64, &[u8]) -> Result<Option<u32>, String>,
    /// Gives the closures a record made, in the order its reader made them, the native code the
    /// section `handle` holds for record `entry`.
    pub attach: unsafe fn(u32, u32, &[Sexp]),
    /// Calls `function` through its native code when the arguments fit it; `None` runs the call
    /// in the interpreter.
    pub call: unsafe fn(Sexp, &[crate::builtins::Arg]) -> Option<R<Sexp>>,
}

/// A closure an install offers for compiling: the record entry of the image it belongs to and
/// its position among the closures that record makes, which together name it in the image.
#[derive(Clone, Copy)]
pub struct NativeCandidate {
    pub entry: u32,
    pub ordinal: u32,
    pub closure: Sexp,
}

/// The machine code an install compiled: how many functions it holds, and its section's bytes
/// for a given offset in the image file.
pub struct NativeBuild {
    pub functions: usize,
    pub encode: Box<dyn Fn(u64) -> Vec<u8>>,
}

static NATIVE_HOOKS: OnceLock<NativeHooks> = OnceLock::new();

/// Sets the compiler's hooks for install-time machine code. Only the first call counts.
pub fn set_native_hooks(hooks: NativeHooks) {
    if NATIVE_HOOKS.set(hooks).is_err() {
        // Every host sets the same hooks, so a second call changes nothing.
    }
}

pub fn native_hooks() -> Option<&'static NativeHooks> {
    NATIVE_HOOKS.get()
}

thread_local! {
    static UNLOCKS: Cell<u64> = const { Cell::new(0) };
}

/// Notes that a binding was unlocked, which may let it change: native code checked against the
/// bindings it calls checks them again.
pub fn binding_unlocked() {
    UNLOCKS.with(|count| count.set(count.get() + 1));
}

/// How many bindings were ever unlocked: a check of locked bindings holds while this stays.
pub fn unlocks() -> u64 {
    UNLOCKS.with(Cell::get)
}

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryCodeImageRecord", 1, do_record).invisible(),
    internal("ryCodeImageWrite", 2, do_write),
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
    images: FxHashMap<String, Option<Rc<Image>>>,
    /// Per closure body fetched from an image but not called yet, where its code lies. Each body
    /// is preserved while it waits, so its address cannot be reused by an unrelated object.
    pending: FxHashMap<Sexp, Pending>,
    /// The fetches an install records while it loads the package it is writing images for.
    recording: Option<Recording>,
    /// The runtime build images are stamped with; empty when the host named none, which turns
    /// images off.
    pub build: &'static str,
}

struct Image {
    file: ImageFile,
    path: PathBuf,
    entries: Vec<Entry>,
    native: Cell<NativeSection>,
    /// One bit per entry, set once its payload's hash has been checked.
    verified: Vec<Cell<u64>>,
}

/// Where the code of a fetched closure lies: `start..end` of the image's bytes, inside the
/// payload of `entry`. The code was compiled for `formals`, the closure's formals as fetched, and
/// the compiler's output depends on them (which names are local, what a call can change), so a
/// closure that shares the body under other formals (`formals<-`, `as.function`) compiles its
/// own. `formals` is preserved while the entry waits, like the body.
struct Pending {
    image: Rc<Image>,
    database: Rc<str>,
    formals: Sexp,
    entry: u32,
    start: u32,
    end: u32,
}

/// Where an image's native code stands in this process.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeSection {
    /// The image has none, or this process cannot use it.
    Absent,
    /// At this offset and length of the file, not mapped yet.
    Waiting(u64, u64),
    /// Mapped, under this handle.
    Loaded(u32),
}

/// An image's record table and where its native section lies, 0 and 0 for none.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    pub native_offset: u64,
    pub native_length: u64,
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

/// Forgets the image of `database` and the code waiting in it, which `lazyLoadDBflush` calls when
/// the database changes or its namespace unloads.
pub fn forget(database: &str) {
    let state = &mut interp().code_image;
    state.images.remove(database);
    drop_pending(state, database);
}

/// Drops the code waiting in the image of `database`, releasing what each entry preserved.
fn drop_pending(state: &mut State, database: &str) {
    let heap = crate::heap();
    state.pending.retain(|&body, pending| {
        if &*pending.database != database {
            return true;
        }
        heap.release(body);
        heap.release(pending.formals);
        false
    });
}

/// Stops using the image of `database` after it turned out damaged, with one warning: the image
/// only saves compiling, so its package goes on loading and its functions compile as they run.
unsafe fn give_up(database: &str, message: &str) -> R<()> {
    let state = &mut interp().code_image;
    state.images.insert(database.to_owned(), None);
    drop_pending(state, database);
    crate::conditions::warning(format!(
        "cannot use the code image {} ({message}), so its functions compile as they run; \
         reinstall the package to repair it",
        path(database).display()
    ))
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
        .is_some_and(|recording| Path::new(database).starts_with(&recording.directory))
    {
        return Ok(true);
    }
    if !state.images.contains_key(database) {
        match open(database, state.build) {
            Ok(image) => {
                state.images.insert(database.to_owned(), image);
            }
            Err(message) => give_up(database, &message)?,
        }
    }
    Ok(matches!(
        interp().code_image.images.get(database),
        Some(Some(_))
    ))
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
        && Path::new(database).starts_with(&recording.directory)
    {
        let (value, closures) = serialize::unserialize_with_closures(data, hook)?;
        crate::heap().root(value);
        record_fetch(database, record, &closures)?;
        return Ok(value);
    }
    let Some((image, entry)) = matching_entry(database, record) else {
        return serialize::unserialize(data, hook);
    };
    let (value, closures) = serialize::unserialize_with_closures(data, hook)?;
    crate::heap().root(value);
    if let Err(message) = note_pending(&image, entry, database, &closures) {
        give_up(database, &message)?;
    }
    attach_native(&image, entry, database, &closures)?;
    Ok(value)
}

/// The compiled code of `function`, whose body `body` was fetched from an image and has not
/// compiled yet, decoded now and installed as its body. `None` when the body has no code waiting
/// for these formals, or the image turned out damaged; the caller then compiles.
///
/// # Safety
/// Runs on the interpreter's thread; `body` is `function`'s body.
pub unsafe fn take_pending(function: Sexp, body: Sexp) -> R<Option<Sexp>> {
    let state = &mut interp().code_image;
    if state.pending.is_empty() {
        return Ok(None);
    }
    // Another closure with the same body keeps the entry for the closure it was written for.
    if state
        .pending
        .get(&body)
        .is_none_or(|pending| pending.formals != function.car())
    {
        return Ok(None);
    }
    let Some(pending) = state.pending.remove(&body) else {
        return Ok(None);
    };
    let heap = crate::heap();
    heap.release(body);
    heap.release(pending.formals);
    match decode_pending(&pending, body) {
        Ok(code) => {
            heap.set_cdr(function, code);
            Ok(Some(code))
        }
        Err(message) => {
            give_up(&pending.database, &message)?;
            Ok(None)
        }
    }
}

/// The image of `database` and the index of its entry for `record`, when it was written for
/// exactly this record.
fn matching_entry(database: &str, record: Record) -> Option<(Rc<Image>, usize)> {
    let Some(Some(image)) = interp().code_image.images.get(database) else {
        return None;
    };
    let position = image
        .entries
        .binary_search_by_key(&record.offset, |entry| entry.offset)
        .ok()?;
    let entry = image.entries[position];
    (entry.length as usize == record.length && entry.hash == record.hash)
        .then(|| (Rc::clone(image), position))
}

/// Notes, for each closure in `closures` the payload of `entry` holds code for, where that code
/// lies, without decoding it.
unsafe fn note_pending(
    image: &Rc<Image>,
    entry: usize,
    database: &str,
    closures: &[Sexp],
) -> Result<(), String> {
    let (payload_start, payload) = payload(image, entry)?;
    let mut input = CodeInput {
        bytes: payload,
        position: 0,
    };
    let failed = |failure: Unwind| match failure {
        Unwind::Error(failure) => failure.message,
        _ => "interrupted".to_owned(),
    };
    if input.count().map_err(failed)? != closures.len() {
        return Err("a record's closures do not match the database's".to_owned());
    }
    let mut noted: Vec<(Sexp, Sexp, u32, u32)> = Vec::new();
    for &closure in closures {
        let size = input.count().map_err(failed)?;
        if size == 0 {
            continue;
        }
        let start = input.position;
        let end = start
            .checked_add(size)
            .filter(|&end| end <= payload.len())
            .ok_or("a code object runs past its record")?;
        input.position = end;
        let body = closure.cdr();
        if body.kind() != SexpType::Language {
            return Err("a record holds code for a closure whose body is not a call".to_owned());
        }
        let absolute = |offset: usize| {
            u32::try_from(payload_start + offset).map_err(|_| "a payload lies past 4GB".to_owned())
        };
        noted.push((body, closure.car(), absolute(start)?, absolute(end)?));
    }
    if input.position != payload.len() {
        return Err("a record holds more than its closures".to_owned());
    }
    let database: Rc<str> = Rc::from(database);
    let state = &mut interp().code_image;
    let heap = crate::heap();
    for (body, formals, start, end) in noted {
        let pending = Pending {
            image: Rc::clone(image),
            database: Rc::clone(&database),
            formals,
            entry: entry as u32,
            start,
            end,
        };
        heap.preserve(body);
        heap.preserve(formals);
        if let Some(previous) = state.pending.insert(body, pending) {
            heap.release(body);
            heap.release(previous.formals);
        }
    }
    Ok(())
}

/// Gives the closures of record `entry` the image's native code for them, mapping the native
/// section on first use. A section that cannot be mapped warns once and is ignored.
unsafe fn attach_native(image: &Image, entry: usize, database: &str, closures: &[Sexp]) -> R<()> {
    let Some(hooks) = native_hooks() else {
        return Ok(());
    };
    let handle = match image.native.get() {
        NativeSection::Absent => return Ok(()),
        NativeSection::Loaded(handle) => handle,
        NativeSection::Waiting(offset, length) => {
            image.native.set(NativeSection::Absent);
            let section = usize::try_from(offset)
                .ok()
                .zip(usize::try_from(length).ok())
                .and_then(|(start, length)| image.file.bytes().get(start..start.checked_add(length)?));
            let loaded = match section {
                Some(section) => (hooks.load)(&image.path, offset, section),
                None => Err("the native section lies outside the file".to_owned()),
            };
            match loaded {
                Ok(Some(handle)) => {
                    image.native.set(NativeSection::Loaded(handle));
                    handle
                }
                Ok(None) => return Ok(()),
                Err(message) => {
                    return crate::conditions::warning(format!(
                        "cannot use the machine code in {} ({message}), so its functions run \
                         in the interpreter; reinstall the package to repair it",
                        path(database).display()
                    ));
                }
            }
        }
    };
    (hooks.attach)(handle, entry as u32, closures);
    Ok(())
}

/// The payload of `entry` and where it starts in the image.
fn payload(image: &Image, entry: usize) -> Result<(usize, &[u8]), String> {
    let record = image.entries.get(entry).ok_or("a record is missing")?;
    let start = usize::try_from(record.payload_offset).map_err(|_| "a payload lies past 4GB")?;
    let bytes = image
        .file
        .bytes()
        .get(start..start.saturating_add(record.payload_length as usize))
        .ok_or("a payload lies outside the file")?;
    Ok((start, bytes))
}

/// Decodes the code `pending` points at for `body`, checking the hash of its record's payload the
/// first time code is taken from that record.
unsafe fn decode_pending(pending: &Pending, body: Sexp) -> Result<Sexp, String> {
    let image = &*pending.image;
    let entry = pending.entry as usize;
    let word = image.verified.get(entry / 64).ok_or("a record is missing")?;
    let bit = 1u64 << (entry % 64);
    if word.get() & bit == 0 {
        let (_, bytes) = payload(image, entry)?;
        if image.entries.get(entry).map(|entry| entry.payload_hash) != Some(hash(bytes)) {
            return Err("a payload's hash does not match".to_owned());
        }
        word.set(word.get() | bit);
    }
    let bytes = image
        .file
        .bytes()
        .get(..pending.end as usize)
        .ok_or("a code object runs past its record")?;
    let mut input = CodeInput {
        bytes,
        position: pending.start as usize,
    };
    let code = decode_code(&mut input, &walk_source(body)).map_err(|failure| match failure {
        Unwind::Error(failure) => failure.message,
        _ => "interrupted".to_owned(),
    })?;
    if input.position != pending.end as usize || bytecode::source_of(code) != body {
        return Err("a code object does not match its closure".to_owned());
    }
    Ok(code)
}

/// Opens the image of `database`. `None` when there is none, `RY_CODE_IMAGE=0` turns images off,
/// or another build wrote it; an error when it cannot be read or is damaged.
fn open(database: &str, build: &str) -> Result<Option<Rc<Image>>, String> {
    if std::env::var_os("RY_CODE_IMAGE").is_some_and(|value| value == "0") {
        return Ok(None);
    }
    let file = match ImageFile::open(&path(database), false) {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(failure) => return Err(failure.to_string()),
    };
    let database_length = std::fs::metadata(database)
        .map_err(|failure| format!("cannot read {database}: {failure}"))?
        .len();
    Ok(parse(file.bytes(), build, database_length)?.map(|parsed| {
        let native = if parsed.native_length == 0 {
            NativeSection::Absent
        } else {
            NativeSection::Waiting(parsed.native_offset, parsed.native_length)
        };
        Rc::new(Image {
            verified: (0..parsed.entries.len().div_ceil(64))
                .map(|_| Cell::new(0))
                .collect(),
            file,
            path: path(database),
            entries: parsed.entries,
            native: Cell::new(native),
        })
    }))
}

/// The record table of an image, or `None` when it was written by another build or for another
/// version of the database.
pub fn parse(bytes: &[u8], build: &str, database_length: u64) -> Result<Option<Parsed>, String> {
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
    let native_offset = input.u64().map_err(failed)?;
    let native_length = input.u64().map_err(failed)?;
    if native_offset
        .checked_add(native_length)
        .is_none_or(|end| end > bytes.len() as u64)
    {
        return Err("the native section lies outside the file".to_owned());
    }
    Ok(Some(Parsed {
        entries,
        native_offset,
        native_length,
    }))
}

/// The FNV-1a hash of `bytes`, the same in every build.
pub fn hash(bytes: &[u8]) -> u64 {
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
    let length = u32::try_from(record.length)
        .map_err(|_| error(format!("a record of {database} is larger than 4GB")))?;
    heap.preserve(list);
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
    let previous = interp().code_image.recording.replace(Recording {
        directory: directory.display().to_string(),
        records: BTreeMap::new(),
    });
    if let Some(previous) = previous {
        release_recording(&previous);
    }
    Ok(nil())
}

/// Releases the closures `recording` preserved.
fn release_recording(recording: &Recording) {
    let heap = crate::heap();
    for recorded in recording.records.values().flat_map(BTreeMap::values) {
        heap.release(recorded.closures);
    }
}

/// `ryCodeImageWrite(trained, explain)`: compiles every recorded closure and writes each
/// database's image, keeping the run's feedback when `trained`, and with it the machine code of
/// the closures whose facts need no run. With `explain`, prints what kept each closure from
/// machine code. Returns how many closures were recorded and written, how many native functions
/// were compiled, and how many closures were left out for each reason.
unsafe fn do_write(call: &Call<'_>) -> R<Sexp> {
    let trained = values::as_logical_scalar(call.arg(0)) == 1;
    let explain = values::as_logical_scalar(call.arg(1)) == 1;
    let state = &mut interp().code_image;
    if state.build.is_empty() {
        return Err(error("this runtime has no build identity to stamp images with"));
    }
    let Some(recording) = state.recording.take() else {
        return Err(error("no fetches were recorded"));
    };
    let build = state.build;
    let mut summary = Summary::default();
    let written = recording
        .records
        .iter()
        .try_for_each(|(database, records)| {
            write_image(database, records, build, trained, explain, &mut summary)
        });
    release_recording(&recording);
    written?;
    let mut items = vec![
        ("closures", summary.closures),
        ("written", summary.written),
        ("native", summary.native),
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

/// How many closures an install recorded and wrote, and how many it left out for each reason.
#[derive(Default)]
pub struct Summary {
    pub closures: usize,
    pub written: usize,
    /// Native functions compiled, one per closure and argument types.
    pub native: usize,
    pub skipped: BTreeMap<&'static str, usize>,
}

unsafe fn write_image(
    database: &str,
    records: &BTreeMap<u64, Recorded>,
    build: &str,
    trained: bool,
    explain: bool,
    summary: &mut Summary,
) -> R<()> {
    let database_length = std::fs::metadata(database)
        .map_err(|failure| error(format!("cannot read {database}: {failure}")))?
        .len();
    let mut payloads = Vec::with_capacity(records.len());
    for recorded in records.values() {
        let closures: Vec<(Sexp, Sexp)> = (0..length(recorded.closures) / 2)
            .map(|index| {
                (
                    recorded.closures.element(index * 2),
                    recorded.closures.element(index * 2 + 1),
                )
            })
            .collect();
        payloads.push(record_payload(&closures, trained, summary));
    }
    let mut candidates = Vec::new();
    for (entry, recorded) in records.values().enumerate() {
        for ordinal in 0..length(recorded.closures) / 2 {
            let closure = recorded.closures.element(ordinal * 2);
            if closure.kind() == SexpType::Closure {
                candidates.push(NativeCandidate {
                    entry: entry as u32,
                    ordinal: ordinal as u32,
                    closure,
                });
            }
        }
    }
    let native = native_hooks().and_then(|hooks| (hooks.compile)(&candidates, explain));
    if let Some(native) = &native {
        summary.native += native.functions;
    }
    let table: Vec<(u64, u32, u64)> = records
        .iter()
        .map(|(offset, recorded)| (*offset, recorded.length, recorded.hash))
        .collect();
    let bytes = image_bytes(
        build,
        trained,
        database_length,
        &table,
        &payloads,
        native.as_ref().map(|native| &*native.encode),
    )?;
    let target = path(database);
    let staged = target.with_extension("ryc.partial");
    // The image is written beside the target and renamed over it, never rewritten in place: a
    // process that maps the old image keeps reading the old file, where writing into it would
    // change the pages under that process or end it with SIGBUS once the file shrank. The data
    // reaches the disk before the rename, so a crash leaves the old image or the new one.
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&staged)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        std::fs::rename(&staged, &target)
    })();
    if let Err(failure) = written {
        let message = format!("cannot write {}: {failure}", target.display());
        return Err(match std::fs::remove_file(&staged) {
            Err(cleanup) if cleanup.kind() != std::io::ErrorKind::NotFound => error(format!(
                "{message}, and cannot remove {}: {cleanup}",
                staged.display()
            )),
            _ => error(message),
        });
    }
    forget(database);
    Ok(())
}

/// The payload of one record: the encoded code of each closure the record made, given with the
/// body it was read with, in the order the reader made them.
///
/// # Safety
/// Runs on the interpreter's thread, with every closure and body rooted.
pub unsafe fn record_payload(
    closures: &[(Sexp, Sexp)],
    trained: bool,
    summary: &mut Summary,
) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(&mut payload, closures.len() as u64);
    for &(closure, body) in closures {
        // A body that is a symbol or a constant evaluates directly and never compiles, so it is
        // not counted as a closure the image could hold.
        if body.kind() != SexpType::Language {
            write_varint(&mut payload, 0);
            continue;
        }
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
    payload
}

/// The bytes of an image for a database of `database_length` bytes: `records` holds each
/// record's offset, length and hash, sorted by offset, `payloads` the payload of each, and
/// `native` the native section's bytes for the offset it is written at.
pub fn image_bytes(
    build: &str,
    trained: bool,
    database_length: u64,
    records: &[(u64, u32, u64)],
    payloads: &[Vec<u8>],
    native: Option<&dyn Fn(u64) -> Vec<u8>>,
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
    let native_at = out.bytes.len();
    out.u64(0);
    out.u64(0);
    for payload in payloads {
        out.bytes.extend_from_slice(payload);
    }
    if let Some(native) = native {
        let offset = out.bytes.len() as u64;
        let section = native(offset);
        out.bytes[native_at..native_at + 8].copy_from_slice(&offset.to_le_bytes());
        out.bytes[native_at + 8..native_at + 16]
            .copy_from_slice(&(section.len() as u64).to_le_bytes());
        out.bytes.extend_from_slice(&section);
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
