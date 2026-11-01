//! Coercion between R's types, and the type predicates (`is.*`, `typeof`).

use crate::{
    builtins::{BuiltinDef, Call, Generic, builtin, internal},
    conditions, deparse,
    unwind::{R, error},
    values::{self, alloc, char_str, length, na_string, nil, scalar_bool, scalar_string, symbols},
};
use heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp, is_na_real};

/// Warnings a coercion raises, collected so each is reported once per call.
#[derive(Default, Clone, Copy)]
pub struct CoercionWarnings {
    pub na: bool,
    pub integer_range: bool,
    pub imaginary: bool,
    pub raw_range: bool,
}

impl CoercionWarnings {
    pub unsafe fn report(self) -> R<()> {
        if self.na {
            conditions::warning("NAs introduced by coercion")?;
        }
        if self.integer_range {
            conditions::warning("NAs introduced by coercion to integer range")?;
        }
        if self.imaginary {
            conditions::warning("imaginary parts discarded in coercion")?;
        }
        if self.raw_range {
            conditions::warning("out-of-range values treated as 0 in coercion to raw")?;
        }
        Ok(())
    }
}

/// A double as an integer, truncated, with whether it was out of range.
pub fn real_to_int(value: f64) -> (i32, bool) {
    if value.is_nan() {
        return (NA_INTEGER, false);
    }
    if value >= 2147483648.0 || value <= -2147483649.0 {
        return (NA_INTEGER, true);
    }
    (value.trunc() as i32, false)
}

/// R's string-to-double rule: surrounding whitespace is ignored, decimal and hexadecimal
/// notation and `Inf`, `NaN` and `NA` are read; anything else is `None`.
pub fn parse_real(text: &str) -> Option<f64> {
    let trimmed = text.trim_matches(|character: char| character.is_ascii_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    let (sign, body) = match trimmed.as_bytes()[0] {
        b'-' => (-1.0, &trimmed[1..]),
        b'+' => (1.0, &trimmed[1..]),
        _ => (1.0, trimmed),
    };
    match body {
        "NA" if sign > 0.0 && !trimmed.starts_with('+') => return Some(NA_REAL),
        "Inf" | "inf" | "infinity" | "Infinity" | "INF" => return Some(sign * f64::INFINITY),
        "NaN" | "nan" => return Some(f64::NAN),
        _ => {}
    }
    let lower = body.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        if hex.is_empty()
            || !hex.chars().all(|character| {
                character.is_ascii_hexdigit() || matches!(character, '.' | 'p' | '-' | '+')
            })
        {
            return None;
        }
        return Some(sign * crate::lower::parse_number(&lower));
    }
    let valid = body.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_digit()
            || byte == b'.'
            || byte == b'e'
            || byte == b'E'
            || ((byte == b'-' || byte == b'+')
                && index > 0
                && matches!(body.as_bytes()[index - 1], b'e' | b'E'))
    });
    if !valid || !body.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let body = if body.ends_with(['e', 'E']) {
        return None;
    } else {
        body
    };
    body.parse::<f64>().ok().map(|value| sign * value)
}

pub fn string_to_logical(text: &str) -> i32 {
    match text {
        "TRUE" | "true" | "T" | "True" => 1,
        "FALSE" | "false" | "F" | "False" => 0,
        _ => NA_LOGICAL,
    }
}

