//! A reader for R's serialization format (versions 2 and 3, XDR and native binary) that builds
//! a plain tree instead of live R objects, and the compressed containers that installed
//! packages wrap their streams in.

use {
    flate2::read::{GzDecoder, ZlibDecoder},
    std::{
        io::{self, Read},
        sync::Arc,
    },
};

/// One R object as read from a serialized stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    /// The empty argument, as in `x[, 1]` or a formal argument without a default.
    Missing,
    Symbol(Arc<str>),
    Pairlist(Vec<Argument>),
    /// A call: the function, then its arguments.
    Language(Box<Value>, Vec<Argument>),
    Closure(Box<Closure>),
    /// A promise reads as its value when it was forced, else as its code.
    Promise(Box<Value>),
    Environment(Arc<Environment>),
    /// A primitive function, by name.
    Builtin(Arc<str>),
    Logical(Vector<Option<bool>>),
    Integer(Vector<Option<i32>>),
    /// Holds R's `NA_real_` as its NaN bit pattern; see [`is_na_real`].
    Double(Vector<f64>),
    Complex(Vector<(f64, f64)>),
    Character(Vector<Option<String>>),
    Raw(Vector<u8>),
    List(Vector<Value>),
    Expression(Vector<Value>),
    /// An object with no useful source form, such as an external pointer or an S4 object.
    Opaque(&'static str),
}

/// A tagged element of a pairlist or call.
#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub name: Option<Arc<str>>,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    pub formals: Vec<Argument>,
    pub body: Value,
    pub attributes: Attributes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vector<T> {
    pub elements: Vec<T>,
    pub attributes: Attributes,
}

pub type Attributes = Vec<(Arc<str>, Value)>;

/// An environment. Only the bindings of an ordinary environment are kept, never its
/// enclosure, so reading a function does not pull in the namespace around it.
#[derive(Clone, Debug, PartialEq)]
pub enum Environment {
    Global,
    Base,
    BaseNamespace,
    Empty,
    Namespace(Vec<String>),
    Package(Vec<String>),
    /// An environment a lazy-load database stores as a separate record, by its key.
    Persistent(Vec<String>),
    Frame(Vec<(Arc<str>, Value)>),
}

/// Reads one serialized object. `bytes` may carry a `save()` magic line before the stream.
pub fn unserialize(bytes: &[u8]) -> io::Result<Value> {
    let mut bytes = bytes;
    for magic in [
        b"RDX2\n", b"RDX3\n", b"RDA2\n", b"RDA3\n", b"RDB2\n", b"RDB3\n",
    ] {
        if let Some(rest) = bytes.strip_prefix(&magic[..]) {
            bytes = rest;
        }
    }
    let format = match bytes.get(..2) {
        Some(b"X\n") => Format::Xdr,
        Some(b"B\n") => Format::Binary,
        Some(b"A\n") | Some(b"\nA") => {
            return Err(invalid("ASCII serialization is not supported"));
        }
        _ => return Err(invalid("unknown serialization format")),
    };
    let mut reader = Reader {
        bytes,
        position: 2,
        format,
        references: Vec::new(),
    };
    let version = reader.integer()?;
    reader.integer()?;
    reader.integer()?;
    match version {
        2 => {}
        3 => {
            let encoding_length = reader.count()?;
            reader.take(encoding_length)?;
        }
        _ => {
            return Err(invalid(format!(
                "unsupported serialization version {version}"
            )));
        }
    }
    reader.item()
}

/// The serialized stream inside an `.rds` file, which is gzip, bzip2 or xz compressed, or
/// not compressed at all.
pub fn decompress_file(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    match bytes {
        [0x1f, 0x8b, ..] => {
            GzDecoder::new(bytes).read_to_end(&mut output)?;
        }
        [b'B', b'Z', b'h', ..] => {
            bzip2::read::MultiBzDecoder::new(bytes).read_to_end(&mut output)?;
        }
        [0xfd, b'7', b'z', b'X', b'Z', 0, ..] => {
            lzma_rust2::XzReader::new(bytes, true).read_to_end(&mut output)?;
        }
        _ => output.extend_from_slice(bytes),
    }
    Ok(output)
}

