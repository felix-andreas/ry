//! ry's native numbers in the interpreter: `I8` through `U128`, `F32` and `Dec`. `F64`, `Bool` and
//! `Str` are R's own length-one `double`, `logical` and `character`, so only these twelve need an
//! object of their own, a record object whose layout names the type (see `heap::records`).
//!
//! This is the correct, slow implementation the runtime contract in the native-types design asks
//! for: every operation reads the payload, computes in Rust's matching primitive type and boxes
//! the result. Integer arithmetic is checked, so overflow is an error, and `Dec` is a fixed-point
//! `i128` scaled by 10^18 whose products and quotients go through 256-bit intermediates, so only
//! a result that does not fit is an error.
//!
//! An R number meeting a native number in an operator converts to its type exactly or raises an
//! error, which is the run-time half of literal typing: `n + 1` with `n` an `I64` is an `I64`.

use {
    crate::{
        arith::{self, Op},
        builtins::Call,
        records,
        standard_library::{arguments, err, ok, wrong},
        unwind::{R, error},
        values::{self, length, nil, scalar_bool, scalar_integer, scalar_real},
    },
    heap::{
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, is_na_real},
        records::{Fields, Layout},
    },
    std::cmp::Ordering,
};

/// Runs `body` with `T` bound to the Rust primitive of an integer native type, or evaluates
/// `fallback` for `F32` and `Dec`.
macro_rules! with_int {
    ($ty:expr, $t:ident => $body:expr, $fallback:expr) => {
        match $ty {
            NumberType::I8 => {
                type $t = i8;
                $body
            }
            NumberType::I16 => {
                type $t = i16;
                $body
            }
            NumberType::I32 => {
                type $t = i32;
                $body
            }
            NumberType::I64 => {
                type $t = i64;
                $body
            }
            NumberType::I128 => {
                type $t = i128;
                $body
            }
            NumberType::U8 => {
                type $t = u8;
                $body
            }
            NumberType::U16 => {
                type $t = u16;
                $body
            }
            NumberType::U32 => {
                type $t = u32;
                $body
            }
            NumberType::U64 => {
                type $t = u64;
                $body
            }
            NumberType::U128 => {
                type $t = u128;
                $body
            }
            NumberType::F32 | NumberType::Dec => $fallback,
        }
    };
}

pub use heap::records::NumberType;

/// A native number: its type and its 16-byte payload, an integer sign- or zero-extended to 128
/// bits, an `F32` as its bits, or a `Dec` as its value times 10^18.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Number {
    pub ty: NumberType,
    pub bits: u128,
}

/// The builtins whose operands may be native numbers: the arithmetic and comparison operators.
pub fn takes_numbers(builtin: &str) -> bool {
    matches!(
        builtin,
        "+" | "-" | "*" | "/" | "^" | "%%" | "%/%" | "==" | "!=" | "<" | ">" | "<=" | ">="
    )
}

/// The type of `value` when it is a native number.
pub unsafe fn number_type(value: Sexp) -> Option<NumberType> {
    if value.kind() != SexpType::Record {
        return None;
    }
    match crate::heap().layouts.get(value.record_layout())?.fields {
        Fields::Number(ty) => Some(ty),
        _ => None,
    }
}

/// `value` as a number, when it is a native number.
pub unsafe fn read(value: Sexp) -> Option<Number> {
    let ty = number_type(value)?;
    let bits = u128::from(value.record_bits(0)) | (u128::from(value.record_bits(1)) << 64);
    Some(Number { ty, bits })
}

/// A fresh native number object, rooted for the current scope.
pub unsafe fn alloc(number: Number) -> R<Sexp> {
    let heap = crate::heap();
    let layout = heap.layouts.intern(Layout {
        tag: None,
        fields: Fields::Number(number.ty),
    });
    let object = heap
        .alloc_number(layout, number.bits)
        .map_err(|failure| error(failure.to_string()))?;
    heap.root(object);
    Ok(object)
}

/// The native number a suffixed literal such as `5i64` or `1.5dec` denotes, from its type and
/// the literal's text without the suffix. The literal lowering calls this.
pub unsafe fn from_text(ty: NumberType, text: &str) -> R<Sexp> {
    alloc(parse(ty, text).map_err(error)?)
}

/// Parses decimal (`5`, `-2.5`, `1e3`) or hexadecimal (`0xFF`) text as a number of type `ty`. The
/// value must fit exactly, except that an `F32` rounds to the nearest `F32` as a float literal
/// does.
pub fn parse(ty: NumberType, text: &str) -> Result<Number, String> {
    let trimmed = text.trim();
    let failure = |reason: TextError| match reason {
        TextError::Malformed => format!("`{trimmed}` is not a number"),
        TextError::Inexact => format!("{trimmed} does not fit {}", ty.name()),
    };
    match ty {
        NumberType::F32 if !is_hex(trimmed) => {
            let value: f32 = trimmed.parse().map_err(|_| failure(TextError::Malformed))?;
            if value.is_infinite() {
                return Err(failure(TextError::Inexact));
            }
            Ok(Number::f32(value))
        }
        NumberType::F32 => {
            let value = parse_exact(trimmed, 0).map_err(failure)?;
            convert(Source::Int(value), ty).ok_or_else(|| failure(TextError::Inexact))
        }
        NumberType::Dec => {
            let raw = parse_exact(trimmed, DEC_DIGITS).map_err(failure)?;
            match raw {
                Wide::Signed(raw) => Ok(Number::dec(raw)),
                Wide::Big(_) => Err(failure(TextError::Inexact)),
            }
        }
        _ => {
            let value = parse_exact(trimmed, 0).map_err(failure)?;
            convert(Source::Int(value), ty).ok_or_else(|| failure(TextError::Inexact))
        }
    }
}

/// The literal that rebuilds the number, as `print` and `deparse` show it: `5i64`, `1.5dec`,
/// `255u8`. A non-finite `F32` has no literal, so it shows the conversion that builds it.
pub fn literal(number: Number) -> String {
    match number.ty {
        NumberType::F32 => {
            let value = number.as_f32();
            if value.is_finite() {
                format!("{}f32", float_text(f64::from(value), value.to_string()))
            } else {
                format!("Num.to_f32({})", to_text(number))
            }
        }
        ty => format!("{}{}", to_text(number), ty.suffix()),
    }
}

/// The number as `Num.to_str` writes it, without a suffix.
pub fn to_text(number: Number) -> String {
    match number.ty {
        NumberType::F32 => {
            let value = number.as_f32();
            if value.is_nan() {
                "NaN".to_owned()
            } else if value.is_infinite() {
                if value > 0.0 { "Inf" } else { "-Inf" }.to_owned()
            } else {
                float_text(f64::from(value), value.to_string())
            }
        }
        NumberType::Dec => dec_text(number.as_dec()),
        ty => with_int!(ty, T => T::from_bits(number.bits).to_string(), String::new()),
    }
}

