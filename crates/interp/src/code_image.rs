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
//! An image also holds the stream of every record it lists, uncompressed, so a fetch reads the
//! record straight from the mapped image: it neither reads the database nor inflates the record,
//! and every process that loads the package shares the image's pages. Records the install did not
//! fetch are read from the database as usual.
//!
//! An image is build output, never a cache. It is stamped with the runtime build that wrote it,
//! and a different build ignores it, so nothing ever checks whether its code is still current. It
//! is also stamped with the length and modification time of the database it was written beside,
//! so an image that does not match its database (a package reinstalled by GNU R over ry's install,
//! say) is ignored whole rather than trusted. A copy that does not keep modification times only
//! costs the image, never a stale record. A record's stream is checked against its hash
//! ([`stream_hash`]) the first time it is read, so a damaged stream gives the database's record
//! instead of a different value.
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
//! | magic | `RYCODE\0\x03` |
//! | build | the runtime's identity, as a length-prefixed blob |
//! | flags | `u32`, bit 0 set when the image was trained |
//! | database length | `u64`, the size of the `.rdb` file the image was written for |
//! | database time | `u64`, that file's modification time in nanoseconds since 1970 |
//! | record count | `u32` |
//! | records | per record: database offset `u64`, record length `u32`, stream offset `u64`, stream length `u32`, stream hash `u64`, payload offset `u64`, payload length `u32`, payload hash `u64`; sorted by database offset |
//! | payloads | per record: the closure count, then per closure the length of its code (0 for none) and the code |
//! | streams | per record: its serialized stream, as decompressing the database's record gives it |

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        bytecode,
        image::ImageFile,
        interp,
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
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryCodeImageRecord", 1, do_record).invisible(),
    internal("ryCodeImageWrite", 2, do_write),
];

const MAGIC: &[u8; 8] = b"RYCODE\x00\x03";

/// The bytes of one record table entry.
const ENTRY_SIZE: usize = 52;

/// The flag bit of an image written after a training run.
const TRAINED: u32 = 1;

/// The code image of the lazy-load database `database`: its path with `.rdb` replaced by `.ryc`.
pub fn path(database: &str) -> PathBuf {
    Path::new(database.strip_suffix(".rdb").unwrap_or(database)).with_extension("ryc")
}

/// The lazy-load databases this process read from and the code images it opened for them.
#[derive(Default)]
pub struct State {
    databases: Databases,
    /// Per closure body fetched from an image but not called yet, where its code lies. Each body
    /// is preserved while it waits, so its address cannot be reused by an unrelated object.
    pending: FxHashMap<Sexp, Pending>,
    /// The fetches an install records while it loads the package it is writing images for.
    recording: Option<Recording>,
    /// The runtime build images are stamped with; empty when the host named none, which turns
    /// images off.
    pub build: &'static str,
}

/// The lazy-load databases this process fetched from, numbered in the order they were first
/// fetched from, one column per fact. A database is named by the interned `CHARSXP` of its path,
/// which every promise of one database shares, so a fetch finds its number by comparing
/// pointers: a session reads from a few dozen databases, and a scan of that many words costs
/// less than hashing the path.
#[derive(Default)]
struct Databases {
    /// Each path's `CHARSXP`, preserved so its address cannot name another string.
    names: Vec<Sexp>,
    paths: Vec<Rc<str>>,
    /// The database file, mapped on its first read and dropped when the database changes.
    files: Vec<Option<ImageFile>>,
    images: Vec<ImageSlot>,
    /// One bit per database under the directory an install is recording.
    recorded: Vec<u64>,
}

/// Whether a database's code image was looked for, and what was found.
enum ImageSlot {
    Unopened,
    /// No usable image, so its absence is looked up once.
    Absent,
    Open(Rc<Image>),
}

struct Image {
    file: ImageFile,
    entries: Vec<Entry>,
    /// One bit per entry, set once its payload's hash has been checked.
    verified: Vec<Cell<u64>>,
    /// One bit per entry, set once its stream's hash has been checked.
    streams_verified: Vec<Cell<u64>>,
}

