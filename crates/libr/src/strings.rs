//! `CHARSXP`s, encodings and symbols.

use crate::{
    Rboolean,
    boundary::{ApiResult, enter, error},
    runtime,
};
use heap::{
    object::{Sexp, SexpType, gp},
    strings::Encoding,
};
use std::{
    borrow::Cow,
    ffi::{CStr, c_char, c_int},
};

/// The bytes of a C string, without its NUL. A null pointer reads as empty.
pub unsafe fn c_bytes<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        &[]
    } else {
        CStr::from_ptr(text).to_bytes()
    }
}

pub unsafe fn check_charsxp(object: Sexp, what: &str) -> ApiResult<()> {
    if object.kind() != SexpType::Char {
        return Err(error(format!(
            "{what}() can only be applied to a 'CHARSXP', not a '{}'",
            object.kind().name()
        )));
    }
    Ok(())
}

/// A `CHARSXP`'s text as UTF-8, converting Latin-1. Bytes-encoded strings have no text.
pub unsafe fn utf8_text<'a>(charsxp: Sexp) -> ApiResult<Cow<'a, str>> {
    let bytes = charsxp.bytes();
    if charsxp.has_gp(gp::LATIN1) {
        return Ok(Cow::Owned(bytes.iter().map(|&byte| byte as char).collect()));
    }
    if charsxp.has_gp(gp::BYTES) {
        return Err(error(
            "translating strings with \"bytes\" encoding is not allowed",
        ));
    }
    Ok(String::from_utf8_lossy(bytes))
}

/// `Rf_translateChar` and friends: the text in the native encoding, which ry fixes as UTF-8.
/// A string that needs no conversion is returned in place; a converted one lives in `R_alloc`
/// memory.
unsafe fn translate(charsxp: Sexp, what: &str) -> ApiResult<*const c_char> {
    if charsxp.kind() != SexpType::Char {
        return Err(error(format!(
            "{what} must be called on a CHARSXP, but got '{}'",
            charsxp.kind().name()
        )));
    }
    if charsxp == runtime().heap.known().na_string || !charsxp.has_gp(gp::LATIN1 | gp::BYTES) {
        return Ok(charsxp.data() as *const c_char);
    }
    let text = utf8_text(charsxp)?;
    arena_c_string(text.as_bytes())
}

pub unsafe fn arena_c_string(bytes: &[u8]) -> ApiResult<*const c_char> {
    let block = runtime()
        .arena
        .allocate(bytes.len() + 1)
        .map_err(|_| error("cannot allocate memory for a translated string"))?;
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), block.as_ptr(), bytes.len());
    *block.as_ptr().add(bytes.len()) = 0;
    Ok(block.as_ptr() as *const c_char)
}

fn encoding(code: c_int) -> ApiResult<Encoding> {
    Encoding::from_code(code as u32).ok_or_else(|| error(format!("unknown encoding: {code}")))
}

pub unsafe fn mk_char(bytes: &[u8], encoding: Encoding) -> ApiResult<Sexp> {
    let charsxp = runtime().heap.mk_char(bytes, encoding)?;
    Ok(charsxp)
}

