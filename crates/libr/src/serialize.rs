//! R's serialization format: what `serialize`, `saveRDS`, `save` and the lazy-load databases of
//! installed packages store, and the stream API (`R_Serialize`, `R_Unserialize`) native code
//! uses to write and read it.
//!
//! A stream is a short header followed by one item. The header names the format (XDR, which is
//! big-endian binary and the default; native binary; or ASCII), the format version (2 or 3), the
//! version of R that wrote it, the oldest version that can read it, and for version 3 the
//! writer's native encoding. An item starts with a flags word: the type code in the low byte,
//! then the is-object, has-attributes and has-tag bits, and the object's general-purpose bits
//! (`LEVELS`) in the top 16 bits. Special objects (`NULL`, the global, base and empty
//! environments, the base namespace, the unbound value and the missing argument) are a type code
//! alone. Symbols, environments, external pointers, weak references and persistent objects are
//! remembered in the order they are written, so a second occurrence is a back-reference to its
//! index, which is how shared symbols and environment cycles survive a round trip.
//!
//! The reader decodes everything GNU R writes. Three things come back in a different shape than
//! GNU R would build, on purpose:
//!
//! - **Bytecode** reads as the expression it was compiled from (the first constant of the
//!   constant pool), so a compiled closure's body is its AST. ry does not run GNU R's bytecode.
//! - **ALTREP objects** of base R's classes (compact sequences, deferred strings, wrappers) read
//!   as the ordinary vectors they stand for. Other classes need the class table of the package
//!   that defines them, which [`unserialize_altrep`] marks; until it exists they read as a
//!   zero-length vector with a warning, which is what GNU R does when a class is not registered.
//! - **Namespace and package environments** are written as their names. Reading resolves a name
//!   through the hook the interpreter installs with [`set_environment_resolver`]; the default
//!   looks in the namespace registry and otherwise creates a placeholder environment and records
//!   it, so every later reference to the same name in the session resolves to the same
//!   placeholder (see [`default_resolver`]).
//!
//! The writer writes an environment's frame as a pairlist (GNU R's unhashed layout) with the
//! newest binding first, which GNU R reads back unchanged. An ALTREP object is written as the
//! vector it stands for, because ry's ALTREP classes do not yet record a class name and a
//! serialized state.

use {
    crate::{
        Global, R_xlen_t, altrep,
        boundary::{ApiResult, enter, error, warn, with_native_scope},
        environments::frame_value,
        eval::primitive_name,
        globals::{R_ClassSymbol, R_NameSymbol, R_NamespaceEnvSymbol, R_SpecSymbol},
        runtime,
        strings::utf8_text,
    },
    heap::{
        WellKnown,
        object::{NA_INTEGER, NA_REAL, Rcomplex, Sexp, SexpType, gp, is_na_real},
        strings::Encoding,
    },
    interp::builtins::{BuiltinDef, Call, internal},
    rustc_hash::FxHashMap,
    std::{
        borrow::Cow,
        ffi::{c_char, c_int, c_void},
        io::Read,
        path::Path,
    },
};

/// The R-level serialization internals. They replace the interpreter's own XDR-only writer, so
/// `serialize(version = 2)`, `ascii = TRUE`, `xdr = FALSE` and `refhook` behave as in GNU R:
/// digest hashes version 2 streams and compares them with GNU R's digests.
pub const BUILTINS: &[BuiltinDef] = &[
    internal("serialize", 5, |call| unsafe {
        do_serialize(call, level_format(call.arg(2))?)
    }),
    internal("serializeb", 5, |call| unsafe {
        let xdr = interp::values::as_logical_scalar(call.arg(2)) != 0;
        do_serialize(call, if xdr { Format::Xdr } else { Format::Binary })
    }),
    internal("serializeToConn", 5, |call| unsafe {
        do_serialize(call, ascii_format(call.arg(2)))?;
        Ok(runtime().nil())
    })
    .invisible(),
    internal("saveToConn", 6, |call| unsafe { do_save_to_conn(call) }).invisible(),
    internal("lazyLoadDBinsertValue", 5, |call| unsafe {
        do_lazy_load_insert(call)
    }),
];

/// `lazyLoadDBinsertValue(value, file, ascii, compress, hook)`: serializes `value` (version 3,
/// with `hook` deciding which environments become references), compresses it as `compress`
/// asks, appends the record to `file`, and returns its `c(offset, length)`, which is the key
/// `lazyLoadDBfetch` takes back.
unsafe fn do_lazy_load_insert(call: &Call<'_>) -> ApiResult<Sexp> {
    use std::io::{Seek, Write};
    let file = interp::values::require_string(call.arg(1), "file")?;
    let format = if interp::values::as_logical_scalar(call.arg(2)) == 1 {
        Format::Ascii
    } else {
        Format::Xdr
    };
    let kind = interp::values::as_integer_scalar(call.arg(3));
    let stream = serialize_to_bytes(call.arg(0), format, 3, call.arg(4))?;
    let record =
        interp::serialize::compress_record(&stream, if kind == NA_INTEGER { 1 } else { kind })?;
    let failed =
        |failure: std::io::Error| error(format!("cannot write to file '{file}': {failure}"));
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .map_err(failed)?;
    let offset = handle.seek(std::io::SeekFrom::End(0)).map_err(failed)?;
    handle.write_all(&record).map_err(failed)?;
    interp::serialize::forget_database(&file);
    let key = interp::values::alloc(SexpType::Integer, 2)?;
    let (Ok(offset), Ok(length)) = (i32::try_from(offset), i32::try_from(record.len())) else {
        return Err(error(format!(
            "lazy-load database '{file}' is larger than 2GB"
        )));
    };
    interp::values::ints_mut(key).copy_from_slice(&[offset, length]);
    Ok(key)
}

/// `serialize()`'s `type` code: 0 is XDR, 1 ASCII, 2 hexadecimal ASCII and 3 native binary.
unsafe fn level_format(code: Sexp) -> ApiResult<Format> {
    Ok(match interp::values::as_integer_scalar(code) {
        0 => Format::Xdr,
        1 => Format::Ascii,
        2 => Format::AsciiHex,
        3 => Format::Binary,
        _ => return Err(error("invalid 'type' argument")),
    })
}

/// An `ascii` flag as `serializeToConn` and `save()` read it: `NA` asks for hexadecimal ASCII.
unsafe fn ascii_format(ascii: Sexp) -> Format {
    match interp::values::as_logical_scalar(ascii) {
        NA_INTEGER => Format::AsciiHex,
        0 => Format::Xdr,
        _ => Format::Ascii,
    }
}

/// `version = NULL` means the current default, version 3.
unsafe fn level_version(version: Sexp) -> ApiResult<c_int> {
    if version == runtime().nil() {
        return Ok(3);
    }
    match interp::values::as_integer_scalar(version) {
        NA_INTEGER => Err(error("bad version value")),
        version => Ok(version),
    }
}

/// `(object, connection, format, version, refhook)`: the bytes as a raw vector when
/// `connection` is `NULL`, and otherwise written to the connection.
unsafe fn do_serialize(call: &Call<'_>, format: Format) -> ApiResult<Sexp> {
    let version = level_version(call.arg(3))?;
    let bytes = serialize_to_bytes(call.arg(0), format, version, call.arg(4))?;
    let connection = call.arg(1);
    if connection != runtime().nil() {
        interp::connections::write_binary(connection, &bytes)?;
        return Ok(runtime().nil());
    }
    let result = interp::values::alloc(SexpType::Raw, bytes.len())?;
    interp::values::raws_mut(result).copy_from_slice(&bytes);
    Ok(result)
}

unsafe fn serialize_to_bytes(
    object: Sexp,
    format: Format,
    version: c_int,
    refhook: Sexp,
) -> ApiResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut sink = |chunk: &[u8]| -> ApiResult<()> {
        bytes.extend_from_slice(chunk);
        Ok(())
    };
    let mut r_hook = |object: Sexp| -> ApiResult<Sexp> {
        let value = interp::eval::apply(
            runtime().nil(),
            refhook,
            &interp::eval::value_args(&[object]),
            interp::values::global_env(),
        )?;
        runtime().heap.root(value);
        Ok(value)
    };
    let hook: Option<PersistHook<'_>> = if refhook == runtime().nil() {
        None
    } else {
        Some(&mut r_hook)
    };
    serialize(object, format, version, &mut sink, hook, Chunking::Buffered)?;
    Ok(bytes)
}

/// `saveToConn(names, con, ascii, version, envir, eval.promises)`: a `save()` file, which is a
/// magic line naming the format and version followed by the pairlist of the saved bindings.
unsafe fn do_save_to_conn(call: &Call<'_>) -> ApiResult<Sexp> {
    let names = call.arg(0);
    if names.kind() != SexpType::String {
        return Err(error("first argument must be a character vector"));
    }
    let env = call.arg(4);
    if env.kind() != SexpType::Environment {
        return Err(error("invalid 'envir' argument"));
    }
    let format = ascii_format(call.arg(2));
    let version = level_version(call.arg(3))?;
    let force = interp::values::as_logical_scalar(call.arg(5)) == 1;
    let items = interp::serialize::saved_bindings(call, names, env, force, false)?;
    let bindings = interp::values::pairlist(&items, SexpType::Pairlist)?;
    runtime().heap.root(bindings);
    let letter = if format.is_text() { 'A' } else { 'X' };
    let mut bytes = format!("RD{letter}{version}\n").into_bytes();
    bytes.extend(serialize_to_bytes(
        bindings,
        format,
        version,
        runtime().nil(),
    )?);
    interp::connections::write_binary(call.arg(1), &bytes)?;
    Ok(runtime().nil())
}

/// The encodings a stream can use, numbered as `R_pstream_format_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Text, with decimal doubles.
    Ascii = 1,
    /// Native-endian binary.
    Binary = 2,
    /// Big-endian binary, the default and the only format portable between machines.
    Xdr = 3,
    /// Text, with doubles in C's hexadecimal notation so they round-trip exactly.
    AsciiHex = 4,
}

impl Format {
    pub fn from_code(code: c_int) -> Option<Format> {
        Some(match code {
            1 => Format::Ascii,
            2 => Format::Binary,
            3 => Format::Xdr,
            4 => Format::AsciiHex,
            _ => return None,
        })
    }

    fn is_text(self) -> bool {
        matches!(self, Format::Ascii | Format::AsciiHex)
    }
}

/// The version `serialize` writes unless asked otherwise.
pub const DEFAULT_VERSION: c_int = 3;

/// What a version-3 stream records as the writer's native encoding.
const NATIVE_ENCODING: &str = "UTF-8";

