//! The payload `ry build --aot --exe` appends to a copy of `ry`: the script to run, the flags it
//! runs with, and the typed code file of its top-level functions, which the executable attaches
//! instead of compiling them.
//!
//! The executable is laid out as
//!
//! ```text
//! [ry] [zero padding] [typed code file] [metadata] [trailer]
//! ```
//!
//! The padding puts the code file at a multiple of [`ALIGN`], so its code maps straight from the
//! executable. The trailer is the file's last [`TRAILER_SIZE`] bytes: five little-endian `u64`
//! words (the code file's offset and length, the metadata's offset and length, and the
//! metadata's hash) and then [`MAGIC`]. The metadata is the flags as a little-endian `i32` and
//! four length-prefixed byte strings: the script's name, its elaborated text, and the runtime
//! library and base R tree a development build found them at (empty for a release, which
//! carries its own). Each length is a little-endian `u64`.

use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// The alignment of the typed code file in the executable: the largest page size of the
/// systems ry runs on, as for a package's code file.
pub const ALIGN: u64 = 65536;

/// The last bytes of an executable that carries a payload.
pub const MAGIC: [u8; 8] = *b"ry-exe\x00\x01";

pub const TRAILER_SIZE: u64 = 8 * 5 + 8;

/// What an executable runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// The `RUN_FLAGS` bits of `libr::eval` the script runs with.
    pub flags: i32,
    /// The script's name, as `commandArgs()` and error messages show it.
    pub script: String,
    /// The script's text, elaborated by the checker.
    pub source: Vec<u8>,
    /// The runtime library a development build used, which a copy moved away from the build's
    /// target directory cannot find on its own.
    pub runtime: Option<PathBuf>,
    /// The base R tree a development build used, for the same reason.
    pub base: Option<PathBuf>,
}

/// A payload found at the end of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub metadata: Metadata,
    /// Where the typed code file starts in the executable.
    pub code_offset: u64,
    pub code_length: u64,
}

/// The payload of the running executable: `Ok(None)` for `ry` itself, and an error when the
/// executable ends in the trailer but the payload is damaged. An executable this process cannot
/// read, such as an execute-only copy or one in a sandbox without `/proc`, is `ry`: a payload is
/// only ever written onto a copy its builder could read.
pub fn own() -> Result<Option<(PathBuf, Payload)>, String> {
    let Ok(executable) = std::env::current_exe() else {
        return Ok(None);
    };
    let found = fs::File::open(&executable).and_then(|mut file| read(&mut file));
    match found.unwrap_or(Found::None) {
        Found::None => Ok(None),
        Found::Payload(payload) => Ok(Some((executable, payload))),
        Found::Damaged => Err(format!(
            "{} ends in a damaged script payload; build it again with `ry build --aot --exe`",
            executable.display()
        )),
    }
}

/// Writes the executable `output`: a copy of `executable` followed by the typed code file
/// `code` and `metadata`. It is written next to `output` and renamed into place, so a failed
/// build leaves no partial executable.
pub fn write(output: &Path, executable: &Path, code: &[u8], metadata: &Metadata) -> io::Result<()> {
    let mut partial = output.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    // `fs::copy` keeps the executable's permissions, so the copy runs.
    let written = fs::copy(executable, &partial).and_then(|length| {
        // An installed `ry` is often read-only (mode 0555), and so is its copy, which its owner
        // must be able to append to.
        let mut permissions = fs::metadata(&partial)?.permissions();
        owner_writable(&mut permissions);
        fs::set_permissions(&partial, permissions)?;
        let mut file = fs::OpenOptions::new().append(true).open(&partial)?;
        let mut payload = Vec::new();
        append(&mut payload, length, code, metadata);
        file.write_all(&payload)?;
        file.sync_all()?;
        fs::rename(&partial, output)
    });
    if written.is_err()
        && let Err(removal) = fs::remove_file(&partial)
        && removal.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!("cannot remove {}: {removal}", partial.display());
    }
    written
}

#[cfg(unix)]
fn owner_writable(permissions: &mut fs::Permissions) {
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(permissions.mode() | 0o200);
}

#[cfg(not(unix))]
fn owner_writable(permissions: &mut fs::Permissions) {
    permissions.set_readonly(false);
}

/// Appends to `bytes` the payload of an executable whose first `length` bytes are `ry`.
pub fn append(bytes: &mut Vec<u8>, length: u64, code: &[u8], metadata: &Metadata) {
    let code_offset = length.next_multiple_of(ALIGN);
    bytes.resize(bytes.len() + (code_offset - length) as usize, 0);
    bytes.extend_from_slice(code);
    let metadata_offset = code_offset + code.len() as u64;
    let encoded = encode(metadata);
    bytes.extend_from_slice(&encoded);
    for word in [
        code_offset,
        code.len() as u64,
        metadata_offset,
        encoded.len() as u64,
        hash(&encoded),
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&MAGIC);
}

/// What the end of a file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    None,
    Payload(Payload),
    Damaged,
}

