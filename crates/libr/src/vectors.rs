//! Vector access, type predicates and coercion.

use {
    crate::{
        R_len_t, R_xlen_t, Rboolean, SEXPTYPE, altrep,
        attributes::{get_attrib, inherits, set_attrib},
        boundary::{ApiResult, enter, error, warn},
        globals::{R_ClassSymbol, R_DimSymbol, R_LevelsSymbol, R_NamesSymbol, R_TspSymbol},
        memory::{alloc_vector, scalar_string},
        runtime,
        strings::{c_bytes, utf8_text},
    },
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, is_na_real},
        strings::Encoding,
    },
    std::ffi::{c_char, c_int, c_void},
};

pub fn sexp_type(code: SEXPTYPE) -> ApiResult<SexpType> {
    SexpType::from_code(code).ok_or_else(|| error(format!("invalid type code {code}")))
}

/// Refuses a ry record, tuple or tag. C code sees such a value as an opaque object of type 27:
/// it can move the handle through a list, but every accessor that would read it as R data
/// raises the conversion error instead.
pub unsafe fn refuse_native(object: Sexp) -> ApiResult<()> {
    if object.kind() == SexpType::Record {
        let kind = interp::records::native_kind(object);
        return Err(error(interp::records::boundary_message("the value", kind)));
    }
    Ok(())
}

/// The length of a vector that needs no ALTREP method, read without entering the API.
#[inline]
unsafe fn plain_length(object: Sexp) -> Option<R_xlen_t> {
    let kind = object.kind();
    if kind.is_vector() && !object.is_altrep() {
        Some(object.length())
    } else if kind == SexpType::Nil {
        Some(0)
    } else {
        None
    }
}

/// Whether `object` is a vector of `kind` whose elements are stored inline, so an accessor can
/// read them without entering the API. `Rinternals.h` inlines the same checks.
#[inline]
unsafe fn is_plain(object: Sexp, kind: SexpType) -> bool {
    object.kind() == kind && !object.is_altrep()
}

/// `INTEGER` also accepts a logical vector, whose elements are C `int`s too.
#[inline]
unsafe fn is_plain_integer(object: Sexp) -> bool {
    matches!(object.kind(), SexpType::Integer | SexpType::Logical) && !object.is_altrep()
}

/// The length of a vector, `LENGTH`'s and `XLENGTH`'s answer.
pub unsafe fn xlength(object: Sexp) -> ApiResult<R_xlen_t> {
    refuse_native(object)?;
    if object.is_altrep() {
        return altrep::length(object);
    }
    let kind = object.kind();
    if kind.is_vector() {
        Ok(object.length())
    } else if kind == SexpType::Nil {
        Ok(0)
    } else {
        Err(error(format!(
            "LENGTH or similar applied to {} object",
            kind.name()
        )))
    }
}

/// `Rf_xlength`, which also counts pairlist cells and environment bindings.
pub unsafe fn generic_length(object: Sexp) -> ApiResult<R_xlen_t> {
    refuse_native(object)?;
    if object.is_altrep() || object.kind().is_vector() {
        return xlength(object);
    }
    Ok(match object.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let mut count = 0;
            let mut cell = object;
            while matches!(
                cell.kind(),
                SexpType::Pairlist | SexpType::Language | SexpType::Dots
            ) {
                count += 1;
                cell = cell.cdr();
            }
            count
        }
        SexpType::Environment => runtime().heap.frame_symbols(object).len() as R_xlen_t,
        _ => 1,
    })
}

/// A vector's element storage. A writable pointer to handle elements escapes the write
/// barrier, so the collector rescans such a vector from then on.
pub unsafe fn data_pointer(object: Sexp, writable: bool) -> ApiResult<*mut u8> {
    refuse_native(object)?;
    if object.is_altrep() {
        return altrep::dataptr(object, writable);
    }
    let kind = object.kind();
    if !kind.is_vector() || kind == SexpType::Char && writable {
        return Err(error(format!(
            "cannot get data pointer of '{}' objects",
            kind.name()
        )));
    }
    if writable
        && matches!(
            kind,
            SexpType::String | SexpType::List | SexpType::Expression
        )
    {
        runtime().heap.escape(object);
    }
    Ok(object.data())
}

unsafe fn typed_data(
    object: Sexp,
    accepted: &[SexpType],
    accessor: &str,
    wanted: &str,
    writable: bool,
) -> ApiResult<*mut u8> {
    check_type(object, accepted, accessor, wanted)?;
    data_pointer(object, writable)
}

unsafe fn check_type(
    object: Sexp,
    accepted: &[SexpType],
    accessor: &str,
    wanted: &str,
) -> ApiResult<()> {
    refuse_native(object)?;
    let kind = object.kind();
    if !accepted.contains(&kind) {
        return Err(error(format!(
            "{accessor}() can only be applied to a '{wanted}', not a '{}'",
            kind.name()
        )));
    }
    Ok(())
}

const INTEGER_TYPES: &[SexpType] = &[SexpType::Integer, SexpType::Logical];

unsafe fn checked_index(object: Sexp, index: R_xlen_t, accessor: &str) -> ApiResult<usize> {
    let length = xlength(object)?;
    if index < 0 || index >= length {
        return Err(error(format!(
            "attempt access index {index}/{length} in {accessor}"
        )));
    }
    Ok(index as usize)
}

/// An integer or logical element, through ALTREP when needed.
pub unsafe fn int_elt(object: Sexp, index: usize) -> ApiResult<i32> {
    if object.is_altrep() {
        return altrep::int_elt(object, index as R_xlen_t);
    }
    Ok(*(object.data() as *const i32).add(index))
}

pub unsafe fn real_elt(object: Sexp, index: usize) -> ApiResult<f64> {
    if object.is_altrep() {
        return altrep::real_elt(object, index as R_xlen_t);
    }
    Ok(*(object.data() as *const f64).add(index))
}

/// A character or list element, through ALTREP when needed.
pub unsafe fn sexp_elt(object: Sexp, index: usize) -> ApiResult<Sexp> {
    if object.is_altrep() {
        return altrep::sexp_elt(object, index as R_xlen_t);
    }
    Ok(object.element(index))
}

pub unsafe fn set_sexp_elt(object: Sexp, index: usize, value: Sexp) -> ApiResult<()> {
    if object.is_altrep() {
        return altrep::set_sexp_elt(object, index as R_xlen_t, value);
    }
    runtime().heap.set_element(object, index, value);
    Ok(())
}

/// The elements of an integer vector, coercing a double one.
pub unsafe fn integers(object: Sexp) -> ApiResult<Vec<i32>> {
    let length = xlength(object)? as usize;
    match object.kind() {
        SexpType::Integer | SexpType::Logical => {
            (0..length).map(|index| int_elt(object, index)).collect()
        }
        SexpType::Real => (0..length)
            .map(|index| Ok(real_to_int(real_elt(object, index)?).0))
            .collect(),
        kind => Err(error(format!(
            "expected an integer vector, not a '{}'",
            kind.name()
        ))),
    }
}

pub unsafe fn set_names_from(object: Sexp, names: Sexp) -> ApiResult<()> {
    set_attrib(object, R_NamesSymbol.get(), names).map(|_| ())
}

// Element conversions, with R's NA rules. The `bool` reports a value that became NA.

pub fn int_to_real(value: i32) -> f64 {
    if value == NA_INTEGER {
        NA_REAL
    } else {
        value as f64
    }
}

pub fn real_to_int(value: f64) -> (i32, bool) {
    if value.is_nan() {
        (NA_INTEGER, false)
    } else if value >= i32::MAX as f64 + 1.0 || value <= i32::MIN as f64 {
        (NA_INTEGER, true)
    } else {
        (value.trunc() as i32, false)
    }
}

pub fn int_to_logical(value: i32) -> i32 {
    if value == NA_INTEGER {
        NA_LOGICAL
    } else {
        (value != 0) as i32
    }
}

pub fn real_to_logical(value: f64) -> i32 {
    if value.is_nan() {
        NA_LOGICAL
    } else {
        (value != 0.0) as i32
    }
}

pub fn string_to_logical(text: &[u8]) -> i32 {
    match text {
        b"T" | b"True" | b"TRUE" | b"true" => 1,
        b"F" | b"False" | b"FALSE" | b"false" => 0,
        _ => NA_LOGICAL,
    }
}