/// `R_Version(4, 6, 2)`: ry presents itself as R 4.6.2.
const WRITER_VERSION: c_int = r_version(4, 6, 2);

const fn r_version(major: c_int, minor: c_int, patch: c_int) -> c_int {
    major * 65536 + minor * 256 + patch
}

/// The oldest R that reads a stream of `version`.
fn min_reader_version(version: c_int) -> c_int {
    if version == 3 {
        r_version(3, 5, 0)
    } else {
        r_version(2, 3, 0)
    }
}

/// The codes the format uses beyond the `SEXPTYPE`s, which GNU R numbers downward from 255.
mod code {
    pub const REFSXP: u32 = 255;
    pub const NILVALUE_SXP: u32 = 254;
    pub const GLOBALENV_SXP: u32 = 253;
    pub const UNBOUNDVALUE_SXP: u32 = 252;
    pub const MISSINGARG_SXP: u32 = 251;
    pub const BASENAMESPACE_SXP: u32 = 250;
    pub const NAMESPACESXP: u32 = 249;
    pub const PACKAGESXP: u32 = 248;
    pub const PERSISTSXP: u32 = 247;
    pub const CLASSREFSXP: u32 = 246;
    pub const GENERICREFSXP: u32 = 245;
    pub const BCREPDEF: u32 = 244;
    pub const BCREPREF: u32 = 243;
    pub const EMPTYENV_SXP: u32 = 242;
    pub const BASEENV_SXP: u32 = 241;
    pub const ATTRLANGSXP: u32 = 240;
    pub const ATTRLISTSXP: u32 = 239;
    pub const ALTREP_SXP: u32 = 238;
}

const IS_OBJECT: u32 = 1 << 8;
const HAS_ATTR: u32 = 1 << 9;
const HAS_TAG: u32 = 1 << 10;
/// A `CHARSXP`'s cache and hash bits describe this session's string table, not the string.
const CHARSXP_SESSION_BITS: u16 = gp::CACHED | 1;
/// Back-reference indices up to this fit in the flags word; larger ones follow it.
const MAX_PACKED_INDEX: usize = (i32::MAX >> 8) as usize;

/// What the persistence hooks see: writing, an object that may be replaced by a name (a
/// character vector, or `NULL` to write the object itself); reading, that name.
pub type PersistHook<'a> = &'a mut dyn FnMut(Sexp) -> ApiResult<Sexp>;

/// Where a namespace or package environment named in a stream comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvironmentReference {
    /// A namespace, named by its spec (`c(name, version)`).
    Namespace,
    /// An attached package environment, named like `"package:stats"`.
    Package,
}

/// Resolves a namespace or package environment named in a stream. `spec` is the character
/// vector GNU R writes for it.
pub type EnvironmentResolver = unsafe fn(EnvironmentReference, Sexp) -> ApiResult<Sexp>;

static RESOLVER: Global<Option<EnvironmentResolver>> = Global::new(None);
/// The package placeholders the default resolver created, by name.
static PACKAGE_PLACEHOLDERS: Global<Option<Sexp>> = Global::new(None);

/// Installs the interpreter's resolver for namespace and package references, or restores the
/// default with `None`.
pub fn set_environment_resolver(resolver: Option<EnvironmentResolver>) {
    RESOLVER.set(resolver);
}

/// What a stream's header says, as `R_SerializeInfo` reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StreamHeader {
    pub format: Format,
    pub version: c_int,
    pub writer_version: c_int,
    pub min_reader_version: c_int,
    /// The writer's native encoding, which only version 3 records.
    pub native_encoding: Option<String>,
}

// Writing.

/// Serializes `object` into a byte vector.
pub unsafe fn to_bytes(object: Sexp, format: Format, version: c_int) -> ApiResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut sink = |chunk: &[u8]| {
            bytes.extend_from_slice(chunk);
            Ok(())
        };
        serialize(object, format, version, &mut sink, None, Chunking::Buffered)?;
    }
    Ok(bytes)
}

/// How the writer hands bytes to its sink.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Chunking {
    /// In large chunks, for a sink that only collects the bytes.
    Buffered,
    /// One call per field, as GNU R calls a stream's `OutBytes`.
    PerField,
}

/// Serializes `object`, handing the bytes to `sink`. `hook` is consulted for every
/// environment (other than the special, namespace and package ones), external pointer and weak
/// reference; a character vector it returns is written in the object's place.
pub unsafe fn serialize(
    object: Sexp,
    format: Format,
    version: c_int,
    sink: &mut dyn FnMut(&[u8]) -> ApiResult<()>,
    hook: Option<PersistHook<'_>>,
    chunking: Chunking,
) -> ApiResult<()> {
    if !matches!(version, 2 | 3) {
        return Err(error(format!("version {version} not supported")));
    }
    let mark = runtime().heap.handle_mark();
    runtime().heap.root(object);
    let mut writer = Writer {
        format,
        version,
        flush_threshold: if chunking == Chunking::Buffered {
            FLUSH_THRESHOLD
        } else {
            0
        },
        buffer: Vec::new(),
        sink,
        refs: FxHashMap::default(),
        remembered: 0,
        hook,
        known: *runtime().heap.known(),
    };
    let result = writer
        .write_header()
        .and_then(|()| writer.write_item(object))
        .and_then(|()| writer.flush());
    runtime().heap.release_handles(mark);
    result
}

const FLUSH_THRESHOLD: usize = 64 * 1024;

struct Writer<'s, 'h> {
    format: Format,
    version: c_int,
    flush_threshold: usize,
    buffer: Vec<u8>,
    sink: &'s mut dyn FnMut(&[u8]) -> ApiResult<()>,
    /// The back-reference index of every remembered object.
    refs: FxHashMap<Sexp, usize>,
    /// How many objects the reader will have remembered, which exceeds `refs.len()` once the
    /// persistence hook names an object a second time: GNU R remembers it again under a new
    /// index, and so does every reader.
    remembered: usize,
    hook: Option<PersistHook<'h>>,
    known: WellKnown,
}

impl Writer<'_, '_> {
    fn flush(&mut self) -> ApiResult<()> {
        if !self.buffer.is_empty() {
            (self.sink)(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> ApiResult<()> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= self.flush_threshold {
            self.flush()?;
        }
        Ok(())
    }

    fn write_integer(&mut self, value: i32) -> ApiResult<()> {
        match self.format {
            Format::Xdr => self.write_bytes(&value.to_be_bytes()),
            Format::Binary => self.write_bytes(&value.to_ne_bytes()),
            Format::Ascii | Format::AsciiHex => {
                if value == NA_INTEGER {
                    self.write_bytes(b"NA\n")
                } else {
                    self.write_bytes(format!("{value}\n").as_bytes())
                }
            }
        }
    }

    fn write_real(&mut self, value: f64) -> ApiResult<()> {
        match self.format {
            Format::Xdr => self.write_bytes(&value.to_bits().to_be_bytes()),
            Format::Binary => self.write_bytes(&value.to_ne_bytes()),
            Format::Ascii | Format::AsciiHex => {
                let text = if value.is_finite() {
                    if self.format == Format::Ascii {
                        format_general(value, 16)
                    } else {
                        format_hex(value)
                    }
                } else if is_na_real(value) {
                    "NA".to_owned()
                } else if value.is_nan() {
                    "NaN".to_owned()
                } else if value < 0.0 {
                    "-Inf".to_owned()
                } else {
                    "Inf".to_owned()
                };
                self.write_bytes(text.as_bytes())?;
                self.write_bytes(b"\n")
            }
        }
    }

    /// A string's bytes, after its length has been written. Text formats escape it with C's
    /// escapes and octal for everything outside printable ASCII, and end it with a newline.
    fn write_string(&mut self, bytes: &[u8]) -> ApiResult<()> {
        if !self.format.is_text() {
            return self.write_bytes(bytes);
        }
        let mut text = Vec::with_capacity(bytes.len() + 1);
        for &byte in bytes {
            match byte {
                b'\n' => text.extend_from_slice(b"\\n"),
                b'\t' => text.extend_from_slice(b"\\t"),
                0x0b => text.extend_from_slice(b"\\v"),
                0x08 => text.extend_from_slice(b"\\b"),
                b'\r' => text.extend_from_slice(b"\\r"),
                0x0c => text.extend_from_slice(b"\\f"),
                0x07 => text.extend_from_slice(b"\\a"),
                b'\\' => text.extend_from_slice(b"\\\\"),
                b'?' => text.extend_from_slice(b"\\?"),
                b'\'' => text.extend_from_slice(b"\\'"),
                b'"' => text.extend_from_slice(b"\\\""),
                33..=126 => text.push(byte),
                _ => text.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
            }
        }
        text.push(b'\n');
        self.write_bytes(&text)
    }

    fn write_length(&mut self, length: R_xlen_t) -> ApiResult<()> {
        if length > i32::MAX as R_xlen_t {
            self.write_integer(-1)?;
            self.write_integer((length >> 32) as i32)?;
            self.write_integer((length & 0xFFFF_FFFF) as u32 as i32)
        } else {
            self.write_integer(length as i32)
        }
    }

    fn write_header(&mut self) -> ApiResult<()> {
        let magic: &[u8] = match self.format {
            Format::Ascii | Format::AsciiHex => b"A\n",
            Format::Binary => b"B\n",
            Format::Xdr => b"X\n",
        };
        self.write_bytes(magic)?;
        for field in [
            self.version,
            WRITER_VERSION,
            min_reader_version(self.version),
        ] {
            self.write_integer(field)?;
        }
        if self.version == 3 {
            self.write_integer(NATIVE_ENCODING.len() as i32)?;
            self.write_string(NATIVE_ENCODING.as_bytes())?;
        }
        Ok(())
    }

    fn remember(&mut self, object: Sexp) {
        self.remembered += 1;
        self.refs.insert(object, self.remembered);
    }

    unsafe fn write_item(&mut self, object: Sexp) -> ApiResult<()> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || unsafe {
            self.write_item_unguarded(object)
        })
    }