/// Reads the payload at the end of `file`. Only an error reading the file is an error: a file
/// without the trailer has no payload, and one whose trailer or metadata does not hold
/// together is damaged.
pub fn read<F: Read + Seek>(file: &mut F) -> io::Result<Found> {
    let length = file.seek(SeekFrom::End(0))?;
    if length < TRAILER_SIZE {
        return Ok(Found::None);
    }
    file.seek(SeekFrom::Start(length - TRAILER_SIZE))?;
    let mut trailer = [0u8; TRAILER_SIZE as usize];
    file.read_exact(&mut trailer)?;
    if trailer[40..] != MAGIC {
        return Ok(Found::None);
    }
    let word = |index: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&trailer[index * 8..index * 8 + 8]);
        u64::from_le_bytes(bytes)
    };
    let (code_offset, code_length) = (word(0), word(1));
    let (metadata_offset, metadata_length, metadata_hash) = (word(2), word(3), word(4));
    let end = length - TRAILER_SIZE;
    let holds_together = code_offset % ALIGN == 0
        && code_offset.checked_add(code_length) == Some(metadata_offset)
        && metadata_offset.checked_add(metadata_length) == Some(end);
    if !holds_together {
        return Ok(Found::Damaged);
    }
    file.seek(SeekFrom::Start(metadata_offset))?;
    let mut encoded = vec![0u8; metadata_length as usize];
    file.read_exact(&mut encoded)?;
    if hash(&encoded) != metadata_hash {
        return Ok(Found::Damaged);
    }
    Ok(match decode(&encoded) {
        Some(metadata) => Found::Payload(Payload {
            metadata,
            code_offset,
            code_length,
        }),
        None => Found::Damaged,
    })
}

fn encode(metadata: &Metadata) -> Vec<u8> {
    let path = |path: &Option<PathBuf>| {
        path.as_ref()
            .map(|path| path.to_string_lossy().into_owned().into_bytes())
            .unwrap_or_default()
    };
    let mut bytes = metadata.flags.to_le_bytes().to_vec();
    for field in [
        metadata.script.as_bytes(),
        &metadata.source,
        &path(&metadata.runtime),
        &path(&metadata.base),
    ] {
        bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

fn decode(bytes: &[u8]) -> Option<Metadata> {
    let (flags, mut rest) = bytes.split_first_chunk::<4>()?;
    let mut field = || {
        let (length, tail) = rest.split_first_chunk::<8>()?;
        let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
        let (field, tail) = tail.split_at_checked(length)?;
        rest = tail;
        Some(field)
    };
    let script = String::from_utf8(field()?.to_vec()).ok()?;
    let source = field()?.to_vec();
    let path = |field: &[u8]| {
        (!field.is_empty())
            .then(|| String::from_utf8(field.to_vec()).ok().map(PathBuf::from))
            .flatten()
    };
    let runtime = path(field()?);
    let base = path(field()?);
    rest.is_empty().then_some(Metadata {
        flags: i32::from_le_bytes(*flags),
        script,
        source,
        runtime,
        base,
    })
}

/// FNV-1a, enough to tell damaged metadata from what `append` wrote.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Checks for the fuzz harnesses, which feed them arbitrary bytes.
pub mod testing {
    use super::{Found, Metadata, Payload, append, read};
    use std::{io::Cursor, path::PathBuf};

    /// Reads `bytes` as an executable: the reader must not panic, and a payload it finds must
    /// lie inside the file.
    pub fn check_parse_input(bytes: &[u8]) {
        let found = read(&mut Cursor::new(bytes)).expect("reading from memory cannot fail");
        if let Found::Payload(payload) = found {
            let end = payload.code_offset + payload.code_length;
            assert!(end <= bytes.len() as u64, "the code file lies past the end");
        }
    }

    /// Appends a payload built from `bytes` to a prefix of `bytes` and reads it back.
    pub fn check_round_trip(bytes: &[u8]) {
        let split = |at: usize| bytes.split_at(at.min(bytes.len()));
        let (executable, rest) = split(bytes.first().copied().unwrap_or(0) as usize);
        let (code, rest) = rest.split_at((rest.len() / 2).min(rest.len()));
        let metadata = Metadata {
            flags: i32::from(bytes.len() as u8),
            script: String::from_utf8_lossy(rest).into_owned(),
            source: rest.to_vec(),
            runtime: (!code.is_empty()).then(|| PathBuf::from("/lib/libR.so")),
            base: None,
        };
        let mut file = executable.to_vec();
        append(&mut file, executable.len() as u64, code, &metadata);
        let found = read(&mut Cursor::new(&file)).expect("reading from memory cannot fail");
        let code_offset = (executable.len() as u64).next_multiple_of(super::ALIGN);
        assert_eq!(
            found,
            Found::Payload(Payload {
                metadata,
                code_offset,
                code_length: code.len() as u64,
            })
        );
        let start = code_offset as usize;
        assert_eq!(&file[start..start + code.len()], code);
    }
}