/// `R_strtod`'s reading of a whole string: `None` when it is not a number.
pub fn parse_real(text: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(text).ok()?.trim();
    if text == "NA" {
        return Some(NA_REAL);
    }
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        parse_hex(hex)?
    } else {
        match digits {
            "Inf" | "inf" | "Infinity" | "infinity" => f64::INFINITY,
            "NaN" => f64::NAN,
            _ if digits.bytes().all(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-')
            }) && digits.bytes().any(|byte| byte.is_ascii_digit()) =>
            {
                digits.parse::<f64>().ok()?
            }
            _ => return None,
        }
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn parse_hex(text: &str) -> Option<f64> {
    let (mantissa, exponent) = match text.find(['p', 'P']) {
        Some(position) => (&text[..position], text[position + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    if mantissa.is_empty() {
        return None;
    }
    let mut value = 0.0f64;
    for digit in mantissa.chars() {
        value = value * 16.0 + digit.to_digit(16)? as f64;
    }
    Some(value * 2f64.powi(exponent))
}

/// R's `as.character` for a double: the shortest of fixed and scientific notation at up to 15
/// significant digits, fixed when the widths tie.
pub fn format_real(value: f64) -> String {
    if is_na_real(value) {
        return "NA".into();
    }
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Inf".into()
        } else {
            "-Inf".into()
        };
    }
    if value == 0.0 {
        return "0".into();
    }
    let scientific = format!("{value:.14e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let significant = digits.trim_end_matches('0').len().max(1) as i32;
    let negative = (value < 0.0) as i32;
    let (left, right) = if exponent >= 0 {
        (exponent + 1, (significant - exponent - 1).max(0))
    } else {
        (1, -exponent - 1 + significant)
    };
    let fixed_width = negative + left + if right > 0 { right + 1 } else { 0 };
    let scientific_width = negative
        + if significant > 1 { significant + 1 } else { 1 }
        + if exponent.abs() >= 100 { 5 } else { 4 };
    if fixed_width <= scientific_width {
        format!("{value:.*}", right as usize)
    } else {
        let mantissa = format!("{:.*e}", (significant - 1) as usize, value);
        let (mantissa, _) = mantissa.split_once('e').unwrap_or((&mantissa, ""));
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    }
}

pub fn format_complex(value: Rcomplex) -> String {
    if is_na_real(value.r) || is_na_real(value.i) {
        return "NA".into();
    }
    let imaginary = format_real(value.i.abs());
    let sign = if value.i < 0.0 { '-' } else { '+' };
    format!("{}{sign}{imaginary}i", format_real(value.r))
}

/// The `CHARSXP` R prints for element `index` of an atomic vector.
pub unsafe fn element_as_charsxp(object: Sexp, index: usize) -> ApiResult<Sexp> {
    let na = runtime().heap.known().na_string;
    let text = match object.kind() {
        SexpType::String => return sexp_elt(object, index),
        SexpType::Logical => match int_elt(object, index)? {
            NA_LOGICAL => return Ok(na),
            0 => "FALSE".to_owned(),
            _ => "TRUE".to_owned(),
        },
        SexpType::Integer => match int_elt(object, index)? {
            NA_INTEGER => return Ok(na),
            value => value.to_string(),
        },
        SexpType::Real => {
            let value = real_elt(object, index)?;
            if is_na_real(value) {
                return Ok(na);
            }
            format_real(value)
        }
        SexpType::Complex => {
            let value = altrep::elt::<Rcomplex>(object, index as R_xlen_t)?;
            if is_na_real(value.r) || is_na_real(value.i) {
                return Ok(na);
            }
            format_complex(value)
        }
        SexpType::Raw => format!("{:02x}", altrep::elt::<u8>(object, index as R_xlen_t)?),
        kind => {
            return Err(error(format!(
                "cannot coerce type '{}' to character",
                kind.name()
            )));
        }
    };
    Ok(runtime().heap.mk_char(text.as_bytes(), Encoding::Native)?)
}

/// `Rf_coerceVector`.
pub unsafe fn coerce_vector(object: Sexp, target: SexpType) -> ApiResult<Sexp> {
    let runtime = runtime();
    let source = object.kind();
    if source == target {
        return Ok(object);
    }
    if source == SexpType::String && target == SexpType::Symbol {
        if xlength(object)? < 1 {
            return Err(error("invalid data of mode 'character' (too short)"));
        }
        let name = sexp_elt(object, 0)?;
        return Ok(runtime.heap.install_char(name)?);
    }
    if source == SexpType::Symbol {
        if target == SexpType::String {
            return scalar_string(object.car());
        }
        let as_string = scalar_string(object.car())?;
        return coerce_vector(as_string, target);
    }
    if source == SexpType::Environment {
        return Err(error("environments cannot be coerced to other types"));
    }
    if target == SexpType::String && source == SexpType::Integer && inherits(object, "factor")? {
        return as_character_factor(object);
    }
    if object.is_altrep()
        && let Some(converted) = altrep::coerce(object, target)?
    {
        return Ok(converted);
    }
    // As in GNU R, a number vector without attributes converts to character lazily, which
    // makes `as.character(1:n)` cheap when only some elements are read.
    if target == SexpType::String
        && matches!(source, SexpType::Integer | SexpType::Real)
        && object.attrib() == runtime.nil()
    {
        return altrep::deferred_string(object);
    }
    match source {
        SexpType::Nil => alloc_vector(target, 0),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            if target == SexpType::Pairlist {
                return crate::lists::copy_as_pairlist(object);
            }
            let vector = crate::lists::pairlist_to_vector(object)?;
            runtime.heap.root(vector);
            if target == SexpType::List {
                return Ok(vector);
            }
            coerce_vector(vector, target)
        }
        SexpType::List | SexpType::Expression => coerce_list(object, target),
        _ if source.is_atomic() => coerce_atomic(object, target),
        _ => Err(error(format!(
            "cannot coerce type '{}' to vector of type '{}'",
            source.name(),
            target.name()
        ))),
    }
}

unsafe fn coerce_atomic(object: Sexp, target: SexpType) -> ApiResult<Sexp> {
    let length = xlength(object)?;
    let count = length as usize;
    match target {
        SexpType::List | SexpType::Expression => {
            let result = runtime().heap.alloc_vector(target, length)?;
            runtime().heap.root(result);
            for index in 0..count {
                let element = element_vector(object, index)?;
                runtime().heap.set_element(result, index, element);
            }
            let names = get_attrib(object, R_NamesSymbol.get())?;
            if names != runtime().nil() {
                set_names_from(result, names)?;
            }
            return Ok(result);
        }
        SexpType::Pairlist => {
            let vector = coerce_atomic(object, SexpType::List)?;
            return crate::lists::vector_to_pairlist(vector);
        }
        _ if !target.is_atomic() => {
            return Err(error(format!(
                "cannot coerce type '{}' to vector of type '{}'",
                object.kind().name(),
                target.name()
            )));
        }
        _ => {}
    }
    let result = runtime().heap.alloc_vector(target, length)?;
    runtime().heap.root(result);
    let mut lost = false;
    let mut out_of_range = false;
    let mut raw_out_of_range = false;
    for index in 0..count {
        match target {
            SexpType::Logical => {
                let value = match object.kind() {
                    SexpType::Integer | SexpType::Logical => {
                        int_to_logical(int_elt(object, index)?)
                    }
                    SexpType::Real => real_to_logical(real_elt(object, index)?),
                    SexpType::Complex => {
                        let value = altrep::elt::<Rcomplex>(object, index as R_xlen_t)?;
                        if value.r.is_nan() || value.i.is_nan() {
                            NA_LOGICAL
                        } else {
                            (value.r != 0.0 || value.i != 0.0) as i32
                        }
                    }
                    SexpType::String => {
                        let element = sexp_elt(object, index)?;
                        if element == runtime().heap.known().na_string {
                            NA_LOGICAL
                        } else {
                            string_to_logical(element.bytes())
                        }
                    }
                    SexpType::Raw => (altrep::elt::<u8>(object, index as R_xlen_t)? != 0) as i32,
                    _ => NA_LOGICAL,
                };
                *(result.data() as *mut i32).add(index) = value;
            }
            SexpType::Integer => {
                let value = match object.kind() {
                    SexpType::Integer | SexpType::Logical => int_elt(object, index)?,
                    SexpType::Real => {
                        let (value, range) = real_to_int(real_elt(object, index)?);
                        out_of_range |= range;
                        value
                    }
                    SexpType::Complex => {
                        let value = altrep::elt::<Rcomplex>(object, index as R_xlen_t)?;
                        let (converted, range) = real_to_int(value.r);
                        out_of_range |= range;
                        converted
                    }
                    SexpType::String => {
                        let element = sexp_elt(object, index)?;
                        if element == runtime().heap.known().na_string {
                            NA_INTEGER
                        } else {
                            match parse_real(element.bytes()) {
                                Some(value) => {
                                    let (converted, range) = real_to_int(value);
                                    out_of_range |= range;
                                    converted
                                }
                                None => {
                                    lost = true;
                                    NA_INTEGER
                                }
                            }
                        }
                    }
                    SexpType::Raw => altrep::elt::<u8>(object, index as R_xlen_t)? as i32,
                    _ => NA_INTEGER,
                };
                *(result.data() as *mut i32).add(index) = value;
            }
            SexpType::Real => {
                let value = match object.kind() {
                    SexpType::Integer | SexpType::Logical => int_to_real(int_elt(object, index)?),
                    SexpType::Real => real_elt(object, index)?,
                    SexpType::Complex => altrep::elt::<Rcomplex>(object, index as R_xlen_t)?.r,
                    SexpType::String => {
                        let element = sexp_elt(object, index)?;
                        if element == runtime().heap.known().na_string {
                            NA_REAL
                        } else {
                            parse_real(element.bytes()).unwrap_or_else(|| {
                                lost = true;
                                NA_REAL
                            })
                        }
                    }
                    SexpType::Raw => altrep::elt::<u8>(object, index as R_xlen_t)? as f64,
                    _ => NA_REAL,
                };
                *(result.data() as *mut f64).add(index) = value;
            }
            SexpType::Complex => {
                let real = match object.kind() {
                    SexpType::Integer | SexpType::Logical => int_to_real(int_elt(object, index)?),
                    SexpType::Real => real_elt(object, index)?,
                    SexpType::String => {
                        let element = sexp_elt(object, index)?;
                        if element == runtime().heap.known().na_string {
                            NA_REAL
                        } else {
                            parse_real(element.bytes()).unwrap_or_else(|| {
                                lost = true;
                                NA_REAL
                            })
                        }
                    }
                    SexpType::Raw => altrep::elt::<u8>(object, index as R_xlen_t)? as f64,
                    _ => NA_REAL,
                };
                let imaginary = if is_na_real(real) { NA_REAL } else { 0.0 };
                *(result.data() as *mut Rcomplex).add(index) = Rcomplex {
                    r: real,
                    i: imaginary,
                };
            }
            SexpType::String => {
                let element = element_as_charsxp(object, index)?;
                runtime().heap.set_element(result, index, element);
            }
            SexpType::Raw => {
                let value = match object.kind() {
                    SexpType::Integer | SexpType::Logical => int_elt(object, index)?,
                    SexpType::Real => real_to_int(real_elt(object, index)?).0,
                    SexpType::String => {
                        let element = sexp_elt(object, index)?;
                        parse_real(element.bytes()).map_or(NA_INTEGER, |value| real_to_int(value).0)
                    }
                    _ => NA_INTEGER,
                };
                let byte = if (0..=255).contains(&value) {
                    value as u8
                } else {
                    raw_out_of_range = true;
                    0
                };
                *result.data().add(index) = byte;
            }
            _ => {}
        }
    }
    if lost {
        warn("NAs introduced by coercion");
    }
    if out_of_range {
        warn("NAs introduced by coercion to integer range");
    }
    if raw_out_of_range {
        warn("out-of-range values treated as 0 in coercion to raw");
    }
    crate::memory::copy_attributes(result, object, false)?;
    Ok(result)
}

/// Element `index` of an atomic vector as a vector of length one.
unsafe fn element_vector(object: Sexp, index: usize) -> ApiResult<Sexp> {
    let runtime = runtime();
    let kind = object.kind();
    let element = runtime.heap.alloc_vector(kind, 1)?;
    runtime.heap.root(element);
    match kind {
        SexpType::String => runtime
            .heap
            .set_element(element, 0, sexp_elt(object, index)?),
        SexpType::Integer | SexpType::Logical => {
            *(element.data() as *mut i32) = int_elt(object, index)?
        }
        SexpType::Real => *(element.data() as *mut f64) = real_elt(object, index)?,
        SexpType::Complex => {
            *(element.data() as *mut Rcomplex) = altrep::elt(object, index as R_xlen_t)?
        }
        SexpType::Raw => *element.data() = altrep::elt(object, index as R_xlen_t)?,
        _ => return Err(error("cannot take an element of this vector")),
    }
    Ok(element)
}

unsafe fn coerce_list(object: Sexp, target: SexpType) -> ApiResult<Sexp> {
    let length = xlength(object)?;
    match target {
        SexpType::List | SexpType::Expression => {
            let result = crate::memory::duplicate(object, false)?;
            let result = if result.kind() == target {
                result
            } else {
                let converted = runtime().heap.alloc_vector(target, length)?;
                runtime().heap.root(converted);
                for index in 0..length as usize {
                    let element = sexp_elt(object, index)?;
                    runtime().heap.set_element(converted, index, element);
                }
                crate::memory::copy_attributes(converted, object, false)?;
                converted
            };
            return Ok(result);
        }
        SexpType::Pairlist => return crate::lists::vector_to_pairlist(object),
        _ if !target.is_atomic() => {
            return Err(error(format!(
                "(list) object cannot be coerced to type '{}'",
                target.name()
            )));
        }
        _ => {}
    }
    let result = runtime().heap.alloc_vector(target, length)?;
    runtime().heap.root(result);
    for index in 0..length as usize {
        let element = sexp_elt(object, index)?;
        let ok = element.kind().is_atomic() && xlength(element)? == 1;
        if !ok {
            return Err(error(format!(
                "(list) object cannot be coerced to type '{}'",
                target.name()
            )));
        }
        let converted = coerce_vector(element, target)?;
        runtime().heap.root(converted);
        match target {
            SexpType::String => runtime()
                .heap
                .set_element(result, index, converted.element(0)),
            _ => {
                let size = target.element_size().unwrap_or(0);
                std::ptr::copy_nonoverlapping(
                    converted.data(),
                    result.data().add(index * size),
                    size,
                );
            }
        }
    }
    let names = get_attrib(object, R_NamesSymbol.get())?;
    if names != runtime().nil() {
        set_names_from(result, names)?;
    }
    Ok(result)
}

/// `Rf_asCharacterFactor`: the levels a factor's codes select.
pub unsafe fn as_character_factor(factor: Sexp) -> ApiResult<Sexp> {
    if !(factor.kind() == SexpType::Integer && inherits(factor, "factor")?) {
        return Err(error("attempting to coerce non-factor"));
    }
    let levels = get_attrib(factor, R_LevelsSymbol.get())?;
    if levels.kind() != SexpType::String {
        return Err(error("malformed factor"));
    }
    let length = xlength(factor)?;
    let result = runtime().heap.alloc_vector(SexpType::String, length)?;
    runtime().heap.root(result);
    let level_count = levels.length();
    for index in 0..length as usize {
        let code = int_elt(factor, index)?;
        let value = if code == NA_INTEGER {
            runtime().heap.known().na_string
        } else if code >= 1 && code as isize <= level_count {
            levels.element(code as usize - 1)
        } else {
            return Err(error("malformed factor"));
        };
        runtime().heap.set_element(result, index, value);
    }
    Ok(result)
}

pub unsafe fn as_logical(object: Sexp) -> ApiResult<i32> {
    let kind = object.kind();
    if kind == SexpType::Char {
        return Ok(if object == runtime().heap.known().na_string {
            NA_LOGICAL
        } else {
            string_to_logical(object.bytes())
        });
    }
    if !kind.is_atomic() || xlength(object)? < 1 {
        return Ok(NA_LOGICAL);
    }
    Ok(match kind {
        SexpType::Logical => int_elt(object, 0)?,
        SexpType::Integer => int_to_logical(int_elt(object, 0)?),
        SexpType::Real => real_to_logical(real_elt(object, 0)?),
        _ => {
            let converted = coerce_atomic(object, SexpType::Logical)?;
            *(converted.data() as *const i32)
        }
    })
}

pub unsafe fn as_integer(object: Sexp) -> ApiResult<i32> {
    let kind = object.kind();
    if kind == SexpType::Char {
        let string = scalar_string(object)?;
        return as_integer(string);
    }
    if !kind.is_atomic() || xlength(object)? < 1 {
        return Ok(NA_INTEGER);
    }
    Ok(match kind {
        SexpType::Integer | SexpType::Logical => int_elt(object, 0)?,
        SexpType::Real => {
            let (value, range) = real_to_int(real_elt(object, 0)?);
            if range {
                warn("NAs introduced by coercion to integer range");
            }
            value
        }
        _ => {
            let first = element_vector(object, 0)?;
            let converted = coerce_atomic(first, SexpType::Integer)?;
            *(converted.data() as *const i32)
        }
    })
}

pub unsafe fn as_real(object: Sexp) -> ApiResult<f64> {
    let kind = object.kind();
    if kind == SexpType::Char {
        let string = scalar_string(object)?;
        return as_real(string);
    }
    if !kind.is_atomic() || xlength(object)? < 1 {
        return Ok(NA_REAL);
    }
    Ok(match kind {
        SexpType::Integer | SexpType::Logical => int_to_real(int_elt(object, 0)?),
        SexpType::Real => real_elt(object, 0)?,
        _ => {
            let first = element_vector(object, 0)?;
            let converted = coerce_atomic(first, SexpType::Real)?;
            *(converted.data() as *const f64)
        }
    })
}

pub unsafe fn as_char(object: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let kind = object.kind();
    match kind {
        SexpType::Char => Ok(object),
        SexpType::Symbol => Ok(object.car()),
        _ if kind.is_atomic() && xlength(object)? >= 1 => element_as_charsxp(object, 0),
        _ => Ok(runtime.heap.known().na_string),
    }
}

/// `Rf_inherits` without the C string.
pub unsafe fn class_names(object: Sexp) -> ApiResult<Vec<Vec<u8>>> {
    if !object.is_object() {
        return Ok(Vec::new());
    }
    let class = get_attrib(object, R_ClassSymbol.get())?;
    if class.kind() != SexpType::String {
        return Ok(Vec::new());
    }
    Ok((0..class.length() as usize)
        .map(|index| class.element(index).bytes().to_vec())
        .collect())
}

unsafe fn dims(object: Sexp) -> ApiResult<Option<Vec<i32>>> {
    let dim = get_attrib(object, R_DimSymbol.get())?;
    if dim.kind() == SexpType::Integer {
        Ok(Some(integers(dim)?))
    } else {
        Ok(None)
    }
}

pub unsafe fn identical(left: Sexp, right: Sexp, flags: c_int) -> ApiResult<bool> {
    if left == right {
        return Ok(true);
    }
    let num_eq = flags & 1 == 0;
    let single_na = flags & 2 == 0;
    let attributes_as_set = flags & 4 == 0;
    if left.kind() != right.kind() || left.is_object() != right.is_object() {
        return Ok(false);
    }
    if !identical_attributes(left.attrib(), right.attrib(), flags, attributes_as_set)? {
        return Ok(false);
    }
    let kind = left.kind();
    if kind.is_vector() || left.is_altrep() || right.is_altrep() {
        let length = xlength(left)?;
        if length != xlength(right)? {
            return Ok(false);
        }
        for index in 0..length as usize {
            let same = match kind {
                SexpType::Logical | SexpType::Integer => {
                    int_elt(left, index)? == int_elt(right, index)?
                }
                SexpType::Real => same_real(
                    real_elt(left, index)?,
                    real_elt(right, index)?,
                    num_eq,
                    single_na,
                ),
                SexpType::Complex => {
                    let a = altrep::elt::<Rcomplex>(left, index as R_xlen_t)?;
                    let b = altrep::elt::<Rcomplex>(right, index as R_xlen_t)?;
                    same_real(a.r, b.r, num_eq, single_na) && same_real(a.i, b.i, num_eq, single_na)
                }
                SexpType::Raw => {
                    altrep::elt::<u8>(left, index as R_xlen_t)?
                        == altrep::elt::<u8>(right, index as R_xlen_t)?
                }
                SexpType::Char => {
                    *data_pointer(left, false)?.add(index)
                        == *data_pointer(right, false)?.add(index)
                }
                SexpType::String => {
                    let (a, b) = (sexp_elt(left, index)?, sexp_elt(right, index)?);
                    let na = runtime().heap.known().na_string;
                    a == b || (a != na && b != na && utf8_text(a)? == utf8_text(b)?)
                }
                _ => identical(sexp_elt(left, index)?, sexp_elt(right, index)?, flags)?,
            };
            if !same {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    match kind {
        SexpType::Pairlist
        | SexpType::Language
        | SexpType::Dots
        | SexpType::Closure
        | SexpType::Promise => Ok(identical(left.car(), right.car(), flags)?
            && identical(left.cdr(), right.cdr(), flags)?
            && identical(left.tag(), right.tag(), flags)?),
        SexpType::ExternalPtr => Ok(left.external_address() == right.external_address()),
        SexpType::Builtin | SexpType::Special => {
            Ok(left.primitive_index() == right.primitive_index())
        }
        SexpType::Object => identical(left.tag(), right.tag(), flags),
        _ => Ok(false),
    }
}

fn same_real(left: f64, right: f64, num_eq: bool, single_na: bool) -> bool {
    if left.is_nan() || right.is_nan() {
        if !(left.is_nan() && right.is_nan()) {
            return false;
        }
        return !single_na || is_na_real(left) == is_na_real(right);
    }
    if num_eq {
        left == right
    } else {
        left.to_bits() == right.to_bits()
    }
}

unsafe fn identical_attributes(
    left: Sexp,
    right: Sexp,
    flags: c_int,
    as_set: bool,
) -> ApiResult<bool> {
    let collect = |list: Sexp| -> Vec<(Sexp, Sexp)> {
        let mut pairs = Vec::new();
        let mut cell = list;
        while cell.kind() == SexpType::Pairlist {
            pairs.push((cell.tag(), cell.car()));
            cell = cell.cdr();
        }
        pairs
    };
    let (left, right) = (collect(left), collect(right));
    if left.len() != right.len() {
        return Ok(false);
    }
    for (position, (tag, value)) in left.iter().enumerate() {
        let other = if as_set {
            right.iter().find(|(other_tag, _)| other_tag == tag)
        } else {
            right
                .get(position)
                .filter(|(other_tag, _)| other_tag == tag)
        };
        match other {
            Some((_, other_value)) if identical(*value, *other_value, flags)? => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

// The exported API.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TYPEOF(object: Sexp) -> c_int {
    object.kind() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn LENGTH(object: Sexp) -> c_int {
    if let Some(length) = plain_length(object)
        && let Ok(length) = c_int::try_from(length)
    {
        return length;
    }
    enter(|| {
        let length = xlength(object)?;
        if length > i32::MAX as isize {
            return Err(error("long vectors not supported yet"));
        }
        Ok(length as c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn XLENGTH(object: Sexp) -> R_xlen_t {
    if let Some(length) = plain_length(object) {
        return length;
    }
    enter(|| xlength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_length(object: Sexp) -> R_len_t {
    if let Some(length) = plain_length(object)
        && let Ok(length) = R_len_t::try_from(length)
    {
        return length;
    }
    enter(|| {
        let length = generic_length(object)?;
        if length > i32::MAX as isize {
            return Err(error("long vectors not supported yet"));
        }
        Ok(length as R_len_t)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_xlength(object: Sexp) -> R_xlen_t {
    if let Some(length) = plain_length(object) {
        return length;
    }
    enter(|| generic_length(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn LOGICAL(object: Sexp) -> *mut c_int {
    if is_plain(object, SexpType::Logical) {
        return object.data() as *mut c_int;
    }
    enter(|| {
        Ok(typed_data(object, &[SexpType::Logical], "LOGICAL", "logical", true)? as *mut c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn INTEGER(object: Sexp) -> *mut c_int {
    if is_plain_integer(object) {
        return object.data() as *mut c_int;
    }
    enter(|| Ok(typed_data(object, INTEGER_TYPES, "INTEGER", "integer", true)? as *mut c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RAW(object: Sexp) -> *mut u8 {
    if is_plain(object, SexpType::Raw) {
        return object.data();
    }
    enter(|| typed_data(object, &[SexpType::Raw], "RAW", "raw", true))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn REAL(object: Sexp) -> *mut f64 {
    if is_plain(object, SexpType::Real) {
        return object.data() as *mut f64;
    }
    enter(|| Ok(typed_data(object, &[SexpType::Real], "REAL", "numeric", true)? as *mut f64))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn COMPLEX(object: Sexp) -> *mut Rcomplex {
    if is_plain(object, SexpType::Complex) {
        return object.data() as *mut Rcomplex;
    }
    enter(|| {
        Ok(typed_data(object, &[SexpType::Complex], "COMPLEX", "complex", true)? as *mut Rcomplex)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn LOGICAL_RO(object: Sexp) -> *const c_int {
    if is_plain(object, SexpType::Logical) {
        return object.data() as *const c_int;
    }
    enter(|| {
        Ok(typed_data(object, &[SexpType::Logical], "LOGICAL", "logical", false)? as *const c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn INTEGER_RO(object: Sexp) -> *const c_int {
    if is_plain_integer(object) {
        return object.data() as *const c_int;
    }
    enter(|| Ok(typed_data(object, INTEGER_TYPES, "INTEGER", "integer", false)? as *const c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RAW_RO(object: Sexp) -> *const u8 {
    if is_plain(object, SexpType::Raw) {
        return object.data();
    }
    enter(|| Ok(typed_data(object, &[SexpType::Raw], "RAW", "raw", false)? as *const u8))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn REAL_RO(object: Sexp) -> *const f64 {
    if is_plain(object, SexpType::Real) {
        return object.data() as *const f64;
    }
    enter(|| Ok(typed_data(object, &[SexpType::Real], "REAL", "numeric", false)? as *const f64))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn COMPLEX_RO(object: Sexp) -> *const Rcomplex {
    if is_plain(object, SexpType::Complex) {
        return object.data() as *const Rcomplex;
    }
    enter(|| {
        Ok(
            typed_data(object, &[SexpType::Complex], "COMPLEX", "complex", false)?
                as *const Rcomplex,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn DATAPTR(object: Sexp) -> *mut c_void {
    // A writable pointer into a list or character vector makes the vector escape, which only the
    // boundary does, so only atomic vectors skip it.
    if object.kind().is_atomic() && object.kind() != SexpType::String && !object.is_altrep() {
        return object.data() as *mut c_void;
    }
    enter(|| Ok(data_pointer(object, true)? as *mut c_void))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn DATAPTR_RO(object: Sexp) -> *const c_void {
    if object.kind().is_vector() && !object.is_altrep() {
        return object.data() as *const c_void;
    }
    enter(|| Ok(data_pointer(object, false)? as *const c_void))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn DATAPTR_OR_NULL(object: Sexp) -> *const c_void {
    enter(|| {
        if object.is_altrep() {
            return altrep::dataptr_or_null(object);
        }
        Ok(data_pointer(object, false)? as *const c_void)
    })
}

/// `DATAPTR_RW`, which GNU R declares for implementing `Dataptr` methods.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DATAPTR_RW(object: Sexp) -> *mut c_void {
    enter(|| Ok(data_pointer(object, true)? as *mut c_void))
}

/// Binaries built against older headers call this after checking `ALTREP(x)`; an ALTREP
/// object still dispatches, so a stale check cannot read the instance's triple as data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn STDVEC_DATAPTR(object: Sexp) -> *mut c_void {
    enter(|| Ok(data_pointer(object, true)? as *mut c_void))
}

/// GNU R refuses this for safety; ry hands out the writable pointer, which the collector
/// tolerates because the vector joins the escaped set, so binaries that import it keep working.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VECTOR_PTR(object: Sexp) -> *mut Sexp {
    enter(|| {
        Ok(typed_data(
            object,
            &[SexpType::List, SexpType::Expression],
            "VECTOR_PTR",
            "list",
            true,
        )? as *mut Sexp)
    })
}

macro_rules! or_null_api {
    ($name:ident, $type:ty, $accepted:expr, $accessor:literal, $wanted:literal) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp) -> *const $type {
            enter(|| {
                check_type(object, &$accepted, $accessor, $wanted)?;
                if object.is_altrep() {
                    return Ok(altrep::dataptr_or_null(object)? as *const $type);
                }
                Ok(object.data() as *const $type)
            })
        }
    };
}

or_null_api!(
    LOGICAL_OR_NULL,
    c_int,
    [SexpType::Logical],
    "LOGICAL",
    "logical"
);
or_null_api!(INTEGER_OR_NULL, c_int, INTEGER_TYPES, "INTEGER", "integer");
or_null_api!(REAL_OR_NULL, f64, [SexpType::Real], "REAL", "numeric");
or_null_api!(
    COMPLEX_OR_NULL,
    Rcomplex,
    [SexpType::Complex],
    "COMPLEX",
    "complex"
);
or_null_api!(RAW_OR_NULL, u8, [SexpType::Raw], "RAW", "raw");

#[unsafe(no_mangle)]
pub unsafe extern "C" fn STRING_ELT(object: Sexp, index: R_xlen_t) -> Sexp {
    // String kernels read every element, so a plain in-bounds read skips the boundary. The
    // element needs no handle: it stays reachable from `object` as long as GNU R's would.
    if object.kind() == SexpType::String
        && !object.is_altrep()
        && (0..object.length()).contains(&index)
    {
        return object.element(index as usize);
    }
    enter(|| {
        refuse_native(object)?;
        if object.kind() != SexpType::String {
            return Err(error(format!(
                "STRING_ELT() can only be applied to a 'character vector', not a '{}'",
                object.kind().name()
            )));
        }
        sexp_elt(object, checked_index(object, index, "STRING_ELT")?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn VECTOR_ELT(object: Sexp, index: R_xlen_t) -> Sexp {
    if matches!(object.kind(), SexpType::List | SexpType::Expression)
        && !object.is_altrep()
        && (0..object.length()).contains(&index)
    {
        return object.element(index as usize);
    }
    enter(|| {
        refuse_native(object)?;
        if !matches!(object.kind(), SexpType::List | SexpType::Expression) {
            return Err(error(format!(
                "VECTOR_ELT() can only be applied to a 'list', not a '{}'",
                object.kind().name()
            )));
        }
        // GNU R checks only the setter's index. data.table reads the over-allocated column
        // slots of a table past its length, which hold `NULL` here because growing a list
        // fills its new slots with `NULL`.
        if !object.is_altrep() && index >= object.length() && index < object.capacity() {
            return Ok(runtime().nil());
        }
        sexp_elt(object, checked_index(object, index, "VECTOR_ELT")?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_STRING_ELT(object: Sexp, index: R_xlen_t, value: Sexp) {
    if object.kind() == SexpType::String
        && value.kind() == SexpType::Char
        && !object.is_altrep()
        && (0..object.length()).contains(&index)
    {
        runtime().heap.set_element(object, index as usize, value);
        return;
    }
    enter(|| {
        refuse_native(object)?;
        if object.kind() != SexpType::String {
            return Err(error(format!(
                "SET_STRING_ELT() can only be applied to a 'character vector', not a '{}'",
                object.kind().name()
            )));
        }
        if value.kind() != SexpType::Char {
            return Err(error(format!(
                "Value of SET_STRING_ELT() must be a 'CHARSXP' not a '{}'",
                value.kind().name()
            )));
        }
        let length = xlength(object)?;
        if index < 0 || index >= length {
            return Err(error(format!(
                "attempt to set index {index}/{length} in SET_STRING_ELT"
            )));
        }
        set_sexp_elt(object, index as usize, value)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_VECTOR_ELT(object: Sexp, index: R_xlen_t, value: Sexp) -> Sexp {
    if matches!(object.kind(), SexpType::List | SexpType::Expression)
        && !object.is_altrep()
        && (0..object.length()).contains(&index)
    {
        runtime().heap.set_element(object, index as usize, value);
        return value;
    }
    enter(|| {
        refuse_native(object)?;
        if !matches!(object.kind(), SexpType::List | SexpType::Expression) {
            return Err(error(format!(
                "SET_VECTOR_ELT() can only be applied to a 'list', not a '{}'",
                object.kind().name()
            )));
        }
        let length = xlength(object)?;
        if index < 0 || index >= length {
            return Err(error(format!(
                "attempt to set index {index}/{length} in SET_VECTOR_ELT"
            )));
        }
        set_sexp_elt(object, index as usize, value)?;
        Ok(value)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn STRING_PTR(object: Sexp) -> *mut Sexp {
    enter(|| {
        Ok(typed_data(object, &[SexpType::String], "STRING_PTR", "character", true)? as *mut Sexp)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn STRING_PTR_RO(object: Sexp) -> *const Sexp {
    if object.kind() == SexpType::String && !object.is_altrep() {
        return object.data() as *const Sexp;
    }
    enter(|| {
        Ok(typed_data(
            object,
            &[SexpType::String],
            "STRING_PTR_RO",
            "character",
            false,
        )? as *const Sexp)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn VECTOR_PTR_RO(object: Sexp) -> *const Sexp {
    enter(|| {
        Ok(typed_data(
            object,
            &[SexpType::List, SexpType::Expression],
            "VECTOR_PTR_RO",
            "list",
            false,
        )? as *const Sexp)
    })
}

macro_rules! element_api {
    ($get:ident, $set:ident, $type:ty, $accepted:expr, $wanted:literal) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get(object: Sexp, index: R_xlen_t) -> $type {
            if $accepted.contains(&object.kind())
                && !object.is_altrep()
                && (0..object.length()).contains(&index)
            {
                return *(object.data() as *const $type).add(index as usize);
            }
            enter(|| {
                if !$accepted.contains(&object.kind()) {
                    return Err(error(format!(
                        concat!(
                            stringify!($get),
                            "() can only be applied to a '",
                            $wanted,
                            "', not a '{}'"
                        ),
                        object.kind().name()
                    )));
                }
                let index = checked_index(object, index, stringify!($get))?;
                if object.is_altrep() {
                    return altrep::elt::<$type>(object, index as R_xlen_t);
                }
                Ok(*(object.data() as *const $type).add(index))
            })
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set(object: Sexp, index: R_xlen_t, value: $type) {
            enter(|| {
                if !$accepted.contains(&object.kind()) {
                    return Err(error(format!(
                        concat!(
                            stringify!($set),
                            "() can only be applied to a '",
                            $wanted,
                            "', not a '{}'"
                        ),
                        object.kind().name()
                    )));
                }
                let index = checked_index(object, index, stringify!($set))?;
                *(data_pointer(object, true)? as *mut $type).add(index) = value;
                Ok(())
            })
        }
    };
}

element_api!(
    INTEGER_ELT,
    SET_INTEGER_ELT,
    c_int,
    INTEGER_TYPES,
    "integer"
);
element_api!(
    LOGICAL_ELT,
    SET_LOGICAL_ELT,
    c_int,
    [SexpType::Logical],
    "logical"
);
element_api!(REAL_ELT, SET_REAL_ELT, f64, [SexpType::Real], "numeric");
element_api!(
    COMPLEX_ELT,
    SET_COMPLEX_ELT,
    Rcomplex,
    [SexpType::Complex],
    "complex"
);
element_api!(RAW_ELT, SET_RAW_ELT, u8, [SexpType::Raw], "raw");

macro_rules! region_api {
    ($name:ident, $type:ty, $accepted:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            object: Sexp,
            start: R_xlen_t,
            count: R_xlen_t,
            buffer: *mut $type,
        ) -> R_xlen_t {
            enter(|| {
                if !$accepted.contains(&object.kind()) {
                    return Err(error(concat!(
                        stringify!($name),
                        " applied to the wrong type"
                    )));
                }
                altrep::get_region::<$type>(object, start, count, buffer)
            })
        }
    };
}

region_api!(INTEGER_GET_REGION, c_int, INTEGER_TYPES);
region_api!(LOGICAL_GET_REGION, c_int, [SexpType::Logical]);
region_api!(REAL_GET_REGION, f64, [SexpType::Real]);
region_api!(COMPLEX_GET_REGION, Rcomplex, [SexpType::Complex]);
region_api!(RAW_GET_REGION, u8, [SexpType::Raw]);

macro_rules! sortedness_api {
    ($sorted:ident, $no_na:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $sorted(object: Sexp) -> c_int {
            enter(|| altrep::is_sorted(object))
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $no_na(object: Sexp) -> c_int {
            enter(|| altrep::no_na(object))
        }
    };
}

sortedness_api!(INTEGER_IS_SORTED, INTEGER_NO_NA);
sortedness_api!(REAL_IS_SORTED, REAL_NO_NA);
sortedness_api!(LOGICAL_IS_SORTED, LOGICAL_NO_NA);
sortedness_api!(STRING_IS_SORTED, STRING_NO_NA);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_nrows(object: Sexp) -> c_int {
    enter(|| {
        if object.kind().is_vector() || object.is_altrep() {
            if let Some(dims) = dims(object)? {
                return Ok(dims.first().copied().unwrap_or(0));
            }
            return Ok(xlength(object)? as c_int);
        }
        Err(error("object is not a matrix"))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ncols(object: Sexp) -> c_int {
    enter(|| {
        if object.kind().is_vector() || object.is_altrep() {
            return Ok(match dims(object)? {
                Some(dims) if dims.len() >= 2 => dims[1],
                _ => 1,
            });
        }
        Err(error("object is not a matrix"))
    })
}

macro_rules! predicate {
    ($name:ident, |$object:ident| $body:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($object: Sexp) -> Rboolean {
            // C code tests every element it walks, and a predicate only reads, so it runs outside
            // the boundary and enters it again only to raise its error.
            if let Ok(answer) = (|| -> ApiResult<bool> { Ok($body) })() {
                return answer as Rboolean;
            }
            enter(|| Ok(($body) as Rboolean))
        }
    };
}

predicate!(Rf_isNull, |object| object.kind() == SexpType::Nil);
predicate!(Rf_isSymbol, |object| object.kind() == SexpType::Symbol);
predicate!(Rf_isLogical, |object| object.kind() == SexpType::Logical);
predicate!(Rf_isReal, |object| object.kind() == SexpType::Real);
predicate!(Rf_isComplex, |object| object.kind() == SexpType::Complex);
predicate!(Rf_isExpression, |object| object.kind()
    == SexpType::Expression);
predicate!(Rf_isEnvironment, |object| object.kind()
    == SexpType::Environment);
predicate!(Rf_isString, |object| object.kind() == SexpType::String);
predicate!(Rf_isObject, |object| object.is_object());
predicate!(Rf_isInteger, |object| object.kind() == SexpType::Integer
    && !inherits(object, "factor")?);
predicate!(Rf_isFactor, |object| object.kind() == SexpType::Integer
    && inherits(object, "factor")?);
predicate!(Rf_isOrdered, |object| object.kind() == SexpType::Integer
    && inherits(object, "factor")?
    && inherits(object, "ordered")?);
predicate!(Rf_isUnordered, |object| object.kind() == SexpType::Integer
    && inherits(object, "factor")?
    && !inherits(object, "ordered")?);
predicate!(Rf_isNumeric, |object| match object.kind() {
    SexpType::Integer => !inherits(object, "factor")?,
    SexpType::Logical | SexpType::Real => true,
    _ => false,
});
predicate!(Rf_isNumber, |object| match object.kind() {
    SexpType::Integer => !inherits(object, "factor")?,
    SexpType::Logical | SexpType::Real | SexpType::Complex => true,
    _ => false,
});
predicate!(Rf_isVector, |object| object.kind().is_vector()
    && object.kind() != SexpType::Char);
predicate!(Rf_isVectorAtomic, |object| object.kind().is_atomic());
predicate!(Rf_isVectorList, |object| matches!(
    object.kind(),
    SexpType::List | SexpType::Expression
));
predicate!(Rf_isList, |object| matches!(
    object.kind(),
    SexpType::Nil | SexpType::Pairlist
));
predicate!(Rf_isNewList, |object| matches!(
    object.kind(),
    SexpType::Nil | SexpType::List
));
predicate!(Rf_isPairList, |object| matches!(
    object.kind(),
    SexpType::Nil | SexpType::Pairlist | SexpType::Language | SexpType::Dots
));
predicate!(Rf_isLanguage, |object| matches!(
    object.kind(),
    SexpType::Nil | SexpType::Language
));
predicate!(Rf_isMatrix, |object| object.kind().is_vector()
    && dims(object)?.is_some_and(|dims| dims.len() == 2));
predicate!(Rf_isArray, |object| object.kind().is_vector()
    && dims(object)?.is_some_and(|dims| !dims.is_empty()));
predicate!(Rf_isTs, |object| object.kind().is_vector()
    && get_attrib(object, R_TspSymbol.get())? != runtime().nil());
predicate!(Rf_isFunction, |object| matches!(
    object.kind(),
    SexpType::Closure | SexpType::Builtin | SexpType::Special
));
predicate!(Rf_isPrimitive, |object| matches!(
    object.kind(),
    SexpType::Builtin | SexpType::Special
));
predicate!(Rf_isFrame, |object| inherits(object, "data.frame")?);
predicate!(Rf_isDataFrame, |object| inherits(object, "data.frame")?);
predicate!(Rf_isS4, |object| object.has_gp(heap::object::gp::S4_OBJECT));
predicate!(Rf_isValidString, |object| object.kind() == SexpType::String
    && xlength(object)? > 0
    && sexp_elt(object, 0)?.kind() != SexpType::Nil);
predicate!(Rf_isValidStringF, |object| object.kind()
    == SexpType::String
    && xlength(object)? > 0
    && !sexp_elt(object, 0)?.bytes().is_empty());
predicate!(Rf_StringBlank, |object| object == runtime().nil()
    || object.bytes().is_empty());

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_coerceVector(object: Sexp, kind: SEXPTYPE) -> Sexp {
    enter(|| coerce_vector(object, sexp_type(kind)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asCharacterFactor(object: Sexp) -> Sexp {
    enter(|| as_character_factor(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asLogical(object: Sexp) -> c_int {
    enter(|| as_logical(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asLogical2(object: Sexp, checking: c_int, call: Sexp) -> c_int {
    enter(|| {
        if checking != 0 && object.kind().is_atomic() {
            let length = xlength(object)?;
            if length > 1 {
                return Err(crate::boundary::error_in(
                    call,
                    format!("'length = {length}' in coercion to 'logical(1)'"),
                ));
            }
        }
        as_logical(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asInteger(object: Sexp) -> c_int {
    enter(|| as_integer(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asReal(object: Sexp) -> f64 {
    enter(|| as_real(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asChar(object: Sexp) -> Sexp {
    enter(|| as_char(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asXLength(object: Sexp) -> R_xlen_t {
    enter(|| {
        let kind = object.kind();
        if !kind.is_atomic() || xlength(object)? < 1 {
            return Ok(-999);
        }
        Ok(match kind {
            SexpType::Integer | SexpType::Logical => match int_elt(object, 0)? {
                value if value == NA_INTEGER || value < 0 => -999,
                value => value as R_xlen_t,
            },
            _ => {
                let value = as_real(object)?;
                if !value.is_finite()
                    || value < 0.0
                    || value > heap::MAX_VECTOR_LENGTH as f64
                    || value != value.floor()
                {
                    -999
                } else {
                    value as R_xlen_t
                }
            }
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_type2char(kind: SEXPTYPE) -> *const c_char {
    match SexpType::from_code(kind) {
        Some(kind) => crate::strings::type_c_name(kind).as_ptr(),
        None => c"unknown".as_ptr(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_type2str(kind: SEXPTYPE) -> Sexp {
    enter(|| {
        let name = SexpType::from_code(kind).map_or("unknown", SexpType::name);
        Ok(runtime().heap.mk_char(name.as_bytes(), Encoding::Native)?)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_type2str_nowarn(kind: SEXPTYPE) -> Sexp {
    Rf_type2str(kind)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_str2type(name: *const c_char) -> SEXPTYPE {
    let name = c_bytes(name);
    if name == b"numeric" {
        return SexpType::Real.code();
    }
    if name == b"name" {
        return SexpType::Symbol.code();
    }
    (0..=25)
        .filter_map(SexpType::from_code)
        .find(|kind| kind.name().as_bytes() == name)
        .map_or(u32::MAX, SexpType::code)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_compute_identical(left: Sexp, right: Sexp, flags: c_int) -> Rboolean {
    enter(|| Ok(identical(left, right, flags)? as Rboolean))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_atof(text: *const c_char) -> f64 {
    parse_real(c_bytes(text)).unwrap_or(0.0)
}

/// `R_strtod`: the longest prefix of `text` that reads as a number.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_strtod(text: *const c_char, end: *mut *mut c_char) -> f64 {
    let bytes = c_bytes(text);
    let start = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    for stop in (start + 1..=bytes.len()).rev() {
        if let Some(value) = parse_real(&bytes[start..stop]) {
            if !end.is_null() {
                *end = text.add(stop) as *mut c_char;
            }
            return value;
        }
    }
    if !end.is_null() {
        *end = text as *mut c_char;
    }
    0.0
}

/// `Rf_match(table, x, nomatch)`: for each element of `x`, the 1-based position of its first
/// match in `table`, compared as strings when either side is character and as numbers
/// otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_match(table: Sexp, x: Sexp, no_match: c_int) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let length = if x.kind() == SexpType::Nil {
            0
        } else {
            xlength(x)?
        };
        let result = runtime.heap.alloc_vector(SexpType::Integer, length)?;
        runtime.heap.root(result);
        let table_length = if table.kind() == SexpType::Nil {
            0
        } else {
            xlength(table)?
        };
        let textual = x.kind() == SexpType::String || table.kind() == SexpType::String;
        if textual {
            let table = coerce_vector(table, SexpType::String)?;
            runtime.heap.root(table);
            let x = coerce_vector(x, SexpType::String)?;
            runtime.heap.root(x);
            let mut positions = rustc_hash::FxHashMap::default();
            for index in (0..table_length as usize).rev() {
                positions.insert(
                    utf8_text(sexp_elt(table, index)?)?.into_owned(),
                    index as i32 + 1,
                );
            }
            for index in 0..length as usize {
                let key = utf8_text(sexp_elt(x, index)?)?.into_owned();
                *(result.data() as *mut i32).add(index) =
                    positions.get(&key).copied().unwrap_or(no_match);
            }
        } else {
            let table = coerce_vector(table, SexpType::Real)?;
            runtime.heap.root(table);
            let x = coerce_vector(x, SexpType::Real)?;
            runtime.heap.root(x);
            let mut positions = rustc_hash::FxHashMap::default();
            for index in (0..table_length as usize).rev() {
                let value = real_elt(table, index)?;
                let key = if value == 0.0 {
                    0
                } else if is_na_real(value) {
                    1
                } else if value.is_nan() {
                    2
                } else {
                    value.to_bits()
                };
                positions.insert(key, index as i32 + 1);
            }
            for index in 0..length as usize {
                let value = real_elt(x, index)?;
                let key = if value == 0.0 {
                    0
                } else if is_na_real(value) {
                    1
                } else if value.is_nan() {
                    2
                } else {
                    value.to_bits()
                };
                *(result.data() as *mut i32).add(index) =
                    positions.get(&key).copied().unwrap_or(no_match);
            }
        }
        Ok(result)
    })
}

/// One element of an atomic vector as a hashable value with R's equality: `NA` equals `NA`,
/// `NaN` equals `NaN` but not `NA`, `-0` equals `0`, and strings compare by their text.
#[derive(Hash, PartialEq, Eq)]
enum ElementKey {
    Integer(i32),
    Real(u64),
    Complex(u64, u64),
    String(Option<Vec<u8>>),
    Raw(u8),
    /// Lists compare elementwise with `identical`, so their key is only a bucket.
    Bucket(SexpType, i64),
}

fn real_key(value: f64) -> u64 {
    if is_na_real(value) {
        NA_REAL.to_bits()
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

unsafe fn element_key(object: Sexp, index: usize) -> ApiResult<ElementKey> {
    Ok(match object.kind() {
        SexpType::Logical | SexpType::Integer => ElementKey::Integer(int_elt(object, index)?),
        SexpType::Real => ElementKey::Real(real_key(real_elt(object, index)?)),
        SexpType::Complex => {
            let value = altrep::elt::<Rcomplex>(object, index as R_xlen_t)?;
            ElementKey::Complex(real_key(value.r), real_key(value.i))
        }
        SexpType::String => {
            let element = sexp_elt(object, index)?;
            if element == runtime().heap.known().na_string {
                ElementKey::String(None)
            } else {
                ElementKey::String(Some(utf8_text(element)?.into_owned().into_bytes()))
            }
        }
        SexpType::Raw => ElementKey::Raw(altrep::elt::<u8>(object, index as R_xlen_t)?),
        SexpType::List | SexpType::Expression => {
            let element = sexp_elt(object, index)?;
            let length = if element.kind().is_vector() {
                xlength(element)? as i64
            } else {
                -1
            };
            ElementKey::Bucket(element.kind(), length)
        }
        kind => {
            return Err(error(format!("{} is not a vector type", kind.name())));
        }
    })
}

/// For each element (scanning from the end with `from_last`), whether an equal element came
/// before it; `incomparables` are never duplicates.
unsafe fn duplicated_flags(
    object: Sexp,
    incomparables: Option<Sexp>,
    from_last: bool,
    stop_at_first: bool,
) -> ApiResult<Vec<bool>> {
    let length = if object.kind() == SexpType::Nil {
        0
    } else {
        xlength(object)? as usize
    };
    let mut flags = vec![false; length];
    let mut seen: rustc_hash::FxHashMap<ElementKey, Vec<usize>> = rustc_hash::FxHashMap::default();
    let mut excluded = Vec::new();
    if let Some(incomparables) = incomparables
        && incomparables.kind() != SexpType::Nil
        && !(incomparables.kind() == SexpType::Logical
            && xlength(incomparables)? == 1
            && int_elt(incomparables, 0)? == 0)
    {
        let incomparables = coerce_vector(incomparables, object.kind())?;
        runtime().heap.root(incomparables);
        for index in 0..xlength(incomparables)? as usize {
            excluded.push(element_key(incomparables, index)?);
        }
    }
    let list = matches!(object.kind(), SexpType::List | SexpType::Expression);
    let order: Box<dyn Iterator<Item = usize>> = if from_last {
        Box::new((0..length).rev())
    } else {
        Box::new(0..length)
    };
    for index in order {
        let key = element_key(object, index)?;
        if excluded.contains(&key) {
            continue;
        }
        let earlier = seen.entry(key).or_default();
        let duplicate = if list {
            // An ALTREP list's `Elt` may allocate each element afresh, so the element compared
            // against is rooted while the next one is fetched.
            let mark = runtime().heap.handle_mark();
            let element = sexp_elt(object, index)?;
            runtime().heap.root(element);
            let mut found = false;
            for &other in earlier.iter() {
                if identical(element, sexp_elt(object, other)?, 0)? {
                    found = true;
                    break;
                }
            }
            runtime().heap.release_handles(mark);
            found
        } else {
            !earlier.is_empty()
        };
        if duplicate {
            flags[index] = true;
            if stop_at_first {
                break;
            }
        } else if list || earlier.is_empty() {
            earlier.push(index);
        }
    }
    Ok(flags)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_duplicated(object: Sexp, from_last: Rboolean) -> Sexp {
    enter(|| {
        let flags = duplicated_flags(object, None, from_last != 0, false)?;
        let runtime = runtime();
        let result = runtime
            .heap
            .alloc_vector(SexpType::Logical, flags.len() as isize)?;
        let values = std::slice::from_raw_parts_mut(result.data() as *mut i32, flags.len());
        for (value, flag) in values.iter_mut().zip(flags) {
            *value = flag as i32;
        }
        Ok(result)
    })
}

unsafe fn first_duplicate(
    object: Sexp,
    incomparables: Option<Sexp>,
    from_last: bool,
) -> ApiResult<R_xlen_t> {
    let flags = duplicated_flags(object, incomparables, from_last, true)?;
    Ok(flags
        .iter()
        .position(|&flag| flag)
        .map_or(0, |index| index as R_xlen_t + 1))
}

/// `Rf_any_duplicated(x, from_last)`: the 1-based position of the first duplicated element, or
/// 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_any_duplicated(object: Sexp, from_last: Rboolean) -> R_xlen_t {
    enter(|| first_duplicate(object, None, from_last != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_any_duplicated3(
    object: Sexp,
    incomparables: Sexp,
    from_last: Rboolean,
) -> R_xlen_t {
    enter(|| first_duplicate(object, Some(incomparables), from_last != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asComplex(object: Sexp) -> Rcomplex {
    enter(|| {
        let na = Rcomplex {
            r: NA_REAL,
            i: NA_REAL,
        };
        if !object.kind().is_atomic() || xlength(object)? < 1 {
            if object.kind() == SexpType::Char {
                let value = parse_real(object.bytes()).unwrap_or(NA_REAL);
                return Ok(Rcomplex { r: value, i: 0.0 });
            }
            return Ok(na);
        }
        let first = coerce_vector(object, SexpType::Complex)?;
        Ok(altrep::elt::<Rcomplex>(first, 0)?)
    })
}

/// `Rf_asRboolean(x)`: `x` as a single `TRUE` or `FALSE`; `NA` and an empty or non-logical
/// value are errors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asRboolean(object: Sexp) -> Rboolean {
    enter(|| as_rboolean(object))
}

unsafe fn as_rboolean(object: Sexp) -> ApiResult<Rboolean> {
    let value = as_logical(object)?;
    if value == NA_LOGICAL {
        return Err(error("NA in coercion to boolean"));
    }
    Ok((value != 0) as Rboolean)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asBool(object: Sexp) -> bool {
    enter(|| Ok(as_rboolean(object)? != 0))
}

/// `R_isTRUE(x)`: whether `x` is a length-one logical `TRUE` without attributes other than
/// names, as `isTRUE()` asks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isTRUE(object: Sexp) -> Rboolean {
    enter(|| {
        Ok(
            (object.kind() == SexpType::Logical
                && xlength(object)? == 1
                && int_elt(object, 0)? == 1) as Rboolean,
        )
    })
}

/// `Rf_copyVector(target, source)`: fills `target` with the elements of `source`, recycled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_copyVector(target: Sexp, source: Sexp) {
    enter(|| {
        if target.kind() != source.kind() {
            return Err(error("vector types do not match in copyVector"));
        }
        let target_length = xlength(target)? as usize;
        let source_length = xlength(source)? as usize;
        if source_length == 0 {
            return Ok(());
        }
        for index in 0..target_length {
            copy_element(target, index, source, index % source_length)?;
        }
        Ok(())
    })
}

unsafe fn copy_element(target: Sexp, to: usize, source: Sexp, from: usize) -> ApiResult<()> {
    match target.kind() {
        SexpType::String | SexpType::List | SexpType::Expression => {
            set_sexp_elt(target, to, sexp_elt(source, from)?)
        }
        kind => {
            let size = kind.element_size().ok_or_else(|| error("not a vector"))?;
            let to_data = data_pointer(target, true)?;
            let from_data = data_pointer(source, false)?;
            std::ptr::copy_nonoverlapping(from_data.add(from * size), to_data.add(to * size), size);
            Ok(())
        }
    }
}

unsafe fn copy_matrix(target: Sexp, source: Sexp, by_row: bool) -> ApiResult<()> {
    let rows = Rf_nrows(target) as usize;
    let columns = Rf_ncols(target) as usize;
    let source_length = xlength(source)? as usize;
    if source_length == 0 {
        return Ok(());
    }
    if !by_row {
        for index in 0..rows * columns {
            copy_element(target, index, source, index % source_length)?;
        }
        return Ok(());
    }
    let mut from = 0;
    for row in 0..rows {
        for column in 0..columns {
            copy_element(target, row + column * rows, source, from % source_length)?;
            from += 1;
        }
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_copyMatrix(target: Sexp, source: Sexp, by_row: Rboolean) {
    enter(|| copy_matrix(target, source, by_row != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_copyListMatrix(target: Sexp, source: Sexp, by_row: Rboolean) {
    enter(|| {
        let source = if source.kind() == SexpType::Pairlist {
            let vector = crate::lists::pairlist_to_vector(source)?;
            runtime().heap.root(vector);
            vector
        } else {
            source
        };
        copy_matrix(target, source, by_row != 0)
    })
}

/// `Rf_isVectorizable(x)`: `NULL`, or a list whose elements are all vectors of length at most
/// one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isVectorizable(object: Sexp) -> Rboolean {
    enter(|| {
        Ok(match object.kind() {
            SexpType::Nil => 1,
            SexpType::List | SexpType::Expression => {
                let mut all = true;
                for index in 0..xlength(object)? as usize {
                    let element = sexp_elt(object, index)?;
                    if !element.kind().is_vector() || xlength(element)? > 1 {
                        all = false;
                        break;
                    }
                }
                all as Rboolean
            }
            SexpType::Pairlist => {
                let mut cell = object;
                let mut all = true;
                while cell.kind() == SexpType::Pairlist {
                    let element = cell.car();
                    if !element.kind().is_vector() || xlength(element)? > 1 {
                        all = false;
                        break;
                    }
                    cell = cell.cdr();
                }
                all as Rboolean
            }
            _ => 0,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isScalarString(object: Sexp) -> Rboolean {
    (object.kind() == SexpType::String && !object.is_altrep() && object.length() == 1) as Rboolean
}

/// `Rf_psmatch(formal, tag, exact)`: whether `tag` matches `formal` exactly, or as a prefix of
/// it when `exact` is false.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_psmatch(
    formal: *const c_char,
    tag: *const c_char,
    exact: Rboolean,
) -> Rboolean {
    let formal = c_bytes(formal);
    let tag = c_bytes(tag);
    (if exact != 0 {
        formal == tag
    } else {
        formal.starts_with(tag)
    }) as Rboolean
}

unsafe fn match_text(object: Sexp) -> ApiResult<Vec<u8>> {
    Ok(match object.kind() {
        SexpType::Symbol => object.car().bytes().to_vec(),
        SexpType::Char => utf8_text(object)?.into_owned().into_bytes(),
        SexpType::String if xlength(object)? >= 1 => {
            utf8_text(sexp_elt(object, 0)?)?.into_owned().into_bytes()
        }
        _ => return Err(error("invalid partial string match")),
    })
}

/// `Rf_pmatch(formal, tag, exact)`: `Rf_psmatch` for symbols, `CHARSXP`s or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_pmatch(formal: Sexp, tag: Sexp, exact: Rboolean) -> Rboolean {
    enter(|| {
        let formal = match_text(formal)?;
        let tag = match_text(tag)?;
        Ok((if exact != 0 {
            formal == tag
        } else {
            formal.starts_with(&tag)
        }) as Rboolean)
    })
}

/// `Rf_NonNullStringMatch(s, t)`: whether two `CHARSXP`s are equal, neither being `NA` or
/// empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_NonNullStringMatch(left: Sexp, right: Sexp) -> Rboolean {
    enter(|| {
        let na = runtime().heap.known().na_string;
        if left == na || right == na || left.bytes().is_empty() || right.bytes().is_empty() {
            return Ok(0);
        }
        Ok((left == right || utf8_text(left)? == utf8_text(right)?) as Rboolean)
    })
}

/// `Rf_acopy_string(s)`: a copy of a C string in `R_alloc` memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_acopy_string(text: *const c_char) -> *mut c_char {
    enter(|| Ok(crate::strings::arena_c_string(c_bytes(text))? as *mut c_char))
}

/// `Rf_isUnsorted(x, strictly)`: whether `x` is not in increasing order (strictly increasing
/// with `strictly`). `NA`s make the answer `TRUE`, as in GNU R's internal check.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isUnsorted(object: Sexp, strictly: Rboolean) -> Rboolean {
    enter(|| {
        if !object.kind().is_atomic() {
            return Err(error("only atomic vectors can be tested to be sorted"));
        }
        let length = xlength(object)? as usize;
        let strictly = strictly != 0;
        for index in 1..length {
            let ordering = compare_elements(object, index - 1, object, index, true)?;
            let unsorted = match ordering {
                None => true,
                Some(std::cmp::Ordering::Greater) => true,
                Some(std::cmp::Ordering::Equal) => strictly,
                Some(std::cmp::Ordering::Less) => false,
            };
            if unsorted {
                return Ok(1);
            }
        }
        Ok(0)
    })
}

/// Compares two elements; `None` when either is `NA` (or `NaN`).
unsafe fn compare_elements(
    left: Sexp,
    left_index: usize,
    right: Sexp,
    right_index: usize,
    _na_last: bool,
) -> ApiResult<Option<std::cmp::Ordering>> {
    Ok(match left.kind() {
        SexpType::Logical | SexpType::Integer => {
            let a = int_elt(left, left_index)?;
            let b = int_elt(right, right_index)?;
            if a == NA_INTEGER || b == NA_INTEGER {
                None
            } else {
                Some(a.cmp(&b))
            }
        }
        SexpType::Real => {
            let a = real_elt(left, left_index)?;
            let b = real_elt(right, right_index)?;
            a.partial_cmp(&b)
        }
        SexpType::Complex => {
            let a = altrep::elt::<Rcomplex>(left, left_index as R_xlen_t)?;
            let b = altrep::elt::<Rcomplex>(right, right_index as R_xlen_t)?;
            match (a.r.partial_cmp(&b.r), a.i.partial_cmp(&b.i)) {
                (Some(std::cmp::Ordering::Equal), imaginary) => imaginary,
                (real, Some(_)) => real,
                _ => None,
            }
        }
        SexpType::String => {
            let na = runtime().heap.known().na_string;
            let a = sexp_elt(left, left_index)?;
            let b = sexp_elt(right, right_index)?;
            if a == na || b == na {
                None
            } else {
                Some(crate::strings::collate(&utf8_text(a)?, &utf8_text(b)?))
            }
        }
        SexpType::Raw => {
            let a = altrep::elt::<u8>(left, left_index as R_xlen_t)?;
            let b = altrep::elt::<u8>(right, right_index as R_xlen_t)?;
            Some(a.cmp(&b))
        }
        _ => None,
    })
}

/// Orders `indices` by the keys in `keys` (each a vector of the same length), stably, with
/// `NA`s last or first.
unsafe fn order_by(
    indices: &mut [c_int],
    keys: &[Sexp],
    na_last: bool,
    decreasing: bool,
) -> ApiResult<()> {
    let mut failure = None;
    indices.sort_by(|&a, &b| {
        for &key in keys {
            let ordering = match compare_elements(key, a as usize, key, b as usize, na_last) {
                Ok(ordering) => ordering,
                Err(unwind) => {
                    failure.get_or_insert(unwind);
                    return std::cmp::Ordering::Equal;
                }
            };
            let ordering = match ordering {
                Some(ordering) if decreasing => ordering.reverse(),
                Some(ordering) => ordering,
                None => {
                    let a_na = compare_elements(key, a as usize, key, a as usize, na_last)
                        .ok()
                        .flatten()
                        .is_none();
                    let b_na = compare_elements(key, b as usize, key, b as usize, na_last)
                        .ok()
                        .flatten()
                        .is_none();
                    match (a_na, b_na) {
                        (true, true) => std::cmp::Ordering::Equal,
                        (true, false) if na_last => std::cmp::Ordering::Greater,
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) if na_last => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        (false, false) => std::cmp::Ordering::Equal,
                    }
                }
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        std::cmp::Ordering::Equal
    });
    match failure {
        Some(unwind) => Err(unwind),
        None => Ok(()),
    }
}

/// `R_orderVector(indx, n, arglist, nalast, decreasing)`: fills `indx` with the 0-based
/// ordering permutation of the keys in the list `arglist`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_orderVector(
    indices: *mut c_int,
    count: c_int,
    arguments: Sexp,
    na_last: Rboolean,
    decreasing: Rboolean,
) {
    enter(|| {
        let mut keys = Vec::new();
        if arguments.kind() == SexpType::Pairlist {
            let mut cell = arguments;
            while cell.kind() == SexpType::Pairlist {
                keys.push(cell.car());
                cell = cell.cdr();
            }
        } else {
            for index in 0..xlength(arguments)? as usize {
                keys.push(sexp_elt(arguments, index)?);
            }
        }
        let indices = std::slice::from_raw_parts_mut(indices, count.max(0) as usize);
        for (position, index) in indices.iter_mut().enumerate() {
            *index = position as c_int;
        }
        order_by(indices, &keys, na_last != 0, decreasing != 0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_orderVector1(
    indices: *mut c_int,
    count: c_int,
    key: Sexp,
    na_last: Rboolean,
    decreasing: Rboolean,
) {
    enter(|| {
        let indices = std::slice::from_raw_parts_mut(indices, count.max(0) as usize);
        for (position, index) in indices.iter_mut().enumerate() {
            *index = position as c_int;
        }
        order_by(indices, &[key], na_last != 0, decreasing != 0)
    })
}

/// `R_duplicateAsResizable(x)`: a copy of `x` that `R_resizeVector` can grow, up to the next
/// power of two at or above its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_duplicateAsResizable(object: Sexp) -> Sexp {
    enter(|| {
        if !object.kind().is_vector() || object.kind() == SexpType::Char {
            return Err(error("cannot make a resizable copy of a non-vector"));
        }
        let runtime = runtime();
        let length = xlength(object)?;
        let capacity = (length.max(1) as usize).next_power_of_two() as isize;
        let copy = runtime
            .heap
            .alloc_resizable_vector(object.kind(), capacity)?;
        runtime.heap.root(copy);
        runtime.heap.resize_vector(copy, length)?;
        for index in 0..length as usize {
            copy_element(copy, index, object, index)?;
        }
        crate::memory::copy_attributes(copy, object, true)?;
        Ok(copy)
    })
}

/// `nchar_type` of `R_nchar`, with GNU R's numbering.
const NCHAR_BYTES: c_int = 0;
const NCHAR_CHARS: c_int = 1;

/// `R_nchar(string, type, allowNA, keepNA, msg_name)`: the length of a `CHARSXP` in bytes,
/// characters or display width, with `NA` giving 2 unless `keepNA`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_nchar(
    string: Sexp,
    kind: c_int,
    _allow_na: Rboolean,
    keep_na: Rboolean,
    _message_name: *const c_char,
) -> c_int {
    enter(|| {
        if string == runtime().heap.known().na_string {
            return Ok(if keep_na != 0 { NA_INTEGER } else { 2 });
        }
        Ok(match kind {
            NCHAR_BYTES => string.bytes().len() as c_int,
            NCHAR_CHARS => utf8_text(string)?.chars().count() as c_int,
            _ => crate::strings::display_width(&utf8_text(string)?) as c_int,
        })
    })
}