    unsafe fn write_item_unguarded(&mut self, mut object: Sexp) -> ApiResult<()> {
        // Pairlist tails are written in a loop, so a long list costs no stack.
        loop {
            // ry's records, tuples and tagged unions have no R representation to write.
            if object.kind() == SexpType::Record {
                let kind = interp::records::native_kind(object);
                return Err(error(interp::records::boundary_message("the value", kind)));
            }
            // ry compiles its own bytecode, which no other R can run, so a compiled body is
            // written as the expression it was compiled from.
            if object.kind() == SexpType::Bytecode {
                object = crate::lists::bytecode_expression(object);
            }
            if let Some(name) = self.persistent_name(object)? {
                self.remember(object);
                self.write_integer(code::PERSISTSXP as i32)?;
                return self.write_string_vector(name);
            }
            if let Some(special) = self.special_code(object) {
                return self.write_integer(special as i32);
            }
            if let Some(&index) = self.refs.get(&object) {
                return self.write_reference(index);
            }
            if self.version >= 3 && object.is_altrep() && self.write_altrep(object)? {
                return Ok(());
            }
            crate::vectors::refuse_native(object)?;
            let kind = object.kind();
            match kind {
                SexpType::Symbol => {
                    self.remember(object);
                    self.write_integer(SexpType::Symbol.code() as i32)?;
                    return self.write_item(object.car());
                }
                SexpType::Environment => return self.write_environment(object),
                _ => {}
            }
            let has_tag = match kind {
                SexpType::Pairlist | SexpType::Language | SexpType::Promise | SexpType::Dots => {
                    object.tag() != self.known.nil
                }
                SexpType::Closure => true,
                _ => false,
            };
            let has_attr = kind != SexpType::Char && object.attrib() != self.known.nil;
            let mut levels = object.gp();
            if kind == SexpType::Char {
                levels &= !CHARSXP_SESSION_BITS;
                // `NA_STRING` is not an ASCII string in GNU R, whatever its bytes.
                if object == self.known.na_string {
                    levels = 0;
                }
            }
            let flags = pack_flags(kind.code(), levels, object.is_object(), has_attr, has_tag);
            match kind {
                SexpType::Pairlist
                | SexpType::Language
                | SexpType::Closure
                | SexpType::Promise
                | SexpType::Dots => {
                    self.write_integer(flags as i32)?;
                    if has_attr {
                        self.write_item(object.attrib())?;
                    }
                    if has_tag {
                        self.write_item(object.tag())?;
                    }
                    self.write_item(object.car())?;
                    object = object.cdr();
                    continue;
                }
                _ => {}
            }
            self.write_integer(flags as i32)?;
            self.write_payload(object, kind)?;
            if has_attr {
                self.write_item(object.attrib())?;
            }
            return Ok(());
        }
    }

    /// Writes an ALTREP object as `ALTREP_SXP`: its class information, the state its class
    /// serializes, and its attributes. A class without a `Serialized_state` method answers
    /// `false`, and the object is written as the ordinary vector it represents.
    unsafe fn write_altrep(&mut self, object: Sexp) -> ApiResult<bool> {
        let Some(info) = altrep::serialized_class(object) else {
            return Ok(false);
        };
        let Some(state) = altrep::serialized_state(object)? else {
            return Ok(false);
        };
        runtime().heap.root(state);
        let flags = pack_flags(
            code::ALTREP_SXP,
            object.gp(),
            object.is_object(),
            false,
            false,
        );
        self.write_integer(flags as i32)?;
        self.write_item(info)?;
        self.write_item(state)?;
        self.write_item(object.attrib())?;
        Ok(true)
    }

    unsafe fn write_payload(&mut self, object: Sexp, kind: SexpType) -> ApiResult<()> {
        // An ALTREP object reaches here only when it is written as the vector it represents
        // (a version-2 stream, or a class without serialized state).
        let object = if object.is_altrep() {
            let expanded = altrep::materialize(object)?;
            runtime().heap.root(expanded);
            expanded
        } else {
            object
        };
        match kind {
            SexpType::ExternalPtr => {
                self.remember(object);
                self.write_item(object.external_protected())?;
                self.write_item(object.external_tag())
            }
            SexpType::WeakRef => {
                self.remember(object);
                Ok(())
            }
            SexpType::Special | SexpType::Builtin => {
                let name = primitive_name(object.primitive_index());
                self.write_integer(name.len() as i32)?;
                self.write_string(name.as_bytes())
            }
            SexpType::Char => {
                if object == self.known.na_string {
                    self.write_integer(-1)
                } else {
                    self.write_integer(object.length() as i32)?;
                    self.write_string(object.bytes())
                }
            }
            SexpType::Logical | SexpType::Integer => {
                self.write_length(object.length())?;
                let values = object.slice::<i32>();
                match self.format {
                    Format::Binary => self.write_bytes(bytes_of(values)),
                    _ => values
                        .iter()
                        .try_for_each(|&value| self.write_integer(value)),
                }
            }
            SexpType::Real => {
                self.write_length(object.length())?;
                let values = object.slice::<f64>();
                match self.format {
                    Format::Binary => self.write_bytes(bytes_of(values)),
                    _ => values.iter().try_for_each(|&value| self.write_real(value)),
                }
            }
            SexpType::Complex => {
                self.write_length(object.length())?;
                let values = object.slice::<Rcomplex>();
                match self.format {
                    Format::Binary => self.write_bytes(bytes_of(values)),
                    _ => values.iter().try_for_each(|value| {
                        self.write_real(value.r)?;
                        self.write_real(value.i)
                    }),
                }
            }
            SexpType::String | SexpType::List | SexpType::Expression => {
                self.write_length(object.length())?;
                for index in 0..object.length() as usize {
                    self.write_item(object.element(index))?;
                }
                Ok(())
            }
            SexpType::Raw => {
                self.write_length(object.length())?;
                let bytes = object.bytes();
                if self.format.is_text() {
                    let mut text = Vec::with_capacity(bytes.len() * 3);
                    for byte in bytes {
                        text.extend_from_slice(format!("{byte:02x}\n").as_bytes());
                    }
                    self.write_bytes(&text)
                } else {
                    self.write_bytes(bytes)
                }
            }
            SexpType::Object => Ok(()),
            SexpType::Bytecode => Err(error("cannot serialize bytecode objects")),
            _ => Err(error(format!("WriteItem: unknown type {}", kind.code()))),
        }
    }

    unsafe fn write_environment(&mut self, env: Sexp) -> ApiResult<()> {
        self.remember(env);
        if let Some(name) = package_name(env) {
            let text = utf8_text(name.element(0))?.into_owned();
            warn(format!(
                "\u{2018}{text}\u{2019} may not be available when loading"
            ));
            self.write_integer(code::PACKAGESXP as i32)?;
            return self.write_string_vector(name);
        }
        if let Some(spec) = namespace_spec(env) {
            self.write_integer(code::NAMESPACESXP as i32)?;
            return self.write_string_vector(spec);
        }
        self.write_integer(SexpType::Environment.code() as i32)?;
        self.write_integer(env.has_gp(gp::FRAME_LOCKED) as i32)?;
        self.write_item(env.enclosure())?;
        // `frame_symbols` already lists in GNU R's order, which puts the newest binding of an
        // unhashed frame first.
        let symbols = runtime().heap.frame_symbols(env);
        for &symbol in &symbols {
            let Some(cell) = runtime().heap.frame_cell(env, symbol) else {
                continue;
            };
            let levels = cell.gp() & (gp::BINDING_LOCKED | gp::ACTIVE_BINDING);
            let flags = pack_flags(SexpType::Pairlist.code(), levels, false, false, true);
            self.write_integer(flags as i32)?;
            self.write_item(cell.car())?;
            self.write_item(cell.cdr())?;
        }
        self.write_integer(code::NILVALUE_SXP as i32)?;
        // The hash table: none, because the frame above holds every binding.
        self.write_integer(code::NILVALUE_SXP as i32)?;
        self.write_item(env.attrib())
    }

    unsafe fn write_string_vector(&mut self, strings: Sexp) -> ApiResult<()> {
        self.write_integer(0)?;
        self.write_integer(strings.length() as i32)?;
        for index in 0..strings.length() as usize {
            self.write_item(strings.element(index))?;
        }
        Ok(())
    }

    fn write_reference(&mut self, index: usize) -> ApiResult<()> {
        if index > MAX_PACKED_INDEX {
            self.write_integer(code::REFSXP as i32)?;
            self.write_integer(index as i32)
        } else {
            self.write_integer(((index << 8) as u32 | code::REFSXP) as i32)
        }
    }

    fn special_code(&self, object: Sexp) -> Option<u32> {
        let known = &self.known;
        Some(if object == known.nil {
            code::NILVALUE_SXP
        } else if object == known.empty_env {
            code::EMPTYENV_SXP
        } else if object == known.base_env {
            code::BASEENV_SXP
        } else if object == known.global_env {
            code::GLOBALENV_SXP
        } else if object == known.unbound {
            code::UNBOUNDVALUE_SXP
        } else if object == known.missing_arg {
            code::MISSINGARG_SXP
        } else if object == known.base_namespace {
            code::BASENAMESPACE_SXP
        } else {
            return None;
        })
    }