/// The NUL-terminated name of a type, for `Rf_type2char`.
pub fn type_c_name(kind: SexpType) -> &'static CStr {
    match kind {
        SexpType::Nil => c"NULL",
        SexpType::Symbol => c"symbol",
        SexpType::Pairlist => c"pairlist",
        SexpType::Closure => c"closure",
        SexpType::Environment => c"environment",
        SexpType::Promise => c"promise",
        SexpType::Language => c"language",
        SexpType::Special => c"special",
        SexpType::Builtin => c"builtin",
        SexpType::Char => c"char",
        SexpType::Logical => c"logical",
        SexpType::Integer => c"integer",
        SexpType::Real => c"double",
        SexpType::Complex => c"complex",
        SexpType::String => c"character",
        SexpType::Dots => c"...",
        SexpType::Any => c"any",
        SexpType::List => c"list",
        SexpType::Expression => c"expression",
        SexpType::Bytecode => c"bytecode",
        SexpType::ExternalPtr => c"externalptr",
        SexpType::WeakRef => c"weakref",
        SexpType::Raw => c"raw",
        SexpType::Object => c"S4",
        SexpType::Record => c"record",
        SexpType::Binding => c"binding",
        SexpType::Free => c"FREESXP",
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_CHAR(object: Sexp) -> *const c_char {
    enter(|| {
        check_charsxp(object, "CHAR")?;
        Ok(object.data() as *const c_char)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_ASCII(object: Sexp) -> c_int {
    object.has_gp(gp::ASCII) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_UTF8(object: Sexp) -> c_int {
    object.has_gp(gp::UTF8) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_LATIN1(object: Sexp) -> c_int {
    object.has_gp(gp::LATIN1) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_BYTES(object: Sexp) -> c_int {
    object.has_gp(gp::BYTES) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_charIsASCII(object: Sexp) -> Rboolean {
    enter(|| {
        check_charsxp(object, "charIsASCII")?;
        Ok(object.has_gp(gp::ASCII) as Rboolean)
    })
}

/// True for text that is valid as UTF-8 as it stands: marked UTF-8, ASCII, or native, because
/// ry's native encoding is UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_charIsUTF8(object: Sexp) -> Rboolean {
    enter(|| {
        check_charsxp(object, "charIsUTF8")?;
        Ok(!object.has_gp(gp::LATIN1 | gp::BYTES) as Rboolean)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_charIsLatin1(object: Sexp) -> Rboolean {
    enter(|| {
        check_charsxp(object, "charIsLatin1")?;
        Ok((object.has_gp(gp::LATIN1) && !object.has_gp(gp::ASCII)) as Rboolean)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkChar(text: *const c_char) -> Sexp {
    enter(|| mk_char(c_bytes(text), Encoding::Native))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkCharCE(text: *const c_char, code: c_int) -> Sexp {
    enter(|| mk_char(c_bytes(text), encoding(code)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkCharLen(text: *const c_char, length: c_int) -> Sexp {
    enter(|| {
        let bytes = std::slice::from_raw_parts(text as *const u8, length.max(0) as usize);
        mk_char(bytes, Encoding::Native)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkCharLenCE(text: *const c_char, length: c_int, code: c_int) -> Sexp {
    enter(|| {
        let bytes = std::slice::from_raw_parts(text as *const u8, length.max(0) as usize);
        mk_char(bytes, encoding(code)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_getCharCE(object: Sexp) -> c_int {
    enter(|| {
        check_charsxp(object, "getCharCE")?;
        Ok(Encoding::of(object) as c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_translateChar(object: Sexp) -> *const c_char {
    enter(|| translate(object, "translateChar"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_translateCharUTF8(object: Sexp) -> *const c_char {
    enter(|| translate(object, "translateCharUTF8"))
}

/// The variant that fails politely: a null pointer instead of an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_translateCharFP(object: Sexp) -> *const c_char {
    enter(|| Ok(translate(object, "translateCharFP").unwrap_or(std::ptr::null())))
}

/// Converts between UTF-8 (and native, which is UTF-8) and Latin-1; other conversions return
/// the input unchanged, as GNU R does when it cannot convert.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_reEnc(
    text: *const c_char,
    from: c_int,
    to: c_int,
    substitute: c_int,
) -> *const c_char {
    enter(|| {
        let (from, to) = (encoding(from)?, encoding(to)?);
        let bytes = c_bytes(text);
        let utf8 = |encoding: Encoding| matches!(encoding, Encoding::Utf8 | Encoding::Native);
        if from == to || bytes.is_ascii() || from == Encoding::Bytes || to == Encoding::Bytes {
            return Ok(text);
        }
        if from == Encoding::Latin1 && utf8(to) {
            let converted: String = bytes.iter().map(|&byte| byte as char).collect();
            return arena_c_string(converted.as_bytes());
        }
        if utf8(from) && to == Encoding::Latin1 {
            let mut converted = Vec::new();
            for character in String::from_utf8_lossy(bytes).chars() {
                match u8::try_from(character as u32) {
                    Ok(byte) => converted.push(byte),
                    Err(_) if substitute == 2 => {
                        converted.extend(format!("<U+{:04X}>", character as u32).bytes())
                    }
                    Err(_) if substitute == 1 => {
                        converted.extend(format!("<{:02x}>", character as u32).bytes())
                    }
                    Err(_) => converted.push(b'?'),
                }
            }
            return arena_c_string(&converted);
        }
        Ok(text)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_install(name: *const c_char) -> Sexp {
    enter(|| Ok(runtime().heap.install(c_bytes(name))?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_installChar(charsxp: Sexp) -> Sexp {
    enter(|| install_translated(charsxp))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_installTrChar(charsxp: Sexp) -> Sexp {
    enter(|| install_translated(charsxp))
}

unsafe fn install_translated(charsxp: Sexp) -> ApiResult<Sexp> {
    check_charsxp(charsxp, "installChar")?;
    let runtime = runtime();
    if !charsxp.has_gp(gp::LATIN1 | gp::BYTES | gp::UTF8) {
        return Ok(runtime.heap.install_char(charsxp)?);
    }
    let text = utf8_text(charsxp)?.into_owned();
    Ok(runtime.heap.install(text.as_bytes())?)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PRINTNAME(symbol: Sexp) -> Sexp {
    enter(|| {
        if symbol.kind() != SexpType::Symbol {
            return Err(error(format!(
                "PRINTNAME() can only be applied to a 'symbol', not a '{}'",
                symbol.kind().name()
            )));
        }
        Ok(symbol.car())
    })
}

/// `Riconv_open`: GNU R's wrapper over the C library's `iconv`, where `""` names the native
/// encoding, which is UTF-8 here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Riconv_open(
    to: *const c_char,
    from: *const c_char,
) -> *mut std::ffi::c_void {
    let native = |name: *const c_char| {
        if c_bytes(name).is_empty() {
            c"UTF-8".as_ptr()
        } else {
            name
        }
    };
    libc::iconv_open(native(to), native(from))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Riconv(
    descriptor: *mut std::ffi::c_void,
    input: *mut *const c_char,
    input_left: *mut usize,
    output: *mut *mut c_char,
    output_left: *mut usize,
) -> usize {
    libc::iconv(
        descriptor,
        input as *mut *mut c_char,
        input_left,
        output,
        output_left,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Riconv_close(descriptor: *mut std::ffi::c_void) -> c_int {
    libc::iconv_close(descriptor)
}

/// How two strings sort. GNU R sorts with the locale's collation; ry's runtime collates in
/// code point order, which is what GNU R does in the C locale.
pub fn collate(left: &str, right: &str) -> std::cmp::Ordering {
    left.cmp(right)
}

/// The number of terminal columns `text` takes, counting East Asian wide characters as two.
pub fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}