/// The stream inside one lazy-load database record, as GNU R's `R_compress1` to
/// `R_compress3` write it: the stream's length (big-endian), then for `compression` 1 a zlib
/// stream, and for 2 and 3 a type byte (`1` zlib, `2` bzip2, `Z` raw LZMA2, `0` stored) and
/// the data.
pub fn decompress_record(record: &[u8], compression: i32) -> io::Result<Vec<u8>> {
    if compression == 0 {
        return Ok(record.to_vec());
    }
    let (length, body) = record
        .split_first_chunk::<4>()
        .ok_or_else(|| invalid("lazy-load record is too short"))?;
    let expected = u32::from_be_bytes(*length) as usize;
    let (method, payload) = match (compression, body.split_first()) {
        (2 | 3, Some((&method, payload))) => (method, payload),
        _ => (b'1', body),
    };
    // A corrupt length must not allocate gigabytes up front.
    let mut output = Vec::with_capacity(expected.min(64 << 20));
    match method {
        b'0' => output.extend_from_slice(payload),
        b'2' => {
            bzip2::read::MultiBzDecoder::new(payload).read_to_end(&mut output)?;
        }
        b'Z' => {
            // No LZMA2 dictionary needs to be larger than the output it produces.
            let dictionary = expected.clamp(4096, u32::MAX as usize) as u32;
            lzma_rust2::Lzma2Reader::new(payload, dictionary, None).read_to_end(&mut output)?;
        }
        _ => {
            ZlibDecoder::new(payload).read_to_end(&mut output)?;
        }
    }
    if output.len() != expected {
        return Err(invalid("lazy-load record length does not match its header"));
    }
    Ok(output)
}

/// Whether `value` is R's `NA_real_` rather than an ordinary NaN: R marks it with the low
/// word 1954.
pub fn is_na_real(value: f64) -> bool {
    value.is_nan() && (value.to_bits() & 0xffff_ffff) == 1954
}

impl Value {
    pub fn attributes(&self) -> &[(Arc<str>, Value)] {
        match self {
            Value::Logical(vector) => &vector.attributes,
            Value::Integer(vector) => &vector.attributes,
            Value::Double(vector) => &vector.attributes,
            Value::Complex(vector) => &vector.attributes,
            Value::Character(vector) => &vector.attributes,
            Value::Raw(vector) => &vector.attributes,
            Value::List(vector) | Value::Expression(vector) => &vector.attributes,
            Value::Closure(closure) => &closure.attributes,
            _ => &[],
        }
    }

    pub fn attribute(&self, name: &str) -> Option<&Value> {
        self.attributes()
            .iter()
            .find(|(attribute, _)| &**attribute == name)
            .map(|(_, value)| value)
    }

    /// The first string of a character vector.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Character(vector) => vector.elements.first()?.as_deref(),
            _ => None,
        }
    }

    /// The `names` attribute as a list of optional strings, one per element.
    pub fn names(&self) -> Option<&[Option<String>]> {
        match self.attribute("names")? {
            Value::Character(names) => Some(&names.elements),
            _ => None,
        }
    }

    fn set_attributes(&mut self, attributes: Attributes) {
        match self {
            Value::Logical(vector) => vector.attributes = attributes,
            Value::Integer(vector) => vector.attributes = attributes,
            Value::Double(vector) => vector.attributes = attributes,
            Value::Complex(vector) => vector.attributes = attributes,
            Value::Character(vector) => vector.attributes = attributes,
            Value::Raw(vector) => vector.attributes = attributes,
            Value::List(vector) | Value::Expression(vector) => vector.attributes = attributes,
            Value::Closure(closure) => closure.attributes = attributes,
            _ => {}
        }
    }
}

impl<T> Vector<T> {
    pub fn new(elements: Vec<T>) -> Vector<T> {
        Vector {
            elements,
            attributes: Vec::new(),
        }
    }
}