    /// The name the persistence hook gives `object`, for the types GNU R offers it.
    unsafe fn persistent_name(&mut self, object: Sexp) -> ApiResult<Option<Sexp>> {
        let Some(hook) = self.hook.as_mut() else {
            return Ok(None);
        };
        match object.kind() {
            SexpType::WeakRef | SexpType::ExternalPtr => {}
            SexpType::Environment => {
                let known = &self.known;
                if object == known.global_env
                    || object == known.base_env
                    || object == known.empty_env
                    || object == known.base_namespace
                    || namespace_spec(object).is_some()
                    || package_name(object).is_some()
                {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
        // GNU R asks the hook again for an object it already wrote, before looking for a
        // back-reference, and writes the name again; hooks that name objects keep their own
        // table, as the lazy-load database writer does.
        let name = hook(object)?;
        runtime().heap.root(name);
        if name.kind() == SexpType::Nil {
            return Ok(None);
        }
        if name.kind() != SexpType::String || name.length() == 0 {
            return Err(error(
                "a persistence hook must return a non-empty character vector or NULL",
            ));
        }
        Ok(Some(name))
    }
}

fn pack_flags(kind: u32, levels: u16, is_object: bool, has_attr: bool, has_tag: bool) -> u32 {
    let mut flags = kind | (u32::from(levels) << 12);
    if is_object {
        flags |= IS_OBJECT;
    }
    if has_attr {
        flags |= HAS_ATTR;
    }
    if has_tag {
        flags |= HAS_TAG;
    }
    flags
}

fn bytes_of<T>(values: &[T]) -> &[u8] {
    // SAFETY: the element types are plain numbers without padding.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values)) }
}

/// The spec of a namespace environment, `c(name, version)`, if `env` is one.
unsafe fn namespace_spec(env: Sexp) -> Option<Sexp> {
    let info = frame_value(env, R_NamespaceEnvSymbol.get());
    if info.kind() != SexpType::Environment {
        return None;
    }
    let spec = frame_value(info, R_SpecSymbol.get());
    (spec.kind() == SexpType::String && spec.length() > 0).then_some(spec)
}

/// The `name` attribute of a package environment (`"package:stats"`), if `env` is one.
unsafe fn package_name(env: Sexp) -> Option<Sexp> {
    let name = crate::attributes::raw_attribute(env, R_NameSymbol.get());
    (name.kind() == SexpType::String
        && name.length() > 0
        && name.element(0).bytes().starts_with(b"package:"))
    .then_some(name)
}

/// Stack the serializer keeps free before recursing, and grows by when it runs short, so a
/// deeply nested object never overflows the thread's stack.
const STACK_RED_ZONE: usize = 128 * 1024;
const STACK_GROWTH: usize = 4 * 1024 * 1024;

// Reading.

/// Where a stream's bytes come from.
pub trait Source {
    /// Fills `buffer` completely, or fails.
    fn read(&mut self, buffer: &mut [u8]) -> ApiResult<()>;
    /// One byte, or `None` at the end of the input. Only text formats read bytewise.
    fn read_byte(&mut self) -> ApiResult<Option<u8>>;
    /// How many bytes remain, when the source knows, so a corrupt length fails before it
    /// allocates.
    fn remaining(&self) -> Option<usize> {
        None
    }
}

/// A stream held in memory.
pub struct SliceSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceSource<'a> {
    pub fn new(bytes: &'a [u8]) -> SliceSource<'a> {
        SliceSource { bytes, position: 0 }
    }
}

impl Source for SliceSource<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> ApiResult<()> {
        let end = self
            .position
            .checked_add(buffer.len())
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| error("error reading from connection"))?;
        buffer.copy_from_slice(&self.bytes[self.position..end]);
        self.position = end;
        Ok(())
    }

    fn read_byte(&mut self) -> ApiResult<Option<u8>> {
        let byte = self.bytes.get(self.position).copied();
        if byte.is_some() {
            self.position += 1;
        }
        Ok(byte)
    }

    fn remaining(&self) -> Option<usize> {
        Some(self.bytes.len() - self.position)
    }
}

/// Unserializes a stream held in memory. The result is rooted in the current handle scope.
pub unsafe fn from_bytes(bytes: &[u8]) -> ApiResult<Sexp> {
    let mut source = SliceSource::new(bytes);
    Ok(unserialize(&mut source, None)?.0)
}

/// Any byte stream either fails to read with an error or reads into an object that writes back
/// and reads again. Nothing panics. Shared by the in-tree corrupted-stream harness and the
/// coverage-guided `fuzz/` target.
///
/// # Safety
/// The caller has the runtime to itself, as for every API call.
pub unsafe fn check_unserialize_input(bytes: &[u8]) {
    unsafe {
        let mark = runtime().heap.handle_mark();
        if let Ok(object) = from_bytes(bytes) {
            let written = to_bytes(object, Format::Xdr, 3);
            let Ok(written) = written else {
                panic!("an object read from {bytes:02x?} does not write back: {written:?}");
            };
            let again = from_bytes(&written);
            assert!(
                again.is_ok(),
                "an object read from {bytes:02x?} wrote a stream that does not read: {again:?}"
            );
        }
        runtime().heap.release_handles(mark);
    }
}

/// Reads a stream's header only, as `R_SerializeInfo` does.
pub unsafe fn header_of(bytes: &[u8]) -> ApiResult<StreamHeader> {
    let mut source = SliceSource::new(bytes);
    let mut reader = Reader::new(&mut source, None);
    reader.read_header(None)
}

/// Unserializes one object from `source`. `hook` restores the objects a persistence hook named
/// when they were written. The result is rooted in the current handle scope.
pub unsafe fn unserialize(
    source: &mut dyn Source,
    hook: Option<PersistHook<'_>>,
) -> ApiResult<(Sexp, StreamHeader)> {
    read_stream(source, hook, None)
}

/// `unserialize`, failing unless the stream is in the `expected` format, when one is given.
unsafe fn read_stream(
    source: &mut dyn Source,
    hook: Option<PersistHook<'_>>,
    expected: Option<Format>,
) -> ApiResult<(Sexp, StreamHeader)> {
    let mark = runtime().heap.handle_mark();
    let mut reader = Reader::new(source, hook);
    let result = reader
        .read_header(expected)
        .and_then(|header| Ok((reader.read_item()?, header)));
    runtime().heap.release_handles(mark);
    let (object, header) = result?;
    runtime().heap.root(object);
    Ok((object, header))
}

struct Reader<'s, 'h> {
    format: Format,
    source: &'s mut dyn Source,
    /// One byte read ahead in a text stream.
    pushed_back: Option<u8>,
    /// Remembered objects in the order they were read; back-references index it from one.
    /// Each is also rooted in the handle scope of the read.
    refs: Vec<Sexp>,
    hook: Option<PersistHook<'h>>,
    /// The writer's native encoding is Latin-1, so its unmarked non-ASCII strings are Latin-1.
    latin1_native: bool,
    known: WellKnown,
}

impl<'s, 'h> Reader<'s, 'h> {
    unsafe fn new(source: &'s mut dyn Source, hook: Option<PersistHook<'h>>) -> Reader<'s, 'h> {
        Reader {
            format: Format::Xdr,
            source,
            pushed_back: None,
            refs: Vec::new(),
            hook,
            latin1_native: false,
            known: *runtime().heap.known(),
        }
    }

