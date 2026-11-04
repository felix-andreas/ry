//! ry's native `I64` in the interpreter. `F64`, `Bool` and `Str` are R's own length-one `double`,
//! `logical` and `character`, so only the `I64` needs a type of its own: a dense vector of `i64`s
//! (`SexpType::I64`, see `heap::object`). A single `I64` is a length-one vector, and a
//! `List<I64>` is the same object at any length with the Array flag set.
//!
//! Arithmetic is checked, so overflow is an error, and an operator on lists works item by item on
//! lists of one length, never recycling.
//!
//! An R number meeting an `I64` in an operator converts to it exactly or raises an error, which is
//! the run-time half of literal typing: `n + 1` with `n` an `I64` is an `I64`.

use {
    crate::{
        arith::Op,
        arrays,
        builtins::Call,
        records,
        standard_library::{arguments, err, ok, wrong},
        unwind::{R, error},
        values::{self, length, nil, scalar_bool, scalar_integer, scalar_real},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, is_na_real},
    std::cmp::Ordering,
};

/// The builtins whose operands may be native numbers: the arithmetic and comparison operators.
pub fn takes_numbers(builtin: &str) -> bool {
    matches!(
        builtin,
        "+" | "-" | "*" | "/" | "^" | "%%" | "%/%" | "==" | "!=" | "<" | ">" | "<=" | ">="
    )
}

/// Whether `value` is a single `I64`, as opposed to a `List<I64>` or an R value.
pub unsafe fn is_number(value: Sexp) -> bool {
    value.kind() == SexpType::I64 && !value.is_array() && value.length() == 1
}

/// `value` as an `i64`, when it is a single `I64`.
pub unsafe fn read(value: Sexp) -> Option<i64> {
    is_number(value).then(|| value.slice::<i64>()[0])
}

/// A fresh single `I64`, rooted for the current scope.
pub unsafe fn alloc(value: i64) -> R<Sexp> {
    let object = values::alloc_uninit(SexpType::I64, 1)?;
    object.slice_mut::<i64>()[0] = value;
    Ok(object)
}

/// A fresh `List<I64>` of `items`, rooted for the current scope.
pub unsafe fn list(items: &[i64]) -> R<Sexp> {
    let object = values::alloc_uninit(SexpType::I64, items.len())?;
    object.slice_mut::<i64>().copy_from_slice(items);
    Ok(arrays::finish(object))
}

/// The `I64` a suffixed literal such as `5i64` denotes, from the literal's text without the
/// suffix. The literal lowering calls this.
pub unsafe fn from_text(text: &str) -> R<Sexp> {
    alloc(parse(text).map_err(error)?)
}

/// Parses decimal (`5`, `-2`, `1e3`) or hexadecimal (`0xFF`) text as an `I64`, which must hold
/// the value exactly.
pub fn parse(text: &str) -> Result<i64, String> {
    let trimmed = text.trim();
    parse_exact(trimmed).map_err(|reason| match reason {
        TextError::Malformed => format!("`{trimmed}` is not a number"),
        TextError::Inexact => format!("{trimmed} does not fit I64"),
    })
}

/// The literal that rebuilds the number, as `print` and `deparse` show it: `5i64`.
pub fn literal(value: i64) -> String {
    format!("{value}i64")
}

/// Whether a literal `match` pattern matches the number: an `I64` pattern of the same value, or
/// an R number that converts exactly to an `I64` and equals it.
pub unsafe fn literal_matches(pattern: Sexp, value: i64) -> bool {
    let pattern = match read(pattern) {
        Some(pattern) => Some(pattern),
        None => r_source(pattern).ok().and_then(convert),
    };
    pattern == Some(value)
}

// Operators.