// Special item codes, which stand in for a type code in an item's flags.
const REFSXP: u32 = 255;
const NILVALUE_SXP: u32 = 254;
const GLOBALENV_SXP: u32 = 253;
const UNBOUNDVALUE_SXP: u32 = 252;
const MISSINGARG_SXP: u32 = 251;
const BASENAMESPACE_SXP: u32 = 250;
const NAMESPACESXP: u32 = 249;
const PACKAGESXP: u32 = 248;
const PERSISTSXP: u32 = 247;
const CLASSREFSXP: u32 = 246;
const GENERICREFSXP: u32 = 245;
const BCREPDEF: u32 = 244;
const BCREPREF: u32 = 243;
const EMPTYENV_SXP: u32 = 242;
const BASEENV_SXP: u32 = 241;
const ATTRLANGSXP: u32 = 240;
const ATTRLISTSXP: u32 = 239;
const ALTREP_SXP: u32 = 238;

const SYMSXP: u32 = 1;
const LISTSXP: u32 = 2;
const CLOSXP: u32 = 3;
const ENVSXP: u32 = 4;
const PROMSXP: u32 = 5;
const LANGSXP: u32 = 6;
const SPECIALSXP: u32 = 7;
const BUILTINSXP: u32 = 8;
const CHARSXP: u32 = 9;
const LGLSXP: u32 = 10;
const INTSXP: u32 = 13;
const REALSXP: u32 = 14;
const CPLXSXP: u32 = 15;
const STRSXP: u32 = 16;
const DOTSXP: u32 = 17;
const VECSXP: u32 = 19;
const EXPRSXP: u32 = 20;
const BCODESXP: u32 = 21;
const EXTPTRSXP: u32 = 22;
const WEAKREFSXP: u32 = 23;
const RAWSXP: u32 = 24;
const S4SXP: u32 = 25;

const HAS_ATTR: u32 = 1 << 9;
const HAS_TAG: u32 = 1 << 10;
/// The `LATIN1` bit of a string's levels, which sit above bit 12 of its flags.
const LATIN1_MASK: u32 = 1 << 2;

/// Stack kept free before recursing into a nested item, and the size of each new segment, so
/// that deeply nested input grows the stack instead of overflowing it.
const STACK_RED_ZONE: usize = 64 * 1024;
const STACK_SEGMENT: usize = 1024 * 1024;

const UNBOUND: Value = Value::Opaque("unbound value");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Xdr,
    Binary,
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    format: Format,
    references: Vec<Value>,
}

