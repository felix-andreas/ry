//! Coercion between R's types, and the type predicates (`is.*`, `typeof`).

use {
    crate::{
        builtins::{BuiltinDef, Call, Generic, builtin, internal, is_native_value},
        conditions, deparse,
        handles::is_na,
        strings::with_text_buffer,
        system::r_at_least,
        unwind::{R, error},
        values::{
            self, alloc, char_str, length, na_string, nil, scalar_bool, scalar_string, symbols,
        },
    },
    heap::{
        handle::{Chars, Gc},
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp, is_na_real},
        strings::Encoding,
    },
    numeric::long_double::{Exactness, strtod},
};

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
    if value >= 2147483648.0 || value <= -2147483648.0 {
        return (NA_INTEGER, true);
    }
    (value.trunc() as i32, false)
}

/// A complex as an integer. The discarded imaginary part is reported only for a value that
/// converts, as in GNU R.
fn complex_to_int(value: Rcomplex, warnings: &mut CoercionWarnings) -> i32 {
    if value.i.is_nan() {
        return NA_INTEGER;
    }
    let (converted, out_of_range) = real_to_int(value.r);
    warnings.integer_range |= out_of_range;
    if converted != NA_INTEGER && value.i != 0.0 {
        warnings.imaginary = true;
    }
    converted
}

/// The first element as an integer, with the coercion warnings GNU R's `asInteger` gives.
pub unsafe fn integer_scalar(object: Sexp) -> R<i32> {
    let mut warnings = CoercionWarnings::default();
    let value = match object.kind() {
        _ if values::length(object) == 0 => NA_INTEGER,
        SexpType::Real => {
            let (converted, out_of_range) = real_to_int(values::reals(object)[0]);
            warnings.integer_range = out_of_range;
            converted
        }
        SexpType::Complex => complex_to_int(values::complexes(object)[0], &mut warnings),
        SexpType::String => integer_from_string(Gc::adopt(object.element(0)), &mut warnings)?,
        _ => values::as_integer_scalar(object),
    };
    warnings.report()?;
    Ok(value)
}

/// An integer on its way to a raw: `NA` becomes a value outside the raw range.
fn raw_candidate(value: i32) -> i64 {
    if value == NA_INTEGER {
        -1
    } else {
        value as i64
    }
}

/// GNU R's `RealFromString`: `NA` and a blank string are missing, a number with only white
/// space after it is its value, and any other text is missing with the coercion warning.
pub fn real_from_string(chars: Gc<'_, Chars>, warnings: &mut CoercionWarnings) -> R<f64> {
    let text = chars.bytes();
    if is_na(chars) || is_blank(text)? {
        return Ok(NA_REAL);
    }
    let number = strtod(text, b'.', false, Exactness::Allow);
    if is_blank(&text[number.length..])? {
        return Ok(number.value);
    }
    warnings.na = true;
    Ok(NA_REAL)
}

/// GNU R's `IntegerFromString`: the string as `real_from_string` reads it, truncated, and
/// missing with the range warning outside the integers.
pub fn integer_from_string(chars: Gc<'_, Chars>, warnings: &mut CoercionWarnings) -> R<i32> {
    let (converted, out_of_range) = real_to_int(real_from_string(chars, warnings)?);
    warnings.integer_range |= out_of_range;
    Ok(converted)
}

/// GNU R's `ComplexFromString`: a real `a`, an imaginary `bi` or `a+bi`.
pub fn complex_from_string(chars: Gc<'_, Chars>, warnings: &mut CoercionWarnings) -> R<Rcomplex> {
    let missing = Rcomplex {
        r: NA_REAL,
        i: NA_REAL,
    };
    let text = chars.bytes();
    if is_na(chars) || is_blank(text)? {
        return Ok(missing);
    }
    let first = strtod(text, b'.', false, Exactness::Allow);
    let rest = &text[first.length..];
    if is_blank(rest)? {
        return Ok(Rcomplex {
            r: first.value,
            i: 0.0,
        });
    }
    if rest[0] == b'i' && is_blank(&rest[1..])? {
        return Ok(Rcomplex {
            r: 0.0,
            i: first.value,
        });
    }
    if matches!(rest[0], b'+' | b'-') {
        let second = strtod(rest, b'.', false, Exactness::Allow);
        let rest = &rest[second.length..];
        if rest.first() == Some(&b'i') && is_blank(&rest[1..])? {
            return Ok(Rcomplex {
                r: first.value,
                i: second.value,
            });
        }
    }
    warnings.na = true;
    Ok(missing)
}