/// Where the code of a fetched closure lies: `start..end` of the image's bytes, inside the
/// payload of `entry`. The code was compiled for `formals`, the closure's formals as fetched, and
/// the compiler's output depends on them (which names are local, what a call can change), so a
/// closure that shares the body under other formals (`formals<-`, `as.function`) compiles its
/// own. `formals` is preserved while the entry waits, like the body.
struct Pending {
    image: Rc<Image>,
    database: u32,
    formals: Sexp,
    entry: u32,
    start: u32,
    end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub length: u32,
    pub stream_offset: u64,
    pub stream_length: u32,
    pub stream_hash: u64,
    pub payload_offset: u64,
    pub payload_length: u32,
    pub payload_hash: u64,
}

struct Recording {
    /// Only databases under this directory are recorded: the package being installed, not its
    /// imports.
    directory: String,
    /// Per database, per record offset: the record's length and stream, and a list holding each
    /// closure the record made followed by the body it was read with.
    records: BTreeMap<String, BTreeMap<u64, Recorded>>,
}

struct Recorded {
    length: u32,
    stream: Vec<u8>,
    closures: Sexp,
}

/// The length and modification time of a database, which an image must have been written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseStamp {
    pub length: u64,
    pub modified: u64,
}

impl DatabaseStamp {
    /// The stamp of `database` as it is on disk, `None` where the file system keeps no
    /// modification time, which leaves the database without a usable image.
    pub fn of(database: &str) -> Result<Option<DatabaseStamp>, String> {
        let metadata = std::fs::metadata(database)
            .map_err(|failure| format!("cannot read {database}: {failure}"))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| u64::try_from(since.as_nanos()).ok());
        Ok(modified.map(|modified| DatabaseStamp {
            length: metadata.len(),
            modified,
        }))
    }
}

/// Forgets the image of `database` and the code waiting in it, which `lazyLoadDBflush` calls when
/// the database changes or its namespace unloads.
/// The database file is read again on its next fetch.
pub fn forget(database: &str) {
    let state = &mut interp().code_image;
    let Some(index) = state
        .databases
        .paths
        .iter()
        .position(|path| &**path == database)
    else {
        return;
    };
    state.databases.files[index] = None;
    state.databases.images[index] = ImageSlot::Unopened;
    drop_pending(state, index as u32);
}

/// Drops the code waiting in the image of `database`, releasing what each entry preserved.
fn drop_pending(state: &mut State, database: u32) {
    let heap = crate::heap();
    state.pending.retain(|&body, pending| {
        if pending.database != database {
            return true;
        }
        heap.release(body);
        heap.release(pending.formals);
        false
    });
}

/// Stops using the image of `database` after it turned out damaged, with one warning: the image
/// only saves compiling, so its package goes on loading and its functions compile as they run.
unsafe fn give_up(database: u32, message: &str) {
    let state = &mut interp().code_image;
    state.databases.images[database as usize] = ImageSlot::Absent;
    drop_pending(state, database);
    report_damage(&format!(
        "cannot use the code image {} ({message}), so its functions compile as they run; \
         reinstall the package to repair it",
        path(&state.databases.paths[database as usize]).display()
    ));
}

/// Prints a warning about a damaged image to the console. It describes a file on disk, not the
/// program, so it is not signalled as a condition: a handler or `options(warn = 2)` turning it
/// into an error would make a lazy-load fetch fail over a cache that only saves compiling.
fn report_damage(message: &str) {
    crate::write_console(format!("Warning: {message}\n").as_bytes(), true);
}

/// Where a fetch from a lazy-load database reads its record.
pub enum Source {
    /// The database's image, which holds the record's stream.
    Image(ImageRecord),
    /// The database, noting the fetch for the image an install is about to write.
    Recording,
    /// The database alone.
    Database,
}

/// A record the image of its database holds.
pub struct ImageRecord {
    image: Rc<Image>,
    entry: usize,
}

/// The number of the lazy-load database whose path is the `CHARSXP` `name`, which the other
/// functions here take.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn database(name: Sexp) -> u32 {
    let state = &mut interp().code_image;
    let databases = &mut state.databases;
    if let Some(index) = databases.names.iter().position(|&known| known == name) {
        return index as u32;
    }
    let index = databases.names.len();
    crate::heap().preserve(name);
    let path: Rc<str> = Rc::from(&*values::char_str(name));
    if index % 64 == 0 {
        databases.recorded.push(0);
    }
    if state
        .recording
        .as_ref()
        .is_some_and(|recording| Path::new(&*path).starts_with(&recording.directory))
    {
        databases.recorded[index / 64] |= 1 << (index % 64);
    }
    databases.names.push(name);
    databases.paths.push(path);
    databases.files.push(None);
    databases.images.push(ImageSlot::Unopened);
    index as u32
}