/// Whether two numbers are the same value: equal integers or decimals of one type, or `F32`s that
/// are equal or both `NaN`, as `identical()` compares doubles.
pub fn identical(left: Number, right: Number) -> bool {
    if left.ty != right.ty {
        return false;
    }
    match left.ty {
        NumberType::F32 => {
            let (left, right) = (left.as_f32(), right.as_f32());
            left == right || left.is_nan() && right.is_nan()
        }
        _ => left.bits == right.bits,
    }
}

/// The payload a dictionary hashes and compares for the number as a key: the bits, with `-0`
/// folded into `0`, or `None` for a `NaN`, which is no key.
pub fn key_bits(number: Number) -> Option<u128> {
    match number.ty {
        NumberType::F32 => {
            let value = number.as_f32();
            (!value.is_nan()).then(|| u128::from((value + 0.0).to_bits()))
        }
        _ => Some(number.bits),
    }
}

/// Whether a literal `match` pattern matches the number: a native number pattern of the same
/// value, or an R number that converts exactly to the number's type and equals it.
pub unsafe fn literal_matches(pattern: Sexp, number: Number) -> bool {
    let pattern = match read(pattern) {
        Some(pattern) => pattern,
        None => match r_source(pattern)
            .ok()
            .and_then(|source| convert(source, number.ty))
        {
            Some(pattern) => pattern,
            None => return false,
        },
    };
    identical(pattern, number)
}

// Operators.

/// `+`, `-`, `*`, `/`, `^`, `%%` and `%/%` with a native number among the operands.
pub unsafe fn arithmetic(call: &Call<'_>, op: Op) -> R<Sexp> {
    let name = call.name();
    match call.args {
        [single] => {
            let Some(number) = read(single.value) else {
                return Err(refusal(call));
            };
            match op {
                Op::Add => Ok(single.value),
                Op::Subtract => alloc(negate(number).map_err(|message| call.error(message))?),
                _ => Err(call.error("invalid unary operator")),
            }
        }
        [left, right] => {
            let (left, right) = operands(call, left.value, right.value)?;
            alloc(binary(name, op, left, right).map_err(|message| call.error(message))?)
        }
        _ => Err(call.error("operator needs one or two arguments")),
    }
}

/// A comparison with a native number among the operands, whose answer `holds` gives from the
/// order of the two numbers, `None` when an `F32` `NaN` leaves them unordered.
pub unsafe fn comparison(call: &Call<'_>, holds: impl Fn(Option<Ordering>) -> bool) -> R<Sexp> {
    let [left, right] = call.args else {
        return Err(call.error("comparison needs two arguments"));
    };
    let (left, right) = operands(call, left.value, right.value)?;
    scalar_bool(holds(order(left, right)))
}

/// The two operands of an operator as numbers of one type: an R number beside a native number
/// converts to the native number's type.
unsafe fn operands(call: &Call<'_>, left: Sexp, right: Sexp) -> R<(Number, Number)> {
    match (read(left), read(right)) {
        (Some(first), Some(second)) if first.ty == second.ty => Ok((first, second)),
        (Some(first), Some(second)) => Err(call.error(format!(
            "`{}` needs two numbers of one type, not {} and {}; convert one with `Num.to_{}()`",
            call.name(),
            first.ty.name(),
            second.ty.name(),
            first.ty.suffix(),
        ))),
        (Some(first), None) => Ok((first, operand(call, first.ty, right)?)),
        (None, Some(second)) => Ok((operand(call, second.ty, left)?, second)),
        (None, None) => Err(refusal(call)),
    }
}

/// An R operand converted to `ty`, which must be a plain length-one double or integer.
unsafe fn operand(call: &Call<'_>, ty: NumberType, value: Sexp) -> R<Number> {
    if value.kind() == SexpType::Record {
        return Err(refusal(call));
    }
    let source = r_source(value).map_err(|problem| {
        call.error(format!(
            "`{}` cannot combine {} {} with {problem}",
            call.name(),
            article(ty),
            ty.name()
        ))
    })?;
    convert(source, ty).ok_or_else(|| {
        call.error(format!(
            "{} does not fit {}",
            source_text(source),
            ty.name()
        ))
    })
}

/// The boundary error for the operand that is a ry value but not a number.
unsafe fn refusal(call: &Call<'_>) -> crate::unwind::Unwind {
    match records::check_native_args(call.call, call.args, true) {
        Err(refused) => refused,
        Ok(()) => call.error("this operator needs numbers"),
    }
}

fn article(ty: NumberType) -> &'static str {
    match ty {
        NumberType::I8 | NumberType::I16 | NumberType::I32 | NumberType::I64 => "an",
        NumberType::I128 => "an",
        _ => "a",
    }
}

/// The result of a binary operator on two numbers of one type, or the error message.
pub fn binary(name: &str, op: Op, left: Number, right: Number) -> Result<Number, String> {
    let ty = left.ty;
    match ty {
        NumberType::F32 => {
            let (a, b) = (left.as_f32(), right.as_f32());
            let (wide_a, wide_b) = (f64::from(a), f64::from(b));
            Ok(Number::f32(match op {
                Op::Add => a + b,
                Op::Subtract => a - b,
                Op::Multiply => a * b,
                Op::Divide => a / b,
                Op::Power => arith::power(wide_a, wide_b) as f32,
                Op::Modulo => arith::real_modulo(wide_a, wide_b) as f32,
                Op::IntegerDivide => arith::real_integer_divide(wide_a, wide_b) as f32,
            }))
        }
        NumberType::Dec => dec_binary(name, op, left.as_dec(), right.as_dec()).map(Number::dec),
        _ => with_int!(
            ty,
            T => int_binary(name, op, T::from_bits(left.bits), T::from_bits(right.bits), ty)
                .map(|value| Number::int(ty, value)),
            Err(String::new())
        ),
    }
}

fn overflow(name: &str, ty: NumberType) -> String {
    format!(
        "integer overflow in `{name}`: the result does not fit {}",
        ty.name()
    )
}