/// The number a whole text spells, by the rule of `real_from_string`, for a caller that
/// reports no coercion warning: `None` for blank text and for text that is no number.
pub fn parse_real(text: &str) -> Option<f64> {
    let number = strtod(text.as_bytes(), b'.', false, Exactness::Allow);
    let blank = |text: &str| text.chars().all(is_wide_space);
    (number.length > 0 && blank(&text[number.length..])).then_some(number.value)
}

/// C's `isspace` in a UTF-8 locale, for one byte.
pub(crate) fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 11 | 12 | b'\r')
}

/// GNU R's `isBlankString` in a UTF-8 locale: only white space characters as `iswspace` knows
/// them. Like GNU R, it stops at the first character that is not white space, and an invalid
/// multibyte sequence before any such character is an error.
pub(crate) fn is_blank(text: &[u8]) -> R<bool> {
    let text = text
        .iter()
        .position(|&byte| byte == 0)
        .map_or(text, |end| &text[..end]);
    let mut rest = text;
    while let Some(&first) = rest.first() {
        if first.is_ascii() {
            if !is_c_space(first) {
                return Ok(false);
            }
            rest = &rest[1..];
            continue;
        }
        // One character is at most four bytes, so only those are validated per step.
        let head = &rest[..rest.len().min(4)];
        let valid = head.utf8_chunks().next().map_or("", |chunk| chunk.valid());
        if let Some(character) = valid.chars().next() {
            if !is_wide_space(character) {
                return Ok(false);
            }
            rest = &rest[character.len_utf8()..];
        } else {
            return Err(crate::connections::invalid_multibyte(rest));
        }
    }
    Ok(true)
}