/// The path of `database`.
pub fn database_path(database: u32) -> Rc<str> {
    Rc::clone(&interp().code_image.databases.paths[database as usize])
}

/// The bytes of the record at `offset` of `database`, `size` bytes long, from the database file,
/// which is mapped on its first read.
pub fn database_record(database: u32, offset: usize, size: usize) -> R<&'static [u8]> {
    let databases = &mut interp().code_image.databases;
    let index = database as usize;
    let file = match &mut databases.files[index] {
        Some(file) => file,
        slot => {
            let path = &databases.paths[index];
            let opened = ImageFile::open(Path::new(&**path))
                .map_err(|failure| error(format!("cannot open file '{path}': {failure}")))?;
            slot.insert(opened)
        }
    };
    file.bytes()
        .get(offset..offset.saturating_add(size))
        .ok_or_else(|| error("lazy-load database is corrupt"))
}

/// Where a fetch of the record at `offset` of `database`, `length` bytes long there, reads it:
/// the database's image when this build wrote one for this version of the database with this
/// record in it, otherwise the database.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn source(database: u32, offset: u64, length: usize) -> R<Source> {
    let state = &mut interp().code_image;
    if state.build.is_empty() {
        return Ok(Source::Database);
    }
    let index = database as usize;
    if state.databases.recorded[index / 64] & (1 << (index % 64)) != 0 {
        return Ok(Source::Recording);
    }
    if let ImageSlot::Unopened = state.databases.images[index] {
        match open(&state.databases.paths[index], state.build) {
            Ok(Some(image)) => state.databases.images[index] = ImageSlot::Open(image),
            Ok(None) => state.databases.images[index] = ImageSlot::Absent,
            Err(message) => give_up(database, &message),
        }
    }
    let ImageSlot::Open(image) = &interp().code_image.databases.images[index] else {
        return Ok(Source::Database);
    };
    let Ok(entry) = image
        .entries
        .binary_search_by_key(&offset, |entry| entry.offset)
    else {
        return Ok(Source::Database);
    };
    if image.entries[entry].length as usize != length {
        return Ok(Source::Database);
    }
    let record = ImageRecord {
        image: Rc::clone(image),
        entry,
    };
    let word = &record.image.streams_verified[entry / 64];
    let bit = 1u64 << (entry % 64);
    if word.get() & bit == 0 {
        if stream_hash(stream(&record)) != record.image.entries[entry].stream_hash {
            give_up(database, "a stream's hash does not match");
            return Ok(Source::Database);
        }
        word.set(word.get() | bit);
    }
    Ok(Source::Image(record))
}

/// The serialized stream of `record`, read in place from the mapped image. [`parse`] checked that
/// every stream lies inside the file.
pub fn stream(record: &ImageRecord) -> &[u8] {
    let entry = &record.image.entries[record.entry];
    let start = entry.stream_offset as usize;
    record
        .image
        .file
        .bytes()
        .get(start..start + entry.stream_length as usize)
        .unwrap_or_default()
}

/// The hash a stream is checked against. Streams are much larger than payloads, so this hashes a
/// word at a time rather than a byte at a time like [`hash`]; it need only agree within one build,
/// because another build ignores the image.
pub fn stream_hash(bytes: &[u8]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    std::hash::Hasher::write(&mut hasher, bytes);
    std::hash::Hasher::finish(&hasher)
}

/// Reads `record` of `database` from its image. The closures it makes get their compiled code
/// from the image.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn read_image_record(database: u32, record: &ImageRecord, hook: Sexp) -> R<Sexp> {
    // `record` holds the image, so its pages stay mapped while the hook runs, even if the hook
    // flushes the database.
    let (value, closures) = serialize::unserialize_with_closures(stream(record), hook)?;
    crate::heap().root(value);
    if let Err(message) = note_pending(&record.image, record.entry, database, &closures) {
        give_up(database, &message);
    }
    Ok(value)
}

/// Reads the record at `offset` of `database`, `length` bytes long there, from its decompressed
/// `stream`, and notes it for the image the install is recording.
///
/// # Safety
/// Runs on the interpreter's thread.
pub unsafe fn read_recorded(
    database: u32,
    offset: u64,
    length: usize,
    stream: Vec<u8>,
    hook: Sexp,
) -> R<Sexp> {
    let (value, closures) = serialize::unserialize_with_closures(&stream, hook)?;
    crate::heap().root(value);
    record_fetch(&database_path(database), offset, length, stream, &closures)?;
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
            give_up(pending.database, &message);
            Ok(None)
        }
    }
}