fn int_binary<T: Int>(name: &str, op: Op, a: T, b: T, ty: NumberType) -> Result<T, String> {
    let overflowed = || overflow(name, ty);
    match op {
        Op::Add => a.checked_add(b).ok_or_else(overflowed),
        Op::Subtract => a.checked_sub(b).ok_or_else(overflowed),
        Op::Multiply => a.checked_mul(b).ok_or_else(overflowed),
        Op::Divide => Err(format!(
            "`/` divides fractional numbers, and {} is an integer type; use `%/%` for floored division",
            ty.name()
        )),
        Op::IntegerDivide | Op::Modulo => {
            if b == T::ZERO {
                return Err(format!("integer division by zero in `{name}`"));
            }
            // `checked_rem` fails only for the most negative value by -1, whose remainder is 0.
            let remainder = a.checked_rem(b).unwrap_or(T::ZERO);
            let floored = remainder != T::ZERO && remainder.is_negative() != b.is_negative();
            if op == Op::Modulo {
                // The remainder and the divisor have opposite signs, so the sum cannot overflow.
                return Ok(if floored {
                    remainder.wrapping_add(b)
                } else {
                    remainder
                });
            }
            let quotient = a.checked_div(b).ok_or_else(overflowed)?;
            // A floored quotient is below zero, so one less still fits.
            Ok(if floored {
                quotient.wrapping_sub(T::ONE)
            } else {
                quotient
            })
        }
        Op::Power => {
            if b.is_negative() {
                return Err(format!(
                    "`^` on {} needs an exponent of zero or more",
                    ty.name()
                ));
            }
            match narrow::<u32>(b.wide()) {
                Some(exponent) => a.checked_pow(exponent).ok_or_else(overflowed),
                None if a == T::ZERO || a == T::ONE => Ok(a),
                // -1 to an even power is 1, to an odd power -1.
                None if a.wide() == Wide::Signed(-1) => {
                    Ok(if b.to_bits() & 1 == 0 { T::ONE } else { a })
                }
                None => Err(overflowed()),
            }
        }
    }
}

fn dec_binary(name: &str, op: Op, a: i128, b: i128) -> Result<i128, String> {
    let overflowed = || format!("`{name}` overflows Dec: the result does not fit");
    match op {
        Op::Add => a.checked_add(b).ok_or_else(overflowed),
        Op::Subtract => a.checked_sub(b).ok_or_else(overflowed),
        Op::Multiply => dec_multiply(a, b).ok_or_else(overflowed),
        Op::Divide => {
            if b == 0 {
                return Err("division by zero in `/` on Dec".to_owned());
            }
            dec_divide(a, b).ok_or_else(overflowed)
        }
        Op::IntegerDivide | Op::Modulo => {
            if b == 0 {
                return Err(format!("division by zero in `{name}` on Dec"));
            }
            let remainder = a.checked_rem(b).unwrap_or(0);
            let floored = remainder != 0 && (remainder < 0) != (b < 0);
            if op == Op::Modulo {
                return Ok(if floored { remainder + b } else { remainder });
            }
            let quotient = a.checked_div(b).ok_or_else(overflowed)?;
            let quotient = if floored { quotient - 1 } else { quotient };
            quotient.checked_mul(DEC_SCALE).ok_or_else(overflowed)
        }
        Op::Power => {
            if b < 0 || b % DEC_SCALE != 0 {
                return Err("`^` on Dec needs a whole exponent of zero or more".to_owned());
            }
            let mut exponent = b / DEC_SCALE;
            let mut base = a;
            let mut result = DEC_SCALE;
            while exponent > 0 {
                if exponent & 1 == 1 {
                    result = dec_multiply(result, base).ok_or_else(overflowed)?;
                }
                exponent >>= 1;
                if exponent > 0 {
                    base = dec_multiply(base, base).ok_or_else(overflowed)?;
                }
            }
            Ok(result)
        }
    }
}

/// Unary minus.
pub fn negate(number: Number) -> Result<Number, String> {
    match number.ty {
        NumberType::F32 => Ok(Number::f32(-number.as_f32())),
        NumberType::Dec => number
            .as_dec()
            .checked_neg()
            .map(Number::dec)
            .ok_or_else(|| "`-` overflows Dec: the result does not fit".to_owned()),
        ty => with_int!(
            ty,
            T => T::from_bits(number.bits)
                .checked_neg()
                .map(|value| Number::int(ty, value))
                .ok_or_else(|| overflow("-", ty)),
            Err(String::new())
        ),
    }
}

/// The order of two numbers of one type, `None` when an `F32` `NaN` leaves them unordered.
pub fn order(left: Number, right: Number) -> Option<Ordering> {
    match left.ty {
        NumberType::F32 => left.as_f32().partial_cmp(&right.as_f32()),
        NumberType::Dec => Some(left.as_dec().cmp(&right.as_dec())),
        ty => with_int!(
            ty,
            T => Some(T::from_bits(left.bits).cmp(&T::from_bits(right.bits))),
            None
        ),
    }
}

// Conversions.

/// A value a conversion starts from: an exact integer, a float, or a decimal's raw value.
#[derive(Clone, Copy)]
pub enum Source {
    Int(Wide),
    F32(f32),
    F64(f64),
    Dec(i128),
}

pub fn source(number: Number) -> Source {
    match number.ty {
        NumberType::F32 => Source::F32(number.as_f32()),
        NumberType::Dec => Source::Dec(number.as_dec()),
        ty => with_int!(ty, T => Source::Int(T::from_bits(number.bits).wide()), Source::Dec(0)),
    }
}

/// A plain length-one R double or integer as a conversion source, or what is wrong with it.
pub unsafe fn r_source(value: Sexp) -> Result<Source, String> {
    let value = values::materialize(value).map_err(|_| "a value it cannot read".to_owned())?;
    let plain = value.attrib() == nil() && length(value) == 1;
    match value.kind() {
        SexpType::Real if plain => {
            let number = values::reals(value)[0];
            if is_na_real(number) {
                Err("`NA`".to_owned())
            } else {
                Ok(Source::F64(number))
            }
        }
        SexpType::Integer if plain => match values::ints(value)[0] {
            NA_INTEGER => Err("`NA`".to_owned()),
            number => Ok(Source::Int(Wide::Signed(i128::from(number)))),
        },
        _ => Err(phrase(value)),
    }
}

/// The value as an error shows it.
pub fn source_text(source: Source) -> String {
    match source {
        Source::Int(Wide::Signed(value)) => value.to_string(),
        Source::Int(Wide::Big(value)) => value.to_string(),
        Source::F32(value) => to_text(Number::f32(value)),
        Source::F64(value) => double_text(value),
        Source::Dec(raw) => dec_text(raw),
    }
}

