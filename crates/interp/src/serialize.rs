//! R's serialization format (versions 2 and 3, XDR, binary and ASCII), and the lazy-load
//! databases installed packages keep their R objects in. Byte-compiled closures from GNU R are
//! read back as their source expressions, which ry compiles itself.

use {
    crate::{
        builtins::{BuiltinDef, Call, builtin, internal},
        code_image,
        connections::Rconnection,
        eval,
        unwind::{R, error},
        values::{self, alloc, length, nil, symbols},
    },
    flate2::{Decompress, FlushDecompress, Status, read::ZlibDecoder},
    heap::{
        object::{Sexp, SexpType, gp},
        strings::Encoding,
    },
    rustc_hash::FxHashMap,
    std::io::Read,
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("lazyLoadDBfetch", 4, do_lazy_load_fetch),
    internal("unserializeFromConn", 2, do_unserialize_from_conn),
    internal("serializeInfoFromConn", 1, do_serialize_info_from_conn),
    internal("unserialize", 2, do_unserialize),
    internal("lazyLoadDBflush", 1, |call| unsafe {
        forget_database(&values::require_string(call.arg(0), "file")?);
        Ok(nil())
    })
    .invisible(),
    internal("loadFromConn2", 3, do_load_from_conn).invisible(),
    internal("getVarsFromFrame", 3, do_get_vars_from_frame),
    internal("memCompress", 2, do_mem_compress),
    internal("memDecompress", 2, do_mem_decompress),
];

// Special item codes.
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
const EMPTYENV_SXP: u32 = 242;
const BASEENV_SXP: u32 = 241;
const ATTRLANGSXP: u32 = 240;
const ATTRLISTSXP: u32 = 239;
const ALTREP_SXP: u32 = 238;
const BCREPREF: u32 = 243;

const IS_OBJECT: u32 = 1 << 8;
const HAS_ATTR: u32 = 1 << 9;
const HAS_TAG: u32 = 1 << 10;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Xdr,
    Binary,
    Ascii,
}

struct Reader<'a> {
    input: Input<'a>,
    format: Format,
    references: Vec<Sexp>,
    hook: Sexp,
    /// The closures read so far, in the order they were made, when the caller asked for them
    /// (see [`unserialize_with_closures`]).
    closures: Option<Vec<Sexp>>,
}