/// Notes, for each closure in `closures` the payload of `entry` holds code for, where that code
/// lies, without decoding it.
unsafe fn note_pending(
    image: &Rc<Image>,
    entry: usize,
    database: u32,
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
    let state = &mut interp().code_image;
    let heap = crate::heap();
    for (body, formals, start, end) in noted {
        let pending = Pending {
            image: Rc::clone(image),
            database,
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
    let word = image
        .verified
        .get(entry / 64)
        .ok_or("a record is missing")?;
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
    let file = match ImageFile::open(&path(database)) {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(failure) => return Err(failure.to_string()),
    };
    let Some(stamp) = DatabaseStamp::of(database)? else {
        return Ok(None);
    };
    Ok(parse(file.bytes(), build, stamp)?.map(|entries| {
        let bits = || {
            (0..entries.len().div_ceil(64))
                .map(|_| Cell::new(0))
                .collect()
        };
        Rc::new(Image {
            verified: bits(),
            streams_verified: bits(),
            file,
            entries,
        })
    }))
}

/// The record table of an image, or `None` when it was written by another build or for another
/// version of the database.
pub fn parse(
    bytes: &[u8],
    build: &str,
    stamp: DatabaseStamp,
) -> Result<Option<Vec<Entry>>, String> {
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
    let length = input.u64().map_err(failed)?;
    let modified = input.u64().map_err(failed)?;
    if (DatabaseStamp { length, modified }) != stamp {
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
            stream_offset: input.u64().map_err(failed)?,
            stream_length: input.u32().map_err(failed)?,
            stream_hash: input.u64().map_err(failed)?,
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
        let outside = |offset: u64, length: u32| {
            offset
                .checked_add(u64::from(length))
                .is_none_or(|end| end > bytes.len() as u64)
        };
        if outside(entry.payload_offset, entry.payload_length) {
            return Err("a payload lies outside the file".to_owned());
        }
        if outside(entry.stream_offset, entry.stream_length) {
            return Err("a stream lies outside the file".to_owned());
        }
        entries.push(entry);
    }
    Ok(Some(entries))
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

unsafe fn record_fetch(
    database: &str,
    offset: u64,
    length: usize,
    stream: Vec<u8>,
    closures: &[Sexp],
) -> R<()> {
    let heap = crate::heap();
    let Some(recording) = &mut interp().code_image.recording else {
        return Ok(());
    };
    let records = recording.records.entry(database.to_owned()).or_default();
    if records.contains_key(&offset) {
        return Ok(());
    }
    let list = values::alloc(SexpType::List, closures.len() * 2)?;
    for (index, &closure) in closures.iter().enumerate() {
        heap.set_element(list, index * 2, closure);
        heap.set_element(list, index * 2 + 1, closure.cdr());
    }
    let too_large = || error(format!("a record of {database} is larger than 4GB"));
    let length = u32::try_from(length).map_err(|_| too_large())?;
    u32::try_from(stream.len()).map_err(|_| too_large())?;
    heap.preserve(list);
    records.insert(
        offset,
        Recorded {
            length,
            stream,
            closures: list,
        },
    );
    Ok(())
}

/// `ryCodeImageRecord(directory)`: from now on, records every lazy-load record fetched from a
/// database under `directory` with the closures it made, for [`do_write`].
unsafe fn do_record(call: &Call<'_>) -> R<Sexp> {
    let directory = values::require_str(call.arg(0), "directory")?.into_owned();
    let directory = std::path::absolute(&directory)
        .map_err(|failure| error(format!("cannot resolve {directory}: {failure}")))?;
    let state = &mut interp().code_image;
    let databases = &mut state.databases;
    for (index, path) in databases.paths.iter().enumerate() {
        let bit = 1 << (index % 64);
        if Path::new(&**path).starts_with(&directory) {
            databases.recorded[index / 64] |= bit;
        } else {
            databases.recorded[index / 64] &= !bit;
        }
    }
    let previous = state.recording.replace(Recording {
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

/// `ryCodeImageWrite(trained, code)`: compiles every recorded closure and writes each
/// database's image, keeping the run's feedback when `trained`. Without `code`, an image holds
/// the records' streams alone: decoding untrained code costs a session more than compiling the
/// closures it calls. Returns how many closures were recorded and written, and how many were left
/// out for each reason.
unsafe fn do_write(call: &Call<'_>) -> R<Sexp> {
    let trained = values::as_logical_scalar(call.arg(0)) == 1;
    let code = values::as_logical_scalar(call.arg(1)) == 1;
    let state = &mut interp().code_image;
    if state.build.is_empty() {
        return Err(error(
            "this runtime has no build identity to stamp images with",
        ));
    }
    let Some(recording) = state.recording.take() else {
        return Err(error("no fetches were recorded"));
    };
    state.databases.recorded.fill(0);
    let build = state.build;
    let mut summary = Summary::default();
    let written = recording
        .records
        .iter()
        .try_for_each(|(database, records)| {
            write_image(database, records, build, trained, code, &mut summary)
        });
    release_recording(&recording);
    written?;
    let mut items = vec![("closures", summary.closures), ("written", summary.written)];
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
    values::set_attr(
        counts,
        values::symbols().names,
        values::string_vector(&names)?,
    )?;
    Ok(counts)
}

/// How many closures an install recorded and wrote, and how many it left out for each reason.
#[derive(Default)]
pub struct Summary {
    pub closures: usize,
    pub written: usize,
    pub skipped: BTreeMap<&'static str, usize>,
}

unsafe fn write_image(
    database: &str,
    records: &BTreeMap<u64, Recorded>,
    build: &str,
    trained: bool,
    code: bool,
    summary: &mut Summary,
) -> R<()> {
    let Some(stamp) = DatabaseStamp::of(database).map_err(error)? else {
        return Ok(());
    };
    let mut payloads = Vec::with_capacity(records.len());
    for recorded in records.values() {
        if !code {
            let count = length(recorded.closures) / 2;
            let mut payload = Vec::with_capacity(count + 1);
            write_varint(&mut payload, count as u64);
            payload.resize(payload.len() + count, 0);
            payloads.push(payload);
            continue;
        }
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
    let table: Vec<(u64, u32, &[u8])> = records
        .iter()
        .map(|(offset, recorded)| (*offset, recorded.length, &recorded.stream[..]))
        .collect();
    let bytes = image_bytes(build, trained, stamp, &table, &payloads)?;
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

/// The bytes of an image for the database `stamp` describes: `records` holds each record's
/// offset, length and stream, sorted by offset, and `payloads` the payload of each.
pub fn image_bytes(
    build: &str,
    trained: bool,
    stamp: DatabaseStamp,
    records: &[(u64, u32, &[u8])],
    payloads: &[Vec<u8>],
) -> R<Vec<u8>> {
    let mut out = Writer::default();
    out.bytes.extend_from_slice(MAGIC);
    out.blob(build.as_bytes());
    out.u32(if trained { TRAINED } else { 0 });
    out.u64(stamp.length);
    out.u64(stamp.modified);
    out.u32(u32::try_from(records.len()).map_err(|_| error("an image holds too many records"))?);
    let too_large = |what: &str| error(format!("{what} is larger than 4GB"));
    let mut payload_offset = (out.bytes.len() + records.len() * ENTRY_SIZE) as u64;
    // Streams follow every payload, so the payloads, which pending code addresses with 32 bits,
    // stay at the front of the file.
    let mut stream_offset = payload_offset + payloads.iter().map(|p| p.len() as u64).sum::<u64>();
    for (&(offset, length, stream), payload) in records.iter().zip(payloads) {
        out.u64(offset);
        out.u32(length);
        out.u64(stream_offset);
        out.u32(u32::try_from(stream.len()).map_err(|_| too_large("a stream"))?);
        out.u64(stream_hash(stream));
        out.u64(payload_offset);
        out.u32(u32::try_from(payload.len()).map_err(|_| too_large("a payload"))?);
        out.u64(hash(payload));
        payload_offset += payload.len() as u64;
        stream_offset += stream.len() as u64;
    }
    for payload in payloads {
        out.bytes.extend_from_slice(payload);
    }
    for (_, _, stream) in records {
        out.bytes.extend_from_slice(stream);
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
        Ok(Ok(code)) => code,
        Ok(Err(_)) => return Err(Unencodable("a body the compiler declines")),
        Err(_) => return Err(Unencodable("a body whose compile raised an error")),
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