/// `source` converted to `ty`, or `None` when it does not fit: an integer out of range, a
/// fractional value converted to an integer type, a decimal with more than 18 decimal places, or
/// a finite value too large for an `F32`. A conversion to a float rounds to the nearest value.
pub fn convert(source: Source, ty: NumberType) -> Option<Number> {
    match ty {
        NumberType::F32 => {
            let value = match source {
                Source::Int(Wide::Signed(value)) => value as f32,
                Source::Int(Wide::Big(value)) => value as f32,
                Source::F32(value) => value,
                Source::F64(value) => value as f32,
                Source::Dec(raw) => dec_text(raw).parse().ok()?,
            };
            let finite_source = match source {
                Source::F32(value) => value.is_finite(),
                Source::F64(value) => value.is_finite(),
                Source::Int(_) | Source::Dec(_) => true,
            };
            (value.is_finite() || !finite_source).then(|| Number::f32(value))
        }
        NumberType::Dec => {
            let raw = match source {
                Source::Int(value) => narrow::<i128>(value)?.checked_mul(DEC_SCALE)?,
                Source::F32(value) if value.is_finite() => {
                    exact_dec(&float_text(f64::from(value), value.to_string()))?
                }
                Source::F64(value) if value.is_finite() => {
                    exact_dec(&float_text(value, value.to_string()))?
                }
                Source::F32(_) | Source::F64(_) => return None,
                Source::Dec(raw) => raw,
            };
            Some(Number::dec(raw))
        }
        _ => {
            let value = match source {
                Source::Int(value) => value,
                Source::F32(value) => whole_float(f64::from(value))?,
                Source::F64(value) => whole_float(value)?,
                Source::Dec(raw) => {
                    if raw % DEC_SCALE != 0 {
                        return None;
                    }
                    Wide::Signed(raw / DEC_SCALE)
                }
            };
            with_int!(ty, T => narrow::<T>(value).map(|value| Number::int(ty, value)), None)
        }
    }
}

/// `source` as the nearest R double.
pub fn to_double(source: Source) -> f64 {
    match source {
        Source::Int(Wide::Signed(value)) => value as f64,
        Source::Int(Wide::Big(value)) => value as f64,
        Source::F32(value) => f64::from(value),
        Source::F64(value) => value,
        // The decimal text parses to the correctly rounded double.
        Source::Dec(raw) => dec_text(raw).parse().unwrap_or(f64::NAN),
    }
}

fn whole_float(value: f64) -> Option<Wide> {
    const TWO_127: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    if value >= 0.0 {
        (value < 2.0 * TWO_127).then(|| Wide::from_unsigned(value as u128))
    } else {
        (value >= -TWO_127).then(|| Wide::Signed(value as i128))
    }
}

fn exact_dec(text: &str) -> Option<i128> {
    match parse_exact(text, DEC_DIGITS).ok()? {
        Wide::Signed(raw) => Some(raw),
        Wide::Big(_) => None,
    }
}

/// A float's text for parsing or showing: Rust's shortest round-trip digits, in scientific
/// notation when they would be long.
fn float_text(value: f64, plain: String) -> String {
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-5..1e16).contains(&magnitude) {
        if value as f32 as f64 == value && plain.len() < 20 {
            return plain;
        }
        format!("{:e}", value)
    } else {
        plain
    }
}

fn double_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Inf" } else { "-Inf" }.to_owned()
    } else {
        float_text(value, value.to_string())
    }
}

/// A short phrase for an R value's kind in an operator error.
unsafe fn phrase(value: Sexp) -> String {
    let kind = match value.kind() {
        SexpType::Nil => return "`NULL`".to_owned(),
        SexpType::Logical => "logical",
        SexpType::Integer => "integer",
        SexpType::Real => "double",
        SexpType::Complex => "complex",
        SexpType::String => "character",
        SexpType::Raw => "raw",
        SexpType::List => return "a list".to_owned(),
        kind => return format!("a value of type {}", kind.name()),
    };
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    match length(value) {
        1 if value.attrib() != nil() => format!("{article} {kind} value with attributes"),
        1 => format!("{article} {kind} value"),
        count => format!("{article} {kind} vector of length {count}"),
    }
}

// The standard library's number functions on native numbers.

/// Whether a library call has a native number among its arguments, which sends an existing `Num`
/// function to its native-number form.
pub unsafe fn has_number_argument(call: &Call<'_>) -> bool {
    call.args.iter().any(|arg| number_type(arg.value).is_some())
}

/// A number argument: a native number, or a plain R double or integer.
unsafe fn source_argument(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Source> {
    if let Some(number) = read(value) {
        return Ok(source(number));
    }
    r_source(value).map_err(|_| wrong(call, parameter, "a single number", value))
}

/// `Num.to_i64(number)` and its siblings: the number converted to `ty`, an error when it does not
/// fit, or with `checked`, `:Ok(value)` and `:Err(:OutOfBounds)`.
unsafe fn convert_call(call: &Call<'_>, ty: NumberType, checked: bool) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let source = source_argument(call, "number", value)?;
    match convert(source, ty) {
        Some(number) if checked => ok(alloc(number)?),
        Some(number) => alloc(number),
        None if checked => err("OutOfBounds"),
        None => Err(call.error(format!(
            "{} does not fit {}; `{}_checked()` returns `:Err(:OutOfBounds)` instead",
            source_text(source),
            ty.name(),
            call.name()
        ))),
    }
}

macro_rules! conversions {
    ($($convert:ident $(, $checked:ident)? => $ty:ident;)*) => {
        $(
            pub(crate) unsafe fn $convert(call: &Call<'_>) -> R<Sexp> {
                convert_call(call, NumberType::$ty, false)
            }

            $(
                pub(crate) unsafe fn $checked(call: &Call<'_>) -> R<Sexp> {
                    convert_call(call, NumberType::$ty, true)
                }
            )?
        )*
    };
}

conversions! {
    num_to_i8, num_to_i8_checked => I8;
    num_to_i16, num_to_i16_checked => I16;
    num_to_i32, num_to_i32_checked => I32;
    num_to_i64, num_to_i64_checked => I64;
    num_to_i128, num_to_i128_checked => I128;
    num_to_u8, num_to_u8_checked => U8;
    num_to_u16, num_to_u16_checked => U16;
    num_to_u32, num_to_u32_checked => U32;
    num_to_u64, num_to_u64_checked => U64;
    num_to_u128, num_to_u128_checked => U128;
    num_to_f32 => F32;
    num_to_dec => Dec;
}

/// `Num.to_f64(number)`: the nearest R double.
pub(crate) unsafe fn num_to_f64(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    scalar_real(to_double(source_argument(call, "number", value)?))
}

/// The two numbers of a two-argument library function, of one native type.
unsafe fn two_numbers(call: &Call<'_>) -> R<(Number, Number)> {
    let [first, second] = arguments(call, ["first", "second"])?;
    match (read(first), read(second)) {
        (Some(left), Some(right)) if left.ty == right.ty => Ok((left, right)),
        (Some(left), Some(right)) => Err(call.error(format!(
            "`{}()` needs two numbers of one type, not {} and {}",
            call.name(),
            left.ty.name(),
            right.ty.name()
        ))),
        (Some(left), None) => Ok((left, library_operand(call, "second", left.ty, second)?)),
        (None, Some(right)) => Ok((library_operand(call, "first", right.ty, first)?, right)),
        (None, None) => Err(wrong(call, "first", "a number", first)),
    }
}

unsafe fn library_operand(
    call: &Call<'_>,
    parameter: &str,
    ty: NumberType,
    value: Sexp,
) -> R<Number> {
    let source = r_source(value).map_err(|_| {
        wrong(
            call,
            parameter,
            &format!("{} {} or a plain R number", article(ty), ty.name()),
            value,
        )
    })?;
    convert(source, ty).ok_or_else(|| {
        call.error(format!(
            "`{parameter}` is {}, which does not fit {}",
            source_text(source),
            ty.name()
        ))
    })
}

/// The overflow behaviors of `Num.add_wrap`, `Num.add_saturated` and `Num.add_checked`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Wrap,
    Saturate,
    Check,
}