/// Where a stream's bytes come from. A connection is read only as far as the object goes, so
/// the next object on it, or a peer's next message on a socket, stays unread.
enum Input<'a> {
    Slice { bytes: &'a [u8], position: usize },
    Connection { con: Rconnection, buffer: Vec<u8> },
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> R<&[u8]> {
        match &mut self.input {
            Input::Slice { bytes, position } => {
                let end = position
                    .checked_add(count)
                    .filter(|&end| end <= bytes.len())
                    .ok_or_else(|| error("ReadItem: unexpected end of serialized data"))?;
                let slice = &bytes[*position..end];
                *position = end;
                Ok(slice)
            }
            Input::Connection { con, buffer } => {
                // The buffer grows a block at a time, so a corrupt length runs into the end of
                // the input instead of allocating it all at once.
                const BLOCK: usize = 1 << 20;
                buffer.clear();
                while buffer.len() < count {
                    let start = buffer.len();
                    let step = (count - start).min(BLOCK);
                    buffer.resize(start + step, 0);
                    // SAFETY: the connection stays open while the reader runs.
                    if unsafe { crate::connections::read_all(*con, &mut buffer[start..])? } != step
                    {
                        return Err(error("error reading from connection"));
                    }
                }
                Ok(buffer)
            }
        }
    }

    fn next_byte(&mut self) -> R<Option<u8>> {
        match &mut self.input {
            Input::Slice { bytes, position } => {
                let byte = bytes.get(*position).copied();
                *position += byte.is_some() as usize;
                Ok(byte)
            }
            Input::Connection { con, .. } => {
                let mut byte = [0u8];
                // SAFETY: the connection stays open while the reader runs.
                let count = unsafe { crate::connections::read_all(*con, &mut byte)? };
                Ok((count == 1).then_some(byte[0]))
            }
        }
    }

    /// The next whitespace-separated word of an ASCII stream, consuming the whitespace that
    /// ends it.
    fn ascii_word(&mut self) -> R<String> {
        let mut word = Vec::new();
        while let Some(byte) = self.next_byte()? {
            if !byte.is_ascii_whitespace() {
                word.push(byte);
                break;
            }
        }
        if !word.is_empty() {
            while let Some(byte) = self.next_byte()? {
                if byte.is_ascii_whitespace() {
                    break;
                }
                word.push(byte);
            }
        }
        Ok(String::from_utf8_lossy(&word).into_owned())
    }

    fn integer(&mut self) -> R<i32> {
        match self.format {
            Format::Xdr => {
                let bytes = self.take(4)?;
                Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            Format::Binary => {
                let bytes = self.take(4)?;
                Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            Format::Ascii => {
                let word = self.ascii_word()?;
                if word == "NA" {
                    return Ok(i32::MIN);
                }
                word.parse().map_err(|_| error("read error"))
            }
        }
    }

    fn real(&mut self) -> R<f64> {
        match self.format {
            Format::Xdr => {
                let bytes = self.take(8)?;
                let mut array = [0u8; 8];
                array.copy_from_slice(bytes);
                Ok(f64::from_be_bytes(array))
            }
            Format::Binary => {
                let bytes = self.take(8)?;
                let mut array = [0u8; 8];
                array.copy_from_slice(bytes);
                Ok(f64::from_le_bytes(array))
            }
            Format::Ascii => {
                let word = self.ascii_word()?;
                Ok(match word.as_str() {
                    "NA" => values::na_real(),
                    "NaN" => f64::NAN,
                    "Inf" => f64::INFINITY,
                    "-Inf" => f64::NEG_INFINITY,
                    _ => word.parse().map_err(|_| error("read error"))?,
                })
            }
        }
    }

    fn string(&mut self, length: usize) -> R<Vec<u8>> {
        if self.format == Format::Ascii {
            return self.ascii_string(length);
        }
        Ok(self.take(length)?.to_vec())
    }

    /// An escaped ASCII string of `length` bytes. Reading stops at the last byte, so the
    /// newline after it stays unread, as it does in GNU R: an object that ends in a string
    /// leaves that newline before whatever follows it on a connection.
    fn ascii_string(&mut self, length: usize) -> R<Vec<u8>> {
        let truncated = || error("ReadItem: unexpected end of serialized data");
        let mut bytes = Vec::with_capacity(length.min(4096));
        // An empty string is written as nothing, so skipping whitespace would consume the
        // first byte of the next item.
        if length == 0 {
            return Ok(bytes);
        }
        let mut next = loop {
            match self.next_byte()? {
                Some(byte) if byte.is_ascii_whitespace() => {}
                Some(byte) => break Some(byte),
                None => return Err(truncated()),
            }
        };
        while bytes.len() < length {
            let current = match next.take() {
                Some(byte) => byte,
                None => self.next_byte()?.ok_or_else(truncated)?,
            };
            if current != b'\\' {
                bytes.push(current);
                continue;
            }
            let escaped = self.next_byte()?.ok_or_else(truncated)?;
            bytes.push(match escaped {
                b'n' => b'\n',
                b't' => b'\t',
                b'v' => 0x0b,
                b'b' => 0x08,
                b'r' => b'\r',
                b'f' => 0x0c,
                b'a' => 0x07,
                // Up to three octal digits; a byte that ends a shorter escape is the next one.
                b'0'..=b'7' => {
                    let mut value = u32::from(escaped - b'0');
                    for _ in 0..2 {
                        match self.next_byte()?.ok_or_else(truncated)? {
                            digit @ b'0'..=b'7' => value = value * 8 + u32::from(digit - b'0'),
                            other => {
                                next = Some(other);
                                break;
                            }
                        }
                    }
                    value as u8
                }
                other => other,
            });
        }
        Ok(bytes)
    }

    /// A byte count, which a corrupt stream may give as negative.
    fn count(&mut self) -> R<usize> {
        usize::try_from(self.integer()?)
            .map_err(|_| error("negative length vectors are not allowed"))
    }

    fn length(&mut self) -> R<usize> {
        let length = self.integer()?;
        if length == -1 {
            let high = self.integer()? as u32 as u64;
            let low = self.integer()? as u32 as u64;
            return Ok(((high << 32) + low) as usize);
        }
        if length < 0 {
            return Err(error("negative serialized length for vector"));
        }
        Ok(length as usize)
    }

    unsafe fn add_reference(&mut self, object: Sexp) {
        crate::heap().root(object);
        self.references.push(object);
    }

    unsafe fn string_vector(&mut self) -> R<Sexp> {
        if self.integer()? != 0 {
            return Err(error("names in persistent strings are not supported yet"));
        }
        let count = self.length()?;
        let result = alloc(SexpType::String, count)?;
        for index in 0..count {
            let item = self.item()?;
            values::set_elt(result, index, item);
        }
        Ok(result)
    }

    unsafe fn item(&mut self) -> R<Sexp> {
        let flags = self.integer()? as u32;
        let kind = flags & 0xFF;
        let levels = (flags >> 12) as u16;
        let is_object = flags & IS_OBJECT != 0;
        let has_attributes = flags & HAS_ATTR != 0;
        let has_tag = flags & HAS_TAG != 0;
        let known = *crate::heap().known();
        match kind {
            NILVALUE_SXP => return Ok(known.nil),
            EMPTYENV_SXP => return Ok(known.empty_env),
            BASEENV_SXP => return Ok(known.base_env),
            GLOBALENV_SXP => return Ok(known.global_env),
            UNBOUNDVALUE_SXP => return Ok(known.unbound),
            MISSINGARG_SXP => return Ok(known.missing_arg),
            BASENAMESPACE_SXP => return Ok(known.base_namespace),
            REFSXP => {
                let mut index = (flags >> 8) as usize;
                if index == 0 {
                    index = self.integer()? as usize;
                }
                return self
                    .references
                    .get(index.wrapping_sub(1))
                    .copied()
                    .ok_or_else(|| error("invalid reference in serialized data"));
            }
            PERSISTSXP => {
                let names = self.string_vector()?;
                let value = if self.hook == nil() {
                    return Err(error("no restore method available"));
                } else {
                    eval::apply(
                        nil(),
                        self.hook,
                        &eval::value_args(&[names]),
                        values::global_env(),
                    )?
                };
                self.add_reference(value);
                return Ok(value);
            }
            1 => {
                let name = self.item()?;
                let symbol = crate::heap().install_char(name)?;
                self.add_reference(symbol);
                return Ok(symbol);
            }
            PACKAGESXP => {
                let names = self.string_vector()?;
                let function =
                    eval::find_fun(values::install("findPackageEnv")?, values::base_env())?;
                let env = eval::apply(
                    nil(),
                    function,
                    &eval::value_args(&[names]),
                    values::global_env(),
                )?;
                self.add_reference(env);
                return Ok(env);
            }
            NAMESPACESXP => {
                let names = self.string_vector()?;
                let function =
                    eval::find_fun(values::install("..getNamespace")?, values::base_env())?;
                // GNU R names the object being read when it knows it, else "<unknown>", which
                // `..getNamespace` compares as a string when the namespace fails to load.
                let location = values::scalar_string("<unknown>")?;
                crate::heap().root(location);
                let env = eval::apply(
                    nil(),
                    function,
                    &eval::value_args(&[names, location]),
                    values::global_env(),
                )?;
                self.add_reference(env);
                return Ok(env);
            }
            4 => {
                let locked = self.integer()? != 0;
                let env = values::new_env(known.empty_env)?;
                self.add_reference(env);
                let enclosure = self.item()?;
                let frame = self.item()?;
                let hash_table = self.item()?;
                let attributes = self.item()?;
                crate::heap().set_enclosure(env, enclosure);
                fill_frame(env, frame)?;
                if hash_table.kind() == SexpType::List {
                    for index in 0..length(hash_table) {
                        fill_frame(env, hash_table.element(index))?;
                    }
                }
                if attributes != nil() {
                    crate::heap().set_attrib(env, attributes);
                    if values::get_attr(env, symbols().class) != nil() {
                        env.set_object(true);
                    }
                }
                if is_object {
                    env.set_object(true);
                }
                env.set_gp(levels & gp::S4_OBJECT | env.gp());
                if locked {
                    env.set_gp_bits(gp::FRAME_LOCKED, true);
                }
                return Ok(env);
            }
            2 | 6 | 3 | 5 | 17 => {
                let attributes = if has_attributes { self.item()? } else { nil() };
                let tag = if has_tag { self.item()? } else { nil() };
                let car = self.item()?;
                let cdr = self.item()?;
                let object_kind = match kind {
                    2 => SexpType::Pairlist,
                    6 => SexpType::Language,
                    3 => SexpType::Closure,
                    5 => SexpType::Promise,
                    _ => SexpType::Dots,
                };
                let (car, cdr) = if matches!(object_kind, SexpType::Closure | SexpType::Promise) {
                    (car, bytecode_expression(cdr))
                } else {
                    (car, cdr)
                };
                if matches!(object_kind, SexpType::Pairlist | SexpType::Language) {
                    argument_cells(cdr)?;
                }
                let object = values::triple(object_kind, car, cdr, tag)?;
                if object_kind == SexpType::Closure
                    && let Some(closures) = &mut self.closures
                {
                    closures.push(object);
                }
                if attributes != nil() {
                    crate::heap().set_attrib(object, attributes);
                }
                object.set_object(is_object);
                object.set_gp(levels);
                return Ok(object);
            }
            22 => {
                let pointer =
                    crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
                self.add_reference(pointer);
                let protected = self.item()?;
                let tag = self.item()?;
                crate::heap().set_external_protected(pointer, protected);
                crate::heap().set_external_tag(pointer, tag);
                self.finish(pointer, is_object, levels, has_attributes)?;
                return Ok(pointer);
            }
            23 => {
                let weak = crate::heap().alloc_weak_ref(nil(), nil(), nil(), 0, false)?;
                self.add_reference(weak);
                self.finish(weak, is_object, levels, has_attributes)?;
                return Ok(weak);
            }
            7 | 8 => {
                let count = self.count()?;
                let name = self.string(count)?;
                let name = String::from_utf8_lossy(&name).into_owned();
                let primitive = crate::builtins::primitive(&name)
                    .or_else(|| {
                        let symbol = values::install(&name).ok()?;
                        crate::builtins::internal_function(symbol)
                    })
                    .ok_or_else(|| {
                        error(format!("unrecognized internal function name \"{name}\""))
                    })?;
                return Ok(primitive);
            }
            9 => {
                let count = self.integer()?;
                if count == -1 {
                    return Ok(known.na_string);
                }
                let count = usize::try_from(count)
                    .map_err(|_| error("negative length vectors are not allowed"))?;
                let bytes = self.string(count)?;
                let encoding = if levels & gp::UTF8 != 0 {
                    Encoding::Utf8
                } else if levels & gp::LATIN1 != 0 {
                    Encoding::Latin1
                } else if levels & gp::BYTES != 0 {
                    Encoding::Bytes
                } else {
                    Encoding::Native
                };
                return values::mkchar_bytes(&bytes, encoding);
            }
            21 => {
                let reps_count = self.integer()? as usize;
                let reps = alloc(SexpType::List, reps_count)?;
                let code = self.bytecode(reps)?;
                return Ok(code);
            }
            ALTREP_SXP => {
                let info = self.item()?;
                let state = self.item()?;
                let attributes = self.item()?;
                let object = altrep_unserialize(info, state)?;
                if attributes != nil() {
                    crate::heap().set_attrib(object, attributes);
                }
                object.set_object(is_object);
                return Ok(object);
            }
            CLASSREFSXP | GENERICREFSXP => {
                return Err(error("this version of R cannot read class references"));
            }
            _ => {}
        }
        let object = match kind {
            10 | 13 => {
                let count = self.length()?;
                let vector = alloc(
                    if kind == 10 {
                        SexpType::Logical
                    } else {
                        SexpType::Integer
                    },
                    count,
                )?;
                let data = values::ints_mut(vector);
                if self.format == Format::Xdr {
                    let bytes = self.take(count * 4)?;
                    for (slot, chunk) in data.iter_mut().zip(bytes.as_chunks::<4>().0) {
                        *slot = i32::from_be_bytes(*chunk);
                    }
                } else {
                    for slot in data.iter_mut() {
                        *slot = self.integer()?;
                    }
                }
                vector
            }
            14 => {
                let count = self.length()?;
                let vector = alloc(SexpType::Real, count)?;
                let data = values::reals_mut(vector);
                if self.format == Format::Xdr {
                    let bytes = self.take(count * 8)?;
                    for (slot, chunk) in data.iter_mut().zip(bytes.as_chunks::<8>().0) {
                        *slot = f64::from_be_bytes(*chunk);
                    }
                } else {
                    for slot in data.iter_mut() {
                        *slot = self.real()?;
                    }
                }
                vector
            }
            15 => {
                let count = self.length()?;
                let vector = alloc(SexpType::Complex, count)?;
                for index in 0..count {
                    let r = self.real()?;
                    let i = self.real()?;
                    values::complexes_mut(vector)[index] = heap::object::Rcomplex { r, i };
                }
                vector
            }
            16 => {
                let count = self.length()?;
                let vector = alloc(SexpType::String, count)?;
                for index in 0..count {
                    let element = self.item()?;
                    values::set_elt(vector, index, element);
                }
                vector
            }
            19 | 20 => {
                let count = self.length()?;
                let vector = alloc(
                    if kind == 19 {
                        SexpType::List
                    } else {
                        SexpType::Expression
                    },
                    count,
                )?;
                for index in 0..count {
                    let element = self.item()?;
                    values::set_elt(vector, index, element);
                }
                vector
            }
            24 => {
                let count = self.length()?;
                let vector = alloc(SexpType::Raw, count)?;
                if self.format == Format::Ascii {
                    for index in 0..count {
                        let word = self.ascii_word()?;
                        values::raws_mut(vector)[index] =
                            u8::from_str_radix(&word, 16).unwrap_or(0);
                    }
                } else {
                    let bytes = self.take(count)?.to_vec();
                    values::raws_mut(vector).copy_from_slice(&bytes);
                }
                vector
            }
            25 => values::triple(SexpType::Object, nil(), nil(), nil())?,
            _ => {
                return Err(error(format!(
                    "ReadItem: unknown type {kind}, perhaps written by later version of R"
                )));
            }
        };
        self.finish(object, is_object, levels, has_attributes)?;
        Ok(object)
    }

    unsafe fn finish(
        &mut self,
        object: Sexp,
        is_object: bool,
        levels: u16,
        has_attributes: bool,
    ) -> R<()> {
        object.set_gp(levels);
        object.set_object(is_object);
        if has_attributes {
            let attributes = self.item()?;
            crate::heap().set_attrib(object, attributes);
        }
        Ok(())
    }

    /// A GNU R byte-code object. Only its source expression (the first constant) is kept.
    unsafe fn bytecode(&mut self, reps: Sexp) -> R<Sexp> {
        // The code vector: ry recompiles from the source expression instead.
        self.item()?;
        let count = self.integer()? as usize;
        let constants = alloc(SexpType::List, count)?;
        for index in 0..count {
            let kind = self.integer()? as u32;
            let value = match kind {
                21 => self.bytecode(reps)?,
                6 | 2 | BCREPDEF | BCREPREF | ATTRLANGSXP | ATTRLISTSXP => {
                    self.bytecode_language(kind, reps)?
                }
                _ => self.item()?,
            };
            values::set_elt(constants, index, value);
        }
        // A byte-code object reads as its source expression.
        let expression = if count > 0 {
            constants.element(0)
        } else {
            nil()
        };
        Ok(expression)
    }

    unsafe fn bytecode_language(&mut self, kind: u32, reps: Sexp) -> R<Sexp> {
        match kind {
            BCREPREF => {
                let index = self.integer()? as usize;
                Ok(reps.element(index))
            }
            BCREPDEF | 6 | 2 | ATTRLANGSXP | ATTRLISTSXP => {
                let mut kind = kind;
                let mut position: Option<usize> = None;
                if kind == BCREPDEF {
                    position = Some(self.integer()? as usize);
                    kind = self.integer()? as u32;
                }
                let mut has_attributes = false;
                let object_kind = match kind {
                    ATTRLANGSXP => {
                        has_attributes = true;
                        SexpType::Language
                    }
                    ATTRLISTSXP => {
                        has_attributes = true;
                        SexpType::Pairlist
                    }
                    6 => SexpType::Language,
                    _ => SexpType::Pairlist,
                };
                let object = values::triple(object_kind, nil(), nil(), nil())?;
                if let Some(position) = position {
                    values::set_elt(reps, position, object);
                }
                if has_attributes {
                    let attributes = self.item()?;
                    crate::heap().set_attrib(object, attributes);
                }
                let tag = self.item()?;
                crate::heap().set_tag(object, tag);
                let car_kind = self.integer()? as u32;
                let car = self.bytecode_language(car_kind, reps)?;
                crate::heap().set_car(object, car);
                let cdr_kind = self.integer()? as u32;
                let cdr = self.bytecode_language(cdr_kind, reps)?;
                argument_cells(cdr)?;
                crate::heap().set_cdr(object, cdr);
                Ok(object)
            }
            _ => self.item(),
        }
    }
}

/// Makes the rest of a call or pairlist a pairlist. GNU R walks argument lists by `CDR` alone,
/// so it never notices a call cell (`LANGSXP`) in an argument position, and its streams carry
/// some, notably in byte-code constants. ry's argument loops stop at the first cell that is not
/// a pairlist, so such a call would silently lose its arguments.
unsafe fn argument_cells(rest: Sexp) -> R<()> {
    if rest.kind() == SexpType::Language {
        crate::heap().retype(rest, SexpType::Pairlist)?;
    }
    Ok(())
}

/// A closure body or promise code that GNU R compiled: its expression.
unsafe fn bytecode_expression(value: Sexp) -> Sexp {
    value
}

unsafe fn fill_frame(env: Sexp, mut frame: Sexp) -> R<()> {
    while frame.kind() == SexpType::Pairlist {
        let symbol = frame.tag();
        let value = frame.car();
        crate::heap().define(env, symbol, value)?;
        // GNU R keeps active and locked bindings in the cell's gp bits, which it serializes
        // as the cell's levels.
        let cell_bits = frame.gp();
        if let Some(cell) = crate::heap().frame_cell(env, symbol) {
            if cell_bits & gp::ACTIVE_BINDING != 0 {
                cell.set_gp_bits(gp::ACTIVE_BINDING, true);
            }
            if cell_bits & gp::BINDING_LOCKED != 0 {
                cell.set_gp_bits(gp::BINDING_LOCKED, true);
            }
        }
        frame = frame.cdr();
    }
    Ok(())
}

/// Materializes the ALTREP classes GNU R serializes: compact sequences, deferred strings and
/// wrappers.
unsafe fn altrep_unserialize(info: Sexp, state: Sexp) -> R<Sexp> {
    let class = if info.kind() == SexpType::Pairlist {
        values::symbol_name(info.car()).into_owned()
    } else {
        String::new()
    };
    match class.as_str() {
        "compact_intseq" => {
            let data = crate::coerce::coerce_vector(state, SexpType::Real)?;
            let count = values::reals(data)[0] as usize;
            let start = values::reals(data)[1] as i64;
            let step = values::reals(data)[2] as i64;
            let result = alloc(SexpType::Integer, count)?;
            for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
                *slot = (start + index as i64 * step) as i32;
            }
            Ok(result)
        }
        "compact_realseq" => {
            let data = values::reals(state);
            let count = data[0] as usize;
            let start = data[1];
            let step = data[2];
            let result = alloc(SexpType::Real, count)?;
            for (index, slot) in values::reals_mut(result).iter_mut().enumerate() {
                *slot = start + index as f64 * step;
            }
            Ok(result)
        }
        "deferred_string" => {
            let argument = if state.kind() == SexpType::Pairlist {
                state.car()
            } else {
                state
            };
            crate::coerce::coerce_vector(argument, SexpType::String)
        }
        name if name.starts_with("wrap_") => {
            // GNU serializes a wrapper's state as the pairlist `(wrapped, metadata)`.
            if state.kind() == SexpType::Pairlist {
                Ok(state.car())
            } else if state.kind() == SexpType::List && length(state) >= 1 {
                Ok(state.element(0))
            } else {
                Ok(state)
            }
        }
        _ => Err(error(format!(
            "cannot unserialize ALTVEC object of class '{class}'"
        ))),
    }
}

/// Reads one serialized object from `bytes`.
pub unsafe fn unserialize(bytes: &[u8], hook: Sexp) -> R<Sexp> {
    Ok(unserialize_into(bytes, hook, None)?.0)
}

/// Unserializes `bytes` as [`unserialize`] does, and also returns every closure the stream made,
/// in the order the reader made them. A code image names a record's closures by that order
/// (`crate::code_image`), which depends only on the stream.
pub unsafe fn unserialize_with_closures(bytes: &[u8], hook: Sexp) -> R<(Sexp, Vec<Sexp>)> {
    let (value, closures) = unserialize_into(bytes, hook, Some(Vec::new()))?;
    Ok((value, closures.unwrap_or_default()))
}

unsafe fn unserialize_into(
    bytes: &[u8],
    hook: Sexp,
    closures: Option<Vec<Sexp>>,
) -> R<(Sexp, Option<Vec<Sexp>>)> {
    let mut bytes = bytes;
    // `save()` files carry an extra magic line.
    for magic in [
        &b"RDX2\n"[..],
        b"RDX3\n",
        b"RDA2\n",
        b"RDA3\n",
        b"RDB2\n",
        b"RDB3\n",
    ] {
        if let Some(rest) = bytes.strip_prefix(magic) {
            bytes = rest;
        }
    }
    let (format, skip) = match bytes.get(..3) {
        Some([b'\n', b'A', b'\n']) => (Format::Ascii, 3),
        _ => (
            stream_format(bytes.get(..2)).ok_or_else(|| error("unknown input format"))?,
            2,
        ),
    };
    read_object(Reader {
        input: Input::Slice {
            bytes,
            position: skip,
        },
        format,
        references: Vec::new(),
        hook,
        closures,
    })
}

/// Reads one object from an open connection, and no further.
pub unsafe fn unserialize_connection(con: Rconnection, hook: Sexp) -> R<Sexp> {
    Ok(read_object(connection_reader(con, hook)?)?.0)
}

/// A reader positioned after the format code of the stream on `con`.
unsafe fn connection_reader(con: Rconnection, hook: Sexp) -> R<Reader<'static>> {
    let mut reader = Reader {
        input: Input::Connection {
            con,
            buffer: Vec::new(),
        },
        format: Format::Xdr,
        references: Vec::new(),
        hook,
        closures: None,
    };
    let mut start = reader.take(2)?.to_vec();
    if start == b"\nA" {
        start = vec![b'A', reader.next_byte()?.unwrap_or(0)];
    }
    reader.format = stream_format(Some(&start)).ok_or_else(|| error("unknown input format"))?;
    // A text-mode connection may translate bytes, so only the ASCII format survives it.
    // SAFETY: the caller holds `con` open for the whole read.
    if unsafe { (*con).text } != 0 && reader.format != Format::Ascii {
        return Err(error("input format does not match specified format"));
    }
    Ok(reader)
}