/// Element `index` of an atomic vector as a `CHARSXP`, formatted as `as.character` does.
pub unsafe fn element_as_charsxp(vector: Sexp, index: usize) -> R<Sexp> {
    let vector = values::materialize(vector)?;
    match vector.kind() {
        SexpType::String => Ok(vector.element(index)),
        SexpType::Logical => {
            let value = values::ints(vector)[index];
            if value == NA_LOGICAL {
                Ok(na_string())
            } else {
                values::mkchar(if value != 0 { "TRUE" } else { "FALSE" })
            }
        }
        SexpType::Integer => {
            let value = values::ints(vector)[index];
            if value == NA_INTEGER {
                Ok(na_string())
            } else {
                values::mkchar(&value.to_string())
            }
        }
        SexpType::Real => {
            let value = values::reals(vector)[index];
            if is_na_real(value) {
                Ok(na_string())
            } else {
                values::mkchar(&crate::format::localize_decimal(
                    crate::format::format_real_alone_scipen(value, 15),
                ))
            }
        }
        SexpType::Complex => {
            let value = values::complexes(vector)[index];
            if is_na_real(value.r) || is_na_real(value.i) {
                Ok(na_string())
            } else {
                values::mkchar(&crate::format::localize_decimal(
                    crate::format::format_complex_alone(value, 15),
                ))
            }
        }
        SexpType::Raw => values::mkchar(&format!("{:02x}", values::raws(vector)[index])),
        SexpType::List | SexpType::Expression => {
            let element = vector.element(index);
            if element.kind() == SexpType::String && length(element) == 1 {
                Ok(element.element(0))
            } else if element.kind().is_atomic()
                && length(element) == 1
                && values::names(element) == nil()
            {
                // A named scalar other than a string deparses with its name, as in GNU R.
                element_as_charsxp(element, 0)
            } else if element.kind() == SexpType::Symbol {
                Ok(element.car())
            } else {
                values::mkchar(&deparse::deparse_one_line(element))
            }
        }
        _ => Err(error(format!(
            "cannot coerce type '{}' to vector of type 'character'",
            vector.kind().name()
        ))),
    }
}

/// Converts a vector to `kind`, keeping its attributes, as GNU R's `coerceVector` does.
pub unsafe fn coerce_vector(object: Sexp, kind: SexpType) -> R<Sexp> {
    let mut warnings = CoercionWarnings::default();
    let result = coerce_vector_quiet(object, kind, &mut warnings)?;
    warnings.report()?;
    Ok(result)
}

/// As `coerce_vector`, collecting warnings instead of raising them.
pub unsafe fn coerce_vector_quiet(
    object: Sexp,
    kind: SexpType,
    warnings: &mut CoercionWarnings,
) -> R<Sexp> {
    if object.kind() == kind {
        return Ok(object);
    }
    let object = values::materialize(object)?;
    let source = object.kind();
    let result = match source {
        SexpType::Nil => {
            if kind == SexpType::Pairlist {
                return Ok(nil());
            }
            return alloc(kind, 0);
        }
        SexpType::Symbol => return coerce_symbol(object, kind),
        SexpType::Record => {
            let native = crate::records::native_kind(object);
            return Err(error(crate::records::boundary_message("the value", native)));
        }
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            return coerce_pairlist(object, kind, warnings);
        }
        SexpType::Environment if kind == SexpType::List => {
            return Err(error(
                "cannot coerce type 'environment' to vector of type 'list'",
            ));
        }
        SexpType::List | SexpType::Expression => {
            coerce_list(object, kind, SexpType::List, warnings)?
        }
        _ if source.is_atomic() => coerce_atomic(object, kind, warnings)?,
        SexpType::Closure | SexpType::Builtin | SexpType::Special if kind == SexpType::List => {
            return values::list_vector(&[object]);
        }
        _ => {
            return Err(error(format!(
                "cannot coerce type '{}' to vector of type '{}'",
                source.name(),
                kind.name()
            )));
        }
    };
    if source.is_atomic() || matches!(source, SexpType::List | SexpType::Expression) {
        if kind.is_vector() {
            values::copy_attributes(object, result)?;
            if kind.is_atomic() && source == SexpType::List {
                // Classes of a list do not carry over to an atomic vector.
            }
        }
    }
    Ok(result)
}

unsafe fn coerce_symbol(symbol: Sexp, kind: SexpType) -> R<Sexp> {
    match kind {
        SexpType::String => values::scalar_string_charsxp(symbol.car()),
        SexpType::List | SexpType::Expression => {
            let result = alloc(kind, 1)?;
            values::set_elt(result, 0, symbol);
            Ok(result)
        }
        SexpType::Language => values::call(symbol, &[]),
        _ => Err(error(format!(
            "cannot coerce type 'symbol' to vector of type '{}'",
            kind.name()
        ))),
    }
}