/// A byte-code language cell, which a later `BCREPREF` may share, or any other constant.
#[derive(Clone)]
enum Piece {
    Cells {
        language: bool,
        arguments: Vec<Argument>,
    },
    Value(Value),
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> io::Result<&[u8]> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| invalid("unexpected end of serialized data"))?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn integer(&mut self) -> io::Result<i32> {
        let format = self.format;
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| invalid("unexpected end of serialized data"))?;
        Ok(match format {
            Format::Xdr => i32::from_be_bytes(bytes),
            Format::Binary => i32::from_le_bytes(bytes),
        })
    }

    fn real(&mut self) -> io::Result<f64> {
        let format = self.format;
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| invalid("unexpected end of serialized data"))?;
        Ok(match format {
            Format::Xdr => f64::from_be_bytes(bytes),
            Format::Binary => f64::from_le_bytes(bytes),
        })
    }

    fn count(&mut self) -> io::Result<usize> {
        usize::try_from(self.integer()?).map_err(|_| invalid("negative length"))
    }

    /// A vector length, checked against the bytes left so that a corrupt length fails instead
    /// of allocating. Every element takes at least `element_size` bytes.
    fn length(&mut self, element_size: usize) -> io::Result<usize> {
        let length = self.integer()?;
        let length = if length == -1 {
            let high = self.integer()? as u32 as u64;
            let low = self.integer()? as u32 as u64;
            usize::try_from((high << 32) | low).map_err(|_| invalid("vector is too long"))?
        } else {
            usize::try_from(length).map_err(|_| invalid("negative length"))?
        };
        if length.saturating_mul(element_size) > self.remaining() {
            return Err(invalid("vector length exceeds the serialized data"));
        }
        Ok(length)
    }

    fn item(&mut self) -> io::Result<Value> {
        let flags = self.integer()? as u32;
        self.item_with(flags)
    }

    fn item_with(&mut self, flags: u32) -> io::Result<Value> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, || self.read_item(flags))
    }

    fn read_item(&mut self, flags: u32) -> io::Result<Value> {
        let kind = flags & 0xff;
        let has_attributes = flags & HAS_ATTR != 0;
        let has_tag = flags & HAS_TAG != 0;
        let environment = |environment| Ok(Value::Environment(Arc::new(environment)));
        let mut value = match kind {
            NILVALUE_SXP => return Ok(Value::Null),
            EMPTYENV_SXP => return environment(Environment::Empty),
            BASEENV_SXP => return environment(Environment::Base),
            GLOBALENV_SXP => return environment(Environment::Global),
            BASENAMESPACE_SXP => return environment(Environment::BaseNamespace),
            UNBOUNDVALUE_SXP => return Ok(UNBOUND),
            MISSINGARG_SXP => return Ok(Value::Missing),
            REFSXP => {
                let mut index = (flags >> 8) as usize;
                if index == 0 {
                    index = self.count()?;
                }
                return self
                    .references
                    .get(index.wrapping_sub(1))
                    .cloned()
                    .ok_or_else(|| invalid("invalid reference in serialized data"));
            }
            PERSISTSXP | NAMESPACESXP | PACKAGESXP => {
                let names = self.string_vector()?;
                let value = Value::Environment(Arc::new(match kind {
                    PERSISTSXP => Environment::Persistent(names),
                    NAMESPACESXP => Environment::Namespace(names),
                    _ => Environment::Package(names),
                }));
                self.references.push(value.clone());
                return Ok(value);
            }
            SYMSXP => {
                let name = match self.item()? {
                    Value::Character(vector) => vector.elements.into_iter().next().flatten(),
                    _ => None,
                };
                let value = Value::Symbol(Arc::from(name.unwrap_or_default()));
                self.references.push(value.clone());
                return Ok(value);
            }
            ENVSXP => {
                self.integer()?;
                let index = self.references.len();
                self.references
                    .push(Value::Environment(Arc::new(Environment::Frame(Vec::new()))));
                // The enclosure is read only to move past it.
                self.item()?;
                let frame = self.item()?;
                let table = self.item()?;
                self.item()?;
                let mut bindings = Vec::new();
                collect_bindings(frame, &mut bindings);
                if let Value::List(buckets) = table {
                    for bucket in buckets.elements {
                        collect_bindings(bucket, &mut bindings);
                    }
                }
                let value = Value::Environment(Arc::new(Environment::Frame(bindings)));
                if let Some(slot) = self.references.get_mut(index) {
                    *slot = value.clone();
                }
                return Ok(value);
            }
            LISTSXP | LANGSXP | DOTSXP => return self.cells(flags),
            CLOSXP => {
                let attributes = if has_attributes {
                    attribute_list(self.item()?)
                } else {
                    Vec::new()
                };
                if has_tag {
                    self.item()?;
                }
                let formals = arguments_of(self.item()?);
                let body = self.item()?;
                return Ok(Value::Closure(Box::new(Closure {
                    formals,
                    body,
                    attributes,
                })));
            }
            PROMSXP => {
                if has_attributes {
                    self.item()?;
                }
                if has_tag {
                    self.item()?;
                }
                let value = self.item()?;
                let code = self.item()?;
                return Ok(Value::Promise(Box::new(if value == UNBOUND {
                    code
                } else {
                    value
                })));
            }
            SPECIALSXP | BUILTINSXP => {
                let length = self.count()?;
                let name = String::from_utf8_lossy(self.take(length)?).into_owned();
                return Ok(Value::Builtin(Arc::from(name)));
            }
            CHARSXP => {
                let string = self.char_contents(flags)?;
                return Ok(Value::Character(Vector::new(vec![string])));
            }
            BCODESXP => {
                let count = self.count()?;
                if count > self.remaining() {
                    return Err(invalid("byte-code table exceeds the serialized data"));
                }
                let mut reps = vec![None; count];
                return self.bytecode(&mut reps);
            }
            ALTREP_SXP => {
                let info = self.item()?;
                let state = self.item()?;
                let attributes = attribute_list(self.item()?);
                let mut value = altrep(info, state)?;
                value.set_attributes(attributes);
                return Ok(value);
            }
            EXTPTRSXP => {
                let value = Value::Opaque("external pointer");
                self.references.push(value.clone());
                self.item()?;
                self.item()?;
                value
            }
            WEAKREFSXP => {
                let value = Value::Opaque("weak reference");
                self.references.push(value.clone());
                value
            }
            CLASSREFSXP | GENERICREFSXP => {
                return Err(invalid("class references cannot be read"));
            }
            LGLSXP | INTSXP => {
                let length = self.length(4)?;
                let mut elements = Vec::with_capacity(length);
                for _ in 0..length {
                    let integer = self.integer()?;
                    elements.push((integer != i32::MIN).then_some(integer));
                }
                if kind == LGLSXP {
                    Value::Logical(Vector::new(
                        elements
                            .into_iter()
                            .map(|element| element.map(|integer| integer != 0))
                            .collect(),
                    ))
                } else {
                    Value::Integer(Vector::new(elements))
                }
            }
            REALSXP => {
                let length = self.length(8)?;
                let mut elements = Vec::with_capacity(length);
                for _ in 0..length {
                    elements.push(self.real()?);
                }
                Value::Double(Vector::new(elements))
            }
            CPLXSXP => {
                let length = self.length(16)?;
                let mut elements = Vec::with_capacity(length);
                for _ in 0..length {
                    elements.push((self.real()?, self.real()?));
                }
                Value::Complex(Vector::new(elements))
            }
            STRSXP => {
                let length = self.length(8)?;
                let mut elements = Vec::with_capacity(length);
                for _ in 0..length {
                    let flags = self.integer()? as u32;
                    if flags & 0xff != CHARSXP {
                        return Err(invalid("a string vector holds a non-string"));
                    }
                    elements.push(self.char_contents(flags)?);
                }
                Value::Character(Vector::new(elements))
            }
            VECSXP | EXPRSXP => {
                let length = self.length(4)?;
                let mut elements = Vec::with_capacity(length);
                for _ in 0..length {
                    elements.push(self.item()?);
                }
                if kind == VECSXP {
                    Value::List(Vector::new(elements))
                } else {
                    Value::Expression(Vector::new(elements))
                }
            }
            RAWSXP => {
                let length = self.length(1)?;
                Value::Raw(Vector::new(self.take(length)?.to_vec()))
            }
            S4SXP => Value::Opaque("S4 object"),
            _ => return Err(invalid(format!("unknown serialized type {kind}"))),
        };
        if has_attributes {
            let attributes = attribute_list(self.item()?);
            value.set_attributes(attributes);
        }
        Ok(value)
    }

    /// A pairlist or call, whose cells R writes one after another: each cell's attributes,
    /// tag and value, then the next cell's flags in place of its tail.
    fn cells(&mut self, mut flags: u32) -> io::Result<Value> {
        let language = flags & 0xff == LANGSXP;
        let mut arguments = Vec::new();
        loop {
            if flags & HAS_ATTR != 0 {
                self.item()?;
            }
            let name = if flags & HAS_TAG != 0 {
                symbol_of(self.item()?)
            } else {
                None
            };
            let value = self.item()?;
            arguments.push(Argument { name, value });
            flags = self.integer()? as u32;
            match flags & 0xff {
                LISTSXP | LANGSXP | DOTSXP => {}
                NILVALUE_SXP => break,
                // A dotted tail, which no source form has.
                _ => {
                    self.item_with(flags)?;
                    break;
                }
            }
        }
        Ok(chain(language, arguments))
    }

    /// A string's contents after its flags: the length, `-1` for `NA`, then the bytes.
    fn char_contents(&mut self, flags: u32) -> io::Result<Option<String>> {
        let length = self.integer()?;
        let Ok(length) = usize::try_from(length) else {
            return Ok(None);
        };
        let bytes = self.take(length)?;
        let levels = flags >> 12;
        Ok(Some(if levels & LATIN1_MASK != 0 {
            bytes.iter().map(|&byte| char::from(byte)).collect()
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        }))
    }

    /// The names a persistent, namespace or package reference carries.
    fn string_vector(&mut self) -> io::Result<Vec<String>> {
        if self.integer()? != 0 {
            return Err(invalid("named persistent strings are not supported"));
        }
        // A count and then bare strings, not a string vector item.
        let length = self.length(8)?;
        let mut names = Vec::with_capacity(length);
        for _ in 0..length {
            let flags = self.integer()? as u32;
            if flags & 0xff != CHARSXP {
                return Err(invalid("a reference name is not a string"));
            }
            names.extend(self.char_contents(flags)?);
        }
        Ok(names)
    }

    /// A GNU R byte-code object, which reads as its source expression: the first constant.
    fn bytecode(&mut self, reps: &mut Vec<Option<Piece>>) -> io::Result<Value> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, || {
            // The instructions, which are skipped without building them because the source
            // expression is all a reader needs.
            let flags = self.integer()? as u32;
            if flags == INTSXP {
                let length = self.length(4)?;
                self.take(length * 4)?;
            } else {
                self.item_with(flags)?;
            }
            let count = self.count()?;
            let mut expression = Value::Null;
            for index in 0..count {
                let kind = self.integer()? as u32;
                let value = match kind {
                    BCODESXP => self.bytecode(reps)?,
                    LANGSXP | LISTSXP | BCREPDEF | BCREPREF | ATTRLANGSXP | ATTRLISTSXP => {
                        piece_value(self.bytecode_language(kind, reps)?)
                    }
                    _ => self.item()?,
                };
                if index == 0 {
                    expression = value;
                }
            }
            Ok(expression)
        })
    }

    /// A language cell inside byte-code constants, where shared cells are written once
    /// (`BCREPDEF`) and referred to by position (`BCREPREF`). Any other item is preceded by a
    /// type marker that carries no information.
    fn bytecode_language(&mut self, kind: u32, reps: &mut Vec<Option<Piece>>) -> io::Result<Piece> {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, || {
            let (position, kind) = match kind {
                BCREPREF => {
                    let index = self.count()?;
                    // A reference to a cell still being read would be a cycle, which no
                    // source expression has.
                    return Ok(reps
                        .get(index)
                        .cloned()
                        .flatten()
                        .unwrap_or(Piece::Value(Value::Null)));
                }
                BCREPDEF => (Some(self.count()?), self.integer()? as u32),
                LANGSXP | LISTSXP | ATTRLANGSXP | ATTRLISTSXP => (None, kind),
                _ => return Ok(Piece::Value(self.item()?)),
            };
            if matches!(kind, ATTRLANGSXP | ATTRLISTSXP) {
                self.item()?;
            }
            let name = symbol_of(self.item()?);
            let car_kind = self.integer()? as u32;
            let value = piece_value(self.bytecode_language(car_kind, reps)?);
            let cdr_kind = self.integer()? as u32;
            let tail = self.bytecode_language(cdr_kind, reps)?;
            let mut arguments = vec![Argument { name, value }];
            arguments.extend(piece_arguments(tail));
            let piece = Piece::Cells {
                language: matches!(kind, LANGSXP | ATTRLANGSXP),
                arguments,
            };
            if let Some(slot) = position.and_then(|position| reps.get_mut(position)) {
                *slot = Some(piece.clone());
            }
            Ok(piece)
        })
    }
}