fn stream_format(start: Option<&[u8]>) -> Option<Format> {
    match start? {
        b"X\n" => Some(Format::Xdr),
        b"B\n" => Some(Format::Binary),
        b"A\n" => Some(Format::Ascii),
        _ => None,
    }
}

/// The rest of a stream after its format line: the version header, then the object.
unsafe fn read_object(mut reader: Reader<'_>) -> R<(Sexp, Option<Vec<Sexp>>)> {
    read_header(&mut reader)?;
    let mark = crate::heap().handle_mark();
    let result = reader.item();
    crate::heap().release_handles(mark);
    let value = result?;
    crate::heap().root(value);
    Ok((value, reader.closures))
}

/// A serialization stream's header, which `serializeInfoFromConn` reports.
struct Header {
    version: i32,
    /// The R versions packed as `major * 65536 + minor * 256 + patch`.
    writer: i32,
    min_reader: i32,
    native_encoding: Option<String>,
}

/// Reads the header that follows the format code, leaving `reader` at the first item.
unsafe fn read_header(reader: &mut Reader<'_>) -> R<Header> {
    let version = reader.integer()?;
    let writer = reader.integer()?;
    let min_reader = reader.integer()?;
    let native_encoding = match version {
        2 => None,
        3 => {
            let encoding_length = reader.count()?;
            Some(if reader.format == Format::Ascii {
                reader.ascii_word()?
            } else {
                String::from_utf8_lossy(reader.take(encoding_length)?).into_owned()
            })
        }
        _ => return Err(error(format!("cannot read workspace version {version}"))),
    };
    Ok(Header {
        version,
        writer,
        min_reader,
        native_encoding,
    })
}