    /// Reads the header. `expected` is the format the caller insists on, if any.
    unsafe fn read_header(&mut self, expected: Option<Format>) -> ApiResult<StreamHeader> {
        let mut magic = [0u8; 2];
        self.source.read(&mut magic)?;
        let format = match magic {
            [b'A', _] => Format::Ascii,
            [b'B', _] => Format::Binary,
            [b'X', _] => Format::Xdr,
            [b'\n', b'A'] => {
                let mut newline = [0u8; 1];
                self.source.read(&mut newline)?;
                Format::Ascii
            }
            _ => return Err(error("unknown input format")),
        };
        if let Some(expected) = expected
            && (expected.is_text() != format.is_text()
                || (!expected.is_text() && expected != format))
        {
            return Err(error("input format does not match specified format"));
        }
        self.format = format;
        let version = self.read_integer()?;
        let writer_version = self.read_integer()?;
        let min_reader_version = self.read_integer()?;
        let native_encoding = match version {
            2 => None,
            3 => {
                let length = self.read_integer()?;
                if !(0..=63).contains(&length) {
                    return Err(error("invalid length of encoding name"));
                }
                let name = self.read_string(length as usize)?;
                Some(String::from_utf8_lossy(&name).into_owned())
            }
            _ => {
                let (major, minor, patch) = split_version(writer_version);
                let (need_major, need_minor, need_patch) = split_version(min_reader_version);
                return Err(error(format!(
                    "cannot read workspace version {version} written by R {major}.{minor}.{patch}; need R {need_major}.{need_minor}.{need_patch} or newer"
                )));
            }
        };
        self.latin1_native = native_encoding.as_deref().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name == "latin1" || name == "iso-8859-1" || name == "iso8859-1"
        });
        Ok(StreamHeader {
            format,
            version,
            writer_version,
            min_reader_version,
            native_encoding,
        })
    }

    fn next_byte(&mut self) -> ApiResult<Option<u8>> {
        match self.pushed_back.take() {
            Some(byte) => Ok(Some(byte)),
            None => self.source.read_byte(),
        }
    }

    /// The next whitespace-delimited word of a text stream.
    fn read_word(&mut self) -> ApiResult<Vec<u8>> {
        let mut byte = self.next_byte()?;
        while byte.is_some_and(is_space) {
            byte = self.next_byte()?;
        }
        let mut word = Vec::new();
        while let Some(current) = byte {
            if is_space(current) {
                break;
            }
            if word.len() >= 1000 {
                return Err(error("read error"));
            }
            word.push(current);
            byte = self.next_byte()?;
        }
        if word.is_empty() {
            return Err(error("read error"));
        }
        Ok(word)
    }

    fn read_integer(&mut self) -> ApiResult<i32> {
        match self.format {
            Format::Xdr => {
                let mut bytes = [0u8; 4];
                self.source.read(&mut bytes)?;
                Ok(i32::from_be_bytes(bytes))
            }
            Format::Binary => {
                let mut bytes = [0u8; 4];
                self.source.read(&mut bytes)?;
                Ok(i32::from_ne_bytes(bytes))
            }
            Format::Ascii | Format::AsciiHex => {
                let word = self.read_word()?;
                if word == b"NA" {
                    return Ok(NA_INTEGER);
                }
                std::str::from_utf8(&word)
                    .ok()
                    .and_then(|text| text.parse::<i32>().ok())
                    .ok_or_else(|| error("read error"))
            }
        }
    }

    fn read_real(&mut self) -> ApiResult<f64> {
        match self.format {
            Format::Xdr => {
                let mut bytes = [0u8; 8];
                self.source.read(&mut bytes)?;
                Ok(f64::from_bits(u64::from_be_bytes(bytes)))
            }
            Format::Binary => {
                let mut bytes = [0u8; 8];
                self.source.read(&mut bytes)?;
                Ok(f64::from_ne_bytes(bytes))
            }
            Format::Ascii | Format::AsciiHex => {
                let word = self.read_word()?;
                match &word[..] {
                    b"NA" => Ok(NA_REAL),
                    b"NaN" => Ok(f64::NAN),
                    b"Inf" => Ok(f64::INFINITY),
                    b"-Inf" => Ok(f64::NEG_INFINITY),
                    _ => std::str::from_utf8(&word)
                        .ok()
                        .and_then(parse_real)
                        .ok_or_else(|| error("read error")),
                }
            }
        }
    }

    /// A string of `length` bytes, decoding a text stream's escapes.
    fn read_string(&mut self, length: usize) -> ApiResult<Vec<u8>> {
        self.check_available(length, 1)?;
        if !self.format.is_text() {
            let mut bytes = vec![0u8; length];
            self.source.read(&mut bytes)?;
            return Ok(bytes);
        }
        let mut bytes = Vec::with_capacity(length);
        if length == 0 {
            return Ok(bytes);
        }
        let mut byte = self.next_byte()?;
        while byte.is_some_and(is_space) {
            byte = self.next_byte()?;
        }
        self.pushed_back = byte;
        let eof = || error("read error");
        while bytes.len() < length {
            let current = self.next_byte()?.ok_or_else(eof)?;
            if current != b'\\' {
                bytes.push(current);
                continue;
            }
            let escaped = self.next_byte()?.ok_or_else(eof)?;
            let decoded = match escaped {
                b'n' => b'\n',
                b't' => b'\t',
                b'v' => 0x0b,
                b'b' => 0x08,
                b'r' => b'\r',
                b'f' => 0x0c,
                b'a' => 0x07,
                b'0'..=b'7' => {
                    let mut value: u32 = 0;
                    let mut digit = Some(escaped);
                    let mut count = 0;
                    while let Some(octal @ b'0'..=b'7') = digit {
                        if count == 3 {
                            break;
                        }
                        value = value * 8 + u32::from(octal - b'0');
                        count += 1;
                        digit = self.next_byte()?;
                    }
                    self.pushed_back = digit;
                    value as u8
                }
                other => other,
            };
            bytes.push(decoded);
        }
        Ok(bytes)
    }

    /// Fails early when a stream claims more than `count` items of at least `size` bytes but
    /// holds fewer bytes than that.
    fn check_available(&self, count: usize, size: usize) -> ApiResult<()> {
        if let Some(remaining) = self.source.remaining()
            && count.saturating_mul(size) > remaining
        {
            return Err(error(
                "error reading from connection: the stream is truncated",
            ));
        }
        Ok(())
    }

    fn read_length(&mut self) -> ApiResult<R_xlen_t> {
        let length = self.read_integer()?;
        let length = match length {
            -1 => {
                let high = self.read_integer()? as u32 as i64;
                let low = self.read_integer()? as u32 as i64;
                (high << 32) + low
            }
            length if length < -1 => {
                return Err(error("negative serialized length for vector"));
            }
            length => length as i64,
        };
        Ok(length as R_xlen_t)
    }

    fn remember(&mut self, object: Sexp) {
        runtime().heap.root(object);
        self.refs.push(object);
    }

    fn reference(&self, index: usize) -> ApiResult<Sexp> {
        index
            .checked_sub(1)
            .and_then(|position| self.refs.get(position).copied())
            .ok_or_else(|| error("reference index out of range"))
    }

    /// Reads one item, which is rooted in the handle scope of the read.
    unsafe fn read_item(&mut self) -> ApiResult<Sexp> {
        let flags = self.read_integer()? as u32;
        self.read_item_with(flags)
    }

    unsafe fn read_item_with(&mut self, flags: u32) -> ApiResult<Sexp> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || unsafe {
            let item = self.read_item_unguarded(flags)?;
            runtime().heap.root(item);
            Ok(item)
        })
    }

    unsafe fn read_item_unguarded(&mut self, flags: u32) -> ApiResult<Sexp> {
        let (kind, levels, is_object, has_attr, _) = unpack_flags(flags);
        let known = self.known;
        match kind {
            code::NILVALUE_SXP => return Ok(known.nil),
            code::EMPTYENV_SXP => return Ok(known.empty_env),
            code::BASEENV_SXP => return Ok(known.base_env),
            code::GLOBALENV_SXP => return Ok(known.global_env),
            code::UNBOUNDVALUE_SXP => return Ok(known.unbound),
            code::MISSINGARG_SXP => return Ok(known.missing_arg),
            code::BASENAMESPACE_SXP => return Ok(known.base_namespace),
            code::REFSXP => {
                let index = match (flags >> 8) as usize {
                    0 => self.read_integer()? as usize,
                    index => index,
                };
                return self.reference(index);
            }
            code::PERSISTSXP => {
                let name = self.read_string_vector()?;
                let hook = self
                    .hook
                    .as_mut()
                    .ok_or_else(|| error("no restore method available"))?;
                let object = hook(name)?;
                self.remember(object);
                return Ok(object);
            }
            code::NAMESPACESXP | code::PACKAGESXP => {
                let spec = self.read_string_vector()?;
                let reference = if kind == code::NAMESPACESXP {
                    EnvironmentReference::Namespace
                } else {
                    EnvironmentReference::Package
                };
                let resolver = RESOLVER.get().unwrap_or(default_resolver);
                let env = resolver(reference, spec)?;
                self.remember(env);
                return Ok(env);
            }
            code::ALTREP_SXP => {
                let info = self.read_item()?;
                let state = self.read_item()?;
                let attributes = self.read_item()?;
                return unserialize_altrep(info, state, attributes, levels, is_object);
            }
            code::CLASSREFSXP => {
                return Err(error("this version of R cannot read class references"));
            }
            code::GENERICREFSXP => {
                return Err(error(
                    "this version of R cannot read generic function references",
                ));
            }
            _ => {}
        }
        let sexp_type = (kind < 256)
            .then(|| SexpType::from_code(kind))
            .flatten()
            .filter(|kind| *kind != SexpType::Any)
            .ok_or_else(|| {
                error(format!(
                    "ReadItem: unknown type {kind}, perhaps written by later version of R"
                ))
            })?;
        match sexp_type {
            SexpType::Symbol => {
                let name = self.read_item()?;
                if name.kind() != SexpType::Char || name == known.na_string {
                    return Err(error("a symbol's name must be a string"));
                }
                let symbol = runtime().heap.install_char(name)?;
                self.remember(symbol);
                Ok(symbol)
            }
            SexpType::Environment => self.read_environment(),
            SexpType::Pairlist
            | SexpType::Language
            | SexpType::Closure
            | SexpType::Promise
            | SexpType::Dots => self.read_pairlist(flags),
            _ => {
                let object = self.read_leaf(sexp_type, levels)?;
                runtime().heap.root(object);
                if sexp_type == SexpType::Char {
                    // A string's attribute field belongs to the string cache; streams from
                    // very old versions of R may still carry one, which is read and dropped.
                    if has_attr {
                        self.read_item()?;
                    }
                    return Ok(object);
                }
                if sexp_type == SexpType::Bytecode {
                    if has_attr {
                        self.read_item()?;
                    }
                    return Ok(object);
                }
                if matches!(
                    object.kind(),
                    SexpType::Nil | SexpType::Special | SexpType::Builtin
                ) {
                    // A primitive is shared by the whole session, so the stream's flags and
                    // attributes, which GNU R never writes for one, are not applied to it.
                    if has_attr {
                        self.read_item()?;
                    }
                    return Ok(object);
                }
                object.set_gp(levels);
                object.set_object(is_object);
                if has_attr {
                    let attributes = self.read_item()?;
                    runtime().heap.set_attrib(object, attributes);
                }
                Ok(object)
            }
        }
    }

    /// A pairlist, call, closure, promise or `...` list, with its tail read in a loop.
    unsafe fn read_pairlist(&mut self, mut flags: u32) -> ApiResult<Sexp> {
        let nil = self.known.nil;
        let mut head = None;
        let mut previous: Option<Sexp> = None;
        loop {
            let (kind, levels, is_object, has_attr, has_tag) = unpack_flags(flags);
            let sexp_type = SexpType::from_code(kind).unwrap_or(SexpType::Pairlist);
            let cell = runtime().heap.alloc_triple(sexp_type, nil, nil, nil)?;
            runtime().heap.root(cell);
            cell.set_gp(levels);
            cell.set_object(is_object);
            match previous {
                Some(previous) => runtime().heap.set_cdr(previous, cell),
                None => head = Some(cell),
            }
            if has_attr {
                let attributes = self.read_item()?;
                runtime().heap.set_attrib(cell, attributes);
            }
            if has_tag {
                let tag = self.read_item()?;
                runtime().heap.set_tag(cell, tag);
            }
            let car = self.read_item()?;
            runtime().heap.set_car(cell, car);
            previous = Some(cell);
            flags = self.read_integer()? as u32;
            if !matches!(
                SexpType::from_code(flags & 0xFF),
                Some(
                    SexpType::Pairlist
                        | SexpType::Language
                        | SexpType::Closure
                        | SexpType::Promise
                        | SexpType::Dots
                )
            ) {
                break;
            }
        }
        let tail = self.read_item_with(flags)?;
        if let Some(previous) = previous {
            runtime().heap.set_cdr(previous, tail);
        }
        head.ok_or_else(|| error("an empty pairlist"))
    }

    unsafe fn read_environment(&mut self) -> ApiResult<Sexp> {
        let locked = self.read_integer()?;
        let known = self.known;
        let env = runtime().heap.alloc_environment(known.base_env)?;
        // Registered before its contents, which may refer back to it.
        self.remember(env);
        let enclosure = self.read_item()?;
        let enclosure = match enclosure.kind() {
            SexpType::Environment => enclosure,
            SexpType::Nil => known.base_env,
            _ => return Err(error("an environment's enclosure must be an environment")),
        };
        runtime().heap.set_enclosure(env, enclosure);
        let frame = self.read_item()?;
        self.bind_chain(env, frame)?;
        let table = self.read_item()?;
        match table.kind() {
            SexpType::Nil => {}
            SexpType::List => {
                for index in 0..table.length() as usize {
                    self.bind_chain(env, table.element(index))?;
                }
            }
            _ => return Err(error("an environment's hash table must be a list")),
        }
        let attributes = self.read_item()?;
        runtime().heap.set_attrib(env, attributes);
        if attributes.kind() != SexpType::Nil
            && crate::attributes::raw_attribute(env, R_ClassSymbol.get()) != known.nil
        {
            env.set_object(true);
        }
        if locked != 0 {
            env.set_gp_bits(gp::FRAME_LOCKED, true);
        }
        Ok(env)
    }

    /// Binds every cell of a frame pairlist in `env`, keeping its locked and active bits. The
    /// pairlist holds the newest binding first, so the cells are bound from its end, which
    /// keeps the order bindings were made in.
    unsafe fn bind_chain(&mut self, env: Sexp, mut chain: Sexp) -> ApiResult<()> {
        let mut cells = Vec::new();
        while chain.kind() == SexpType::Pairlist {
            if chain.tag().kind() != SexpType::Symbol {
                return Err(error("a frame binding has no symbol"));
            }
            cells.push(chain);
            chain = chain.cdr();
        }
        if chain.kind() != SexpType::Nil {
            return Err(error("an environment's frame must be a pairlist"));
        }
        for chain in cells.into_iter().rev() {
            let heap = &mut runtime().heap;
            let symbol = chain.tag();
            heap.define(env, symbol, chain.car())?;
            if let Some(cell) = heap.frame_cell(env, symbol) {
                cell.set_gp_bits(chain.gp() & (gp::BINDING_LOCKED | gp::ACTIVE_BINDING), true);
            }
        }
        Ok(())
    }

    /// The types whose payload is not a pairlist: vectors, strings, external pointers, weak
    /// references, primitives, S4 objects and bytecode.
    unsafe fn read_leaf(&mut self, kind: SexpType, levels: u16) -> ApiResult<Sexp> {
        let known = self.known;
        match kind {
            SexpType::ExternalPtr => {
                let pointer = runtime().heap.alloc_external_ptr(
                    std::ptr::null_mut(),
                    known.nil,
                    known.nil,
                )?;
                self.remember(pointer);
                let protected = self.read_item()?;
                runtime().heap.set_external_protected(pointer, protected);
                let tag = self.read_item()?;
                runtime().heap.set_external_tag(pointer, tag);
                Ok(pointer)
            }
            SexpType::WeakRef => {
                let weak = runtime()
                    .heap
                    .alloc_weak_ref(known.nil, known.nil, known.nil, 0, false)?;
                self.remember(weak);
                Ok(weak)
            }
            SexpType::Special | SexpType::Builtin => {
                let length = self.read_integer()?;
                if length < 0 {
                    return Err(error("invalid length of a primitive's name"));
                }
                let name = self.read_string(length as usize)?;
                let symbol = runtime().heap.install(&name)?;
                let value = frame_value(known.base_env, symbol);
                if matches!(value.kind(), SexpType::Special | SexpType::Builtin) {
                    Ok(value)
                } else {
                    warn(format!(
                        "unrecognized internal function name \"{}\"",
                        String::from_utf8_lossy(&name)
                    ));
                    Ok(known.nil)
                }
            }
            SexpType::Char => {
                let length = self.read_integer()?;
                match length {
                    -1 => Ok(known.na_string),
                    length if length < 0 => Err(error("negative serialized length for string")),
                    length => {
                        let bytes = self.read_string(length as usize)?;
                        let encoding = if levels & gp::UTF8 != 0 {
                            Encoding::Utf8
                        } else if levels & gp::LATIN1 != 0 {
                            Encoding::Latin1
                        } else if levels & gp::BYTES != 0 {
                            Encoding::Bytes
                        } else if self.latin1_native && !bytes.is_ascii() {
                            Encoding::Latin1
                        } else {
                            Encoding::Native
                        };
                        Ok(runtime().heap.mk_char(&bytes, encoding)?)
                    }
                }
            }
            SexpType::Logical | SexpType::Integer => {
                let length = self.read_length()?;
                self.check_available(length as usize, self.min_element_size(4))?;
                let vector = runtime().heap.alloc_vector(kind, length)?;
                runtime().heap.root(vector);
                let values = vector.slice_mut::<i32>();
                match self.format {
                    Format::Xdr | Format::Binary => {
                        self.source.read(bytes_of_mut(values))?;
                        if self.format == Format::Xdr {
                            for value in values.iter_mut() {
                                *value = i32::from_be(*value);
                            }
                        }
                    }
                    _ => {
                        for value in values.iter_mut() {
                            *value = self.read_integer()?;
                        }
                    }
                }
                Ok(vector)
            }
            SexpType::Real => {
                let length = self.read_length()?;
                self.check_available(length as usize, self.min_element_size(8))?;
                let vector = runtime().heap.alloc_vector(kind, length)?;
                runtime().heap.root(vector);
                let values = vector.slice_mut::<f64>();
                self.read_reals(values)?;
                Ok(vector)
            }
            SexpType::Complex => {
                let length = self.read_length()?;
                self.check_available(length as usize, self.min_element_size(16))?;
                let vector = runtime().heap.alloc_vector(kind, length)?;
                runtime().heap.root(vector);
                let values = vector.slice_mut::<Rcomplex>();
                // SAFETY: `Rcomplex` is two doubles with C layout.
                let parts = std::slice::from_raw_parts_mut(
                    values.as_mut_ptr() as *mut f64,
                    values.len() * 2,
                );
                self.read_reals(parts)?;
                Ok(vector)
            }
            SexpType::String | SexpType::List | SexpType::Expression => {
                let length = self.read_length()?;
                self.check_available(length as usize, self.min_element_size(4))?;
                let vector = runtime().heap.alloc_vector(kind, length)?;
                runtime().heap.root(vector);
                for index in 0..length as usize {
                    let mark = runtime().heap.handle_mark();
                    let remembered = self.refs.len();
                    let element = self.read_item()?;
                    if kind == SexpType::String && element.kind() != SexpType::Char {
                        return Err(error("a character vector's element must be a string"));
                    }
                    runtime().heap.set_element(vector, index, element);
                    // Everything the element allocated is now reachable from the rooted
                    // vector, unless the element remembered something that must stay rooted.
                    if self.refs.len() == remembered {
                        runtime().heap.release_handles(mark);
                    }
                }
                Ok(vector)
            }
            SexpType::Raw => {
                let length = self.read_length()?;
                self.check_available(length as usize, self.min_element_size(1))?;
                let vector = runtime().heap.alloc_vector(kind, length)?;
                runtime().heap.root(vector);
                let bytes = vector.slice_mut::<u8>();
                if self.format.is_text() {
                    for byte in bytes.iter_mut() {
                        let word = self.read_word()?;
                        *byte = std::str::from_utf8(&word)
                            .ok()
                            .and_then(|text| u8::from_str_radix(text, 16).ok())
                            .ok_or_else(|| error("read error"))?;
                    }
                } else {
                    self.source.read(bytes)?;
                }
                Ok(vector)
            }
            SexpType::Object => {
                let object = runtime().heap.alloc_triple(
                    SexpType::Object,
                    known.nil,
                    known.nil,
                    known.nil,
                )?;
                Ok(object)
            }
            SexpType::Bytecode => self.read_bytecode(),
            _ => Err(error(format!(
                "ReadItem: unknown type {}, perhaps written by later version of R",
                kind.code()
            ))),
        }
    }

    fn min_element_size(&self, binary: usize) -> usize {
        if self.format.is_text() { 2 } else { binary }
    }

    fn read_reals(&mut self, values: &mut [f64]) -> ApiResult<()> {
        match self.format {
            Format::Xdr | Format::Binary => {
                self.source.read(bytes_of_mut(values))?;
                if self.format == Format::Xdr {
                    for value in values.iter_mut() {
                        *value = f64::from_bits(u64::from_be(value.to_bits()));
                    }
                }
            }
            _ => {
                for value in values.iter_mut() {
                    *value = self.read_real()?;
                }
            }
        }
        Ok(())
    }

    unsafe fn read_string_vector(&mut self) -> ApiResult<Sexp> {
        if self.read_integer()? != 0 {
            return Err(error("names in persistent strings are not supported yet"));
        }
        let length = self.read_integer()?;
        if length < 0 {
            return Err(error("negative serialized length for vector"));
        }
        self.check_available(length as usize, self.min_element_size(4))?;
        let strings = runtime()
            .heap
            .alloc_vector(SexpType::String, length as isize)?;
        runtime().heap.root(strings);
        for index in 0..length as usize {
            let element = self.read_item()?;
            if element.kind() != SexpType::Char {
                return Err(error("a persistent name must be a character vector"));
            }
            runtime().heap.set_element(strings, index, element);
        }
        Ok(strings)
    }

    /// GNU R bytecode: the code vector, then the constant pool, whose first entry is the
    /// expression the code was compiled from. That expression is what the object reads as.
    /// Language objects in the pool use their own encoding, where cells shared within the pool
    /// are defined once and referred to by position.
    unsafe fn read_bytecode(&mut self) -> ApiResult<Sexp> {
        let count = self.read_integer()?;
        if count < 0 {
            return Err(error("invalid bytecode reference count"));
        }
        let mut shared = FxHashMap::default();
        self.read_bytecode_body(count as usize, &mut shared)
    }

    unsafe fn read_bytecode_body(
        &mut self,
        shared_count: usize,
        shared: &mut FxHashMap<usize, Sexp>,
    ) -> ApiResult<Sexp> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || unsafe {
            let code = self.read_item()?;
            if code.kind() != SexpType::Integer {
                return Err(error("bytecode must start with its code vector"));
            }
            let constants = self.read_integer()?;
            if constants < 0 {
                return Err(error("negative bytecode constant count"));
            }
            self.check_available(constants as usize, self.min_element_size(4))?;
            let mut expression = None;
            for _ in 0..constants {
                let kind = self.read_integer()? as u32;
                let constant = match kind {
                    21 => self.read_bytecode_body(shared_count, shared)?,
                    code::BCREPDEF | code::BCREPREF | code::ATTRLANGSXP | code::ATTRLISTSXP => {
                        self.read_bytecode_language(kind, shared_count, shared)?
                    }
                    kind if kind == SexpType::Language.code()
                        || kind == SexpType::Pairlist.code() =>
                    {
                        self.read_bytecode_language(kind, shared_count, shared)?
                    }
                    _ => self.read_item()?,
                };
                expression.get_or_insert(constant);
            }
            Ok(expression.unwrap_or(self.known.nil))
        })
    }

    unsafe fn read_bytecode_language(
        &mut self,
        kind: u32,
        shared_count: usize,
        shared: &mut FxHashMap<usize, Sexp>,
    ) -> ApiResult<Sexp> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || unsafe {
            let mut kind = kind;
            let mut position = None;
            match kind {
                code::BCREPREF => {
                    let index = self.read_integer()?;
                    return usize::try_from(index)
                        .ok()
                        .and_then(|index| shared.get(&index).copied())
                        .ok_or_else(|| error("bytecode refers to an undefined shared cell"));
                }
                code::BCREPDEF => {
                    let index = self.read_integer()?;
                    let index = usize::try_from(index)
                        .ok()
                        .filter(|&index| index < shared_count)
                        .ok_or_else(|| error("bytecode shared cell index out of range"))?;
                    position = Some(index);
                    kind = self.read_integer()? as u32;
                }
                _ => {}
            }
            let (cell_type, has_attr) = match kind {
                code::ATTRLANGSXP => (SexpType::Language, true),
                code::ATTRLISTSXP => (SexpType::Pairlist, true),
                kind if kind == SexpType::Language.code() => (SexpType::Language, false),
                kind if kind == SexpType::Pairlist.code() => (SexpType::Pairlist, false),
                // A padding word, then an ordinary item.
                _ => return self.read_item(),
            };
            let nil = self.known.nil;
            let cell = runtime().heap.alloc_triple(cell_type, nil, nil, nil)?;
            runtime().heap.root(cell);
            if let Some(position) = position {
                shared.insert(position, cell);
            }
            if has_attr {
                let attributes = self.read_item()?;
                runtime().heap.set_attrib(cell, attributes);
            }
            let tag = self.read_item()?;
            runtime().heap.set_tag(cell, tag);
            let car_kind = self.read_integer()? as u32;
            let car = self.read_bytecode_language(car_kind, shared_count, shared)?;
            runtime().heap.set_car(cell, car);
            let cdr_kind = self.read_integer()? as u32;
            let cdr = self.read_bytecode_language(cdr_kind, shared_count, shared)?;
            runtime().heap.set_cdr(cell, cdr);
            Ok(cell)
        })
    }
}