unsafe fn coerce_pairlist(list: Sexp, kind: SexpType, warnings: &mut CoercionWarnings) -> R<Sexp> {
    // A call or pairlist is a single expression, as in GNU R.
    if kind == SexpType::Expression {
        let result = alloc(kind, 1)?;
        values::set_elt(result, 0, list);
        return Ok(result);
    }
    match kind {
        SexpType::List => {
            let result = values::pairlist_to_list(list)?;
            if list.kind() != SexpType::Language {
                let attributes = list.attrib();
                let mut cell = attributes;
                while cell.kind() == SexpType::Pairlist {
                    if cell.tag() != symbols().names {
                        values::set_attr(result, cell.tag(), cell.car())?;
                    }
                    cell = cell.cdr();
                }
            }
            Ok(result)
        }
        SexpType::Pairlist | SexpType::Language => {
            let items = values::pairlist_items(list);
            values::pairlist(&items, kind)
        }
        SexpType::String => {
            let items = values::pairlist_items(list);
            let result = alloc(SexpType::String, items.len())?;
            for (index, (_, value)) in items.iter().enumerate() {
                let charsxp = if value.kind() == SexpType::Symbol {
                    value.car()
                } else if value.kind() == SexpType::String && length(*value) == 1 {
                    value.element(0)
                } else if value.kind().is_atomic() && length(*value) == 1 {
                    element_as_charsxp(*value, 0)?
                } else {
                    values::mkchar(&deparse::deparse_one_line(*value))?
                };
                values::set_elt(result, index, charsxp);
            }
            Ok(result)
        }
        _ => {
            let list_vector = values::pairlist_to_list(list)?;
            coerce_list(list_vector, kind, list.kind(), warnings)
        }
    }
}

/// `source` is the type the error names: a call or pairlist arrives here as a list.
unsafe fn coerce_list(
    list: Sexp,
    kind: SexpType,
    source: SexpType,
    warnings: &mut CoercionWarnings,
) -> R<Sexp> {
    let count = length(list);
    match kind {
        SexpType::List | SexpType::Expression => {
            let result = alloc(kind, count)?;
            for index in 0..count {
                values::set_elt(result, index, list.element(index));
            }
            Ok(result)
        }
        SexpType::Pairlist | SexpType::Language => values::list_to_pairlist(list, kind),
        SexpType::String => {
            let result = alloc(SexpType::String, count)?;
            for index in 0..count {
                let charsxp = element_as_charsxp(list, index)?;
                values::set_elt(result, index, charsxp);
            }
            Ok(result)
        }
        _ if kind.is_atomic() => {
            for index in 0..count {
                let element = list.element(index);
                if !(element.kind().is_atomic() && length(element) == 1) {
                    return Err(error(format!(
                        "'{}' object cannot be coerced to type '{}'",
                        source.name(),
                        kind.name()
                    )));
                }
            }
            let result = alloc(kind, count)?;
            for index in 0..count {
                let element = coerce_atomic(list.element(index), kind, warnings)?;
                crate::subset::copy_element(element, 0, result, index)?;
            }
            Ok(result)
        }
        _ => Err(error(format!(
            "'{}' object cannot be coerced to type '{}'",
            source.name(),
            kind.name()
        ))),
    }
}