// Writing.

// Lazy-load databases.

/// The contents of lazy-load database files, read once per session.
fn file_cache() -> &'static mut FxHashMap<String, Vec<u8>> {
    static mut CACHE: Option<FxHashMap<String, Vec<u8>>> = None;
    // SAFETY: single-threaded access, as for the rest of the interpreter state.
    #[allow(static_mut_refs)]
    unsafe {
        CACHE.get_or_insert_with(FxHashMap::default)
    }
}

/// Drops a database file from the read cache, because the installer appends to it.
pub fn forget_database(path: &str) {
    file_cache().remove(path);
    code_image::forget(path);
}

/// One lazy-load database record holding `stream`, as GNU R's `R_compress1`, `R_compress2` and
/// `R_compress3` write it: the stream's length (big-endian), then for `kind` 1 a zlib stream, and
/// for 2 and 3 a type byte and bzip2 or raw LZMA2 data, or `0` and the stream itself when
/// compressing does not make it smaller. `kind` 0 is the stream alone.
pub fn compress_record(stream: &[u8], kind: i32) -> R<Vec<u8>> {
    use std::io::Write;
    if kind == 0 {
        return Ok(stream.to_vec());
    }
    let length = u32::try_from(stream.len())
        .map_err(|_| error("a lazy-load database record cannot exceed 4GB"))?;
    let mut record = length.to_be_bytes().to_vec();
    fn failed(failure: impl std::fmt::Display) -> crate::unwind::Unwind {
        error(format!("compression failed: {failure}"))
    }
    let packed = match kind {
        2 => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
            encoder.write_all(stream).map_err(failed)?;
            Some((b'2', encoder.finish().map_err(failed)?))
        }
        3 => {
            let options = lzma_rust2::Lzma2Options::with_preset(9);
            let mut encoder = lzma_rust2::Lzma2Writer::new(Vec::new(), options);
            encoder.write_all(stream).map_err(failed)?;
            Some((b'Z', encoder.finish().map_err(failed)?))
        }
        _ => None,
    };
    match packed {
        Some((_, data)) if data.len() >= stream.len() => {
            record.push(b'0');
            record.extend_from_slice(stream);
        }
        Some((method, data)) => {
            record.push(method);
            record.extend_from_slice(&data);
        }
        None => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(record, flate2::Compression::default());
            encoder.write_all(stream).map_err(failed)?;
            record = encoder.finish().map_err(failed)?;
        }
    }
    Ok(record)
}

/// The stream inside one lazy-load database record. `kind` is `lazyLoadDBfetch`'s `compressed`:
/// `1` is a length and a zlib stream, `2` and `3` a length, a type byte and the data.
pub fn decompress_record(record: &[u8], kind: i32) -> R<Vec<u8>> {
    if record.len() < 4 {
        return Err(error("lazy-load database record is too short"));
    }
    let expected = u32::from_be_bytes([record[0], record[1], record[2], record[3]]) as usize;
    let body = &record[4..];
    let (method, payload) = match kind {
        2 | 3 => (
            body.first().copied().unwrap_or(b'1'),
            body.get(1..).unwrap_or(&[]),
        ),
        _ => (b'1', body),
    };
    let mut output = Vec::with_capacity(expected);
    let corrupt = |failure: &dyn std::fmt::Display| {
        error(format!("lazy-load database is corrupt: {failure}"))
    };
    match method {
        b'1' => inflate_into(payload, &mut output).map_err(|failure| corrupt(&failure))?,
        b'0' => output.extend_from_slice(payload),
        // A raw LZMA2 stream (GNU R writes no xz container), which `datasets` and packages built
        // with `--data-compress=xz` use.
        b'Z' => {
            // The record states its decompressed size, and no LZMA2 dictionary needs to be
            // larger than the output it produces.
            let dictionary = expected.clamp(4096, u32::MAX as usize) as u32;
            lzma_rust2::Lzma2Reader::new(payload, dictionary, None)
                .read_to_end(&mut output)
                .map_err(|failure| corrupt(&failure))?;
        }
        b'2' => {
            bzip2::read::MultiBzDecoder::new(payload)
                .read_to_end(&mut output)
                .map_err(|failure| corrupt(&failure))?;
        }
        _ => inflate_into(body, &mut output).map_err(|failure| corrupt(&failure))?,
    }
    if output.len() != expected {
        return Err(corrupt(&"its length does not match"));
    }
    Ok(output)
}

/// Inflates the zlib stream `input` into `output`, whose capacity is the size the record states.
/// A package load fetches hundreds of records, so one inflater is reset between them rather than
/// built per record, and the stream is decoded straight from the record without the intermediate
/// buffer a reading decoder allocates.
fn inflate_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), String> {
    thread_local! {
        static INFLATER: std::cell::RefCell<Decompress> = std::cell::RefCell::new(Decompress::new(true));
    }
    INFLATER.with_borrow_mut(|inflater| {
        inflater.reset(true);
        loop {
            let consumed = inflater.total_in() as usize;
            let rest = input.get(consumed..).unwrap_or_default();
            if output.len() == output.capacity() {
                output.reserve(output.capacity().max(4096));
            }
            let before = (inflater.total_in(), inflater.total_out());
            let status = inflater
                .decompress_vec(rest, output, FlushDecompress::Finish)
                .map_err(|failure| failure.to_string())?;
            if status == Status::StreamEnd {
                return Ok(());
            }
            if (inflater.total_in(), inflater.total_out()) == before {
                return Err("the stream ends early".to_owned());
            }
        }
    })
}