fn unpack_flags(flags: u32) -> (u32, u16, bool, bool, bool) {
    (
        flags & 0xFF,
        (flags >> 12) as u16,
        flags & IS_OBJECT != 0,
        flags & HAS_ATTR != 0,
        flags & HAS_TAG != 0,
    )
}

fn split_version(version: c_int) -> (c_int, c_int, c_int) {
    (version / 65536, (version % 65536) / 256, version % 256)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn bytes_of_mut<T>(values: &mut [T]) -> &mut [u8] {
    // SAFETY: the element types are plain numbers, valid for every bit pattern.
    unsafe {
        std::slice::from_raw_parts_mut(values.as_mut_ptr().cast(), std::mem::size_of_val(values))
    }
}

/// Rebuilds an ALTREP object from its class and serialized state, through the class registry
/// (`crate::altrep`), which holds base R's classes and every class a loaded package registered.
/// A class that is not registered makes the reader load its package's namespace and retry, as
/// GNU R does; failing that, it reads as a zero-length vector of its type, with GNU R's warning.
unsafe fn unserialize_altrep(
    info: Sexp,
    state: Sexp,
    attributes: Sexp,
    levels: u16,
    is_object: bool,
) -> ApiResult<Sexp> {
    let malformed = || error("invalid ALTREP class information");
    if info.kind() != SexpType::Pairlist
        || info.cdr().kind() != SexpType::Pairlist
        || info.cdr().cdr().kind() != SexpType::Pairlist
    {
        return Err(malformed());
    }
    let class = info.car();
    let package = info.cdr().car();
    let type_code = info.cdr().cdr().car();
    if class.kind() != SexpType::Symbol
        || package.kind() != SexpType::Symbol
        || type_code.kind() != SexpType::Integer
        || type_code.length() != 1
    {
        return Err(malformed());
    }
    let kind = SexpType::from_code(type_code.slice::<i32>()[0] as u32)
        .filter(|kind| kind.is_vector() && *kind != SexpType::Char)
        .ok_or_else(malformed)?;
    for handle in [info, state, attributes] {
        runtime().heap.root(handle);
    }
    let object_bit = is_object as c_int;
    let levels = levels as c_int;
    if let Some(object) = altrep::unserialize(info, state, attributes, object_bit, levels)? {
        return Ok(object);
    }
    if crate::eval::namespaces_loadable()? {
        let name = crate::strings::mk_char(package.car().bytes(), heap::strings::Encoding::Utf8)?;
        let name = crate::memory::scalar_string(name)?;
        runtime().heap.root(name);
        crate::eval::call_namespaced("base", "loadNamespace", &[name])?;
        if let Some(object) = altrep::unserialize(info, state, attributes, object_bit, levels)? {
            return Ok(object);
        }
    }
    warn(format!(
        "cannot unserialize ALTVEC object of class '{}' from package '{}'; returning length zero vector",
        String::from_utf8_lossy(class.car().bytes()),
        String::from_utf8_lossy(package.car().bytes()),
    ));
    let object = runtime().heap.alloc_vector(kind, 0)?;
    runtime().heap.root(object);
    runtime().heap.set_attrib(object, attributes);
    object.set_gp(levels as u16);
    object.set_object(is_object);
    Ok(object)
}

/// The resolver used until the interpreter installs its own. A namespace comes from the
/// namespace registry; one that is not registered becomes a placeholder namespace (an
/// environment whose `.__NAMESPACE__.` holds the spec) that is registered under its name. A
/// package environment comes from the search path (the global environment's enclosures);
/// one that is not attached becomes a placeholder named like it, recorded for the session.
pub unsafe fn default_resolver(reference: EnvironmentReference, spec: Sexp) -> ApiResult<Sexp> {
    let heap = &mut runtime().heap;
    let known = *heap.known();
    if spec.kind() != SexpType::String || spec.length() == 0 {
        return Err(error("bad namespace or package name"));
    }
    let name = spec.element(0);
    match reference {
        EnvironmentReference::Namespace => {
            if name.bytes() == b"base" {
                return Ok(known.base_namespace);
            }
            let symbol = heap.install_char(name)?;
            let registered = frame_value(known.namespace_registry, symbol);
            if registered.kind() == SexpType::Environment {
                return Ok(registered);
            }
            let heap = &mut runtime().heap;
            let namespace = heap.alloc_environment(known.base_namespace)?;
            heap.root(namespace);
            let info = heap.alloc_environment(known.base_env)?;
            heap.root(info);
            heap.define(info, R_SpecSymbol.get(), spec)?;
            heap.define(namespace, R_NamespaceEnvSymbol.get(), info)?;
            heap.define(known.namespace_registry, symbol, namespace)?;
            Ok(namespace)
        }
        EnvironmentReference::Package => {
            let mut env = known.global_env.enclosure();
            while env != known.empty_env {
                if let Some(found) = package_name(env)
                    && found.element(0).bytes() == name.bytes()
                {
                    return Ok(env);
                }
                env = env.enclosure();
            }
            let records = match PACKAGE_PLACEHOLDERS.get() {
                Some(records) => records,
                None => {
                    let records = heap.alloc_environment(known.empty_env)?;
                    heap.add_global_root(records);
                    PACKAGE_PLACEHOLDERS.set(Some(records));
                    records
                }
            };
            let heap = &mut runtime().heap;
            let symbol = heap.install_char(name)?;
            let recorded = frame_value(records, symbol);
            if recorded.kind() == SexpType::Environment {
                return Ok(recorded);
            }
            let heap = &mut runtime().heap;
            let placeholder = heap.alloc_environment(known.base_env)?;
            heap.root(placeholder);
            let names = heap.alloc_vector(SexpType::String, 1)?;
            heap.set_element(names, 0, name);
            heap.root(names);
            crate::attributes::set_attrib(placeholder, R_NameSymbol.get(), names)?;
            runtime().heap.define(records, symbol, placeholder)?;
            Ok(placeholder)
        }
    }
}

// Number formatting for the text formats, matching C's `%.16g` and `%a`.

/// C's `%.<precision>g` for a finite value.
fn format_general(value: f64, precision: usize) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    let scientific = format!("{:.*e}", precision - 1, value);
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if exponent < -4 || exponent >= precision as i32 {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{}e{sign}{:02}",
            trim_fraction(mantissa),
            exponent.unsigned_abs()
        )
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        trim_fraction(&format!("{value:.decimals$}")).to_owned()
    }
}