/// Converts between atomic types (and to lists), element by element.
pub unsafe fn coerce_atomic(
    object: Sexp,
    kind: SexpType,
    warnings: &mut CoercionWarnings,
) -> R<Sexp> {
    let count = length(object);
    let source = object.kind();
    if source == kind {
        return Ok(object);
    }
    let result = alloc(kind, count)?;
    match kind {
        SexpType::Logical => {
            let target = values::ints_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                *slot = match source {
                    SexpType::Integer => {
                        let value = values::ints(object)[index];
                        if value == NA_INTEGER {
                            NA_LOGICAL
                        } else {
                            (value != 0) as i32
                        }
                    }
                    SexpType::Real => {
                        let value = values::reals(object)[index];
                        if value.is_nan() {
                            NA_LOGICAL
                        } else {
                            (value != 0.0) as i32
                        }
                    }
                    SexpType::Complex => {
                        let value = values::complexes(object)[index];
                        if value.r.is_nan() || value.i.is_nan() {
                            NA_LOGICAL
                        } else {
                            (value.r != 0.0 || value.i != 0.0) as i32
                        }
                    }
                    SexpType::String => match values::string_at(object, index) {
                        Some(text) => string_to_logical(&text),
                        None => NA_LOGICAL,
                    },
                    SexpType::Raw => (values::raws(object)[index] != 0) as i32,
                    _ => NA_LOGICAL,
                };
            }
        }
        SexpType::Integer => {
            let target = values::ints_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                *slot = match source {
                    SexpType::Logical => values::ints(object)[index],
                    SexpType::Real => {
                        let (value, out_of_range) = real_to_int(values::reals(object)[index]);
                        warnings.integer_range |= out_of_range;
                        value
                    }
                    SexpType::Complex => {
                        let value = values::complexes(object)[index];
                        if value.i != 0.0 && !value.i.is_nan() {
                            warnings.imaginary = true;
                        }
                        let (converted, out_of_range) = real_to_int(value.r);
                        warnings.integer_range |= out_of_range;
                        converted
                    }
                    SexpType::String => match values::string_at(object, index) {
                        Some(text) => match parse_real(&text) {
                            Some(value) if is_na_real(value) => NA_INTEGER,
                            Some(value) => {
                                let (converted, out_of_range) = real_to_int(value);
                                if out_of_range || value.is_nan() {
                                    warnings.na = true;
                                }
                                converted
                            }
                            None => {
                                if !text.trim().is_empty() {
                                    warnings.na = true;
                                }
                                NA_INTEGER
                            }
                        },
                        None => NA_INTEGER,
                    },
                    SexpType::Raw => values::raws(object)[index] as i32,
                    _ => NA_INTEGER,
                };
            }
        }
        SexpType::Real => {
            let target = values::reals_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                *slot = match source {
                    SexpType::Logical | SexpType::Integer => {
                        let value = values::ints(object)[index];
                        if value == NA_INTEGER {
                            NA_REAL
                        } else {
                            value as f64
                        }
                    }
                    SexpType::Complex => {
                        let value = values::complexes(object)[index];
                        if value.i != 0.0 && !value.i.is_nan() {
                            warnings.imaginary = true;
                        }
                        value.r
                    }
                    SexpType::String => match values::string_at(object, index) {
                        Some(text) => match parse_real(&text) {
                            Some(value) => value,
                            None => {
                                if !text.trim().is_empty() {
                                    warnings.na = true;
                                }
                                NA_REAL
                            }
                        },
                        None => NA_REAL,
                    },
                    SexpType::Raw => values::raws(object)[index] as f64,
                    _ => NA_REAL,
                };
            }
        }
        SexpType::Complex => {
            let target = values::complexes_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                *slot = match source {
                    SexpType::Logical | SexpType::Integer => {
                        let value = values::ints(object)[index];
                        if value == NA_INTEGER {
                            Rcomplex {
                                r: NA_REAL,
                                i: NA_REAL,
                            }
                        } else {
                            Rcomplex {
                                r: value as f64,
                                i: 0.0,
                            }
                        }
                    }
                    SexpType::Real => {
                        let value = values::reals(object)[index];
                        if is_na_real(value) {
                            Rcomplex {
                                r: NA_REAL,
                                i: NA_REAL,
                            }
                        } else {
                            Rcomplex { r: value, i: 0.0 }
                        }
                    }
                    SexpType::String => match values::string_at(object, index) {
                        Some(text) => match parse_complex(&text) {
                            Some(value) => value,
                            None => {
                                warnings.na = true;
                                Rcomplex {
                                    r: NA_REAL,
                                    i: NA_REAL,
                                }
                            }
                        },
                        None => Rcomplex {
                            r: NA_REAL,
                            i: NA_REAL,
                        },
                    },
                    SexpType::Raw => Rcomplex {
                        r: values::raws(object)[index] as f64,
                        i: 0.0,
                    },
                    _ => Rcomplex {
                        r: NA_REAL,
                        i: NA_REAL,
                    },
                };
            }
        }
        SexpType::String => {
            for index in 0..count {
                let charsxp = element_as_charsxp(object, index)?;
                values::set_elt(result, index, charsxp);
            }
        }
        SexpType::Raw => {
            let target = values::raws_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                let value = match source {
                    SexpType::Logical | SexpType::Integer => {
                        let value = values::ints(object)[index];
                        if value == NA_INTEGER {
                            -1
                        } else {
                            value as i64
                        }
                    }
                    SexpType::Real => {
                        let value = values::reals(object)[index];
                        if value.is_nan() {
                            -1
                        } else {
                            value.trunc() as i64
                        }
                    }
                    SexpType::String => values::string_at(object, index)
                        .and_then(|text| parse_real(&text))
                        .filter(|value| !value.is_nan())
                        .map_or(-1, |value| value.trunc() as i64),
                    SexpType::Complex => values::complexes(object)[index].r as i64,
                    _ => -1,
                };
                *slot = if (0..=255).contains(&value) {
                    value as u8
                } else {
                    warnings.raw_range = true;
                    0
                };
            }
        }
        SexpType::List | SexpType::Expression => {
            let names = values::get_attr(object, symbols().names);
            for index in 0..count {
                let element = crate::subset::vector_element(object, index)?;
                values::set_elt(result, index, element);
            }
            if names != nil() {
                values::set_attr(result, symbols().names, names)?;
            }
            return Ok(result);
        }
        _ => {
            return Err(error(format!(
                "cannot coerce type '{}' to vector of type '{}'",
                source.name(),
                kind.name()
            )));
        }
    }
    Ok(result)
}