/// `lazyLoadDBfetch(key, file, compressed, hook)`.
unsafe fn do_lazy_load_fetch(call: &Call<'_>) -> R<Sexp> {
    let key = call.arg(0);
    let file = values::require_string(call.arg(1), "file")?;
    let compressed = values::as_integer_scalar(call.arg(2));
    let hook = call.arg(3);
    let offset = crate::subset::numeric_element(key, 0) as usize;
    let size = crate::subset::numeric_element(key, 1) as usize;
    let cache = file_cache();
    if !cache.contains_key(&file) {
        let bytes = std::fs::read(&file)
            .map_err(|failure| error(format!("cannot open file '{file}': {failure}")))?;
        cache.insert(file.clone(), bytes);
    }
    let bytes = &cache[&file];
    let record = bytes
        .get(offset..offset.saturating_add(size))
        .ok_or_else(|| error("lazy-load database is corrupt"))?;
    let image_record = code_image::wants(&file)?.then(|| code_image::Record::of(offset as u64, record));
    // The record is decoded (or copied) out of the cached file before `unserialize` runs, because
    // its refhook can fetch from another database and so insert into the cache.
    let data = if compressed != 0 {
        decompress_record(record, compressed)?
    } else {
        record.to_vec()
    };
    let value = match image_record {
        Some(image_record) => code_image::read_record(&file, image_record, &data, hook)?,
        None => unserialize(&data, hook)?,
    };
    if value.kind() == SexpType::Promise {
        return eval::force_promise(value);
    }
    Ok(value)
}

unsafe fn do_unserialize_from_conn(call: &Call<'_>) -> R<Sexp> {
    let hook = call.arg(1);
    crate::connections::with_binary_input(call.arg(0), |con| unserialize_connection(con, hook))
}

/// `serializeInfoFromConn(con)`: the header of the serialized stream on `con`.
unsafe fn do_serialize_info_from_conn(call: &Call<'_>) -> R<Sexp> {
    let (format, header) = crate::connections::with_binary_input(call.arg(0), |con| {
        let mut reader = connection_reader(con, nil())?;
        let header = read_header(&mut reader)?;
        Ok((reader.format, header))
    })?;
    let release = |packed: i32| {
        format!(
            "{}.{}.{}",
            packed / 65536,
            (packed / 256) % 256,
            packed % 256
        )
    };
    let format = match format {
        Format::Xdr => "xdr",
        Format::Binary => "binary",
        Format::Ascii => "ascii",
    };
    let rooted = |value: Sexp| {
        crate::heap().root(value);
        value
    };
    let mut items = vec![
        ("version", rooted(values::scalar_integer(header.version)?)),
        (
            "writer_version",
            rooted(values::scalar_string(&release(header.writer))?),
        ),
        (
            "min_reader_version",
            rooted(values::scalar_string(&release(header.min_reader))?),
        ),
        ("format", rooted(values::scalar_string(format)?)),
    ];
    if let Some(encoding) = &header.native_encoding {
        items.push(("native_encoding", rooted(values::scalar_string(encoding)?)));
    }
    values::named_list(&items)
}

/// `loadFromConn2(con, envir, verbose)`: reads a `save()` file, whose single object is a
/// pairlist of the saved bindings, defines each in `envir`, and returns their names.
unsafe fn do_load_from_conn(call: &Call<'_>) -> R<Sexp> {
    let env = call.arg(1);
    if env.kind() != SexpType::Environment {
        return Err(call.error("invalid 'envir' argument"));
    }
    let bytes = crate::connections::read_binary(call.arg(0))?;
    let body = match bytes.get(..5) {
        // The stream carries its own format header, so XDR, ASCII and binary read alike.
        Some(b"RDX2\n" | b"RDX3\n" | b"RDA2\n" | b"RDA3\n" | b"RDB2\n" | b"RDB3\n") => &bytes[5..],
        _ => return Err(call.error("the input does not start with a magic number compatible with loading from a connection")),
    };
    let mut bindings = unserialize(body, nil())?;
    crate::heap().root(bindings);
    let mut names = Vec::new();
    while bindings.kind() == SexpType::Pairlist {
        let symbol = bindings.tag();
        crate::heap().define(env, symbol, bindings.car())?;
        names.push(values::symbol_name(symbol).into_owned());
        bindings = bindings.cdr();
    }
    values::string_vector(&names)
}

/// `getVarsFromFrame(vars, env, force)`: the named list of the bindings in `env`'s own frame.
unsafe fn do_get_vars_from_frame(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let env = call.arg(1);
    if names.kind() != SexpType::String || env.kind() != SexpType::Environment {
        return Err(call.error("bad variable names or environment"));
    }
    let force = values::as_logical_scalar(call.arg(2)) == 1;
    let items = saved_bindings(call, names, env, force, true)?;
    let result = alloc(SexpType::List, items.len())?;
    for (index, &(_, value)) in items.iter().enumerate() {
        values::set_elt(result, index, value);
    }
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

/// The values bound to `names`, as `(symbol, value)` pairs. Each value stays reachable from
/// `env`, which keeps it alive.
pub unsafe fn saved_bindings(
    call: &Call<'_>,
    names: Sexp,
    env: Sexp,
    force: bool,
    frame_only: bool,
) -> R<Vec<(Sexp, Sexp)>> {
    let mut items = Vec::with_capacity(length(names));
    for index in 0..length(names) {
        let name = values::string_at(names, index).unwrap_or_default();
        let symbol = values::install(&name)?;
        let mut value = if frame_only {
            crate::eval::find_var_in_frame(symbol, env)?
        } else {
            crate::eval::find_var(symbol, env)?
        };
        if value == values::unbound() {
            return Err(call.error(format!("object '{name}' not found")));
        }
        if force && value.kind() == SexpType::Promise {
            value = crate::eval::force_promise(value)?;
        }
        items.push((symbol, value));
    }
    Ok(items)
}

/// `memCompress(from, type)`, with `type` 1 to 5 for none, gzip (a zlib stream, as GNU R
/// writes), bzip2, xz and zstd. Base R offers zstd from R 4.5 on.
unsafe fn do_mem_compress(call: &Call<'_>) -> R<Sexp> {
    let from = values::raws(call.arg(0));
    let compressed = match values::as_integer_scalar(call.arg(1)) {
        1 => from.to_vec(),
        2 => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            std::io::Write::write_all(&mut encoder, from)
                .map_err(|failure| call.error(failure.to_string()))?;
            encoder
                .finish()
                .map_err(|failure| call.error(failure.to_string()))?
        }
        3 => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
            std::io::Write::write_all(&mut encoder, from)
                .map_err(|failure| call.error(failure.to_string()))?;
            encoder
                .finish()
                .map_err(|failure| call.error(failure.to_string()))?
        }
        4 => {
            let options = crate::connections::xz_options(9);
            let mut encoder = lzma_rust2::XzWriter::new(Vec::new(), options)
                .map_err(|failure| call.error(failure.to_string()))?;
            std::io::Write::write_all(&mut encoder, from)
                .map_err(|failure| call.error(failure.to_string()))?;
            encoder
                .finish()
                .map_err(|failure| call.error(failure.to_string()))?
        }
        // GNU R compresses at zstd's default level.
        5 => zstd::bulk::compress(from, 3).map_err(|failure| {
            call.error(format!("internal libzstd error ({failure}) in memCompress"))
        })?,
        _ => return Err(call.error("invalid 'type' argument")),
    };
    raw_vector(&compressed)
}

/// `memDecompress(from, type)`, with `type` 1 to 6 for none, gzip, bzip2, xz, unknown (which
/// detects the format from its magic bytes) and zstd.
unsafe fn do_mem_decompress(call: &Call<'_>) -> R<Sexp> {
    let from = values::raws(call.arg(0));
    let gzip_header = from.starts_with(&[0x1f, 0x8b]);
    let xz_header = from.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]);
    let mut kind = values::as_integer_scalar(call.arg(1));
    if kind == 5 {
        kind = if gzip_header {
            2
        } else if from.starts_with(b"BZh") {
            3
        } else if xz_header {
            4
        } else if from.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            6
        } else {
            crate::conditions::warning("unknown compression, assuming none")?;
            1
        };
    }
    let mut output = Vec::new();
    let outcome = match kind {
        1 => return raw_vector(from),
        2 if gzip_header => flate2::read::MultiGzDecoder::new(from)
            .read_to_end(&mut output)
            .map(drop),
        2 => ZlibDecoder::new(from).read_to_end(&mut output).map(drop),
        3 => bzip2::read::MultiBzDecoder::new(from)
            .read_to_end(&mut output)
            .map(drop),
        4 => lzma_rust2::XzReader::new(from, true)
            .read_to_end(&mut output)
            .map(drop),
        6 => zstd::stream::read::Decoder::new(from)
            .and_then(|mut decoder| decoder.read_to_end(&mut output))
            .map(drop),
        _ => return Err(call.error("invalid 'type' argument")),
    };
    outcome.map_err(|_| call.error("internal error in memDecompress"))?;
    raw_vector(&output)
}

unsafe fn raw_vector(bytes: &[u8]) -> R<Sexp> {
    let result = alloc(SexpType::Raw, bytes.len())?;
    values::raws_mut(result).copy_from_slice(bytes);
    Ok(result)
}