/// `Num.add_wrap` and its siblings on native numbers. `op` is `+`, `-` or `*`.
pub unsafe fn overflowing(call: &Call<'_>, op: Op, overflow: Overflow) -> R<Sexp> {
    let (left, right) = two_numbers(call)?;
    let ty = left.ty;
    let symbol = match op {
        Op::Add => "+",
        Op::Subtract => "-",
        _ => "*",
    };
    let result: Option<Number> = match ty {
        NumberType::F32 if overflow == Overflow::Wrap => {
            return Err(call.error(format!(
                "`{}()` needs an integer type, not F32",
                call.name()
            )));
        }
        NumberType::Dec if overflow == Overflow::Wrap => {
            return Err(call.error(format!(
                "`{}()` needs an integer type, not Dec",
                call.name()
            )));
        }
        NumberType::F32 => {
            let value = binary(symbol, op, left, right).map_err(|message| call.error(message))?;
            let (a, b, result) = (left.as_f32(), right.as_f32(), value.as_f32());
            if overflow == Overflow::Check {
                return if result.is_infinite() && a.is_finite() && b.is_finite() {
                    err("Overflow")
                } else if result.is_nan() {
                    err("Undefined")
                } else {
                    ok(alloc(value)?)
                };
            }
            Some(value)
        }
        NumberType::Dec => {
            let value = binary(symbol, op, left, right).ok();
            match (value, overflow) {
                (Some(value), Overflow::Check) => return ok(alloc(value)?),
                (None, Overflow::Check) => return err("Overflow"),
                (Some(value), _) => Some(value),
                (None, _) => {
                    let (a, b) = (left.as_dec(), right.as_dec());
                    let negative = match op {
                        Op::Multiply => (a < 0) != (b < 0),
                        _ => a < 0,
                    };
                    Some(Number::dec(if negative { i128::MIN } else { i128::MAX }))
                }
            }
        }
        _ => with_int!(
            ty,
            T => {
                let (a, b) = (T::from_bits(left.bits), T::from_bits(right.bits));
                let value = match (op, overflow) {
                    (Op::Add, Overflow::Wrap) => Some(a.wrapping_add(b)),
                    (Op::Subtract, Overflow::Wrap) => Some(a.wrapping_sub(b)),
                    (_, Overflow::Wrap) => Some(a.wrapping_mul(b)),
                    (Op::Add, Overflow::Saturate) => Some(a.saturating_add(b)),
                    (Op::Subtract, Overflow::Saturate) => Some(a.saturating_sub(b)),
                    (_, Overflow::Saturate) => Some(a.saturating_mul(b)),
                    (Op::Add, Overflow::Check) => a.checked_add(b),
                    (Op::Subtract, Overflow::Check) => a.checked_sub(b),
                    (_, Overflow::Check) => a.checked_mul(b),
                };
                value.map(|value| Number::int(ty, value))
            },
            None
        ),
    };
    match (result, overflow) {
        (Some(value), Overflow::Check) => ok(alloc(value)?),
        (None, Overflow::Check) => err("Overflow"),
        (Some(value), _) => alloc(value),
        (None, _) => Err(call.error(overflow_message(call.name(), ty))),
    }
}

fn overflow_message(name: &str, ty: NumberType) -> String {
    format!(
        "`{name}()` overflows: the result does not fit {}",
        ty.name()
    )
}

pub(crate) unsafe fn num_add_wrap(call: &Call<'_>) -> R<Sexp> {
    overflowing(call, Op::Add, Overflow::Wrap)
}

pub(crate) unsafe fn num_sub_wrap(call: &Call<'_>) -> R<Sexp> {
    overflowing(call, Op::Subtract, Overflow::Wrap)
}

pub(crate) unsafe fn num_mul_wrap(call: &Call<'_>) -> R<Sexp> {
    overflowing(call, Op::Multiply, Overflow::Wrap)
}

/// `Num.min` and `Num.max` on native numbers.
pub unsafe fn extreme(call: &Call<'_>, maximum: bool) -> R<Sexp> {
    let (left, right) = two_numbers(call)?;
    let wanted = if maximum {
        Ordering::Greater
    } else {
        Ordering::Less
    };
    let pick = if order(right, left) == Some(wanted) {
        right
    } else {
        left
    };
    alloc(pick)
}

/// `Num.abs` on a native number.
pub unsafe fn absolute(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let Some(number) = read(value) else {
        return Err(wrong(call, "number", "a number", value));
    };
    let result = match number.ty {
        NumberType::F32 => Some(Number::f32(number.as_f32().abs())),
        NumberType::Dec => number.as_dec().checked_abs().map(Number::dec),
        ty => with_int!(
            ty,
            T => T::from_bits(number.bits).checked_abs().map(|value| Number::int(ty, value)),
            None
        ),
    };
    match result {
        Some(result) => alloc(result),
        None => Err(call.error(overflow_message("Num.abs", number.ty))),
    }
}

/// How `Num.round` and its siblings make a fraction whole.
#[derive(Clone, Copy)]
pub enum Rounding {
    /// Half away from zero, as Roc's `Num.round`.
    Nearest,
    Floor,
    Ceiling,
    Truncate,
}