fn parse_complex(text: &str) -> Option<Rcomplex> {
    let trimmed = text.trim();
    if let Some(value) = parse_real(trimmed) {
        return Some(Rcomplex { r: value, i: 0.0 });
    }
    let body = trimmed.strip_suffix('i')?;
    let split = body
        .char_indices()
        .skip(1)
        .filter(|(index, character)| {
            matches!(character, '+' | '-') && !matches!(body.as_bytes()[index - 1], b'e' | b'E')
        })
        .map(|(index, _)| index)
        .last();
    match split {
        Some(position) => Some(Rcomplex {
            r: parse_real(&body[..position])?,
            i: parse_real(&body[position..])?,
        }),
        None => Some(Rcomplex {
            r: 0.0,
            i: parse_real(body)?,
        }),
    }
}

/// A factor's labels as a character vector.
pub unsafe fn factor_as_character(factor: Sexp) -> R<Sexp> {
    let levels = values::get_attr(factor, symbols().levels);
    let codes = values::materialize(factor)?;
    let count = length(codes);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let code = values::ints(codes)[index];
        let charsxp = if code == NA_INTEGER || code < 1 || code as usize > length(levels) {
            na_string()
        } else {
            levels.element(code as usize - 1)
        };
        values::set_elt(result, index, charsxp);
    }
    let names = values::get_attr(factor, symbols().names);
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// The type named by `as.vector`'s `mode`.
pub fn mode_type(mode: &str) -> Option<SexpType> {
    Some(match mode {
        "logical" => SexpType::Logical,
        "integer" => SexpType::Integer,
        "numeric" | "double" => SexpType::Real,
        "complex" => SexpType::Complex,
        "character" => SexpType::String,
        "list" => SexpType::List,
        "expression" => SexpType::Expression,
        "raw" => SexpType::Raw,
        "symbol" | "name" => SexpType::Symbol,
        "pairlist" => SexpType::Pairlist,
        "function" | "closure" => SexpType::Closure,
        "call" | "language" => SexpType::Language,
        _ => return None,
    })
}

/// `as.vector(x, mode)`: converts and drops attributes, except a list's names.
pub unsafe fn as_vector(object: Sexp, mode: &str) -> R<Sexp> {
    if mode == "any" {
        if object.kind().is_atomic() && values::has_attributes(object) {
            let copy = values::shallow_duplicate(object)?;
            values::clear_attributes(copy);
            return Ok(copy);
        }
        if object.kind() == SexpType::Pairlist || object.kind() == SexpType::Language {
            return coerce_vector(object, SexpType::List);
        }
        return Ok(object);
    }
    let Some(kind) = mode_type(mode) else {
        return Err(error("invalid 'mode' argument"));
    };
    if kind == SexpType::Symbol {
        let text = values::as_string_scalar(object)?
            .ok_or_else(|| error("invalid type/length (symbol/0) in vector allocation"))?;
        return values::install(&text);
    }
    if kind == SexpType::Closure {
        if values::is_function(object) {
            return Ok(object);
        }
        return Err(error("cannot coerce to function"));
    }
    let result = if object.kind() == kind {
        object
    } else {
        coerce_vector(object, kind)?
    };
    if kind.is_atomic() && values::has_attributes(result) {
        let copy = if result == object {
            values::shallow_duplicate(result)?
        } else {
            result
        };
        values::clear_attributes(copy);
        return Ok(copy);
    }
    if matches!(kind, SexpType::List | SexpType::Expression)
        && result.is_object()
        && object.kind() != kind
    {
        let names = values::get_attr(result, symbols().names);
        values::clear_attributes(result);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
        }
    }
    Ok(result)
}