unsafe fn do_unserialize(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    if source.kind() == SexpType::Raw {
        let bytes = values::raws(source).to_vec();
        return unserialize(&bytes, call.arg(1));
    }
    let hook = call.arg(1);
    crate::connections::with_binary_input(source, |con| unserialize_connection(con, hook))
}

// ry's encoding of compiled code objects, which installed packages keep beside their lazy-load
// databases (see `crate::code_image`). GNU R's stream format carries byte code too, but only its
// own instruction set, and ry reads that back as source, so ry's code has a format of its own.
//
// A code object is encoded against the closure body it was compiled from. Every object the body
// reaches is numbered in the order `walk_source` visits it, and a constant that is one of them
// (the source expression itself, a call site's call, a nested function's body, a srcref) is
// written as its number. Loading numbers the freshly read body the same way and takes the object
// back by number, so a code object shares its source with the closure as a compiled one does, and
// the image never holds a second copy of the package's code.

/// Why a code object cannot be encoded: a constant of a kind the encoding does not carry, such as
/// an environment the body does not hold or a closure. The closure then compiles at its first call
/// instead, which is always correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unencodable(pub &'static str);

/// The objects a closure body reaches, numbered in `walk_source` order.
pub struct SourceObjects {
    pub objects: Vec<Sexp>,
}

/// The numbers [`SourceObjects`] gives, for encoding.
pub struct SourceNumbers {
    numbers: FxHashMap<Sexp, u32>,
}

/// A walk past this many objects stops numbering, in the encoder and the decoder alike, so a
/// pathological body costs a bounded walk and its later constants are written by value.
const MAX_SOURCE_OBJECTS: usize = 1 << 20;

/// Nesting deeper than this is refused by both sides: the encoder reports the constant as
/// unencodable and the decoder reports the input as corrupt, so neither recurses without bound.
const MAX_DEPTH: u32 = 512;

/// Numbers the objects `body` reaches: every object but symbols and `NULL` (which encode by name
/// and by tag), in preorder over calls, pairlists, lists, expression vectors and attributes. An
/// environment is numbered but not entered.
///
/// # Safety
/// `body` is a live object.
pub unsafe fn walk_source(body: Sexp) -> SourceObjects {
    let mut objects = Vec::new();
    let mut stack = vec![body];
    while let Some(object) = stack.pop() {
        if objects.len() >= MAX_SOURCE_OBJECTS {
            break;
        }
        if matches!(object.kind(), SexpType::Symbol | SexpType::Nil) {
            continue;
        }
        objects.push(object);
        if object.kind() == SexpType::Environment {
            continue;
        }
        let attributes = object.attrib();
        match object.kind() {
            SexpType::Language | SexpType::Pairlist | SexpType::Dots => {
                stack.push(object.cdr());
                stack.push(object.car());
            }
            SexpType::List | SexpType::Expression => {
                for index in (0..length(object)).rev() {
                    stack.push(object.element(index));
                }
            }
            _ => {}
        }
        if attributes != nil() {
            stack.push(attributes);
        }
    }
    SourceObjects { objects }
}

impl SourceObjects {
    pub fn numbers(&self) -> SourceNumbers {
        let mut numbers = FxHashMap::default();
        for (number, object) in self.objects.iter().enumerate() {
            numbers.entry(*object).or_insert(number as u32);
        }
        SourceNumbers { numbers }
    }
}

/// Encodes the code object `code`, compiled from a body numbered as `source`, onto `out`.
/// `trained` keeps what running the code recorded (type feedback, profile counters, slot hints);
/// otherwise those are written as a fresh compile leaves them, so an install's output depends on
/// the package alone.
///
/// # Safety
/// `code` is a live `BCODESXP` made by [`crate::bytecode`].
pub unsafe fn encode_code(
    code: Sexp,
    source: &SourceNumbers,
    trained: bool,
    out: &mut Vec<u8>,
) -> Result<(), Unencodable> {
    let mut encoder = Encoder {
        out,
        source,
        trained,
    };
    encoder.code(code, 0)
}

/// Decodes a code object [`encode_code`] wrote, against the numbered body `source`.
///
/// # Safety
/// Runs on the interpreter's thread; `source` holds live objects.
pub unsafe fn decode_code(input: &mut CodeInput<'_>, source: &SourceObjects) -> R<Sexp> {
    let mut decoder = Decoder { input, source };
    decoder.code(0)
}

/// The bytes a code object is decoded from.
pub struct CodeInput<'a> {
    pub bytes: &'a [u8],
    pub position: usize,
}

impl CodeInput<'_> {
    fn byte(&mut self) -> R<u8> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or_else(|| error("the code image ends early"))?;
        self.position += 1;
        Ok(byte)
    }

    pub fn varint(&mut self) -> R<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(error("the code image holds an overlong number"))
    }

    fn signed(&mut self) -> R<i64> {
        let value = self.varint()?;
        Ok((value >> 1) as i64 ^ -((value & 1) as i64))
    }

    /// A count of at most `limit`, for items that take no bytes of their own.
    pub fn bounded(&mut self, limit: usize) -> R<usize> {
        let count = self.varint()?;
        usize::try_from(count)
            .ok()
            .filter(|&count| count <= limit)
            .ok_or_else(|| error("the code image holds an impossible length"))
    }

    /// A count of items that each take at least one byte, so a corrupt count is caught before
    /// anything that large is allocated.
    pub fn count(&mut self) -> R<usize> {
        let count = self.varint()?;
        usize::try_from(count)
            .ok()
            .filter(|&count| count <= self.bytes.len() - self.position.min(self.bytes.len()))
            .ok_or_else(|| error("the code image holds an impossible length"))
    }

    fn take(&mut self, count: usize) -> R<&[u8]> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| error("the code image ends early"))?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u32_value(&mut self) -> R<u32> {
        u32::try_from(self.varint()?).map_err(|_| error("the code image holds an overlong number"))
    }

    fn i32_value(&mut self) -> R<i32> {
        i32::try_from(self.signed()?).map_err(|_| error("the code image holds an overlong number"))
    }
}

pub fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn write_signed(out: &mut Vec<u8>, value: i64) {
    write_varint(out, ((value << 1) ^ (value >> 63)) as u64);
}

mod tag {
    pub const NULL: u8 = 0;
    pub const UNBOUND: u8 = 1;
    pub const MISSING_ARG: u8 = 2;
    pub const SYMBOL: u8 = 3;
    pub const SOURCE: u8 = 4;
    pub const LOGICAL: u8 = 5;
    pub const INTEGER: u8 = 6;
    pub const REAL: u8 = 7;
    pub const COMPLEX: u8 = 8;
    pub const STRING: u8 = 9;
    pub const RAW: u8 = 10;
    pub const LIST: u8 = 11;
    pub const EXPRESSION: u8 = 12;
    pub const CELLS: u8 = 13;
    pub const BUILTIN: u8 = 14;
    pub const CODE: u8 = 15;
    pub const GLOBAL_ENV: u8 = 16;
    pub const BASE_ENV: u8 = 17;
    pub const EMPTY_ENV: u8 = 18;
    pub const BASE_NAMESPACE: u8 = 19;
    pub const BASE_FUNCTION: u8 = 20;
}

/// Base closures the compiler resolves while compiling and keeps as constants, found again by
/// name in the base environment.
const BASE_FUNCTIONS: &[&str] = &["local"];

struct Encoder<'a> {
    out: &'a mut Vec<u8>,
    source: &'a SourceNumbers,
    trained: bool,
}

