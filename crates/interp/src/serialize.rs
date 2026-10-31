//! R's serialization format (versions 2 and 3, XDR, binary and ASCII), and the lazy-load
//! databases installed packages keep their R objects in. Byte-compiled closures from GNU R are
//! read back as their source expressions, which ry compiles itself.

use crate::{
    builtins::{BuiltinDef, Call, builtin, internal},
    eval, heap, interp,
    unwind::{R, error},
    values::{self, alloc, length, nil, symbols},
};
use flate2::read::ZlibDecoder;
use heap::{
    object::{Sexp, SexpType, gp},
    strings::Encoding,
};
use rustc_hash::FxHashMap;
use std::io::Read;

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("lazyLoadDBfetch", 4, do_lazy_load_fetch),
    internal("unserializeFromConn", 2, do_unserialize_from_conn),
    internal("serializeToConn", 5, do_serialize_to_conn).invisible(),
    internal("unserialize", 2, do_unserialize),
    internal("serialize", 6, do_serialize),
    internal("serializeb", 6, do_serialize),
    internal("lazyLoadDBinsertValue", 5, |call| unsafe { Err(call.error("lazyLoadDBinsertValue is not supported")) }),
    internal("lazyLoadDBflush", 1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("loadFromConn2", 3, |call| unsafe { Err(call.error("load() is not supported yet")) }),
    internal("saveToConn", 8, |call| unsafe { Err(call.error("save() is not supported yet")) }),
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
enum Format {
    Xdr,
    Binary,
    Ascii,
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    format: Format,
    references: Vec<Sexp>,
    hook: Sexp,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> R<&[u8]> {
        if self.position + count > self.bytes.len() {
            return Err(error("ReadItem: unexpected end of serialized data"));
        }
        let slice = &self.bytes[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    fn ascii_word(&mut self) -> R<String> {
        while self.position < self.bytes.len() && self.bytes[self.position].is_ascii_whitespace() {
            self.position += 1;
        }
        let start = self.position;
        while self.position < self.bytes.len() && !self.bytes[self.position].is_ascii_whitespace() {
            self.position += 1;
        }
        Ok(String::from_utf8_lossy(&self.bytes[start..self.position]).into_owned())
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
            let word = self.ascii_word()?;
            let unescaped = crate::lower::unescape(&word).unwrap_or(word);
            return Ok(unescaped.into_bytes());
        }
        Ok(self.take(length)?.to_vec())
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
                    eval::apply(nil(), self.hook, &eval::value_args(&[names]), values::global_env())?
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
                let function = eval::find_fun(values::install("findPackageEnv")?, values::base_env())?;
                let env = eval::apply(nil(), function, &eval::value_args(&[names]), values::global_env())?;
                self.add_reference(env);
                return Ok(env);
            }
            NAMESPACESXP => {
                let names = self.string_vector()?;
                let function = eval::find_fun(values::install("..getNamespace")?, values::base_env())?;
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
                let (car, cdr) = if object_kind == SexpType::Closure {
                    (car, bytecode_expression(cdr))
                } else if object_kind == SexpType::Promise {
                    (car, bytecode_expression(cdr))
                } else {
                    (car, cdr)
                };
                if matches!(object_kind, SexpType::Pairlist | SexpType::Language) {
                    argument_cells(cdr)?;
                }
                let object = values::triple(object_kind, car, cdr, tag)?;
                if attributes != nil() {
                    crate::heap().set_attrib(object, attributes);
                }
                object.set_object(is_object);
                object.set_gp(levels);
                return Ok(object);
            }
            22 => {
                let pointer = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
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
                let count = self.integer()? as usize;
                let name = self.string(count)?;
                let name = String::from_utf8_lossy(&name).into_owned();
                let primitive = crate::builtins::primitive(&name)
                    .or_else(|| {
                        let symbol = values::install(&name).ok()?;
                        crate::builtins::internal_function(symbol)
                    })
                    .ok_or_else(|| error(format!("unrecognized internal function name \"{name}\"")))?;
                return Ok(primitive);
            }
            9 => {
                let count = self.integer()?;
                if count == -1 {
                    return Ok(known.na_string);
                }
                let bytes = self.string(count as usize)?;
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
                let vector = alloc(if kind == 10 { SexpType::Logical } else { SexpType::Integer }, count)?;
                let data = values::ints_mut(vector);
                if self.format == Format::Xdr {
                    let bytes = self.take(count * 4)?;
                    for (slot, chunk) in data.iter_mut().zip(bytes.chunks_exact(4)) {
                        *slot = i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
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
                    for (slot, chunk) in data.iter_mut().zip(bytes.chunks_exact(8)) {
                        let mut array = [0u8; 8];
                        array.copy_from_slice(chunk);
                        *slot = f64::from_be_bytes(array);
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
                let vector = alloc(if kind == 19 { SexpType::List } else { SexpType::Expression }, count)?;
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
                        values::raws_mut(vector)[index] = u8::from_str_radix(&word, 16).unwrap_or(0);
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

    unsafe fn finish(&mut self, object: Sexp, is_object: bool, levels: u16, has_attributes: bool) -> R<()> {
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
        let code = self.item()?;
        let _ = code;
        let count = self.integer()? as usize;
        let constants = alloc(SexpType::List, count)?;
        for index in 0..count {
            let kind = self.integer()? as u32;
            let value = match kind {
                21 => self.bytecode(reps)?,
                6 | 2 | BCREPDEF | BCREPREF | ATTRLANGSXP | ATTRLISTSXP => self.bytecode_language(kind, reps)?,
                _ => self.item()?,
            };
            values::set_elt(constants, index, value);
        }
        // A byte-code object reads as its source expression.
        let expression = if count > 0 { constants.element(0) } else { nil() };
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
            let argument = if state.kind() == SexpType::Pairlist { state.car() } else { state };
            crate::coerce::coerce_vector(argument, SexpType::String)
        }
        name if name.starts_with("wrap_") => {
            if state.kind() == SexpType::List && length(state) >= 1 {
                Ok(state.element(0))
            } else {
                Ok(state)
            }
        }
        _ => Err(error(format!("cannot unserialize ALTVEC object of class '{class}'"))),
    }
}

/// Reads one serialized object from `bytes`.
pub unsafe fn unserialize(bytes: &[u8], hook: Sexp) -> R<Sexp> {
    let mut bytes = bytes;
    // `save()` files carry an extra magic line.
    for magic in [&b"RDX2\n"[..], b"RDX3\n", b"RDA2\n", b"RDA3\n", b"RDB2\n", b"RDB3\n"] {
        if let Some(rest) = bytes.strip_prefix(magic) {
            bytes = rest;
        }
    }
    let format = match bytes.get(..2) {
        Some(b"X\n") => Format::Xdr,
        Some(b"B\n") => Format::Binary,
        Some(b"A\n") => Format::Ascii,
        _ => {
            if bytes.first() == Some(&b'\n') && bytes.get(1..3) == Some(b"A\n") {
                Format::Ascii
            } else {
                return Err(error("unknown input format"));
            }
        }
    };
    let skip = if bytes.first() == Some(&b'\n') { 3 } else { 2 };
    let mut reader = Reader {
        bytes,
        position: skip,
        format,
        references: Vec::new(),
        hook,
    };
    let version = reader.integer()?;
    let _writer = reader.integer()?;
    let _reader_version = reader.integer()?;
    match version {
        2 => {}
        3 => {
            let encoding_length = reader.integer()? as usize;
            reader.take(encoding_length)?;
        }
        _ => return Err(error(format!("cannot read workspace version {version}"))),
    }
    let mark = crate::heap().handle_mark();
    let result = reader.item();
    crate::heap().release_handles(mark);
    let value = result?;
    crate::heap().root(value);
    Ok(value)
}

// Writing.

struct Writer {
    bytes: Vec<u8>,
    references: FxHashMap<Sexp, usize>,
}

impl Writer {
    fn integer(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn real(&mut self, value: f64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn length(&mut self, length: usize) {
        if length > i32::MAX as usize {
            self.integer(-1);
            self.integer((length >> 32) as i32);
            self.integer(length as u32 as i32);
        } else {
            self.integer(length as i32);
        }
    }

    unsafe fn flags(&mut self, kind: u32, levels: u16, is_object: bool, has_attributes: bool, has_tag: bool) {
        let mut flags = kind | ((levels as u32) << 12);
        if is_object {
            flags |= IS_OBJECT;
        }
        if has_attributes {
            flags |= HAS_ATTR;
        }
        if has_tag {
            flags |= HAS_TAG;
        }
        self.integer(flags as i32);
    }

    unsafe fn reference(&mut self, object: Sexp) -> bool {
        if let Some(&index) = self.references.get(&object) {
            if index < (1 << 23) {
                self.integer(((index << 8) as u32 | REFSXP) as i32);
            } else {
                self.integer(REFSXP as i32);
                self.integer(index as i32);
            }
            return true;
        }
        false
    }

    unsafe fn add_reference(&mut self, object: Sexp) {
        let index = self.references.len() + 1;
        self.references.insert(object, index);
    }

    unsafe fn item(&mut self, object: Sexp) -> R<()> {
        let known = *crate::heap().known();
        if object == known.nil {
            self.integer(NILVALUE_SXP as i32);
            return Ok(());
        }
        if object == known.empty_env {
            self.integer(EMPTYENV_SXP as i32);
            return Ok(());
        }
        if object == known.base_env {
            self.integer(BASEENV_SXP as i32);
            return Ok(());
        }
        if object == known.global_env {
            self.integer(GLOBALENV_SXP as i32);
            return Ok(());
        }
        if object == known.unbound {
            self.integer(UNBOUNDVALUE_SXP as i32);
            return Ok(());
        }
        if object == known.missing_arg {
            self.integer(MISSINGARG_SXP as i32);
            return Ok(());
        }
        if object == known.base_namespace {
            self.integer(BASENAMESPACE_SXP as i32);
            return Ok(());
        }
        let object = values::materialize(object)?;
        let kind = object.kind();
        if matches!(kind, SexpType::Symbol | SexpType::Environment | SexpType::ExternalPtr | SexpType::WeakRef)
            && self.reference(object)
        {
            return Ok(());
        }
        let has_attributes = object.attrib() != known.nil && kind != SexpType::Char;
        let levels = if kind == SexpType::Char { object.gp() & (gp::UTF8 | gp::LATIN1 | gp::BYTES | gp::ASCII) } else { object.gp() & gp::S4_OBJECT };
        match kind {
            SexpType::Symbol => {
                self.add_reference(object);
                self.integer(1);
                self.item(object.car())?;
            }
            SexpType::Environment => {
                if crate::envir::is_namespace(object) {
                    self.add_reference(object);
                    self.integer(NAMESPACESXP as i32);
                    let name = crate::envir::namespace_name(object).unwrap_or_default();
                    self.integer(0);
                    self.integer(1);
                    self.charsxp_text(&name);
                    return Ok(());
                }
                self.add_reference(object);
                self.integer(4);
                self.integer(object.has_gp(gp::FRAME_LOCKED) as i32);
                self.item(object.enclosure())?;
                let symbols_list = crate::heap().frame_symbols(object);
                let mut items = Vec::new();
                for symbol in symbols_list {
                    if let Some(cell) = crate::heap().frame_cell(object, symbol) {
                        items.push((symbol, cell.cdr()));
                    }
                }
                let frame = values::pairlist(&items, SexpType::Pairlist)?;
                self.item(frame)?;
                self.item(nil())?;
                self.item(object.attrib())?;
            }
            SexpType::Pairlist | SexpType::Language | SexpType::Closure | SexpType::Promise | SexpType::Dots => {
                let code = match kind {
                    SexpType::Pairlist => 2,
                    SexpType::Language => 6,
                    SexpType::Closure => 3,
                    SexpType::Promise => 5,
                    _ => 17,
                };
                let tag = object.tag();
                self.flags(code, levels, object.is_object(), has_attributes, tag != known.nil);
                if has_attributes {
                    self.item(object.attrib())?;
                }
                if tag != known.nil {
                    self.item(tag)?;
                }
                let car = object.car();
                self.item(car)?;
                let cdr = if kind == SexpType::Closure {
                    crate::bytecode::closure_body(object)
                } else {
                    object.cdr()
                };
                self.item(cdr)?;
            }
            SexpType::ExternalPtr => {
                self.add_reference(object);
                self.flags(22, levels, object.is_object(), has_attributes, false);
                self.item(object.external_protected())?;
                self.item(object.external_tag())?;
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::WeakRef => {
                self.add_reference(object);
                self.flags(23, levels, object.is_object(), has_attributes, false);
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::Builtin | SexpType::Special => {
                let name = values::primitive_name(object);
                self.integer(if kind == SexpType::Builtin { 8 } else { 7 });
                self.integer(name.len() as i32);
                self.bytes.extend_from_slice(name.as_bytes());
            }
            SexpType::Char => {
                if object == known.na_string {
                    self.flags(9, 0, false, false, false);
                    self.integer(-1);
                } else {
                    self.flags(9, levels, false, false, false);
                    let bytes = object.bytes();
                    self.integer(bytes.len() as i32);
                    self.bytes.extend_from_slice(bytes);
                }
            }
            SexpType::Logical | SexpType::Integer => {
                self.flags(kind.code(), levels, object.is_object(), has_attributes, false);
                self.length(length(object));
                for &value in values::ints(object) {
                    self.integer(value);
                }
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::Real => {
                self.flags(14, levels, object.is_object(), has_attributes, false);
                self.length(length(object));
                for &value in values::reals(object) {
                    self.real(value);
                }
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::Complex => {
                self.flags(15, levels, object.is_object(), has_attributes, false);
                self.length(length(object));
                for value in values::complexes(object) {
                    self.real(value.r);
                    self.real(value.i);
                }
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::String | SexpType::List | SexpType::Expression => {
                self.flags(kind.code(), levels, object.is_object(), has_attributes, false);
                self.length(length(object));
                for index in 0..length(object) {
                    self.item(object.element(index))?;
                }
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::Raw => {
                self.flags(24, levels, object.is_object(), has_attributes, false);
                self.length(length(object));
                self.bytes.extend_from_slice(values::raws(object));
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            SexpType::Object => {
                self.flags(25, levels, object.is_object(), has_attributes, false);
                if has_attributes {
                    self.item(object.attrib())?;
                }
            }
            _ => return Err(error(format!("WriteItem: unknown type {}", kind.code()))),
        }
        Ok(())
    }

    fn charsxp_text(&mut self, text: &str) {
        self.integer(9 | ((gp::UTF8 as i32) << 12));
        self.integer(text.len() as i32);
        self.bytes.extend_from_slice(text.as_bytes());
    }
}

/// Serializes `object` in GNU R's XDR format, version 3.
pub unsafe fn serialize(object: Sexp) -> R<Vec<u8>> {
    let mut writer = Writer {
        bytes: b"X\n".to_vec(),
        references: FxHashMap::default(),
    };
    writer.integer(3);
    writer.integer(0x040602);
    writer.integer(0x030500);
    writer.integer(5);
    writer.bytes.extend_from_slice(b"UTF-8");
    writer.item(object)?;
    Ok(writer.bytes)
}

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

/// Decompresses a lazy-load database record.
fn decompress(record: &[u8], kind: i32) -> R<Vec<u8>> {
    if record.len() < 4 {
        return Err(error("lazy-load database record is too short"));
    }
    let expected = u32::from_be_bytes([record[0], record[1], record[2], record[3]]) as usize;
    let body = &record[4..];
    let (method, payload) = match kind {
        2 | 3 => (body.first().copied().unwrap_or(b'1'), body.get(1..).unwrap_or(&[])),
        _ => (b'1', body),
    };
    let mut output = Vec::with_capacity(expected);
    let corrupt = |failure: &dyn std::fmt::Display| error(format!("lazy-load database is corrupt: {failure}"));
    match method {
        b'1' => {
            ZlibDecoder::new(payload).read_to_end(&mut output).map_err(|failure| corrupt(&failure))?;
        }
        b'0' => output.extend_from_slice(payload),
        // A raw LZMA2 stream (GNU R writes no xz container), which `datasets` and packages built
        // with `--data-compress=xz` use.
        b'Z' => {
            let mut input = std::io::BufReader::new(payload);
            lzma_rs::lzma2_decompress(&mut input, &mut output).map_err(|failure| corrupt(&failure))?;
        }
        b'2' => return Err(error("bzip2-compressed lazy-load databases are not supported yet")),
        _ => {
            ZlibDecoder::new(body)
                .read_to_end(&mut output)
                .map_err(|failure| error(format!("lazy-load database is corrupt: {failure}")))?;
        }
    }
    Ok(output)
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
        .get(offset..offset + size)
        .ok_or_else(|| error("lazy-load database is corrupt"))?
        .to_vec();
    let data = if compressed != 0 { decompress(&record, compressed)? } else { record };
    let value = unserialize(&data, hook)?;
    if value.kind() == SexpType::Promise {
        return eval::force_promise(value);
    }
    Ok(value)
}

unsafe fn do_unserialize_from_conn(call: &Call<'_>) -> R<Sexp> {
    let bytes = crate::connections::read_all_bytes(call.arg(0))?;
    unserialize(&bytes, call.arg(1))
}

unsafe fn do_serialize_to_conn(call: &Call<'_>) -> R<Sexp> {
    let bytes = serialize(call.arg(0))?;
    crate::connections::write_bytes(call.arg(1), &bytes)?;
    Ok(nil())
}

unsafe fn do_unserialize(call: &Call<'_>) -> R<Sexp> {
    let source = call.arg(0);
    if source.kind() == SexpType::Raw {
        let bytes = values::raws(source).to_vec();
        return unserialize(&bytes, call.arg(1));
    }
    let bytes = crate::connections::read_all_bytes(source)?;
    unserialize(&bytes, call.arg(1))
}

unsafe fn do_serialize(call: &Call<'_>) -> R<Sexp> {
    let bytes = serialize(call.arg(0))?;
    let connection = call.arg(1);
    if connection == nil() {
        let result = alloc(SexpType::Raw, bytes.len())?;
        values::raws_mut(result).copy_from_slice(&bytes);
        return Ok(result);
    }
    crate::connections::write_bytes(connection, &bytes)?;
    Ok(nil())
}

pub fn unused() {
    let _ = interp;
}