/// `iswspace` in glibc's UTF-8 locales.
fn is_wide_space(character: char) -> bool {
    matches!(
        character,
        '\t'..='\r'
            | ' '
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

pub fn string_to_logical(text: &str) -> i32 {
    match text {
        "TRUE" | "true" | "T" | "True" => 1,
        "FALSE" | "false" | "F" | "False" => 0,
        _ => NA_LOGICAL,
    }
}

/// One element of a list or pairlist as a `CHARSXP`. Only a string scalar is taken as it is.
/// GNU R deparses every other element, so a missing logical or number is the string `"NA"`, and
/// a named scalar keeps its name.
unsafe fn list_element_as_charsxp(element: Sexp) -> R<Sexp> {
    if element.kind() == SexpType::String && length(element) == 1 {
        return Ok(element.element(0));
    }
    let number = matches!(
        element.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex
    );
    if number && length(element) == 1 && values::names(element) == nil() {
        let text = element_as_charsxp(element, 0)?;
        return if text == na_string() {
            values::mkchar("NA")
        } else {
            Ok(text)
        };
    }
    values::mkchar(&deparse::deparse_for_coercion(element)?)
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
        SexpType::Real => real_as_charsxp(
            values::reals(vector)[index],
            crate::format::Settings::current_scipen(),
        ),
        SexpType::Complex => {
            let value = values::complexes(vector)[index];
            if is_na_real(value.r) || is_na_real(value.i) {
                Ok(na_string())
            } else {
                values::mkchar(&crate::format::localize_decimal(
                    crate::format::format_complex_alone(
                        value,
                        15,
                        crate::format::Settings::current_scipen(),
                    ),
                ))
            }
        }
        SexpType::Raw => values::mkchar(&format!("{:02x}", values::raws(vector)[index])),
        SexpType::List | SexpType::Expression => list_element_as_charsxp(vector.element(index)),
        _ => Err(error(format!(
            "cannot coerce type '{}' to vector of type 'character'",
            vector.kind().name()
        ))),
    }
}

/// A double as the `CHARSXP` `as.character` gives it, under scientific-notation penalty `scipen`.
unsafe fn real_as_charsxp(value: f64, scipen: i32) -> R<Sexp> {
    if is_na_real(value) {
        return Ok(na_string());
    }
    with_text_buffer(|text| {
        crate::format::push_real_alone(text, value, 15, scipen);
        if crate::format::decimal_is_point() {
            values::mkchar(text)
        } else {
            values::mkchar(&crate::format::localize_decimal(text.clone()))
        }
    })
}

/// Converts a vector to `kind` as GNU R's `coerceVector` does. All attributes follow between
/// atomic types and between a list and an expression vector. Between a list and an atomic
/// vector only the names do, and an atomic vector made an expression vector keeps none.
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
        SexpType::Record | SexpType::I64 => {
            return Err(error(crate::records::boundary_message("the value", object)));
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
    let generic = |kind| matches!(kind, SexpType::List | SexpType::Expression);
    if (source.is_atomic() && kind.is_atomic()) || (generic(source) && generic(kind)) {
        values::copy_attributes(object, result)?;
    } else if (source.is_atomic() && kind == SexpType::List)
        || (generic(source) && kind.is_atomic())
    {
        let names = values::names(object);
        if names != nil() {
            values::set_attr(result, symbols().names, names)?;
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
                } else {
                    list_element_as_charsxp(*value)?
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
        SexpType::Pairlist | SexpType::Language => values::list_to_pairlist(list, kind, true),
        SexpType::String => {
            let result = alloc(SexpType::String, count)?;
            for index in 0..count {
                let charsxp = element_as_charsxp(list, index)?;
                values::set_elt(result, index, charsxp);
            }
            Ok(result)
        }
        _ if kind.is_atomic() => {
            // GNU R's `isVectorizable`: every element is a vector, a list included, of length
            // at most one. An empty vector or a list element becomes NA.
            for index in 0..count {
                let element = list.element(index);
                let vector = element.kind().is_atomic()
                    || matches!(element.kind(), SexpType::List | SexpType::Expression);
                if !(vector && length(element) <= 1) {
                    return Err(error(format!(
                        "'{}' object cannot be coerced to type '{}'",
                        source.name(),
                        kind.name()
                    )));
                }
            }
            let result = alloc(kind, count)?;
            for index in 0..count {
                let element = list.element(index);
                // GNU R converts each element with `asInteger` and its kin, which warn once per
                // element; an empty or list element is NA. Raw goes through `asInteger` and
                // collects its range warning for the whole list.
                let atomic = element.kind().is_atomic() && length(element) == 1;
                if kind == SexpType::Raw {
                    let value = if atomic {
                        let mut element_warnings = CoercionWarnings::default();
                        let integer =
                            coerce_atomic(element, SexpType::Integer, &mut element_warnings)?;
                        element_warnings.report()?;
                        values::ints(integer)[0]
                    } else {
                        NA_INTEGER
                    };
                    values::raws_mut(result)[index] = match u8::try_from(value) {
                        Ok(byte) => byte,
                        Err(_) => {
                            warnings.raw_range = true;
                            0
                        }
                    };
                } else if atomic {
                    let mut element_warnings = CoercionWarnings::default();
                    let converted = coerce_atomic(element, kind, &mut element_warnings)?;
                    if kind != SexpType::Logical {
                        element_warnings.report()?;
                    }
                    crate::subset::copy_element(converted, 0, result, index)?;
                } else {
                    match kind {
                        SexpType::Logical | SexpType::Integer => {
                            values::ints_mut(result)[index] = NA_INTEGER;
                        }
                        SexpType::Real => values::reals_mut(result)[index] = NA_REAL,
                        SexpType::Complex => {
                            values::complexes_mut(result)[index] = Rcomplex {
                                r: NA_REAL,
                                i: NA_REAL,
                            };
                        }
                        _ => {}
                    }
                }
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
    match (source, kind) {
        (SexpType::Real, SexpType::Integer) => {
            let mut out_of_range = false;
            for (slot, &value) in values::ints_mut(result)
                .iter_mut()
                .zip(values::reals(object))
            {
                // `as i32` saturates and maps NaN to 0, so range and NaN are decided first.
                *slot = if value.is_nan() {
                    NA_INTEGER
                } else if value >= 2147483648.0 || value <= -2147483648.0 {
                    out_of_range = true;
                    NA_INTEGER
                } else {
                    value as i32
                };
            }
            warnings.integer_range |= out_of_range;
            return Ok(result);
        }
        (SexpType::Logical, SexpType::Integer) => {
            values::ints_mut(result).copy_from_slice(values::ints(object));
            return Ok(result);
        }
        (SexpType::Integer | SexpType::Logical, SexpType::Real) => {
            for (slot, &value) in values::reals_mut(result)
                .iter_mut()
                .zip(values::ints(object))
            {
                *slot = if value == NA_INTEGER {
                    NA_REAL
                } else {
                    f64::from(value)
                };
            }
            return Ok(result);
        }
        _ => {}
    }
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
                    SexpType::Complex => complex_to_int(values::complexes(object)[index], warnings),
                    SexpType::String => {
                        integer_from_string(Gc::adopt(object.element(index)), warnings)?
                    }
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
                    SexpType::String => {
                        real_from_string(Gc::adopt(object.element(index)), warnings)?
                    }
                    SexpType::Raw => values::raws(object)[index] as f64,
                    _ => NA_REAL,
                };
            }
        }
        SexpType::Complex => {
            // A double keeps its value, NA included, as the real part. An integer or logical NA
            // gave NA in both parts before R 4.4.0 and gives `NA + 0i` since.
            let integer_na = Rcomplex {
                r: NA_REAL,
                i: if r_at_least(4, 4) { 0.0 } else { NA_REAL },
            };
            let target = values::complexes_mut(result);
            for (index, slot) in target.iter_mut().enumerate() {
                *slot = match source {
                    SexpType::Logical | SexpType::Integer => {
                        let value = values::ints(object)[index];
                        if value == NA_INTEGER {
                            integer_na
                        } else {
                            Rcomplex {
                                r: value as f64,
                                i: 0.0,
                            }
                        }
                    }
                    SexpType::Real => Rcomplex {
                        r: values::reals(object)[index],
                        i: 0.0,
                    },
                    SexpType::String => {
                        complex_from_string(Gc::adopt(object.element(index)), warnings)?
                    }
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
        SexpType::String if matches!(source, SexpType::Real | SexpType::Integer) => {
            let object = values::materialize(object)?;
            write_number_strings(object, result, crate::format::Settings::current_scipen())?;
        }
        SexpType::String if source == SexpType::Logical => {
            let object = values::materialize(object)?;
            let true_text = values::mkchar("TRUE")?;
            let false_text = values::mkchar("FALSE")?;
            let missing = na_string();
            for (slot, &value) in result
                .slice_mut::<Sexp>()
                .iter_mut()
                .zip(values::ints(object))
            {
                *slot = match value {
                    NA_LOGICAL => missing,
                    0 => false_text,
                    _ => true_text,
                };
            }
            crate::heap().count_written_elements(result);
        }
        SexpType::String => {
            for index in 0..count {
                let charsxp = element_as_charsxp(object, index)?;
                values::set_elt(result, index, charsxp);
            }
        }
        SexpType::Raw if matches!(source, SexpType::Logical | SexpType::Integer) => {
            for (slot, &value) in values::raws_mut(result)
                .iter_mut()
                .zip(values::ints(object))
            {
                *slot = match u8::try_from(value) {
                    Ok(byte) => byte,
                    Err(_) => {
                        warnings.raw_range = true;
                        0
                    }
                };
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
                        let (converted, out_of_range) = real_to_int(values::reals(object)[index]);
                        warnings.integer_range |= out_of_range;
                        raw_candidate(converted)
                    }
                    SexpType::String => raw_candidate(integer_from_string(
                        Gc::adopt(object.element(index)),
                        warnings,
                    )?),
                    SexpType::Complex => {
                        raw_candidate(complex_to_int(values::complexes(object)[index], warnings))
                    }
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
            for index in 0..count {
                let element = crate::subset::vector_element(object, index)?;
                values::set_elt(result, index, element);
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

/// Writes `as.character` of the integer or double vector `numbers` into the string vector
/// `result`, of the same length, formatting doubles under scientific-notation penalty `scipen`.
pub unsafe fn write_number_strings(numbers: Sexp, result: Sexp, scipen: i32) -> R<()> {
    if numbers.kind() == SexpType::Real {
        for (index, &value) in values::reals(numbers).iter().enumerate() {
            let charsxp = real_as_charsxp(value, scipen)?;
            values::set_elt(result, index, charsxp);
        }
        return Ok(());
    }
    integers_as_strings(values::ints(numbers), result)
}

/// Element `index` of the integer or double vector `numbers` as `as.character` gives it, with
/// doubles formatted under scientific-notation penalty `scipen`.
pub unsafe fn number_as_charsxp(numbers: Sexp, index: usize, scipen: i32) -> R<Sexp> {
    if numbers.kind() == SexpType::Real {
        real_as_charsxp(values::real_elt(numbers, index)?, scipen)
    } else {
        match values::int_elt(numbers, index)? {
            NA_INTEGER => Ok(na_string()),
            value => {
                Ok(crate::heap()
                    .mk_char(integer_digits(value, &mut [0_u8; 11]), Encoding::Native)?)
            }
        }
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
    let object = values::plain(object)?;
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
        values::refuse_bytes(object)?;
        // A missing first element names the symbol `NA`, as GNU R's `installTrChar` of
        // `NA_STRING` does; only an empty vector has no name to give.
        return match values::as_string_scalar(object)? {
            Some(text) => values::install(&text),
            None if object.kind().is_atomic() && values::length(object) > 0 => {
                values::install("NA")
            }
            None if object.kind().is_atomic() => Err(error(format!(
                "invalid data of mode '{}' (too short)",
                object.kind().name()
            ))),
            None => Err(error("invalid type/length (symbol/0) in vector allocation")),
        };
    }
    if kind == SexpType::Closure {
        if values::is_function(object) {
            return Ok(object);
        }
        return Err(error("cannot coerce to function"));
    }
    let result = if object.kind() == kind {
        object
    } else if kind == SexpType::Pairlist && object.kind().is_atomic() {
        // GNU R makes one cell per element, tagged by the element's name.
        let list = coerce_vector(object, SexpType::List)?;
        crate::heap().root(list);
        coerce_vector(list, kind)?
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
    internal("typeof", 1, |call| unsafe { type_of(call.arg(0)) }),
    internal("vector", 2, do_vector),
    builtin("storage.mode<-", 2, do_set_storage_mode),
    internal("is.vector", 2, do_is_vector),
    builtin("is.null", 1, |call| unsafe {
        scalar_bool(call.arg(0) == nil())
    })
    .unary(|value| unsafe {
        if is_native_value(value) {
            return Ok(None);
        }
        values::scalar_logical((value == nil()) as i32).map(Some)
    }),
    builtin("is.logical", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Logical)
    })
    .unary(|value| unsafe {
        if is_native_value(value) {
            return Ok(None);
        }
        values::scalar_logical((value.kind() == SexpType::Logical) as i32).map(Some)
    }),
    // A factor is stored as integers, but GNU R does not count it as an integer vector.
    builtin("is.integer", 1, |call| unsafe {
        let x = call.arg(0);
        scalar_bool(x.kind() == SexpType::Integer && !values::inherits(x, "factor"))
    }),
    builtin("is.double", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Real)
    }),
    builtin("is.complex", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::Complex)
    }),
    builtin("is.character", 1, |call| unsafe {
        scalar_bool(call.arg(0).kind() == SexpType::String)
    })
    .unary(|value| unsafe {
        if is_native_value(value) {
            return Ok(None);
        }
        values::scalar_logical((value.kind() == SexpType::String) as i32).map(Some)
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
        scalar_bool(values::dims(call.arg(0)).is_some_and(|dims| dims.len() == 2))
    })
    .generic(Generic::Internal),
    builtin("is.array", 1, |call| unsafe {
        scalar_bool(values::dims(call.arg(0)).is_some())
    })
    .generic(Generic::Internal),
    builtin("is.atomic", 1, |call| unsafe {
        {
            // NULL stopped counting as atomic in R 4.4, and R 4.3's own `var()` relies on
            // the older answer.
            let kind = call.arg(0).kind();
            scalar_bool(kind.is_atomic() || (kind == SexpType::Nil && !r_at_least(4, 4)))
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
    })
    .unary(|value| unsafe {
        if is_native_value(value) {
            return Ok(None);
        }
        values::scalar_logical((values::is_function(value)) as i32).map(Some)
    }),
    builtin("is.primitive", 1, |call| unsafe {
        {
            scalar_bool(matches!(
                call.arg(0).kind(),
                SexpType::Builtin | SexpType::Special
            ))
        }
    }),
    builtin("is.single", 1, |call| {
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
    let object = values::plain(call.arg(0))?;
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
        return factor_as_character(object).inspect(|&result| {
            values::clear_attributes(result);
        });
    }
    // As in GNU R, a compact integer sequence becomes a compact double one without expanding.
    // Only the conversion a user sees does this: a builtin's own coercion reads the elements
    // right away, and an expansion on that read would allocate while the builtin holds
    // unrooted results.
    if kind == SexpType::Real
        && object.kind() == SexpType::Integer
        && !values::has_attributes(object)
        && let Some((_, first, increment)) = values::compact_sequence(object)
        && let Some(sequence) = (crate::interp().hooks.compact_sequence)(
            SexpType::Real,
            values::length(object),
            first,
            increment,
        )?
    {
        return Ok(sequence);
    }
    // As in GNU R, a number vector without attributes converts to character lazily, so the
    // strings are made only when something reads them.
    if kind == SexpType::String
        && matches!(object.kind(), SexpType::Integer | SexpType::Real)
        && !values::has_attributes(object)
        && let Some(deferred) = (crate::interp().hooks.defer_strings)(object)?
    {
        return Ok(deferred);
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
    let mode = values::require_str(call.arg(1), "mode")?;
    let object = call.arg(0);
    // GNU R's `coerceVector` makes a number vector without attributes into deferred strings, as
    // `as.character` does.
    if mode == "character"
        && matches!(object.kind(), SexpType::Integer | SexpType::Real)
        && !values::has_attributes(object)
        && let Some(deferred) = (crate::interp().hooks.defer_strings)(object)?
    {
        return Ok(deferred);
    }
    as_vector(object, &mode)
}

unsafe fn do_as_call(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let result = match object.kind() {
        SexpType::Language => return Ok(object),
        SexpType::List | SexpType::Expression => {
            if length(object) == 0 {
                return Err(call.error("invalid length 0 argument"));
            }
            values::list_to_pairlist(object, SexpType::Language, true)?
        }
        SexpType::Pairlist => {
            let items = values::pairlist_items(object);
            values::pairlist(&items, SexpType::Language)?
        }
        SexpType::String => {
            return Err(call.error("as.call(<character>) not feasible; consider str2lang(<char.>)"));
        }
        _ => return Err(call.error("invalid argument list")),
    };
    // The function position carries no name.
    crate::heap().set_tag(result, nil());
    Ok(result)
}

unsafe fn do_as_function(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let env = call.arg(1);
    if values::is_function(list) {
        return Ok(list);
    }
    if list.kind() != SexpType::List {
        return Err(error("list argument expected"));
    }
    if env == nil() {
        return Err(error("use of NULL environment is defunct"));
    }
    if env.kind() != SexpType::Environment {
        return Err(error("invalid environment"));
    }
    if length(list) == 0 {
        return Err(error("argument must have length at least 1"));
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
            return Err(error("invalid formal argument list for \"as.function\""));
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
    let body = list.element(count - 1);
    if !matches!(
        body.kind(),
        SexpType::Nil
            | SexpType::Pairlist
            | SexpType::Language
            | SexpType::Symbol
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
            | SexpType::Bytecode
    ) {
        return Err(error("invalid body for function"));
    }
    crate::eval::make_closure(formals, body, values::nil(), env)
}

unsafe fn do_vector(call: &Call<'_>) -> R<Sexp> {
    let length = call.arg(1);
    if values::length(length) != 1 {
        return Err(error("invalid 'length' argument"));
    }
    let count = crate::random::vector_size(length)?;
    if count < 0 {
        return Err(error("invalid 'length' argument"));
    }
    let count = count as usize;
    let modes = coerce_vector(call.arg(0), SexpType::String)?;
    crate::heap().root(modes);
    if values::length(modes) != 1 {
        return Err(error("invalid 'mode' argument"));
    }
    let mode = char_str(modes.element(0));
    let kind = match mode.as_ref() {
        "any" => SexpType::Logical,
        mode => mode_type(mode)
            .ok_or_else(|| error(format!("vector: cannot make a vector of mode '{mode}'.")))?,
    };
    if kind == SexpType::Pairlist {
        let items = vec![(nil(), nil()); count];
        return values::pairlist(&items, SexpType::Pairlist);
    }
    if !kind.is_vector() {
        return Err(error(format!(
            "vector: cannot make a vector of mode '{mode}'."
        )));
    }
    alloc(kind, count)
}

unsafe fn do_set_storage_mode(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let value = call.arg(1);
    if value.kind() != SexpType::String || length(value) == 0 || value.element(0) == na_string() {
        return Err(error("'value' must be non-null character string"));
    }
    let mode = char_str(value.element(0));
    let kind = mode_type(&mode).ok_or_else(|| {
        error(match mode.as_ref() {
            "real" => "use of 'real' is defunct: use 'double' instead",
            "single" => "use of 'single' is defunct: use mode<- instead",
            _ => "invalid value",
        })
    })?;
    if object.kind() == kind {
        return Ok(object);
    }
    if values::inherits(object, "factor") {
        return Err(error("invalid to change the storage mode of a factor"));
    }
    let result = coerce_vector(object, kind)?;
    if result != object && result.kind().is_vector() {
        crate::heap().root(result);
        values::copy_attributes(object, result)?;
    }
    Ok(result)
}

pub(crate) unsafe fn do_is_vector(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let mode = values::require_str(call.arg(1), "mode")?;
    let kind = object.kind();
    let kind_matches = match mode.as_ref() {
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

/// `typeof(object)` as a character vector, built from the interned type name.
pub unsafe fn type_of(object: Sexp) -> R<Sexp> {
    match object.kind() {
        SexpType::Record => scalar_string(type_name(object)),
        kind => values::scalar_string_charsxp(symbols().type_names[kind as usize]),
    }
}

/// `as.character` on integers into the string vector `result`. Integer data usually repeats a
/// few values (codes, ids, years), so when the values span a range no wider than the vector, each
/// distinct value is formatted and interned once; interning is most of the cost per element.
unsafe fn integers_as_strings(source: &[i32], result: Sexp) -> R<()> {
    let mut low = i32::MAX;
    let mut high = i32::MIN;
    for &value in source {
        if value != NA_INTEGER {
            low = low.min(value);
            high = high.max(value);
        }
    }
    let span = if low <= high {
        i64::from(high) - i64::from(low) + 1
    } else {
        0
    };
    let mut cache: Vec<Option<Sexp>> = if span <= source.len() as i64 {
        vec![None; span as usize]
    } else {
        Vec::new()
    };
    let heap = crate::heap();
    let mut buffer = [0_u8; 11];
    for (index, &value) in source.iter().enumerate() {
        let charsxp = if value == NA_INTEGER {
            na_string()
        } else {
            let slot = cache.get_mut((i64::from(value) - i64::from(low)) as usize);
            match slot {
                Some(Some(charsxp)) => *charsxp,
                slot => {
                    let charsxp =
                        heap.mk_char(integer_digits(value, &mut buffer), Encoding::Native)?;
                    if let Some(slot) = slot {
                        *slot = Some(charsxp);
                    }
                    charsxp
                }
            }
        };
        // Storing at once keeps the string reachable, and the collector does not move objects,
        // so the cached handle stays valid for the rest of the loop.
        heap.set_element(result, index, charsxp);
    }
    Ok(())
}

/// The decimal digits of `value`, written at the end of `buffer`, which fits any `i32`.
pub(crate) fn integer_digits(value: i32, buffer: &mut [u8; 11]) -> &[u8] {
    let mut magnitude = value.unsigned_abs();
    let mut start = buffer.len();
    loop {
        start -= 1;
        buffer[start] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if value < 0 {
        start -= 1;
        buffer[start] = b'-';
    }
    &buffer[start..]
}