impl Encoder<'_> {
    unsafe fn code(&mut self, code: Sexp, depth: u32) -> Result<(), Unencodable> {
        use crate::bytecode::{
            EPOCHS, FEEDBACK, FIRST_CONSTANT, HINTS, LOOPS, PROFILE, REGISTERS, SOURCE, SUMMARY,
            profile,
        };
        if depth > MAX_DEPTH {
            return Err(Unencodable("code nested too deeply"));
        }
        let words = values::ints(code.car());
        write_varint(self.out, words.len() as u64);
        for &word in words {
            write_varint(self.out, u64::from(word as u32));
        }
        let constants = code.cdr();
        let count = length(constants);
        if count < FIRST_CONSTANT {
            return Err(Unencodable("a code object without its fixed constants"));
        }
        self.value(constants.element(SOURCE), depth + 1)?;
        let caches = length(constants.element(EPOCHS));
        if caches > words.len().saturating_mul(4) {
            return Err(Unencodable("more inline caches than the code has room for"));
        }
        write_varint(self.out, caches as u64);
        let loops = values::ints(constants.element(LOOPS));
        write_varint(self.out, loops.len() as u64);
        for &value in loops {
            write_signed(self.out, i64::from(value));
        }
        write_varint(
            self.out,
            values::ints(constants.element(REGISTERS))
                .first()
                .map_or(0, |&count| count as u32 as u64),
        );
        self.value(constants.element(SUMMARY), depth + 1)?;
        let trained = self.trained
            && values::ints(constants.element(PROFILE))[..profile::LENGTH]
                .iter()
                .any(|&counter| counter != 0);
        self.out.push(u8::from(trained));
        if trained {
            for &counter in &values::ints(constants.element(PROFILE))[..profile::LENGTH] {
                write_signed(self.out, i64::from(counter));
            }
            let hints = values::ints(constants.element(HINTS));
            for &hint in hints {
                write_signed(self.out, i64::from(hint));
            }
            // Feedback is one word per instruction word and mostly zero, so it is written as
            // runs: a count of zeros, then the next nonzero word.
            let feedback = values::ints(constants.element(FEEDBACK));
            if feedback.len() != words.len() || hints.len() != caches {
                return Err(Unencodable("a code object whose tables disagree"));
            }
            let mut position = 0;
            while position < feedback.len() {
                let zeros = feedback[position..]
                    .iter()
                    .take_while(|&&value| value == 0)
                    .count();
                write_varint(self.out, zeros as u64);
                position += zeros;
                if position < feedback.len() {
                    write_signed(self.out, i64::from(feedback[position]));
                    position += 1;
                }
            }
        }
        write_varint(self.out, (count - FIRST_CONSTANT) as u64);
        for index in FIRST_CONSTANT..count {
            self.value(constants.element(index), depth + 1)?;
        }
        Ok(())
    }

    unsafe fn value(&mut self, value: Sexp, depth: u32) -> Result<(), Unencodable> {
        if depth > MAX_DEPTH {
            return Err(Unencodable("a constant nested too deeply"));
        }
        let heap = crate::heap();
        let known = heap.known();
        if value == nil() {
            self.out.push(tag::NULL);
            return Ok(());
        }
        if value == values::unbound() {
            self.out.push(tag::UNBOUND);
            return Ok(());
        }
        if value == values::missing_arg() {
            self.out.push(tag::MISSING_ARG);
            return Ok(());
        }
        if let Some(&number) = self.source.numbers.get(&value) {
            self.out.push(tag::SOURCE);
            write_varint(self.out, u64::from(number));
            return Ok(());
        }
        match value.kind() {
            SexpType::Symbol => {
                self.out.push(tag::SYMBOL);
                self.bytes(value.car().bytes());
                return Ok(());
            }
            SexpType::Environment => {
                let environment_tag = if value == known.global_env {
                    tag::GLOBAL_ENV
                } else if value == known.base_env {
                    tag::BASE_ENV
                } else if value == known.empty_env {
                    tag::EMPTY_ENV
                } else if value == known.base_namespace {
                    tag::BASE_NAMESPACE
                } else {
                    return Err(Unencodable("an environment"));
                };
                self.out.push(environment_tag);
                return Ok(());
            }
            SexpType::Builtin | SexpType::Special => {
                let definition =
                    eval::builtin_def(value).map_err(|_| Unencodable("an unknown builtin"))?;
                self.out.push(tag::BUILTIN);
                self.bytes(definition.name.as_bytes());
                return Ok(());
            }
            SexpType::Closure => {
                for name in BASE_FUNCTIONS {
                    if base_function(name) == Some(value) {
                        self.out.push(tag::BASE_FUNCTION);
                        self.bytes(name.as_bytes());
                        return Ok(());
                    }
                }
                return Err(Unencodable("a closure"));
            }
            SexpType::Bytecode => {
                self.out.push(tag::CODE);
                return self.code(value, depth + 1);
            }
            _ => {}
        }
        if value.is_altrep() {
            return Err(Unencodable("an ALTREP vector"));
        }
        let kind_tag = match value.kind() {
            SexpType::Logical => tag::LOGICAL,
            SexpType::Integer => tag::INTEGER,
            SexpType::Real => tag::REAL,
            SexpType::Complex => tag::COMPLEX,
            SexpType::String => tag::STRING,
            SexpType::Raw => tag::RAW,
            SexpType::List => tag::LIST,
            SexpType::Expression => tag::EXPRESSION,
            SexpType::Language | SexpType::Pairlist => tag::CELLS,
            _ => return Err(Unencodable("a constant of an unsupported type")),
        };
        self.out.push(kind_tag);
        if kind_tag == tag::CELLS {
            return self.cells(value, depth);
        }
        self.header(value, depth)?;
        let count = length(value);
        write_varint(self.out, count as u64);
        match value.kind() {
            SexpType::Logical | SexpType::Integer => {
                for &element in values::ints(value) {
                    write_signed(self.out, i64::from(element));
                }
            }
            SexpType::Real => {
                for &element in values::reals(value) {
                    self.out.extend_from_slice(&element.to_bits().to_le_bytes());
                }
            }
            SexpType::Complex => {
                for element in values::complexes(value) {
                    self.out.extend_from_slice(&element.r.to_bits().to_le_bytes());
                    self.out.extend_from_slice(&element.i.to_bits().to_le_bytes());
                }
            }
            SexpType::String => {
                for index in 0..count {
                    let charsxp = value.element(index);
                    if charsxp == known.na_string {
                        self.out.push(0);
                        continue;
                    }
                    self.out.push(match Encoding::of(charsxp) {
                        Encoding::Utf8 => 2,
                        Encoding::Latin1 => 3,
                        Encoding::Bytes => 4,
                        _ => 1,
                    });
                    self.bytes(charsxp.bytes());
                }
            }
            SexpType::Raw => self.out.extend_from_slice(values::raws(value)),
            _ => {
                for index in 0..count {
                    self.value(value.element(index), depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// A call or pairlist, written as its run of cells and then the final `cdr`, so a long
    /// argument list does not nest.
    unsafe fn cells(&mut self, first: Sexp, depth: u32) -> Result<(), Unencodable> {
        let mut cells = Vec::new();
        let mut cell = first;
        loop {
            cells.push(cell);
            let next = cell.cdr();
            if matches!(next.kind(), SexpType::Language | SexpType::Pairlist)
                && !self.source.numbers.contains_key(&next)
            {
                cell = next;
            } else {
                break;
            }
        }
        write_varint(self.out, cells.len() as u64);
        for &cell in &cells {
            self.out
                .push(u8::from(cell.kind() == SexpType::Language));
            self.header(cell, depth)?;
            self.value(cell.tag(), depth + 1)?;
            self.value(cell.car(), depth + 1)?;
        }
        let last = cells[cells.len() - 1];
        self.value(last.cdr(), depth + 1)
    }

    /// The general-purpose bits, the object bit and the attributes of `value`.
    unsafe fn header(&mut self, value: Sexp, depth: u32) -> Result<(), Unencodable> {
        write_varint(self.out, u64::from(value.gp()));
        let attributes = value.attrib();
        self.out
            .push(u8::from(value.is_object()) | u8::from(attributes != nil()) << 1);
        if attributes != nil() {
            self.value(attributes, depth + 1)?;
        }
        Ok(())
    }

    fn bytes(&mut self, bytes: &[u8]) {
        write_varint(self.out, bytes.len() as u64);
        self.out.extend_from_slice(bytes);
    }
}

/// The base closure bound to `name`, once something forced its lazy-load promise.
unsafe fn base_function(name: &str) -> Option<Sexp> {
    let heap = crate::heap();
    let symbol = values::install(name).ok()?;
    let cell = heap.frame_cell(heap.known().base_env, symbol)?;
    let mut value = cell.cdr();
    if value.kind() == SexpType::Promise {
        value = value.car();
    }
    (value.kind() == SexpType::Closure).then_some(value)
}

struct Decoder<'a, 'b> {
    input: &'a mut CodeInput<'b>,
    source: &'a SourceObjects,
}

impl Decoder<'_, '_> {
    unsafe fn code(&mut self, depth: u32) -> R<Sexp> {
        use crate::bytecode::{
            EPOCHS, FEEDBACK, FIRST_CONSTANT, HINTS, LOOPS, PROFILE, REGISTERS, SOURCE, SUMMARY,
            TIER1, profile,
        };
        if depth > MAX_DEPTH {
            return Err(error("the code image nests too deeply"));
        }
        let heap = crate::heap();
        let word_count = self.input.count()?;
        let words = alloc(SexpType::Integer, word_count)?;
        heap.root(words);
        for index in 0..word_count {
            values::ints_mut(words)[index] = self.input.u32_value()? as i32;
        }
        let source = self.value(depth + 1)?;
        heap.root(source);
        // Inline caches take no bytes of their own, so their count is bounded by the code's size.
        let caches = self.input.bounded(word_count.saturating_mul(4))?;
        let loop_count = self.input.count()?;
        if loop_count % 4 != 0 {
            return Err(error("the code image holds a malformed loop table"));
        }
        let loops = alloc(SexpType::Integer, loop_count)?;
        heap.root(loops);
        for index in 0..loop_count {
            values::ints_mut(loops)[index] = self.input.i32_value()?;
        }
        let registers = values::scalar_integer(
            i32::try_from(self.input.u32_value()?)
                .map_err(|_| error("the code image holds an overlong number"))?,
        )?;
        let summary = self.value(depth + 1)?;
        heap.root(summary);
        let epochs = alloc(SexpType::Real, caches)?;
        heap.root(epochs);
        values::reals_mut(epochs).fill(f64::from_bits(u64::MAX));
        let hints = alloc(SexpType::Integer, caches)?;
        heap.root(hints);
        values::ints_mut(hints).fill(0);
        let feedback = alloc(SexpType::Integer, word_count)?;
        heap.root(feedback);
        values::ints_mut(feedback).fill(0);
        let counters = alloc(SexpType::Integer, profile::LENGTH)?;
        heap.root(counters);
        values::ints_mut(counters).fill(0);
        match self.input.byte()? {
            0 => {}
            1 => {
                for index in 0..profile::LENGTH {
                    values::ints_mut(counters)[index] = self.input.i32_value()?;
                }
                for index in 0..caches {
                    values::ints_mut(hints)[index] = self.input.i32_value()?;
                }
                let mut position = 0;
                while position < word_count {
                    let zeros = self.input.bounded(word_count)?;
                    position = position
                        .checked_add(zeros)
                        .filter(|&end| end <= word_count)
                        .ok_or_else(|| error("the code image holds malformed feedback"))?;
                    if position < word_count {
                        values::ints_mut(feedback)[position] = self.input.i32_value()?;
                        position += 1;
                    }
                }
            }
            _ => return Err(error("the code image holds a malformed code object")),
        }
        let constant_count = self.input.count()?;
        let constants = alloc(SexpType::List, FIRST_CONSTANT + constant_count)?;
        heap.root(constants);
        heap.set_element(constants, SOURCE, source);
        heap.set_element(constants, EPOCHS, epochs);
        heap.set_element(constants, FEEDBACK, feedback);
        heap.set_element(constants, LOOPS, loops);
        heap.set_element(constants, REGISTERS, registers);
        heap.set_element(constants, PROFILE, counters);
        heap.set_element(constants, TIER1, nil());
        heap.set_element(constants, HINTS, hints);
        heap.set_element(constants, SUMMARY, summary);
        for index in 0..constant_count {
            let constant = self.value(depth + 1)?;
            heap.set_element(constants, FIRST_CONSTANT + index, constant);
        }
        let inline_caches = alloc(SexpType::List, caches * 2)?;
        heap.root(inline_caches);
        heap.mark_weak_pairs(inline_caches);
        let code = values::triple(SexpType::Bytecode, words, constants, inline_caches)?;
        heap.root(code);
        Ok(code)
    }

    unsafe fn value(&mut self, depth: u32) -> R<Sexp> {
        if depth > MAX_DEPTH {
            return Err(error("the code image nests too deeply"));
        }
        let heap = crate::heap();
        let known = *heap.known();
        let kind_tag = self.input.byte()?;
        let kind = match kind_tag {
            tag::NULL => return Ok(nil()),
            tag::UNBOUND => return Ok(values::unbound()),
            tag::MISSING_ARG => return Ok(values::missing_arg()),
            tag::SYMBOL => {
                let count = self.input.count()?;
                let name = self.input.take(count)?;
                return values::install_bytes(name);
            }
            tag::SOURCE => {
                let number = self.input.varint()?;
                return usize::try_from(number)
                    .ok()
                    .and_then(|number| self.source.objects.get(number).copied())
                    .ok_or_else(|| error("the code image does not match the closure's body"));
            }
            tag::GLOBAL_ENV => return Ok(known.global_env),
            tag::BASE_ENV => return Ok(known.base_env),
            tag::EMPTY_ENV => return Ok(known.empty_env),
            tag::BASE_NAMESPACE => return Ok(known.base_namespace),
            tag::BUILTIN => {
                let count = self.input.count()?;
                let name = String::from_utf8_lossy(self.input.take(count)?).into_owned();
                return crate::builtins::primitive(&name)
                    .or_else(|| {
                        let symbol = values::install(&name).ok()?;
                        crate::builtins::internal_function(symbol)
                    })
                    .ok_or_else(|| error(format!("the code image names an unknown builtin '{name}'")));
            }
            tag::BASE_FUNCTION => {
                let count = self.input.count()?;
                let name = String::from_utf8_lossy(self.input.take(count)?).into_owned();
                return base_function(&name).ok_or_else(|| {
                    error(format!("the code image names base function '{name}', which is unbound"))
                });
            }
            tag::CODE => return self.code(depth + 1),
            tag::CELLS => return self.cells(depth),
            tag::LOGICAL => SexpType::Logical,
            tag::INTEGER => SexpType::Integer,
            tag::REAL => SexpType::Real,
            tag::COMPLEX => SexpType::Complex,
            tag::STRING => SexpType::String,
            tag::RAW => SexpType::Raw,
            tag::LIST => SexpType::List,
            tag::EXPRESSION => SexpType::Expression,
            _ => return Err(error("the code image holds an unknown constant")),
        };
        let (levels, is_object, attributes) = self.header(depth)?;
        let count = self.input.count()?;
        let value = alloc(kind, count)?;
        heap.root(value);
        match kind {
            SexpType::Logical | SexpType::Integer => {
                for index in 0..count {
                    values::ints_mut(value)[index] = self.input.i32_value()?;
                }
            }
            SexpType::Real => {
                for index in 0..count {
                    let bytes = self.input.take(8)?;
                    let mut array = [0u8; 8];
                    array.copy_from_slice(bytes);
                    values::reals_mut(value)[index] = f64::from_bits(u64::from_le_bytes(array));
                }
            }
            SexpType::Complex => {
                for index in 0..count {
                    let mut parts = [0f64; 2];
                    for part in &mut parts {
                        let mut array = [0u8; 8];
                        array.copy_from_slice(self.input.take(8)?);
                        *part = f64::from_bits(u64::from_le_bytes(array));
                    }
                    let element = &mut values::complexes_mut(value)[index];
                    element.r = parts[0];
                    element.i = parts[1];
                }
            }
            SexpType::String => {
                for index in 0..count {
                    let encoding = match self.input.byte()? {
                        0 => {
                            heap.set_element(value, index, known.na_string);
                            continue;
                        }
                        1 => Encoding::Native,
                        2 => Encoding::Utf8,
                        3 => Encoding::Latin1,
                        4 => Encoding::Bytes,
                        _ => return Err(error("the code image holds a malformed string")),
                    };
                    let length = self.input.count()?;
                    let bytes = self.input.take(length)?;
                    values::set_mkchar_bytes(value, index, bytes, encoding)?;
                }
            }
            SexpType::Raw => {
                let bytes = self.input.take(count)?;
                values::raws_mut(value).copy_from_slice(bytes);
            }
            _ => {
                for index in 0..count {
                    let element = self.value(depth + 1)?;
                    heap.set_element(value, index, element);
                }
            }
        }
        if attributes != nil() {
            heap.set_attrib(value, attributes);
        }
        value.set_gp(levels);
        value.set_object(is_object);
        Ok(value)
    }

    unsafe fn cells(&mut self, depth: u32) -> R<Sexp> {
        let heap = crate::heap();
        let count = self.input.count()?;
        if count == 0 {
            return Err(error("the code image holds an empty call"));
        }
        let mut cells = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = match self.input.byte()? {
                0 => SexpType::Pairlist,
                1 => SexpType::Language,
                _ => return Err(error("the code image holds a malformed call")),
            };
            let (levels, is_object, attributes) = self.header(depth)?;
            let tag = self.value(depth + 1)?;
            heap.root(tag);
            let car = self.value(depth + 1)?;
            heap.root(car);
            let cell = values::triple(kind, car, nil(), tag)?;
            heap.root(cell);
            if attributes != nil() {
                heap.set_attrib(cell, attributes);
            }
            cell.set_gp(levels);
            cell.set_object(is_object);
            cells.push(cell);
        }
        let last = self.value(depth + 1)?;
        let mut next = last;
        for &cell in cells.iter().rev() {
            heap.set_cdr(cell, next);
            next = cell;
        }
        Ok(next)
    }

    unsafe fn header(&mut self, depth: u32) -> R<(u16, bool, Sexp)> {
        let levels = u16::try_from(self.input.varint()?)
            .map_err(|_| error("the code image holds an overlong number"))?;
        let flags = self.input.byte()?;
        let attributes = if flags & 2 != 0 {
            let attributes = self.value(depth + 1)?;
            crate::heap().root(attributes);
            attributes
        } else {
            nil()
        };
        Ok((levels, flags & 1 != 0, attributes))
    }
}