// The builtins.

pub const BUILTINS: &[BuiltinDef] = &[
    internal("asCharacterFactor", 1, |call| unsafe {
        as_character_factor(call.arg(0))
    }),
    builtin("as.character", -1, |call| unsafe {
        as_type(call, SexpType::String)
    })
    .generic(Generic::Internal),
    builtin("as.integer", -1, |call| unsafe {
        as_type(call, SexpType::Integer)
    })
    .generic(Generic::Internal),
    builtin("as.double", -1, |call| unsafe {
        as_type(call, SexpType::Real)
    })
    .generic(Generic::Internal),
    builtin("as.numeric", -1, |call| unsafe {
        as_type(call, SexpType::Real)
    })
    .generic(Generic::Internal),
    builtin("as.logical", -1, |call| unsafe {
        as_type(call, SexpType::Logical)
    })
    .generic(Generic::Internal),
    builtin("as.complex", -1, |call| unsafe {
        as_type(call, SexpType::Complex)
    })
    .generic(Generic::Internal),
    builtin("as.raw", 1, |call| unsafe { as_type(call, SexpType::Raw) }).generic(Generic::Internal),
    internal("as.vector", 2, do_as_vector).generic(Generic::Internal),
    builtin("as.call", 1, do_as_call).generic(Generic::Internal),
    internal("as.function.default", 2, do_as_function),
    internal("typeof", 1, |call| unsafe {
        scalar_string(type_name(call.arg(0)))
    }),
    internal("vector", 2, do_vector),
    builtin("storage.mode<-", 2, do_set_storage_mode),
    internal("is.vector", 2, do_is_vector),
    builtin("is.null", 1, |call| unsafe {
        scalar_bool(call.arg(0) == nil())
    }),
    builtin("is.logical", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Logical)
    }),
    builtin("is.integer", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Integer)
    }),
    builtin("is.double", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Real)
    }),
    builtin("is.complex", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Complex)
    }),
    builtin("is.character", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::String)
    }),
    builtin("is.symbol", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Symbol)
    }),
    builtin("is.name", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Symbol)
    }),
    builtin("is.environment", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Environment)
    }),
    builtin("is.list", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::List | SexpType::Pairlist
            ))
        }
    }),
    builtin("is.pairlist", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::Pairlist | SexpType::Nil
            ))
        }
    }),
    builtin("is.expression", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Expression)
    }),
    builtin("is.raw", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Raw)
    }),
    builtin("is.object", 1, |call| unsafe {
        scalar_bool(call.arg(0).is_object())
    }),
    builtin("is.numeric", 1, do_is_numeric).generic(Generic::Internal),
    builtin("is.matrix", 1, |call| unsafe {
        { scalar_bool(values::dims(call.arg(0)).is_some_and(|dims| dims.len() == 2)) }
    })
    .generic(Generic::Internal),
    builtin("is.array", 1, |call| unsafe {
        scalar_bool(values::dims(call.arg(0)).is_some())
    })
    .generic(Generic::Internal),
    builtin("is.atomic", 1, |call| unsafe {
        {
            // GNU R 4.3, whose R code runs here, still counts NULL as atomic.
            let kind = call.arg(0).kind();
            scalar_bool(kind.is_atomic() || kind == SexpType::Nil)
        }
    }),
    builtin("is.recursive", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::List
                    | SexpType::Pairlist
                    | SexpType::Language
                    | SexpType::Closure
                    | SexpType::Environment
                    | SexpType::Expression
                    | SexpType::Special
                    | SexpType::Builtin
                    | SexpType::Promise
                    | SexpType::Dots
            ))
        }
    }),
    builtin("is.call", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Language)
    }),
    builtin("is.language", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::Symbol | SexpType::Language | SexpType::Expression
            ))
        }
    }),
    builtin("is.function", 1, |call| unsafe {
        scalar_bool(values::is_function(call.arg(0)))
    }),
    builtin("is.primitive", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::Builtin | SexpType::Special
            ))
        }
    }),
    builtin("is.single", 1, |call| unsafe {
        Err(call.error("type \"single\" unimplemented in R"))
    }),
    internal("is.builtin.internal", 1, |call| unsafe {
        {
            let symbol = call.arg(0);
            scalar_bool(
                symbol.kind() == SexpType::Symbol
                    && crate::builtins::internal_function(symbol)
                        .is_some_and(|object| object.kind() == SexpType::Builtin),
            )
        }
    }),
];