/// `Num.round`, `Num.floor`, `Num.ceiling` and `Num.truncate` on a fractional number (an `F32`,
/// a `Dec`, or for `Num.truncate` also an R double), whose result is an `I64`.
pub unsafe fn round(call: &Call<'_>, rounding: Rounding) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let needed = "a fractional number (F32, F64 or Dec)";
    let source = match read(value) {
        Some(number) if number.ty.is_integer() => {
            return Err(wrong(call, "number", needed, value));
        }
        Some(number) => source(number),
        None => match r_source(value) {
            Ok(source @ (Source::F64(_) | Source::Int(_))) => source,
            _ => return Err(wrong(call, "number", needed, value)),
        },
    };
    let whole = match source {
        Source::F32(value) => Source::F64(round_float(f64::from(value), rounding)),
        Source::F64(value) => Source::F64(round_float(value, rounding)),
        Source::Dec(raw) => {
            let quotient = raw / DEC_SCALE;
            let remainder = raw % DEC_SCALE;
            let whole = match rounding {
                Rounding::Truncate => quotient,
                Rounding::Floor if remainder < 0 => quotient - 1,
                Rounding::Ceiling if remainder > 0 => quotient + 1,
                Rounding::Floor | Rounding::Ceiling => quotient,
                Rounding::Nearest if remainder.abs() * 2 >= DEC_SCALE => {
                    quotient + remainder.signum()
                }
                Rounding::Nearest => quotient,
            };
            Source::Int(Wide::Signed(whole))
        }
        Source::Int(value) => Source::Int(value),
    };
    match convert(whole, NumberType::I64) {
        Some(number) => alloc(number),
        None => Err(call.error(format!("{} does not fit I64", source_text(whole)))),
    }
}

fn round_float(value: f64, rounding: Rounding) -> f64 {
    match rounding {
        Rounding::Nearest => value.round(),
        Rounding::Floor => value.floor(),
        Rounding::Ceiling => value.ceil(),
        Rounding::Truncate => value.trunc(),
    }
}

/// `Num.to_str` on a native number.
pub unsafe fn text_of(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    match read(value) {
        Some(number) => values::scalar_string(&to_text(number)),
        None => Err(wrong(call, "number", "a number", value)),
    }
}

/// `List.range(start, end)`: the integers from `start` up to `end` in the endpoints' type, `I64`
/// for two R numbers, or an empty list when `end` is smaller.
pub unsafe fn range(call: &Call<'_>, limit: usize) -> R<Sexp> {
    let [start, end] = arguments(call, ["start", "end"])?;
    let (start, end) = match (read(start), read(end)) {
        (Some(left), Some(right)) if left.ty == right.ty => (left, right),
        (Some(left), None) => (left, library_operand(call, "end", left.ty, end)?),
        (None, Some(right)) => (library_operand(call, "start", right.ty, start)?, right),
        // Two R numbers are untyped literals, which count in `I64` as an integer literal types.
        (None, None) => (
            library_operand(call, "start", NumberType::I64, start)?,
            library_operand(call, "end", NumberType::I64, end)?,
        ),
        (Some(left), Some(right)) => {
            return Err(call.error(format!(
                "`List.range()` needs two endpoints of one number type, not {} and {}",
                left.ty.name(),
                right.ty.name()
            )));
        }
    };
    let ty = start.ty;
    if !ty.is_integer() {
        return Err(call.error(format!(
            "`List.range()` counts in an integer type, not {}",
            ty.name()
        )));
    }
    let mut items = Vec::new();
    with_int!(
        ty,
        T => {
            let (mut current, last) = (T::from_bits(start.bits), T::from_bits(end.bits));
            // Sized before anything is allocated, so a huge range fails at once.
            let size = match (current.wide(), last.wide()) {
                (Wide::Signed(first), Wide::Signed(last)) if last >= first => {
                    (last.abs_diff(first)).saturating_add(1)
                }
                (Wide::Signed(first), Wide::Big(last)) => {
                    last.saturating_sub(first as u128).saturating_add(1)
                }
                (Wide::Big(first), Wide::Big(last)) if last >= first => {
                    (last - first).saturating_add(1)
                }
                _ => 0,
            };
            if size > limit as u128 {
                return Err(call.error(format!(
                    "the list would have {size} elements, more than the limit of {limit}"
                )));
            }
            items
                .try_reserve_exact(size as usize)
                .map_err(|_| error(format!("cannot allocate a list of {size} elements")))?;
            while current <= last {
                items.push(alloc(Number::int(ty, current))?);
                match current.checked_add(T::ONE) {
                    Some(next) => current = next,
                    None => break,
                }
            }
        },
        ()
    );
    values::list_vector(&items)
}

// The R boundary.

/// `Num.from_r(value)`: a plain length-one double as an `F64` (itself) and an integer as an `I32`.
pub(crate) unsafe fn num_from_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["value"])?;
    let needed = "a single double or integer without attributes";
    match r_source(value) {
        Ok(Source::F64(number)) => scalar_real(number),
        Ok(Source::Int(Wide::Signed(number))) => alloc(Number::int(NumberType::I32, number as i32)),
        _ => Err(wrong(call, "value", needed, value)),
    }
}

/// `Num.to_r(number)`: an integer type as an R integer when it fits 32 bits and is not the bit
/// pattern R reads as `NA`, otherwise as a double, which is an error past 2^53. An `F32` or a
/// `Dec` becomes the nearest double, and an R double is itself.
pub(crate) unsafe fn num_to_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let source = source_argument(call, "number", value)?;
    match r_number(source) {
        Ok(RNumber::Integer(number)) => scalar_integer(number),
        Ok(RNumber::Double(number)) => scalar_real(number),
        Err(message) => Err(call.error(message)),
    }
}

/// A native number as the R scalar `Num.to_r` gives for it.
pub unsafe fn to_r(call: &Call<'_>, number: Number) -> R<Sexp> {
    match r_number(source(number)) {
        Ok(RNumber::Integer(value)) => scalar_integer(value),
        Ok(RNumber::Double(value)) => scalar_real(value),
        Err(message) => Err(call.error(message)),
    }
}

enum RNumber {
    Integer(i32),
    Double(f64),
}

fn r_number(source: Source) -> Result<RNumber, String> {
    const LIMIT: i128 = 1 << 53;
    match source {
        Source::Int(Wide::Signed(value)) => match i32::try_from(value) {
            Ok(number) if number != NA_INTEGER => Ok(RNumber::Integer(number)),
            _ if (-LIMIT..=LIMIT).contains(&value) => Ok(RNumber::Double(value as f64)),
            _ => Err(format!(
                "{value} is past 2^53, so an R double cannot hold it exactly"
            )),
        },
        Source::Int(Wide::Big(value)) => Err(format!(
            "{value} is past 2^53, so an R double cannot hold it exactly"
        )),
        source => Ok(RNumber::Double(to_double(source))),
    }
}