fn piece_value(piece: Piece) -> Value {
    match piece {
        Piece::Cells {
            language,
            arguments,
        } => chain(language, arguments),
        Piece::Value(value) => value,
    }
}

/// The cells of a list tail. GNU R walks argument lists by their tails alone, so a call cell
/// in a tail position is one more argument, not a nested call.
fn piece_arguments(piece: Piece) -> Vec<Argument> {
    match piece {
        Piece::Cells { arguments, .. } => arguments,
        Piece::Value(value) => arguments_of(value),
    }
}

fn chain(language: bool, mut arguments: Vec<Argument>) -> Value {
    if language && !arguments.is_empty() {
        let function = arguments.remove(0).value;
        Value::Language(Box::new(function), arguments)
    } else {
        Value::Pairlist(arguments)
    }
}

fn arguments_of(value: Value) -> Vec<Argument> {
    match value {
        Value::Pairlist(arguments) => arguments,
        Value::Language(function, arguments) => {
            let mut all = vec![Argument {
                name: None,
                value: *function,
            }];
            all.extend(arguments);
            all
        }
        _ => Vec::new(),
    }
}

fn symbol_of(value: Value) -> Option<Arc<str>> {
    match value {
        Value::Symbol(name) => Some(name),
        _ => None,
    }
}