unsafe fn as_type(call: &Call<'_>, kind: SexpType) -> R<Sexp> {
    let object = call.arg(0);
    if object.kind() == kind && !values::has_attributes(object) {
        return Ok(object);
    }
    if object.kind() == SexpType::Environment {
        return Err(call.error(format!(
            "cannot coerce type 'environment' to vector of type '{}'",
            kind.name()
        )));
    }
    if values::inherits(object, "factor") && kind == SexpType::String {
        return factor_as_character(object).and_then(|result| {
            values::clear_attributes(result);
            Ok(result)
        });
    }
    let result = match object.kind() {
        SexpType::Symbol | SexpType::Language | SexpType::Pairlist if kind == SexpType::String => {
            coerce_vector(object, kind)?
        }
        SexpType::List
        | SexpType::Expression
        | SexpType::Pairlist
        | SexpType::Language
        | SexpType::Nil => coerce_vector(object, kind)?,
        kind_of if kind_of.is_atomic() => {
            let converted = coerce_vector(object, kind)?;
            if converted == object {
                values::shallow_duplicate(object)?
            } else {
                converted
            }
        }
        SexpType::Symbol => coerce_vector(object, kind)?,
        _ => {
            return Err(call.error(format!(
                "cannot coerce type '{}' to vector of type '{}'",
                object.kind().name(),
                kind.name()
            )));
        }
    };
    values::clear_attributes(result);
    Ok(result)
}

unsafe fn do_as_vector(call: &Call<'_>) -> R<Sexp> {
    let mode = values::require_string(call.arg(1), "mode")?;
    as_vector(call.arg(0), &mode)
}

unsafe fn do_as_call(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    match object.kind() {
        SexpType::Language => Ok(object),
        SexpType::List | SexpType::Expression => {
            if length(object) == 0 {
                return Err(call.error("invalid argument list"));
            }
            values::list_to_pairlist(object, SexpType::Language)
        }
        SexpType::Pairlist => {
            let items = values::pairlist_items(object);
            values::pairlist(&items, SexpType::Language)
        }
        _ => Err(call.error("invalid argument list")),
    }
}

unsafe fn do_as_function(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let env = call.arg(1);
    if values::is_function(list) {
        return Ok(list);
    }
    if list.kind() != SexpType::List || length(list) == 0 {
        return Err(call.error("argument must have length at least 1"));
    }
    let count = length(list);
    let names = values::names(list);
    let mut formals = Vec::new();
    for index in 0..count - 1 {
        let name = if names != nil() {
            char_str(names.element(index)).into_owned()
        } else {
            String::new()
        };
        if name.is_empty() {
            return Err(call.error("invalid formal argument list for \"function\""));
        }
        let value = list.element(index);
        let value = if value.kind() == SexpType::Symbol && value.car().bytes().is_empty() {
            values::missing_arg()
        } else {
            value
        };
        formals.push((values::install(&name)?, value));
    }
    let formals = values::pairlist(&formals, SexpType::Pairlist)?;
    crate::eval::make_closure(formals, list.element(count - 1), values::nil(), env)
}