/// `Bool.from_r(value)`: a plain length-one logical that is not `NA`.
pub(crate) unsafe fn bool_from_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["value"])?;
    let materialized = values::materialize(value)?;
    if materialized.kind() == SexpType::Logical
        && length(materialized) == 1
        && materialized.attrib() == nil()
    {
        match values::ints(materialized)[0] {
            NA_LOGICAL => {}
            flag => return scalar_bool(flag != 0),
        }
    }
    Err(wrong(
        call,
        "value",
        "a single logical without attributes that is not `NA`",
        value,
    ))
}

/// `Str.from_r(value)`: a plain length-one character string that is not `NA`.
pub(crate) unsafe fn str_from_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["value"])?;
    let materialized = values::materialize(value)?;
    if materialized.kind() == SexpType::String
        && length(materialized) == 1
        && materialized.attrib() == nil()
    {
        let text = materialized.element(0);
        if text != values::na_string() {
            return values::scalar_string_charsxp(text);
        }
    }
    Err(wrong(
        call,
        "value",
        "a single string without attributes that is not `NA`",
        value,
    ))
}

/// `List.from_r(vector)`: the elements of a double, integer, logical, character or raw vector as
/// a list of `F64`, `I32`, `Bool`, `Str` or `U8`, or the items of an R list converted the same
/// way. Names are dropped; `NA`, a class and other attributes are refused.
pub(crate) unsafe fn list_from_r(call: &Call<'_>) -> R<Sexp> {
    let [vector] = arguments(call, ["vector"])?;
    let items = from_r_items(call, vector)?;
    values::list_vector(&items)
}

unsafe fn from_r_items(call: &Call<'_>, vector: Sexp) -> R<Vec<Sexp>> {
    let vector = values::materialize(vector)?;
    let mut attribute = vector.attrib();
    while attribute != nil() {
        if attribute.tag() != values::symbols().names {
            return Err(call.error(format!(
                "`vector` has the attribute `{}`, which a ry list cannot hold",
                values::symbol_name(attribute.tag())
            )));
        }
        attribute = attribute.cdr();
    }
    let count = length(vector);
    let missing = |position: usize| {
        call.error(format!(
            "`vector` is `NA` at position {}, which no ry value holds",
            position + 1
        ))
    };
    let mut items = Vec::with_capacity(count);
    match vector.kind() {
        SexpType::Real => {
            for (position, &number) in values::reals(vector).iter().enumerate() {
                if is_na_real(number) {
                    return Err(missing(position));
                }
                items.push(scalar_real(number)?);
            }
        }
        SexpType::Integer => {
            for (position, &number) in values::ints(vector).iter().enumerate() {
                if number == NA_INTEGER {
                    return Err(missing(position));
                }
                items.push(alloc(Number::int(NumberType::I32, number))?);
            }
        }
        SexpType::Logical => {
            for (position, &flag) in values::ints(vector).iter().enumerate() {
                if flag == NA_LOGICAL {
                    return Err(missing(position));
                }
                items.push(scalar_bool(flag != 0)?);
            }
        }
        SexpType::String => {
            for position in 0..count {
                let text = vector.element(position);
                if text == values::na_string() {
                    return Err(missing(position));
                }
                items.push(values::scalar_string_charsxp(text)?);
            }
        }
        SexpType::Raw => {
            for &byte in values::raws(vector) {
                items.push(alloc(Number::int(NumberType::U8, byte))?);
            }
        }
        SexpType::List => {
            for position in 0..count {
                let item = vector.element(position);
                if item.kind() == SexpType::Record {
                    items.push(item);
                    continue;
                }
                let converted = from_r_items(call, item)?;
                match converted.as_slice() {
                    [single] if item.kind() != SexpType::List => items.push(*single),
                    _ => items.push(values::list_vector(&converted)?),
                }
            }
        }
        SexpType::Nil => {}
        _ => {
            return Err(wrong(
                call,
                "vector",
                "a double, integer, logical, character or raw vector, or a list",
                vector,
            ));
        }
    }
    Ok(items)
}

// Representation helpers.

const DEC_DIGITS: u32 = 18;
const DEC_SCALE: i128 = 1_000_000_000_000_000_000;

impl Number {
    fn int<T: Int>(ty: NumberType, value: T) -> Number {
        Number {
            ty,
            bits: value.to_bits(),
        }
    }

    fn f32(value: f32) -> Number {
        Number {
            ty: NumberType::F32,
            bits: u128::from(value.to_bits()),
        }
    }

    fn dec(raw: i128) -> Number {
        Number {
            ty: NumberType::Dec,
            bits: raw as u128,
        }
    }

    fn as_f32(self) -> f32 {
        f32::from_bits(self.bits as u32)
    }

    fn as_dec(self) -> i128 {
        self.bits as i128
    }
}

fn dec_text(raw: i128) -> String {
    let magnitude = raw.unsigned_abs();
    let scale = DEC_SCALE as u128;
    let whole = magnitude / scale;
    let fraction = magnitude % scale;
    let sign = if raw < 0 { "-" } else { "" };
    if fraction == 0 {
        return format!("{sign}{whole}");
    }
    let digits = format!("{fraction:018}");
    format!("{sign}{whole}.{}", digits.trim_end_matches('0'))
}

/// A decimal or decimal product as a 256-bit unsigned integer, least significant limb first.
type U256 = [u64; 4];

fn multiply_wide(left: u128, right: u128) -> U256 {
    let (a0, a1) = (left as u64 as u128, left >> 64);
    let (b0, b1) = (right as u64 as u128, right >> 64);
    let low = a0 * b0;
    let cross_one = a0 * b1;
    let cross_two = a1 * b0;
    let high = a1 * b1;
    let middle = (low >> 64) + (cross_one as u64 as u128) + (cross_two as u64 as u128);
    let top = high + (cross_one >> 64) + (cross_two >> 64) + (middle >> 64);
    [low as u64, middle as u64, top as u64, (top >> 64) as u64]
}

/// The quotient of a 256-bit value by a 128-bit divisor, truncated, when it fits 128 bits.
fn divide_wide(numerator: U256, divisor: u128) -> Option<u128> {
    let mut quotient: U256 = [0; 4];
    let mut remainder: u128 = 0;
    for bit in (0..256).rev() {
        let carry = remainder >> 127;
        remainder = (remainder << 1) | u128::from((numerator[bit / 64] >> (bit % 64)) & 1);
        if carry == 1 || remainder >= divisor {
            remainder = remainder.wrapping_sub(divisor);
            quotient[bit / 64] |= 1 << (bit % 64);
        }
    }
    (quotient[2] == 0 && quotient[3] == 0)
        .then(|| u128::from(quotient[0]) | (u128::from(quotient[1]) << 64))
}