/// `+`, `-`, `*`, `/`, `^`, `%%` and `%/%` with an `I64` or a `List<I64>` among the operands.
pub unsafe fn arithmetic(call: &Call<'_>, op: Op) -> R<Sexp> {
    let name = call.name();
    match call.args {
        [single] => {
            if single.value.kind() != SexpType::I64 {
                return Err(refusal(call));
            }
            match op {
                Op::Add => Ok(single.value),
                Op::Subtract => {
                    let items = single.value.slice::<i64>();
                    let result = values::alloc_uninit(SexpType::I64, items.len())?;
                    for (slot, &item) in result.slice_mut::<i64>().iter_mut().zip(items) {
                        *slot = negate(item).map_err(|message| call.error(message))?;
                    }
                    Ok(shaped(result, single.value.is_array()))
                }
                _ => Err(call.error("invalid unary operator")),
            }
        }
        [left, right] => {
            let operands = operands(call, left.value, right.value)?;
            let result = values::alloc_uninit(SexpType::I64, operands.count)?;
            let slots = result.slice_mut::<i64>();
            for (position, slot) in slots.iter_mut().enumerate() {
                let (a, b) = operands.at(position);
                *slot = binary(name, op, a, b)
                    .map_err(|message| call.error(operands.locate(message, position)))?;
            }
            Ok(shaped(result, operands.list))
        }
        _ => Err(call.error("operator needs one or two arguments")),
    }
}

/// A comparison with an `I64` or a `List<I64>` among the operands, whose answer `holds` gives
/// from the order of two numbers: a `Bool`, or a `List<Bool>` for lists.
pub unsafe fn comparison(call: &Call<'_>, holds: impl Fn(Ordering) -> bool) -> R<Sexp> {
    let [left, right] = call.args else {
        return Err(call.error("comparison needs two arguments"));
    };
    let operands = operands(call, left.value, right.value)?;
    let result = values::alloc_uninit(SexpType::Logical, operands.count)?;
    for (position, slot) in result.slice_mut::<i32>().iter_mut().enumerate() {
        let (a, b) = operands.at(position);
        *slot = i32::from(holds(a.cmp(&b)));
    }
    Ok(shaped(result, operands.list))
}

/// A fresh result vector as a list when an operand was one, and as a single value otherwise.
unsafe fn shaped(result: Sexp, list: bool) -> Sexp {
    if list { arrays::finish(result) } else { result }
}

/// The operands of a binary operator, at least one of them an `I64` or a `List<I64>`.
struct Operands<'a> {
    left: Operand<'a>,
    right: Operand<'a>,
    /// The number of results: the length of the list operands, or one.
    count: usize,
    list: bool,
}

#[derive(Clone, Copy)]
enum Operand<'a> {
    Single(i64),
    Items(&'a [i64]),
}

impl Operands<'_> {
    fn at(&self, position: usize) -> (i64, i64) {
        let item = |operand: Operand<'_>| match operand {
            Operand::Single(value) => value,
            // `count` is the length of every list operand, so the position is in bounds.
            Operand::Items(items) => items[position],
        };
        (item(self.left), item(self.right))
    }

    /// An item's error message, naming the item when the operator works on lists.
    fn locate(&self, message: String, position: usize) -> String {
        if self.list {
            format!("{message} (at item {})", position + 1)
        } else {
            message
        }
    }
}

/// The two operands of an operator: an R number beside an `I64` converts to one, and two lists
/// must have one length, because a list is never recycled.
unsafe fn operands<'a>(call: &Call<'_>, left: Sexp, right: Sexp) -> R<Operands<'a>> {
    let (left_operand, right_operand) = match (native_operand(left), native_operand(right)) {
        (Some(first), Some(second)) => (first, second),
        // An empty list of another storage holds no item that could disagree with an `I64`, and
        // a typed empty list meets an unannotated `[]`, which is `logical(0)`.
        (Some(first), None) if is_empty_list(right) => (first, Operand::Items(&[])),
        (None, Some(second)) if is_empty_list(left) => (Operand::Items(&[]), second),
        (Some(first), None) => (first, Operand::Single(r_operand(call, right)?)),
        (None, Some(second)) => (Operand::Single(r_operand(call, left)?), second),
        (None, None) => return Err(refusal(call)),
    };
    let count = match (left_operand, right_operand) {
        (Operand::Items(first), Operand::Items(second)) if first.len() != second.len() => {
            return Err(call.error(format!(
                "`{}` needs lists of the same length, but they have {} and {} items; a list is never recycled",
                call.name(),
                first.len(),
                second.len()
            )));
        }
        (Operand::Items(items), _) | (_, Operand::Items(items)) => items.len(),
        _ => 1,
    };
    Ok(Operands {
        left: left_operand,
        right: right_operand,
        count,
        list: left.is_array() || right.is_array(),
    })
}