unsafe fn do_vector(call: &Call<'_>) -> R<Sexp> {
    let mode = values::require_string(call.arg(0), "mode")?;
    let count = crate::subset::length_argument(call.arg(1))?;
    let kind = match mode.as_str() {
        "any" => SexpType::Logical,
        mode => mode_type(mode)
            .ok_or_else(|| call.error(format!("vector: cannot make a vector of mode '{mode}'.")))?,
    };
    if kind == SexpType::Pairlist {
        let items = vec![(nil(), nil()); count];
        return values::pairlist(&items, SexpType::Pairlist);
    }
    if !kind.is_vector() {
        return Err(call.error(format!("vector: cannot make a vector of mode '{mode}'.")));
    }
    alloc(kind, count)
}

unsafe fn do_set_storage_mode(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let mode = values::require_string(call.arg(1), "value")?;
    let kind = mode_type(&mode).ok_or_else(|| call.error("invalid value"))?;
    if object.kind() == kind {
        return Ok(object);
    }
    if values::inherits(object, "factor") {
        return Err(call.error("invalid to change the storage mode of a factor"));
    }
    coerce_vector(object, kind)
}

unsafe fn do_is_vector(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let mode = values::require_string(call.arg(1), "mode")?;
    let kind = object.kind();
    let kind_matches = match mode.as_str() {
        "any" => kind.is_vector(),
        "numeric" => {
            matches!(kind, SexpType::Integer | SexpType::Real)
                && !values::inherits(object, "factor")
        }
        "name" | "symbol" => kind == SexpType::Symbol,
        mode => {
            kind.is_vector() && kind.name() == mode || (mode == "list" && kind == SexpType::List)
        }
    };
    if !kind_matches {
        return scalar_bool(false);
    }
    // Only names are allowed as attributes.
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() != symbols().names {
            return scalar_bool(false);
        }
        cell = cell.cdr();
    }
    scalar_bool(true)
}

unsafe fn do_is_numeric(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    scalar_bool(
        matches!(object.kind(), SexpType::Integer | SexpType::Real)
            && !values::inherits(object, "factor"),
    )
}

/// Whether a value is an S4 object.
pub unsafe fn is_s4(object: Sexp) -> bool {
    object.has_gp(gp::S4_OBJECT)
}

/// A named vector's type for `c()` and `unlist()`: the highest type of its elements.
pub fn higher_type(left: SexpType, right: SexpType) -> SexpType {
    fn rank(kind: SexpType) -> u8 {
        match kind {
            SexpType::Nil => 0,
            SexpType::Raw => 1,
            SexpType::Logical => 2,
            SexpType::Integer => 3,
            SexpType::Real => 4,
            SexpType::Complex => 5,
            SexpType::String => 6,
            SexpType::List => 7,
            SexpType::Expression => 8,
            _ => 7,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

pub unsafe fn na_of(kind: SexpType) -> R<Sexp> {
    let result = alloc(kind, 1)?;
    match kind {
        SexpType::Logical | SexpType::Integer => values::ints_mut(result)[0] = NA_INTEGER,
        SexpType::Real => values::reals_mut(result)[0] = NA_REAL,
        SexpType::Complex => {
            values::complexes_mut(result)[0] = Rcomplex {
                r: NA_REAL,
                i: NA_REAL,
            }
        }
        SexpType::String => values::set_elt(result, 0, na_string()),
        _ => {}
    }
    Ok(result)
}

/// The labels of a factor's codes, `NA` for a missing or out-of-range code.
pub unsafe fn as_character_factor(factor: Sexp) -> R<Sexp> {
    if !values::inherits(factor, "factor") || factor.kind() != SexpType::Integer {
        return Err(error("attempting to coerce non-factor"));
    }
    let levels = values::get_attr(factor, values::symbols().levels);
    if levels.kind() != SexpType::String {
        return Err(error("malformed factor"));
    }
    let codes = values::ints(factor);
    let level_count = values::length(levels);
    let result = values::alloc(SexpType::String, codes.len())?;
    for (index, &code) in codes.iter().enumerate() {
        let label = if code >= 1 && (code as usize) <= level_count {
            levels.element(code as usize - 1)
        } else {
            values::na_string()
        };
        values::set_elt(result, index, label);
    }
    Ok(result)
}

/// `typeof()`: the type's name, with a ry record, tuple or tag reported as itself rather than as
/// the object kind they share.
pub unsafe fn type_name(object: Sexp) -> &'static str {
    match object.kind() {
        SexpType::Record => crate::records::native_kind(object).type_name(),
        kind => kind.name(),
    }
}