fn trim_fraction(text: &str) -> &str {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    }
}

/// C's `%a` for a finite value, as glibc prints it: the shortest exact hexadecimal mantissa.
fn format_hex(value: f64) -> String {
    let bits = value.to_bits();
    let sign = if bits >> 63 != 0 { "-" } else { "" };
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = match (exponent, fraction) {
        (0, 0) => return format!("{sign}0x0p+0"),
        (0, _) => (0, -1022),
        _ => (1, exponent - 1023),
    };
    let digits = format!("{fraction:013x}");
    let digits = digits.trim_end_matches('0');
    let point = if digits.is_empty() { "" } else { "." };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{sign}0x{lead}{point}{digits}p{exponent_sign}{}",
        exponent.unsigned_abs()
    )
}

/// A double written by `%.16g` or `%a`.
fn parse_real(text: &str) -> Option<f64> {
    let (negative, unsigned) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = match unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        Some(hex) => parse_hex_magnitude(hex)?,
        None => unsigned.parse::<f64>().ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn parse_hex_magnitude(text: &str) -> Option<f64> {
    let (digits, exponent) = match text.find(['p', 'P']) {
        Some(at) => (&text[..at], text[at + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let mut mantissa: u64 = 0;
    let mut shift: i32 = 0;
    for (index, character) in whole.chars().chain(fraction.chars()).enumerate() {
        let digit = character.to_digit(16)? as u64;
        if mantissa >> 56 != 0 {
            // Digits beyond 64 bits of precision cannot change a double; keep the scale.
            if index < whole.len() {
                shift -= 4;
            }
            continue;
        }
        mantissa = mantissa * 16 + digit;
        if index >= whole.len() {
            shift += 4;
        }
    }
    let scale = exponent.checked_sub(shift)?;
    // Two steps, so neither factor overflows or underflows on its own.
    let half = scale / 2;
    Some(mantissa as f64 * 2f64.powi(half) * 2f64.powi(scale - half))
}

// Compressed files and lazy-load databases.

/// The bytes inside a file written by `saveRDS` or `save`, which may be gzip, bzip2 or xz
/// compressed, or not compressed at all.
pub fn decompress(bytes: &[u8]) -> ApiResult<Cow<'_, [u8]>> {
    interp::connections::decompress(bytes)
        .map_err(|problem| error(format!("cannot decompress data: {problem}")))
}

/// `readRDS`: the object in a file `saveRDS` wrote. The result is rooted in the current handle
/// scope.
pub unsafe fn read_rds(path: &Path) -> ApiResult<Sexp> {
    let bytes = std::fs::read(path)
        .map_err(|problem| error(format!("cannot open file '{}': {problem}", path.display())))?;
    from_bytes(&decompress(&bytes)?)
}

/// How a lazy-load database compresses its entries, which its `.rdx` index records in its
/// `compressed` element.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LazyLoadCompression {
    /// `compressed = FALSE`: the entries are plain streams.
    None,
    /// `compressed = TRUE`: a big-endian 4-byte length, then a zlib stream.
    Zlib,
    /// `compressed = 2` or `3`: the length, then a type byte (`1` zlib, `2` bzip2, `Z` raw
    /// LZMA2 as xz uses it, `0` stored) and the data, so one database may mix methods.
    Typed,
}

impl LazyLoadCompression {
    /// The method an index's `compressed` value names (`FALSE`, `TRUE`, `2` or `3`).
    pub fn from_index_value(value: i32) -> LazyLoadCompression {
        match value {
            0 => LazyLoadCompression::None,
            1 => LazyLoadCompression::Zlib,
            _ => LazyLoadCompression::Typed,
        }
    }
}

/// The object stored at `offset` with `length` bytes in a lazy-load database (`.rdb`), which
/// the `.rdx` index (read with [`read_rds`]) locates by name. Environments other than
/// namespaces are stored as separate entries and referred to by persistent names (`"env::1"`,
/// listed in the index's `references`), which `hook` restores. The result is rooted in the
/// current handle scope.
pub unsafe fn lazyload_fetch(
    rdb_path: &Path,
    offset: u64,
    length: usize,
    compression: LazyLoadCompression,
    hook: Option<PersistHook<'_>>,
) -> ApiResult<Sexp> {
    use std::io::{Seek, SeekFrom};
    let cannot = |problem: std::io::Error| {
        error(format!(
            "cannot read lazy-load database '{}': {problem}",
            rdb_path.display()
        ))
    };
    let mut file = std::fs::File::open(rdb_path).map_err(cannot)?;
    file.seek(SeekFrom::Start(offset)).map_err(cannot)?;
    let mut entry = vec![0u8; length];
    file.read_exact(&mut entry).map_err(cannot)?;
    let stream = lazyload_decompress(&entry, compression)?;
    let mut source = SliceSource::new(&stream);
    Ok(unserialize(&mut source, hook)?.0)
}

/// The stream inside one lazy-load database entry.
pub fn lazyload_decompress(
    entry: &[u8],
    compression: LazyLoadCompression,
) -> ApiResult<Cow<'_, [u8]>> {
    if compression == LazyLoadCompression::None {
        return Ok(Cow::Borrowed(entry));
    }
    let kind = match compression {
        LazyLoadCompression::Typed => 2,
        _ => 1,
    };
    interp::serialize::decompress_record(entry, kind).map(Cow::Owned)
}

// The C API.

pub type OutCharFn = unsafe extern "C" fn(*mut R_outpstream_st, c_int);
pub type OutBytesFn = unsafe extern "C" fn(*mut R_outpstream_st, *mut c_void, c_int);
pub type InCharFn = unsafe extern "C" fn(*mut R_inpstream_st) -> c_int;
pub type InBytesFn = unsafe extern "C" fn(*mut R_inpstream_st, *mut c_void, c_int);
pub type HookFn = unsafe extern "C" fn(Sexp, Sexp) -> Sexp;

/// `struct R_outpstream_st`, laid out as GNU R lays it out.
#[repr(C)]
pub struct R_outpstream_st {
    pub data: *mut c_void,
    pub r#type: c_int,
    pub version: c_int,
    pub OutChar: Option<OutCharFn>,
    pub OutBytes: Option<OutBytesFn>,
    pub OutPersistHookFunc: Option<HookFn>,
    pub OutPersistHookData: Sexp,
}

pub const R_CODESET_MAX: usize = 63;

/// `struct R_inpstream_st`, laid out as GNU R lays it out.
#[repr(C)]
pub struct R_inpstream_st {
    pub data: *mut c_void,
    pub r#type: c_int,
    pub InChar: Option<InCharFn>,
    pub InBytes: Option<InBytesFn>,
    pub InPersistHookFunc: Option<HookFn>,
    pub InPersistHookData: Sexp,
    pub native_encoding: [c_char; R_CODESET_MAX + 1],
    pub nat2nat_obj: *mut c_void,
    pub nat2utf8_obj: *mut c_void,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<R_outpstream_st>() == 48 && size_of::<R_inpstream_st>() == 128);

unsafe extern "C" {
    fn ry_stream_out_bytes(
        function: OutBytesFn,
        stream: *mut R_outpstream_st,
        bytes: *mut c_void,
        length: c_int,
    ) -> c_int;
    fn ry_stream_in_bytes(
        function: InBytesFn,
        stream: *mut R_inpstream_st,
        bytes: *mut c_void,
        length: c_int,
    ) -> c_int;
    fn ry_stream_in_char(
        function: InCharFn,
        stream: *mut R_inpstream_st,
        result: *mut c_int,
    ) -> c_int;
    fn ry_stream_hook(
        function: HookFn,
        object: Sexp,
        data: Sexp,
        result: *mut *mut heap::object::Header,
    ) -> c_int;
}

/// Calls a native persistence hook and roots what it returns.
unsafe fn call_hook(function: HookFn, object: Sexp, data: Sexp) -> ApiResult<Sexp> {
    let mut result = std::ptr::null_mut();
    with_native_scope(|| ry_stream_hook(function, object, data, &mut result))?;
    let value = Sexp::from_raw(result).unwrap_or_else(|| runtime().nil());
    runtime().heap.root(value);
    Ok(value)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_InitOutPStream(
    stream: *mut R_outpstream_st,
    data: *mut c_void,
    r#type: c_int,
    version: c_int,
    out_char: Option<OutCharFn>,
    out_bytes: Option<OutBytesFn>,
    hook: Option<HookFn>,
    hook_data: Sexp,
) {
    let stream = &mut *stream;
    stream.data = data;
    stream.r#type = r#type;
    stream.version = if version == 0 {
        DEFAULT_VERSION
    } else {
        version
    };
    stream.OutChar = out_char;
    stream.OutBytes = out_bytes;
    stream.OutPersistHookFunc = hook;
    stream.OutPersistHookData = hook_data;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_InitInPStream(
    stream: *mut R_inpstream_st,
    data: *mut c_void,
    r#type: c_int,
    in_char: Option<InCharFn>,
    in_bytes: Option<InBytesFn>,
    hook: Option<HookFn>,
    hook_data: Sexp,
) {
    let stream = &mut *stream;
    stream.data = data;
    stream.r#type = r#type;
    stream.InChar = in_char;
    stream.InBytes = in_bytes;
    stream.InPersistHookFunc = hook;
    stream.InPersistHookData = hook_data;
    stream.native_encoding = [0; R_CODESET_MAX + 1];
    stream.nat2nat_obj = std::ptr::null_mut();
    stream.nat2utf8_obj = std::ptr::null_mut();
}

unsafe extern "C" fn file_out_char(stream: *mut R_outpstream_st, byte: c_int) {
    enter(|| {
        if libc::fputc(byte, (*stream).data.cast()) == libc::EOF {
            return Err(error("write failed"));
        }
        Ok(())
    })
}

unsafe extern "C" fn file_out_bytes(
    stream: *mut R_outpstream_st,
    bytes: *mut c_void,
    length: c_int,
) {
    enter(|| {
        let length = length.max(0) as usize;
        if libc::fwrite(bytes, 1, length, (*stream).data.cast()) != length {
            return Err(error("write failed"));
        }
        Ok(())
    })
}

unsafe extern "C" fn file_in_char(stream: *mut R_inpstream_st) -> c_int {
    enter(|| {
        let byte = libc::fgetc((*stream).data.cast());
        if byte == libc::EOF {
            return Err(error("read error"));
        }
        Ok(byte)
    })
}

unsafe extern "C" fn file_in_bytes(stream: *mut R_inpstream_st, bytes: *mut c_void, length: c_int) {
    enter(|| {
        let length = length.max(0) as usize;
        if libc::fread(bytes, 1, length, (*stream).data.cast()) != length {
            return Err(error("read error"));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_InitFileOutPStream(
    stream: *mut R_outpstream_st,
    file: *mut libc::FILE,
    r#type: c_int,
    version: c_int,
    hook: Option<HookFn>,
    hook_data: Sexp,
) {
    R_InitOutPStream(
        stream,
        file.cast(),
        r#type,
        version,
        Some(file_out_char),
        Some(file_out_bytes),
        hook,
        hook_data,
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_InitFileInPStream(
    stream: *mut R_inpstream_st,
    file: *mut libc::FILE,
    r#type: c_int,
    hook: Option<HookFn>,
    hook_data: Sexp,
) {
    R_InitInPStream(
        stream,
        file.cast(),
        r#type,
        Some(file_in_char),
        Some(file_in_bytes),
        hook,
        hook_data,
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_Serialize(object: Sexp, stream: *mut R_outpstream_st) {
    enter(|| {
        let format =
            Format::from_code((*stream).r#type).ok_or_else(|| error("unknown output format"))?;
        let out_bytes = (*stream)
            .OutBytes
            .ok_or_else(|| error("the output stream has no OutBytes function"))?;
        let mut sink = |chunk: &[u8]| -> ApiResult<()> {
            for piece in chunk.chunks(i32::MAX as usize) {
                with_native_scope(|| {
                    ry_stream_out_bytes(
                        out_bytes,
                        stream,
                        piece.as_ptr() as *mut c_void,
                        piece.len() as c_int,
                    )
                })?;
            }
            Ok(())
        };
        let hook_data = (*stream).OutPersistHookData;
        let mut native_hook;
        let hook: Option<PersistHook> = match (*stream).OutPersistHookFunc {
            Some(function) => {
                native_hook = move |object: Sexp| call_hook(function, object, hook_data);
                Some(&mut native_hook)
            }
            None => None,
        };
        // Native streams count calls: digest's spookyhash skips a fixed number of leading
        // bytes and fails when one call straddles that boundary.
        serialize(
            object,
            format,
            (*stream).version,
            &mut sink,
            hook,
            Chunking::PerField,
        )
    })
}

/// Reads through a native stream's `InBytes` and `InChar`.
struct NativeSource {
    stream: *mut R_inpstream_st,
}

impl Source for NativeSource {
    fn read(&mut self, buffer: &mut [u8]) -> ApiResult<()> {
        // SAFETY: the stream outlives the read, as `R_Unserialize`'s contract requires.
        unsafe {
            let function = (*self.stream)
                .InBytes
                .ok_or_else(|| error("the input stream has no InBytes function"))?;
            for piece in buffer.chunks_mut(i32::MAX as usize) {
                with_native_scope(|| {
                    ry_stream_in_bytes(
                        function,
                        self.stream,
                        piece.as_mut_ptr().cast(),
                        piece.len() as c_int,
                    )
                })?;
            }
            Ok(())
        }
    }

    fn read_byte(&mut self) -> ApiResult<Option<u8>> {
        // SAFETY: as in `read`.
        unsafe {
            let Some(function) = (*self.stream).InChar else {
                let mut byte = [0u8];
                self.read(&mut byte)?;
                return Ok(Some(byte[0]));
            };
            let mut result = 0;
            with_native_scope(|| ry_stream_in_char(function, self.stream, &mut result))?;
            Ok((result >= 0).then_some(result as u8))
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_Unserialize(stream: *mut R_inpstream_st) -> Sexp {
    enter(|| {
        let expected = match (*stream).r#type {
            0 => None,
            code => Some(Format::from_code(code).ok_or_else(|| error("unknown input format"))?),
        };
        let hook_data = (*stream).InPersistHookData;
        let mut native_hook;
        let hook: Option<PersistHook> = match (*stream).InPersistHookFunc {
            Some(function) => {
                native_hook = move |name: Sexp| call_hook(function, name, hook_data);
                Some(&mut native_hook)
            }
            None => None,
        };
        let mut source = NativeSource { stream };
        let (object, header) = read_stream(&mut source, hook, expected)?;
        let stream = &mut *stream;
        if stream.r#type == 0 {
            stream.r#type = header.format as c_int;
        }
        if let Some(name) = header.native_encoding {
            let bytes = name.as_bytes();
            let length = bytes.len().min(R_CODESET_MAX);
            for (slot, &byte) in stream.native_encoding.iter_mut().zip(&bytes[..length]) {
                *slot = byte as c_char;
            }
            stream.native_encoding[length] = 0;
        }
        Ok(object)
    })
}