/// A signed decimal raw value from its sign and magnitude, when it fits.
fn signed(negative: bool, magnitude: u128) -> Option<i128> {
    if negative {
        if magnitude == 1 << 127 {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok().map(|value| -value)
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// `a * b` on decimals, truncated toward zero.
fn dec_multiply(a: i128, b: i128) -> Option<i128> {
    let product = multiply_wide(a.unsigned_abs(), b.unsigned_abs());
    let magnitude = divide_wide(product, DEC_SCALE as u128)?;
    signed((a < 0) != (b < 0), magnitude)
}

/// `a / b` on decimals, truncated toward zero. `b` is not zero.
fn dec_divide(a: i128, b: i128) -> Option<i128> {
    let numerator = multiply_wide(a.unsigned_abs(), DEC_SCALE as u128);
    let magnitude = divide_wide(numerator, b.unsigned_abs())?;
    signed((a < 0) != (b < 0), magnitude)
}

/// An exact integer of any of the integer types: `Big` holds the `U128` values past `i128::MAX`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Wide {
    Signed(i128),
    Big(u128),
}

impl Wide {
    fn from_unsigned(value: u128) -> Wide {
        match i128::try_from(value) {
            Ok(value) => Wide::Signed(value),
            Err(_) => Wide::Big(value),
        }
    }
}

fn narrow<T: TryFrom<i128> + TryFrom<u128>>(value: Wide) -> Option<T> {
    match value {
        Wide::Signed(value) => T::try_from(value).ok(),
        Wide::Big(value) => T::try_from(value).ok(),
    }
}

enum TextError {
    Malformed,
    Inexact,
}

fn is_hex(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    digits.starts_with("0x") || digits.starts_with("0X")
}

/// The exact value of decimal or hexadecimal `text` times 10^`scale`, which must be whole.
fn parse_exact(text: &str, scale: u32) -> Result<Wide, TextError> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TextError::Malformed);
        }
        let value = u128::from_str_radix(hex, 16).map_err(|_| TextError::Inexact)?;
        value
            .checked_mul(10u128.checked_pow(scale).ok_or(TextError::Inexact)?)
            .ok_or(TextError::Inexact)?
    } else {
        let (mantissa, exponent) = match digits.find(['e', 'E']) {
            Some(at) => {
                let exponent: i64 = digits[at + 1..].parse().map_err(|_| TextError::Malformed)?;
                (&digits[..at], exponent)
            }
            None => (digits, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(TextError::Malformed);
        }
        let all: String = whole.chars().chain(fraction.chars()).collect();
        let mut significant = all.trim_start_matches('0');
        let mut power = exponent - fraction.len() as i64 + i64::from(scale);
        if power < 0 {
            let dropped = usize::try_from(-power).map_err(|_| TextError::Inexact)?;
            if dropped >= significant.len() {
                if !significant.is_empty() {
                    return Err(TextError::Inexact);
                }
            } else {
                let (kept, tail) = significant.split_at(significant.len() - dropped);
                if tail.bytes().any(|byte| byte != b'0') {
                    return Err(TextError::Inexact);
                }
                significant = kept;
            }
            power = 0;
        }
        let mut value: u128 = 0;
        for byte in significant.bytes() {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u128::from(byte - b'0')))
                .ok_or(TextError::Inexact)?;
        }
        if value != 0 {
            let factor = u32::try_from(power)
                .ok()
                .and_then(|power| 10u128.checked_pow(power))
                .ok_or(TextError::Inexact)?;
            value = value.checked_mul(factor).ok_or(TextError::Inexact)?;
        }
        value
    };
    if negative {
        signed(true, magnitude)
            .map(Wide::Signed)
            .ok_or(TextError::Inexact)
    } else {
        Ok(Wide::from_unsigned(magnitude))
    }
}

/// The operations the interpreter needs from each of Rust's integer types.
trait Int: Copy + Ord + std::fmt::Display + TryFrom<i128> + TryFrom<u128> {
    const ZERO: Self;
    const ONE: Self;
    fn from_bits(bits: u128) -> Self;
    fn to_bits(self) -> u128;
    fn wide(self) -> Wide;
    fn is_negative(self) -> bool;
    fn checked_add(self, other: Self) -> Option<Self>;
    fn checked_sub(self, other: Self) -> Option<Self>;
    fn checked_mul(self, other: Self) -> Option<Self>;
    fn checked_div(self, other: Self) -> Option<Self>;
    fn checked_rem(self, other: Self) -> Option<Self>;
    fn checked_abs(self) -> Option<Self>;
    fn checked_pow(self, exponent: u32) -> Option<Self>;
    fn wrapping_add(self, other: Self) -> Self;
    fn wrapping_sub(self, other: Self) -> Self;
}

macro_rules! int_common {
    ($t:ty) => {
        const ZERO: Self = 0;
        const ONE: Self = 1;
        fn from_bits(bits: u128) -> Self {
            bits as $t
        }
        fn checked_add(self, other: Self) -> Option<Self> {
            <$t>::checked_add(self, other)
        }
        fn checked_sub(self, other: Self) -> Option<Self> {
            <$t>::checked_sub(self, other)
        }
        fn checked_mul(self, other: Self) -> Option<Self> {
            <$t>::checked_mul(self, other)
        }
        fn checked_div(self, other: Self) -> Option<Self> {
            <$t>::checked_div(self, other)
        }
        fn checked_rem(self, other: Self) -> Option<Self> {
            <$t>::checked_rem(self, other)
        }
        fn checked_pow(self, exponent: u32) -> Option<Self> {
            <$t>::checked_pow(self, exponent)
        }
        fn wrapping_add(self, other: Self) -> Self {
            <$t>::wrapping_add(self, other)
        }
        fn wrapping_sub(self, other: Self) -> Self {
            <$t>::wrapping_sub(self, other)
        }
    };
}

macro_rules! signed_int {
    ($($t:ty),*) => {
        $(
            impl Int for $t {
                int_common!($t);
                fn to_bits(self) -> u128 {
                    self as i128 as u128
                }
                fn wide(self) -> Wide {
                    Wide::Signed(self as i128)
                }
                fn is_negative(self) -> bool {
                    self < 0
                }
                fn checked_abs(self) -> Option<Self> {
                    <$t>::checked_abs(self)
                }
            }
        )*
    };
}

macro_rules! unsigned_int {
    ($($t:ty),*) => {
        $(
            impl Int for $t {
                int_common!($t);
                fn to_bits(self) -> u128 {
                    self as u128
                }
                fn wide(self) -> Wide {
                    Wide::from_unsigned(self as u128)
                }
                fn is_negative(self) -> bool {
                    false
                }
                fn checked_abs(self) -> Option<Self> {
                    Some(self)
                }
            }
        )*
    };
}

signed_int!(i8, i16, i32, i64, i128);
unsigned_int!(u8, u16, u32, u64, u128);