unsafe fn is_empty_list(value: Sexp) -> bool {
    value.is_array() && value.length() == 0
}

/// An `I64` operand: a single number, or the items of a list.
unsafe fn native_operand<'a>(value: Sexp) -> Option<Operand<'a>> {
    if value.kind() != SexpType::I64 {
        return None;
    }
    let items = value.slice::<i64>();
    Some(match items {
        [single] if !value.is_array() => Operand::Single(*single),
        _ => Operand::Items(items),
    })
}

/// An R operand converted to an `I64`, which must be a plain length-one double or integer.
unsafe fn r_operand(call: &Call<'_>, value: Sexp) -> R<i64> {
    if value.kind() == SexpType::Record {
        return Err(refusal(call));
    }
    if value.is_array() {
        return Err(call.error(format!(
            "`{}` cannot combine I64 with {}; convert one side explicitly",
            call.name(),
            arrays::describe(value)
        )));
    }
    let source = r_source(value).map_err(|problem| {
        call.error(format!(
            "`{}` cannot combine an I64 with {problem}",
            call.name()
        ))
    })?;
    convert(source).ok_or_else(|| call.error(format!("{} does not fit I64", source_text(source))))
}

/// The boundary error for the operand that is a ry value but not a number.
unsafe fn refusal(call: &Call<'_>) -> crate::unwind::Unwind {
    match records::check_native_args(call.call, call.args, true) {
        Err(refused) => refused,
        Ok(()) => call.error("this operator needs numbers"),
    }
}

/// The result of a binary operator on two `I64`s, or the error message.
pub fn binary(name: &str, op: Op, a: i64, b: i64) -> Result<i64, String> {
    let overflowed = || overflow(name);
    match op {
        Op::Add => a.checked_add(b).ok_or_else(overflowed),
        Op::Subtract => a.checked_sub(b).ok_or_else(overflowed),
        Op::Multiply => a.checked_mul(b).ok_or_else(overflowed),
        Op::Divide => Err(
            "`/` divides fractional numbers, and I64 is an integer type; use `%/%` for floored division"
                .to_owned(),
        ),
        Op::IntegerDivide | Op::Modulo => {
            if b == 0 {
                return Err(format!("integer division by zero in `{name}`"));
            }
            // `checked_rem` fails only for the most negative value by -1, whose remainder is 0.
            let remainder = a.checked_rem(b).unwrap_or(0);
            let floored = remainder != 0 && (remainder < 0) != (b < 0);
            if op == Op::Modulo {
                // The remainder and the divisor have opposite signs, so the sum cannot overflow.
                return Ok(if floored { remainder + b } else { remainder });
            }
            let quotient = a.checked_div(b).ok_or_else(overflowed)?;
            // A floored quotient is below zero, so one less still fits.
            Ok(if floored { quotient - 1 } else { quotient })
        }
        Op::Power => {
            if b < 0 {
                return Err("`^` on I64 needs an exponent of zero or more".to_owned());
            }
            match u32::try_from(b) {
                Ok(exponent) => a.checked_pow(exponent).ok_or_else(overflowed),
                Err(_) if a == 0 || a == 1 => Ok(a),
                // -1 to an even power is 1, to an odd power -1.
                Err(_) if a == -1 => Ok(if b & 1 == 0 { 1 } else { -1 }),
                Err(_) => Err(overflowed()),
            }
        }
    }
}

fn overflow(name: &str) -> String {
    format!("integer overflow in `{name}`: the result does not fit I64")
}

/// Unary minus.
pub fn negate(value: i64) -> Result<i64, String> {
    value.checked_neg().ok_or_else(|| overflow("-"))
}

// Conversions.

/// A value a conversion starts from: an exact integer or an R double.
#[derive(Clone, Copy)]
pub enum Source {
    Int(i64),
    F64(f64),
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
            number => Ok(Source::Int(i64::from(number))),
        },
        _ => Err(phrase(value)),
    }
}