fn attribute_list(value: Value) -> Attributes {
    arguments_of(value)
        .into_iter()
        .filter_map(|argument| Some((argument.name?, argument.value)))
        .collect()
}

fn collect_bindings(frame: Value, bindings: &mut Vec<(Arc<str>, Value)>) {
    for argument in arguments_of(frame) {
        if let Some(name) = argument.name {
            bindings.push((name, argument.value));
        }
    }
}

/// Materializes the ALTREP classes GNU R serializes in package code: compact sequences,
/// deferred strings and wrappers.
fn altrep(info: Value, state: Value) -> io::Result<Value> {
    const LONGEST_SEQUENCE: f64 = (1u64 << 28) as f64;
    let class = match arguments_of(info).into_iter().next() {
        Some(Argument {
            value: Value::Symbol(name),
            ..
        }) => name,
        _ => return Ok(Value::Opaque("ALTREP object")),
    };
    let numbers = |state: &Value| -> Option<Vec<f64>> {
        match state {
            Value::Double(vector) => Some(vector.elements.clone()),
            Value::Integer(vector) => Some(
                vector
                    .elements
                    .iter()
                    .map(|element| element.map_or(f64::NAN, f64::from))
                    .collect(),
            ),
            _ => None,
        }
    };
    match &*class {
        "compact_intseq" | "compact_realseq" => {
            let Some([count, start, step]) =
                numbers(&state).and_then(|numbers| <[f64; 3]>::try_from(numbers).ok())
            else {
                return Err(invalid("malformed compact sequence"));
            };
            if !(0.0..=LONGEST_SEQUENCE).contains(&count) {
                return Err(invalid("compact sequence is too long"));
            }
            let values = (0..count as usize).map(|index| start + index as f64 * step);
            Ok(if &*class == "compact_intseq" {
                Value::Integer(Vector::new(
                    values.map(|value| Some(value as i32)).collect(),
                ))
            } else {
                Value::Double(Vector::new(values.collect()))
            })
        }
        "deferred_string" => {
            let argument = match state {
                Value::Pairlist(arguments) => arguments
                    .into_iter()
                    .next()
                    .map_or(Value::Null, |argument| argument.value),
                other => other,
            };
            let elements = match argument {
                Value::Integer(vector) => vector
                    .elements
                    .iter()
                    .map(|element| element.map(|integer| integer.to_string()))
                    .collect(),
                Value::Double(vector) => vector
                    .elements
                    .iter()
                    .map(|&double| {
                        (!is_na_real(double)).then(|| crate::deparse::format_double(double))
                    })
                    .collect(),
                _ => return Err(invalid("malformed deferred string")),
            };
            Ok(Value::Character(Vector::new(elements)))
        }
        name if name.starts_with("wrap_") => Ok(match state {
            Value::List(vector) => vector.elements.into_iter().next().unwrap_or(Value::Null),
            other => other,
        }),
        _ => Ok(Value::Opaque("ALTREP object")),
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an XDR stream from 32-bit words and raw byte runs.
    enum Part<'a> {
        Word(i32),
        Bytes(&'a [u8]),
    }

    fn stream(parts: &[Part<'_>]) -> Vec<u8> {
        let mut bytes = b"X\n".to_vec();
        for word in [2, 0x040300, 0x020300] {
            bytes.extend_from_slice(&i32::to_be_bytes(word));
        }
        for part in parts {
            match part {
                Part::Word(word) => bytes.extend_from_slice(&word.to_be_bytes()),
                Part::Bytes(run) => bytes.extend_from_slice(run),
            }
        }
        bytes
    }

    use Part::{Bytes, Word};

    const CHAR: i32 = 9 | (64 << 12);

    #[test]
    fn reads_a_named_integer_vector() {
        let bytes = stream(&[
            Word(13 | HAS_ATTR as i32),
            Word(2),
            Word(1),
            Word(i32::MIN),
            Word(LISTSXP as i32 | HAS_TAG as i32),
            Word(SYMSXP as i32),
            Word(CHAR),
            Word(5),
            Bytes(b"names"),
            Word(STRSXP as i32),
            Word(2),
            Word(CHAR),
            Word(1),
            Bytes(b"a"),
            Word(CHAR),
            Word(-1),
            Word(NILVALUE_SXP as i32),
        ]);
        let value = unserialize(&bytes).expect("stream is well formed");
        let Value::Integer(vector) = &value else {
            panic!("expected an integer vector, got {value:?}");
        };
        assert_eq!(vector.elements, vec![Some(1), None]);
        assert_eq!(
            value.names(),
            Some(&[Some("a".to_string()), None][..]),
            "names attribute"
        );
    }

    #[test]
    fn reads_a_call_and_shares_symbols_by_reference() {
        // f(x, x): the second `x` is a reference to the first.
        let bytes = stream(&[
            Word(LANGSXP as i32),
            Word(SYMSXP as i32),
            Word(CHAR),
            Word(1),
            Bytes(b"f"),
            Word(LISTSXP as i32),
            Word(SYMSXP as i32),
            Word(CHAR),
            Word(1),
            Bytes(b"x"),
            Word(LISTSXP as i32),
            Word(REFSXP as i32 | (2 << 8)),
            Word(NILVALUE_SXP as i32),
        ]);
        let value = unserialize(&bytes).expect("stream is well formed");
        let x = || Argument {
            name: None,
            value: Value::Symbol(Arc::from("x")),
        };
        assert_eq!(
            value,
            Value::Language(Box::new(Value::Symbol(Arc::from("f"))), vec![x(), x()])
        );
    }

    /// Mutates small valid streams with a seeded generator: reading, deparsing and rendering
    /// whatever comes out must fail cleanly or succeed, never panic.
    #[test]
    fn mutated_streams_never_panic() {
        const SEED: u64 = 0x5eed_f7db;
        let samples = [
            stream(&[
                Word(LANGSXP as i32),
                Word(SYMSXP as i32),
                Word(CHAR),
                Word(1),
                Bytes(b"f"),
                Word(LISTSXP as i32 | HAS_TAG as i32),
                Word(SYMSXP as i32),
                Word(CHAR),
                Word(1),
                Bytes(b"x"),
                Word(VECSXP as i32),
                Word(2),
                Word(REALSXP as i32),
                Word(1),
                Bytes(&1.5f64.to_be_bytes()),
                Word(REFSXP as i32 | (2 << 8)),
                Word(NILVALUE_SXP as i32),
                Word(NILVALUE_SXP as i32),
            ]),
            stream(&[
                Word(BCODESXP as i32),
                Word(1),
                Word(INTSXP as i32),
                Word(1),
                Word(12),
                Word(2),
                Word(BCREPDEF as i32),
                Word(0),
                Word(LANGSXP as i32),
                Word(NILVALUE_SXP as i32),
                Word(0),
                Word(SYMSXP as i32),
                Word(CHAR),
                Word(1),
                Bytes(b"g"),
                Word(0),
                Word(NILVALUE_SXP as i32),
                Word(BCREPREF as i32),
                Word(0),
            ]),
        ];
        let bytecode = unserialize(&samples[1]).expect("byte code reads");
        assert_eq!(crate::deparse::deparse(&bytecode), "g()");
        let mut state = SEED;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for sample in &samples {
            assert!(
                unserialize(sample).is_ok(),
                "sample streams are well formed"
            );
            for round in 0..2000 {
                let mut bytes = sample.clone();
                for _ in 0..1 + next() % 4 {
                    let position = 14 + next() % (bytes.len() - 14);
                    bytes[position] = match next() % 3 {
                        0 => 0xff,
                        1 => 0,
                        _ => next() as u8,
                    };
                }
                if round % 5 == 0 {
                    bytes.truncate(14 + next() % (bytes.len() - 14));
                }
                if let Ok(value) = unserialize(&bytes) {
                    crate::deparse::deparse(&value);
                    crate::rd::render(&value);
                }
            }
        }
    }

    #[test]
    fn rejects_truncated_and_oversized_input() {
        let truncated = stream(&[Word(INTSXP as i32), Word(3), Word(1)]);
        assert!(unserialize(&truncated).is_err());
        let oversized = stream(&[Word(REALSXP as i32), Word(i32::MAX)]);
        assert!(unserialize(&oversized).is_err());
        assert!(unserialize(b"garbage").is_err());
        assert!(decompress_record(&[0, 0], 1).is_err());
    }
}