/// The value as an error shows it.
pub fn source_text(source: Source) -> String {
    match source {
        Source::Int(value) => value.to_string(),
        Source::F64(value) => double_text(value),
    }
}

/// `source` as an `I64`, or `None` when it is fractional or out of range.
pub fn convert(source: Source) -> Option<i64> {
    // 2^63, the first double past `i64::MAX`.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    match source {
        Source::Int(value) => Some(value),
        Source::F64(value) => {
            (value.is_finite() && value.fract() == 0.0 && (-LIMIT..LIMIT).contains(&value))
                .then(|| value as i64)
        }
    }
}

/// `source` as the nearest R double.
pub fn to_double(source: Source) -> f64 {
    match source {
        Source::Int(value) => value as f64,
        Source::F64(value) => value,
    }
}

fn double_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Inf" } else { "-Inf" }.to_owned()
    } else {
        let magnitude = value.abs();
        if magnitude != 0.0 && !(1e-5..1e16).contains(&magnitude) {
            format!("{value:e}")
        } else {
            value.to_string()
        }
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

/// Whether a library call has an `I64` among its arguments, which sends an existing `Num`
/// function to its `I64` form.
pub unsafe fn has_number_argument(call: &Call<'_>) -> bool {
    call.args.iter().any(|arg| is_number(arg.value))
}

/// A number argument: an `I64`, or a plain R double or integer.
unsafe fn source_argument(call: &Call<'_>, parameter: &str, value: Sexp) -> R<Source> {
    if let Some(number) = read(value) {
        return Ok(Source::Int(number));
    }
    r_source(value).map_err(|_| wrong(call, parameter, "a single number", value))
}

/// `Num.to_i64(number)`: the number as an `I64`, or an error when it does not fit.
pub(crate) unsafe fn num_to_i64(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let source = source_argument(call, "number", value)?;
    match convert(source) {
        Some(number) => alloc(number),
        None => Err(call.error(format!(
            "{} does not fit I64; `Num.to_i64_checked()` returns `:Err(:OutOfBounds)` instead",
            source_text(source)
        ))),
    }
}

/// `Num.to_i64_checked(number)`: `:Ok(value)`, or `:Err(:OutOfBounds)` when it does not fit.
pub(crate) unsafe fn num_to_i64_checked(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    match convert(source_argument(call, "number", value)?) {
        Some(number) => ok(alloc(number)?),
        None => err("OutOfBounds"),
    }
}

/// `Num.to_f64(number)`: the nearest R double.
pub(crate) unsafe fn num_to_f64(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    scalar_real(to_double(source_argument(call, "number", value)?))
}

/// The two numbers of a two-argument library function.
unsafe fn two_numbers(call: &Call<'_>) -> R<(i64, i64)> {
    named_numbers(call, ["first", "second"])
}

unsafe fn library_operand(call: &Call<'_>, parameter: &str, value: Sexp) -> R<i64> {
    let source =
        r_source(value).map_err(|_| wrong(call, parameter, "an I64 or a plain R number", value))?;
    convert(source).ok_or_else(|| {
        call.error(format!(
            "`{parameter}` is {}, which does not fit I64",
            source_text(source)
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

/// `Num.add_wrap` and its siblings on `I64`s. `op` is `+`, `-` or `*`.
pub unsafe fn overflowing(call: &Call<'_>, op: Op, overflow: Overflow) -> R<Sexp> {
    let (a, b) = two_numbers(call)?;
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
    match (value, overflow) {
        (Some(value), Overflow::Check) => ok(alloc(value)?),
        (None, Overflow::Check) => err("Overflow"),
        (Some(value), _) => alloc(value),
        (None, _) => Err(call.error(overflow_message(call.name()))),
    }
}

fn overflow_message(name: &str) -> String {
    format!("`{name}()` overflows: the result does not fit I64")
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

/// `Num.compare` on `I64`s.
pub unsafe fn compare(call: &Call<'_>) -> R<Ordering> {
    let (left, right) = two_numbers(call)?;
    Ok(left.cmp(&right))
}

/// `Num.min` and `Num.max` on `I64`s.
pub unsafe fn extreme(call: &Call<'_>, maximum: bool) -> R<Sexp> {
    let (left, right) = two_numbers(call)?;
    alloc(if maximum {
        left.max(right)
    } else {
        left.min(right)
    })
}

/// `Num.abs` on an `I64`.
pub unsafe fn absolute(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let Some(number) = read(value) else {
        return Err(wrong(call, "number", "a number", value));
    };
    match number.checked_abs() {
        Some(result) => alloc(result),
        None => Err(call.error(overflow_message("Num.abs"))),
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

/// `Num.round`, `Num.floor`, `Num.ceiling` and `Num.truncate` on an R double, whose result is an
/// `I64`.
pub unsafe fn round(call: &Call<'_>, rounding: Rounding) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    let needed = "a fractional number (F64)";
    if is_number(value) {
        return Err(wrong(call, "number", needed, value));
    }
    let whole = match r_source(value) {
        Ok(Source::F64(value)) => Source::F64(match rounding {
            Rounding::Nearest => value.round(),
            Rounding::Floor => value.floor(),
            Rounding::Ceiling => value.ceil(),
            Rounding::Truncate => value.trunc(),
        }),
        Ok(source @ Source::Int(_)) => source,
        Err(_) => return Err(wrong(call, "number", needed, value)),
    };
    match convert(whole) {
        Some(number) => alloc(number),
        None => Err(call.error(format!("{} does not fit I64", source_text(whole)))),
    }
}

/// `Num.to_str` on an `I64`.
pub unsafe fn text_of(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    match read(value) {
        Some(number) => values::scalar_string(&number.to_string()),
        None => Err(wrong(call, "number", "a number", value)),
    }
}

/// `List.range(start, end)`: the `I64`s from `start` up to `end`, or an empty list when `end` is
/// smaller.
pub unsafe fn range(call: &Call<'_>, limit: usize) -> R<Sexp> {
    let [start, end] = arguments(call, ["start", "end"])?;
    let first = match read(start) {
        Some(number) => number,
        None => library_operand(call, "start", start)?,
    };
    let last = match read(end) {
        Some(number) => number,
        None => library_operand(call, "end", end)?,
    };
    // Sized before anything is allocated, so a huge range fails at once.
    let size = if last >= first {
        u128::from(last.abs_diff(first)) + 1
    } else {
        0
    };
    if size > limit as u128 {
        return Err(call.error(format!(
            "the list would have {size} elements, more than the limit of {limit}"
        )));
    }
    let result = values::alloc_uninit(SexpType::I64, size as usize)?;
    for (slot, number) in result.slice_mut::<i64>().iter_mut().zip(first..=last) {
        *slot = number;
    }
    Ok(arrays::finish(result))
}

// The R boundary.

/// `Num.from_r(value)`: a plain length-one double as an `F64` (itself) and an integer as an `I64`.
pub(crate) unsafe fn num_from_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["value"])?;
    let needed = "a single double or integer without attributes";
    match r_source(value) {
        Ok(Source::F64(number)) => scalar_real(number),
        Ok(Source::Int(number)) => alloc(number),
        Err(_) => Err(wrong(call, "value", needed, value)),
    }
}

/// `Num.to_r(number)`: an `I64` as an R integer when it fits 32 bits and is not the bit pattern R
/// reads as `NA`, otherwise as a double, which is an error past 2^53. An R double is itself.
pub(crate) unsafe fn num_to_r(call: &Call<'_>) -> R<Sexp> {
    let [value] = arguments(call, ["number"])?;
    match source_argument(call, "number", value)? {
        Source::Int(number) => to_r(call, number),
        Source::F64(number) => scalar_real(number),
    }
}

/// An `I64` as the R scalar `Num.to_r` gives for it.
pub unsafe fn to_r(call: &Call<'_>, value: i64) -> R<Sexp> {
    const LIMIT: i64 = 1 << 53;
    match i32::try_from(value) {
        Ok(number) if number != NA_INTEGER => scalar_integer(number),
        _ if (-LIMIT..=LIMIT).contains(&value) => scalar_real(value as f64),
        _ => Err(call.error(format!(
            "{value} is past 2^53, so an R double cannot hold it exactly"
        ))),
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
/// a list of `F64`, `I64`, `Bool` or `Str` (an integer and a raw byte both become an `I64`), or
/// the items of an R list converted the same way. Names are dropped; `NA`, a class and other attributes are refused.
pub(crate) unsafe fn list_from_r(call: &Call<'_>) -> R<Sexp> {
    let [vector] = arguments(call, ["vector"])?;
    if vector.is_array() {
        return Ok(vector);
    }
    let vector = values::materialize(vector)?;
    if let SexpType::Logical | SexpType::Real | SexpType::String = vector.kind() {
        if vector.is_object() {
            return Err(call.error(format!(
                "`vector` has the class {}, which a ry list cannot hold; convert it with `as.vector()` first",
                arrays::class_text(vector)
            )));
        }
        // `F64`, `Bool` and `Str` share R's representation, so a vector of them without `NA` is
        // already the packed layout their list uses, and the conversion is one copy.
        if let Some(position) = arrays::first_missing(vector) {
            return Err(call.error(format!(
                "`vector` is `NA` at position {}, which no ry value holds",
                position + 1
            )));
        }
        let copy = values::shallow_duplicate(vector)?;
        values::clear_attributes(copy);
        return Ok(arrays::finish(copy));
    }
    if let SexpType::Integer | SexpType::Raw = vector.kind()
        && vector.attrib() == nil()
    {
        let widened: Vec<i64> = if vector.kind() == SexpType::Integer {
            let numbers = values::ints(vector);
            if let Some(position) = numbers.iter().position(|&number| number == NA_INTEGER) {
                return Err(call.error(format!(
                    "`vector` is `NA` at position {}, which no ry value holds",
                    position + 1
                )));
            }
            numbers.iter().map(|&number| i64::from(number)).collect()
        } else {
            values::raws(vector)
                .iter()
                .map(|&byte| i64::from(byte))
                .collect()
        };
        return list(&widened);
    }
    let items = from_r_items(call, vector)?;
    arrays::from_items(&items)?.map_err(|message| call.error(message))
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
                items.push(alloc(i64::from(number))?);
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
                items.push(alloc(i64::from(byte))?);
            }
        }
        SexpType::List => {
            for position in 0..count {
                let item = vector.element(position);
                if item.kind() == SexpType::Record || item.kind() == SexpType::I64 {
                    items.push(item);
                    continue;
                }
                let converted = from_r_items(call, item)?;
                match converted.as_slice() {
                    [single] if item.kind() != SexpType::List => items.push(*single),
                    _ => items.push(
                        arrays::from_items(&converted)?.map_err(|message| call.error(message))?,
                    ),
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

// Text.

enum TextError {
    Malformed,
    Inexact,
}

/// The exact value of decimal or hexadecimal `text`, which must be whole and fit an `i64`.
fn parse_exact(text: &str) -> Result<i64, TextError> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude: u128 = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TextError::Malformed);
        }
        u128::from_str_radix(hex, 16).map_err(|_| TextError::Inexact)?
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
        let mut power = exponent - fraction.len() as i64;
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
    let signed = if negative {
        i128::try_from(magnitude).map(|value| -value)
    } else {
        i128::try_from(magnitude)
    };
    signed
        .ok()
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(TextError::Inexact)
}

// The rest of `Num`.

/// The `Num` functions on `I64`s that have no dedicated path, picked by the function's name. A
/// library function calls this when one of its arguments is an `I64`.
pub unsafe fn library(call: &Call<'_>) -> R<Sexp> {
    let name = call.name();
    let fail = |message: String| call.error(message);
    match name {
        "Num.add" | "Num.sub" | "Num.mul" | "Num.div" | "Num.pow" | "Num.pow_int" => {
            let op = match name {
                "Num.add" => Op::Add,
                "Num.sub" => Op::Subtract,
                "Num.mul" => Op::Multiply,
                "Num.div" => Op::Divide,
                _ => Op::Power,
            };
            let parameters = if op == Op::Power {
                ["base", "exponent"]
            } else {
                ["first", "second"]
            };
            let (left, right) = named_numbers(call, parameters)?;
            alloc(binary(name, op, left, right).map_err(fail)?)
        }
        "Num.div_trunc" | "Num.div_ceil" | "Num.rem" => {
            let (left, right) = two_numbers(call)?;
            match division(name, left, right).map_err(fail)? {
                Some(result) => alloc(result),
                None => Err(fail(format!(
                    "`second` is zero; `{name}_checked()` returns `:Err(:DivByZero)` instead"
                ))),
            }
        }
        "Num.div_checked" => {
            let (left, right) = two_numbers(call)?;
            ok(alloc(binary(name, Op::Divide, left, right).map_err(fail)?)?)
        }
        "Num.div_trunc_checked" | "Num.div_ceil_checked" | "Num.rem_checked" => {
            let (left, right) = two_numbers(call)?;
            match division(name.trim_end_matches("_checked"), left, right).map_err(fail)? {
                Some(result) => ok(alloc(result)?),
                None => err("DivByZero"),
            }
        }
        "Num.is_lt" | "Num.is_gt" | "Num.is_lte" | "Num.is_gte" => {
            let (left, right) = two_numbers(call)?;
            scalar_bool(match name {
                "Num.is_lt" => left < right,
                "Num.is_gt" => left > right,
                "Num.is_lte" => left <= right,
                _ => left >= right,
            })
        }
        "Num.is_multiple_of" => {
            let (left, right) = two_numbers(call)?;
            // `checked_rem` fails only for the most negative value by -1, a multiple.
            scalar_bool(match right {
                0 => left == 0,
                _ => left.checked_rem(right).unwrap_or(0) == 0,
            })
        }
        "Num.abs_diff" => {
            let (left, right) = two_numbers(call)?;
            let (low, high) = (left.min(right), left.max(right));
            alloc(binary(name, Op::Subtract, high, low).map_err(fail)?)
        }
        "Num.neg" => alloc(negate(one_number(call)?).map_err(fail)?),
        "Num.is_zero" => scalar_bool(one_number(call)? == 0),
        "Num.is_positive" => scalar_bool(one_number(call)? > 0),
        "Num.is_negative" => scalar_bool(one_number(call)? < 0),
        "Num.is_even" => scalar_bool(one_number(call)? % 2 == 0),
        "Num.is_odd" => scalar_bool(one_number(call)? % 2 != 0),
        _ => {
            one_number(call)?;
            Err(fail(format!(
                "`{name}()` takes a fractional number (F64), not an I64; convert it with `Num.to_f64()`"
            )))
        }
    }
}

/// `Num.div_trunc`, `Num.div_ceil` or `Num.rem` on two `I64`s: Roc's truncating quotient and
/// remainder, or the quotient rounded up. `None` for a zero divisor.
fn division(name: &str, a: i64, b: i64) -> Result<Option<i64>, String> {
    if b == 0 {
        return Ok(None);
    }
    let overflowed = || overflow(name);
    // Only the most negative value by -1 overflows, and its remainder is 0.
    let remainder = a.checked_rem(b).unwrap_or(0);
    if name == "Num.rem" {
        return Ok(Some(remainder));
    }
    let quotient = a.checked_div(b).ok_or_else(overflowed)?;
    let rounds_up = name == "Num.div_ceil" && remainder != 0 && (a < 0) == (b < 0);
    Ok(Some(if rounds_up {
        quotient.checked_add(1).ok_or_else(overflowed)?
    } else {
        quotient
    }))
}

unsafe fn one_number(call: &Call<'_>) -> R<i64> {
    let [value] = arguments(call, ["number"])?;
    read(value).ok_or_else(|| wrong(call, "number", "a number", value))
}

unsafe fn named_numbers(call: &Call<'_>, parameters: [&str; 2]) -> R<(i64, i64)> {
    let [first, second] = arguments(call, parameters)?;
    match (read(first), read(second)) {
        (Some(left), Some(right)) => Ok((left, right)),
        (Some(left), None) => Ok((left, library_operand(call, parameters[1], second)?)),
        (None, Some(right)) => Ok((library_operand(call, parameters[0], first)?, right)),
        (None, None) => Err(wrong(call, parameters[0], "a number", first)),
    }
}
