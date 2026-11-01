//! Arithmetic, comparison and logic operators, the `Math` and `Summary` groups, NA tests,
//! sequences and repetition.

use crate::{
    builtins::{Arg, BuiltinDef, Call, Generic, builtin, internal, present},
    coerce::{self, coerce_vector},
    conditions, rmath,
    unwind::{R, error},
    values::{
        self, alloc, get_attr, length, nil, scalar_bool, scalar_integer, scalar_real, symbols,
    },
};
use heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, is_na_real};

unsafe extern "C" {
    fn expm1(x: f64) -> f64;
    fn log1p(x: f64) -> f64;
    // C99's complex functions, which GNU R calls. A `double complex` has the ABI of a struct of
    // two doubles, which `Rcomplex` is.
    fn csqrt(z: Rcomplex) -> Rcomplex;
    fn cexp(z: Rcomplex) -> Rcomplex;
    fn ccos(z: Rcomplex) -> Rcomplex;
    fn csin(z: Rcomplex) -> Rcomplex;
    fn ctan(z: Rcomplex) -> Rcomplex;
    fn cacos(z: Rcomplex) -> Rcomplex;
    fn casin(z: Rcomplex) -> Rcomplex;
    fn catan(z: Rcomplex) -> Rcomplex;
    fn ccosh(z: Rcomplex) -> Rcomplex;
    fn csinh(z: Rcomplex) -> Rcomplex;
}

pub const BUILTINS: &[BuiltinDef] = &[
    internal("intToBits", 1, |call| unsafe {
        let integers = coerce_vector(call.arg(0), SexpType::Integer)?;
        let source = values::ints(integers);
        let result = alloc(SexpType::Raw, source.len() * 32)?;
        for (index, value) in source.iter().enumerate() {
            for bit in 0..32 {
                values::raws_mut(result)[index * 32 + bit] = ((*value as u32 >> bit) & 1) as u8;
            }
        }
        Ok(result)
    }),
    internal("rawToBits", 1, |call| unsafe {
        let raw = call.arg(0);
        if raw.kind() != SexpType::Raw {
            return Err(call.error("argument 'x' must be a raw vector"));
        }
        let source = values::raws(raw);
        let result = alloc(SexpType::Raw, source.len() * 8)?;
        for (index, value) in source.iter().enumerate() {
            for bit in 0..8 {
                values::raws_mut(result)[index * 8 + bit] = (value >> bit) & 1;
            }
        }
        Ok(result)
    }),
    internal("numToBits", 1, |call| unsafe {
        let reals = call.arg(0);
        if reals.kind() != SexpType::Real {
            return Err(call.error("argument 'x' must be a numeric vector"));
        }
        let source = values::reals(reals);
        let result = alloc(SexpType::Raw, source.len() * 64)?;
        for (index, value) in source.iter().enumerate() {
            let bits = value.to_bits();
            for bit in 0..64 {
                values::raws_mut(result)[index * 64 + bit] = ((bits >> bit) & 1) as u8;
            }
        }
        Ok(result)
    }),
    internal("numToInts", 1, |call| unsafe {
        let reals = call.arg(0);
        if reals.kind() != SexpType::Real {
            return Err(call.error("argument 'x' must be a numeric vector"));
        }
        let source = values::reals(reals);
        let result = alloc(SexpType::Integer, source.len() * 2)?;
        for (index, value) in source.iter().enumerate() {
            let bits = value.to_bits();
            values::ints_mut(result)[index * 2] = bits as u32 as i32;
            values::ints_mut(result)[index * 2 + 1] = (bits >> 32) as u32 as i32;
        }
        Ok(result)
    }),
    internal("packBits", 2, do_pack_bits),
    internal("rawShift", 2, |call| unsafe {
        let raw = call.arg(0);
        let shift = values::as_integer_scalar(call.arg(1));
        if raw.kind() != SexpType::Raw {
            return Err(call.error("argument 'x' must be a raw vector"));
        }
        if !(-8..=8).contains(&shift) {
            return Err(call.error("argument 'n' must be a small integer"));
        }
        let source = values::raws(raw);
        let result = alloc(SexpType::Raw, source.len())?;
        for (index, value) in source.iter().enumerate() {
            values::raws_mut(result)[index] = if shift >= 0 {
                ((*value as u32) << shift) as u8
            } else {
                value >> -shift
            };
        }
        Ok(result)
    }),
    builtin("+", -1, |call| unsafe { arithmetic(call, Op::Add) }).generic(Generic::Ops),
    builtin("-", -1, |call| unsafe { arithmetic(call, Op::Subtract) }).generic(Generic::Ops),
    builtin("*", 2, |call| unsafe { arithmetic(call, Op::Multiply) }).generic(Generic::Ops),
    builtin("/", 2, |call| unsafe { arithmetic(call, Op::Divide) }).generic(Generic::Ops),
    builtin("^", 2, |call| unsafe { arithmetic(call, Op::Power) }).generic(Generic::Ops),
    builtin("%%", 2, |call| unsafe { arithmetic(call, Op::Modulo) }).generic(Generic::Ops),
    builtin("%/%", 2, |call| unsafe {
        arithmetic(call, Op::IntegerDivide)
    })
    .generic(Generic::Ops),
    builtin("==", 2, |call| unsafe {
        comparison(call, Comparison::Equal)
    })
    .generic(Generic::Ops),
    builtin("!=", 2, |call| unsafe {
        comparison(call, Comparison::NotEqual)
    })
    .generic(Generic::Ops),
    builtin("<", 2, |call| unsafe { comparison(call, Comparison::Less) }).generic(Generic::Ops),
    builtin(">", 2, |call| unsafe {
        comparison(call, Comparison::Greater)
    })
    .generic(Generic::Ops),
    builtin("<=", 2, |call| unsafe {
        comparison(call, Comparison::LessEqual)
    })
    .generic(Generic::Ops),
    builtin(">=", 2, |call| unsafe {
        comparison(call, Comparison::GreaterEqual)
    })
    .generic(Generic::Ops),
    builtin("&", 2, |call| unsafe { logic(call, true) }).generic(Generic::Ops),
    builtin("|", 2, |call| unsafe { logic(call, false) }).generic(Generic::Ops),
    builtin("!", 1, do_not).generic(Generic::Ops),
    builtin("abs", 1, |call| unsafe { math1(call, Math::Abs) }).generic(Generic::Math),
    builtin("sign", 1, |call| unsafe { math1(call, Math::Sign) }).generic(Generic::Math),
    builtin("sqrt", 1, |call| unsafe { math1(call, Math::Sqrt) }).generic(Generic::Math),
    builtin("floor", 1, |call| unsafe { math1(call, Math::Floor) }).generic(Generic::Math),
    builtin("ceiling", 1, |call| unsafe { math1(call, Math::Ceiling) }).generic(Generic::Math),
    builtin("trunc", -1, |call| unsafe { math1(call, Math::Trunc) }).generic(Generic::Math),
    builtin("exp", 1, |call| unsafe { math1(call, Math::Exp) }).generic(Generic::Math),
    builtin("expm1", 1, |call| unsafe { math1(call, Math::Expm1) }).generic(Generic::Math),
    builtin("log1p", 1, |call| unsafe { math1(call, Math::Log1p) }).generic(Generic::Math),
    builtin("log2", 1, |call| unsafe { math1(call, Math::Log2) }).generic(Generic::Math),
    builtin("log10", 1, |call| unsafe { math1(call, Math::Log10) }).generic(Generic::Math),
    builtin("cos", 1, |call| unsafe { math1(call, Math::Cos) }).generic(Generic::Math),
    builtin("sin", 1, |call| unsafe { math1(call, Math::Sin) }).generic(Generic::Math),
    builtin("tan", 1, |call| unsafe { math1(call, Math::Tan) }).generic(Generic::Math),
    builtin("cospi", 1, |call| unsafe { math1(call, Math::Cospi) }).generic(Generic::Math),
    builtin("sinpi", 1, |call| unsafe { math1(call, Math::Sinpi) }).generic(Generic::Math),
    builtin("tanpi", 1, |call| unsafe { math1(call, Math::Tanpi) }).generic(Generic::Math),
    builtin("acos", 1, |call| unsafe { math1(call, Math::Acos) }).generic(Generic::Math),
    builtin("asin", 1, |call| unsafe { math1(call, Math::Asin) }).generic(Generic::Math),
    builtin("atan", 1, |call| unsafe { math1(call, Math::Atan) }).generic(Generic::Math),
    builtin("cosh", 1, |call| unsafe { math1(call, Math::Cosh) }).generic(Generic::Math),
    builtin("sinh", 1, |call| unsafe { math1(call, Math::Sinh) }).generic(Generic::Math),
    builtin("tanh", 1, |call| unsafe { math1(call, Math::Tanh) }).generic(Generic::Math),
    builtin("acosh", 1, |call| unsafe { math1(call, Math::Acosh) }).generic(Generic::Math),
    builtin("asinh", 1, |call| unsafe { math1(call, Math::Asinh) }).generic(Generic::Math),
    builtin("atanh", 1, |call| unsafe { math1(call, Math::Atanh) }).generic(Generic::Math),
    builtin("gamma", 1, |call| unsafe { math1(call, Math::Gamma) }).generic(Generic::Math),
    builtin("lgamma", 1, |call| unsafe { math1(call, Math::Lgamma) }).generic(Generic::Math),
    builtin("digamma", 1, |call| unsafe { math1(call, Math::Digamma) }).generic(Generic::Math),
    builtin("trigamma", 1, |call| unsafe { math1(call, Math::Trigamma) }).generic(Generic::Math),
    builtin("log", -1, do_log).generic(Generic::Math),
    builtin("round", -1, |call| unsafe { round_like(call, false) }).generic(Generic::Math),
    builtin("signif", -1, |call| unsafe { round_like(call, true) }).generic(Generic::Math),
    builtin("cumsum", 1, |call| unsafe {
        cumulative(call, Cumulative::Sum)
    })
    .generic(Generic::Math),
    builtin("cumprod", 1, |call| unsafe {
        cumulative(call, Cumulative::Product)
    })
    .generic(Generic::Math),
    builtin("cummax", 1, |call| unsafe {
        cumulative(call, Cumulative::Max)
    })
    .generic(Generic::Math),
    builtin("cummin", 1, |call| unsafe {
        cumulative(call, Cumulative::Min)
    })
    .generic(Generic::Math),
    builtin("sum", -1, |call| unsafe { summary(call, Summary::Sum) }).generic(Generic::Summary),
    builtin("prod", -1, |call| unsafe {
        summary(call, Summary::Product)
    })
    .generic(Generic::Summary),
    builtin("max", -1, |call| unsafe { summary(call, Summary::Max) }).generic(Generic::Summary),
    builtin("min", -1, |call| unsafe { summary(call, Summary::Min) }).generic(Generic::Summary),
    builtin("range", -1, do_range).generic(Generic::Summary),
    builtin("all", -1, |call| unsafe { all_any(call, true) }).generic(Generic::Summary),
    builtin("any", -1, |call| unsafe { all_any(call, false) }).generic(Generic::Summary),
    internal("mean", 1, do_mean),
    builtin("is.na", 1, |call| unsafe { na_test(call, NaTest::Na) }).generic(Generic::Internal),
    builtin("is.nan", 1, |call| unsafe { na_test(call, NaTest::NaN) }).generic(Generic::Internal),
    builtin("is.finite", 1, |call| unsafe {
        na_test(call, NaTest::Finite)
    })
    .generic(Generic::Internal),
    builtin("is.infinite", 1, |call| unsafe {
        na_test(call, NaTest::Infinite)
    })
    .generic(Generic::Internal),
    builtin("anyNA", -1, do_any_na).generic(Generic::Internal),
    builtin(":", 2, do_colon),
    builtin("seq_len", 1, do_seq_len),
    builtin("seq_along", 1, do_seq_along),
    builtin("seq.int", -1, do_seq_int).generic(Generic::Internal),
    builtin("rep", -1, do_rep).generic(Generic::Internal),
    internal("rep.int", 2, do_rep_int),
    internal("rep_len", 2, do_rep_len),
    internal("atan2", 2, |call| unsafe { math2(call, f64::atan2) }),
    internal("choose", 2, |call| unsafe { math2(call, rmath::choose) }),
    internal("lchoose", 2, |call| unsafe { math2(call, rmath::lchoose) }),
    internal("beta", 2, |call| unsafe { math2(call, rmath::beta) }),
    internal("lbeta", 2, |call| unsafe { math2(call, rmath::lbeta) }),
    internal("bitwiseAnd", 2, |call| unsafe { bitwise(call, |a, b| a & b) }),
    internal("bitwiseOr", 2, |call| unsafe { bitwise(call, |a, b| a | b) }),
    internal("bitwiseXor", 2, |call| unsafe { bitwise(call, |a, b| a ^ b) }),
    internal("bitwiseNot", 1, do_bitw_not),
    internal("bitwiseShiftL", 2, |call| unsafe {
        bitwise(call, |a, b| ((a as u32) << (b & 31)) as i32)
    }),
    internal("bitwiseShiftR", 2, |call| unsafe {
        bitwise(call, |a, b| ((a as u32) >> (b & 31)) as i32)
    }),
    builtin("xtfrm", 1, |call| unsafe { Ok(call.arg(0)) }).generic(Generic::Internal),
    builtin("Re", 1, |call| unsafe { complex_part(call, 0) }).generic(Generic::Complex),
    builtin("Im", 1, |call| unsafe { complex_part(call, 1) }).generic(Generic::Complex),
    builtin("Mod", 1, |call| unsafe { complex_part(call, 2) }).generic(Generic::Complex),
    builtin("Arg", 1, |call| unsafe { complex_part(call, 3) }).generic(Generic::Complex),
    builtin("Conj", 1, |call| unsafe { complex_part(call, 4) }).generic(Generic::Complex),
    internal("complex", 3, do_complex),
    internal("polyroot", 1, do_polyroot),
    internal("pmin", -1, |call| unsafe { parallel_extreme(call, false) }),
    internal("pmax", -1, |call| unsafe { parallel_extreme(call, true) }),
    internal("cumall", 1, |call| unsafe { Ok(call.arg(0)) }),
    internal("tabulate", 2, do_tabulate),
    internal("which", 1, do_which),
    internal("which.max", 1, |call| unsafe { which_extreme(call, true) }),
    internal("which.min", 1, |call| unsafe { which_extreme(call, false) }),
    internal("trunc", -1, |call| unsafe { math1(call, Math::Trunc) }),
    internal("is.unsorted", 3, do_is_unsorted),
    internal("compareNumericVersion", 2, do_compare_numeric_version),
    internal("colSums", 4, |call| unsafe {
        margin_sums(call, false, false)
    }),
    internal("colMeans", 4, |call| unsafe {
        margin_sums(call, false, true)
    }),
    internal("rowSums", 4, |call| unsafe {
        margin_sums(call, true, false)
    }),
    internal("rowMeans", 4, |call| unsafe {
        margin_sums(call, true, true)
    }),
    internal("findInterval", 5, do_find_interval),
    internal("pretty", 8, do_pretty),
    internal("sequence", 3, do_sequence),
    internal("bincode", 4, do_bincode),
];

/// `compareNumericVersion(x, y)`: -1, 0 or 1 per pair of versions, `NA` for an empty one.
unsafe fn do_compare_numeric_version(call: &Call<'_>) -> R<Sexp> {
    let left = call.arg(0);
    let right = call.arg(1);
    let left_count = length(left);
    let right_count = length(right);
    let count = if left_count == 0 || right_count == 0 {
        0
    } else {
        left_count.max(right_count)
    };
    let result = alloc(SexpType::Integer, count)?;
    for index in 0..count {
        let a = coerce_vector(left.element(index % left_count), SexpType::Integer)?;
        let b = coerce_vector(right.element(index % right_count), SexpType::Integer)?;
        let (a, b) = (values::ints(a), values::ints(b));
        values::ints_mut(result)[index] = if a.is_empty() || b.is_empty() {
            NA_INTEGER
        } else {
            match a.cmp(b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        };
    }
    Ok(result)
}

/// `colSums(x, n, p, na.rm)` and friends over an `n` by `p` matrix.
unsafe fn margin_sums(call: &Call<'_>, rows: bool, mean: bool) -> R<Sexp> {
    let matrix = values::materialize(call.arg(0))?;
    let row_count = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let col_count = values::as_integer_scalar(call.arg(2)).max(0) as usize;
    let na_rm = values::as_logical_scalar(call.arg(3)) == 1;
    if !matches!(
        matrix.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) {
        return Err(call.error("'x' must be numeric"));
    }
    let outer = if rows { row_count } else { col_count };
    let inner = if rows { col_count } else { row_count };
    let result = alloc(SexpType::Real, outer)?;
    for position in 0..outer {
        let mut total = CompensatedSum::default();
        let mut counted = 0usize;
        for along in 0..inner {
            let index = if rows {
                position + along * row_count
            } else {
                along + position * row_count
            };
            let value = real_at(matrix, index);
            if value.is_nan() && na_rm {
                continue;
            }
            total.add(value);
            counted += 1;
        }
        let sum = total.total();
        values::reals_mut(result)[position] = if mean { sum / counted as f64 } else { sum };
    }
    Ok(result)
}

/// `findInterval(vec, x, rightmost.closed, all.inside, left.open)`.
unsafe fn do_find_interval(call: &Call<'_>) -> R<Sexp> {
    let breaks = values::reals(call.arg(0)).to_vec();
    let points = call.arg(1);
    let rightmost_closed = values::as_logical_scalar(call.arg(2)) == 1;
    let all_inside = values::as_logical_scalar(call.arg(3)) == 1;
    let left_open = values::as_logical_scalar(call.arg(4)) == 1;
    let count = breaks.len();
    let result = alloc(SexpType::Integer, length(points))?;
    for index in 0..length(points) {
        let value = values::reals(points)[index];
        if value.is_nan() {
            values::ints_mut(result)[index] = NA_INTEGER;
            continue;
        }
        let mut interval = if left_open {
            breaks
                .iter()
                .filter(|&&breakpoint| breakpoint < value)
                .count()
        } else {
            breaks
                .iter()
                .filter(|&&breakpoint| breakpoint <= value)
                .count()
        } as i32;
        if rightmost_closed && count > 0 {
            if !left_open && value == breaks[count - 1] {
                interval = count as i32 - 1;
            } else if left_open && value == breaks[0] {
                interval = 1;
            }
        }
        if all_inside {
            interval = interval.clamp(1, count as i32 - 1);
        }
        values::ints_mut(result)[index] = interval;
    }
    Ok(result)
}

/// `pretty(lo, up, n, min.n, shrink.sml, c(high.u.bias, u5.bias), eps.correct, bounds)`: GNU R's
/// rule for round tick values covering `[lo, up]` in about `n` steps.
unsafe fn do_pretty(call: &Call<'_>) -> R<Sexp> {
    let biases = call.arg(5);
    let high_bias = crate::subset::numeric_element(biases, 0);
    let five_bias = if length(biases) > 1 {
        crate::subset::numeric_element(biases, 1)
    } else {
        0.5 + 1.5 * high_bias
    };
    let result = pretty(
        values::as_real_scalar(call.arg(0)),
        values::as_real_scalar(call.arg(1)),
        values::as_integer_scalar(call.arg(2)).max(0),
        values::as_integer_scalar(call.arg(3)).max(0),
        values::as_real_scalar(call.arg(4)),
        [high_bias, five_bias],
        values::as_integer_scalar(call.arg(6)),
        values::as_logical_scalar(call.arg(7)) != 0,
    );
    values::named_list(&[
        ("l", scalar_real(result.low)?),
        ("u", scalar_real(result.high)?),
        ("n", scalar_integer(result.count)?),
        ("unit", scalar_real(result.unit)?),
    ])
}

/// The outcome of [`pretty`]: with `bounds`, `low` and `high` are the covering bounds, otherwise
/// they are the bounds in units of `unit`.
pub struct Pretty {
    pub low: f64,
    pub high: f64,
    pub count: i32,
    pub unit: f64,
}

/// The rounding behind `pretty()` and axis ticks: about `count` intervals of 1, 2 or 5 times a
/// power of ten that cover `[low, high]`.
#[allow(clippy::too_many_arguments)]
pub fn pretty(
    mut low: f64,
    mut high: f64,
    count: i32,
    min_count: i32,
    shrink: f64,
    [high_bias, five_bias]: [f64; 2],
    eps_correction: i32,
    bounds: bool,
) -> Pretty {
    let range = high - low;
    let scale = low.abs().max(high.abs());
    let small = if range == 0.0 && scale == 0.0 {
        true
    } else {
        let bias = 1.0
            + if five_bias >= 1.5 * high_bias + 0.5 {
                1.0 / (1.0 + high_bias)
            } else {
                1.5 / (1.0 + five_bias)
            };
        range < scale * bias * count.max(1) as f64 * f64::EPSILON * 3.0
    };
    let mut cell;
    if small {
        cell = if scale == 0.0 { 1.0 } else { scale };
        if scale > 10.0 {
            cell = 9.0 + cell / 10.0;
        }
        cell *= shrink;
        if min_count > 1 {
            cell /= min_count as f64;
        }
    } else {
        cell = range;
        if count > 1 {
            cell /= count as f64;
        }
    }
    if cell < 20.0 * f64::MIN_POSITIVE {
        cell = 20.0 * f64::MIN_POSITIVE;
    } else if cell * 10.0 > f64::MAX {
        cell = 0.1 * f64::MAX;
    }
    let base = 10f64.powf(cell.log10().floor());
    let mut unit = base;
    if 2.0 * base - cell < high_bias * (cell - unit) {
        unit = 2.0 * base;
        if 5.0 * base - cell < five_bias * (cell - unit) {
            unit = 5.0 * base;
            if 10.0 * base - cell < high_bias * (cell - unit) {
                unit = 10.0 * base;
            }
        }
    }
    let mut start = (low / unit + 1e-10).floor();
    let mut end = (high / unit - 1e-10).ceil();
    if eps_correction != 0 && (eps_correction > 1 || !small) {
        if low != 0.0 {
            low *= 1.0 - f64::EPSILON
        } else {
            low = -f64::MIN_POSITIVE
        }
        if high != 0.0 {
            high *= 1.0 + f64::EPSILON
        } else {
            high = f64::MIN_POSITIVE
        }
    }
    while start * unit > low + 1e-10 * unit {
        start -= 1.0;
    }
    while end * unit < high - 1e-10 * unit {
        end += 1.0;
    }
    let mut steps = (0.5 + end - start) as i32;
    if steps < min_count {
        let missing = min_count - steps;
        let (half, rest) = ((missing / 2) as f64, (missing % 2) as f64);
        if start >= 0.0 {
            end += half;
            start -= half + rest;
        } else {
            start -= half;
            end += half + rest;
        }
        steps = min_count;
    }
    if bounds {
        if start * unit < low {
            low = start * unit;
        }
        if end * unit > high {
            high = end * unit;
        }
    } else {
        low = start;
        high = end;
    }
    Pretty {
        low,
        high,
        count: steps,
        unit,
    }
}

/// `sequence(nvec, from, by)`.
unsafe fn do_sequence(call: &Call<'_>) -> R<Sexp> {
    let lengths = coerce_vector(call.arg(0), SexpType::Integer)?;
    let from = coerce_vector(call.arg(1), SexpType::Integer)?;
    let by = coerce_vector(call.arg(2), SexpType::Integer)?;
    let mut output = Vec::new();
    for index in 0..length(lengths) {
        let count = values::ints(lengths)[index];
        let start = values::ints(from)[index % length(from).max(1)];
        let step = values::ints(by)[index % length(by).max(1)];
        for position in 0..count.max(0) {
            output.push(start + position * step);
        }
    }
    values::integer_vector(&output)
}

/// `.bincode(x, breaks, right, include.lowest)`.
unsafe fn do_bincode(call: &Call<'_>) -> R<Sexp> {
    let points = values::reals(call.arg(0)).to_vec();
    let breaks = values::reals(call.arg(1)).to_vec();
    let right = values::as_logical_scalar(call.arg(2)) == 1;
    let lowest = values::as_logical_scalar(call.arg(3)) == 1;
    let count = breaks.len();
    let mut output = Vec::with_capacity(points.len());
    for value in points {
        let mut code = NA_INTEGER;
        if !value.is_nan() && count >= 2 {
            for bin in 0..count - 1 {
                let (lower, upper) = (breaks[bin], breaks[bin + 1]);
                let inside = if right {
                    (value > lower || (lowest && bin == 0 && value == lower)) && value <= upper
                } else {
                    value >= lower
                        && (value < upper || (lowest && bin == count - 2 && value == upper))
                };
                if inside {
                    code = bin as i32 + 1;
                    break;
                }
            }
        }
        output.push(code);
    }
    values::integer_vector(&output)
}

// Arithmetic.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Modulo,
    IntegerDivide,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Subtract => "-",
            Op::Multiply => "*",
            Op::Divide => "/",
            Op::Power => "^",
            Op::Modulo => "%%",
            Op::IntegerDivide => "%/%",
        }
    }
}

unsafe fn arithmetic(call: &Call<'_>, op: Op) -> R<Sexp> {
    match call.args {
        [single] => unary_arithmetic(call, op, single.value),
        [left, right] => binary_arithmetic(call.call, op, left.value, right.value),
        _ => Err(call.error(format!("operator needs one or two arguments"))),
    }
}

unsafe fn unary_arithmetic(call: &Call<'_>, op: Op, operand: Sexp) -> R<Sexp> {
    let operand = values::materialize(operand)?;
    if !matches!(op, Op::Add | Op::Subtract) {
        return Err(call.error(format!("invalid unary operator")));
    }
    let result = match operand.kind() {
        SexpType::Logical => {
            // Coercing to integer makes a new kind of value, so like `!` on a number it keeps
            // only the structural attributes: `+x` of a logical time series is no time series.
            let result = alloc(SexpType::Integer, length(operand))?;
            values::ints_mut(result).copy_from_slice(values::ints(operand));
            for symbol in [symbols().names, symbols().dim, symbols().dimnames] {
                let value = get_attr(operand, symbol);
                if value != nil() {
                    values::set_attr(result, symbol, value)?;
                }
            }
            result
        }
        SexpType::Integer | SexpType::Real | SexpType::Complex => {
            if op == Op::Add {
                return Ok(operand);
            }
            let result = values::shallow_duplicate(operand)?;
            result
        }
        _ => return Err(call.error("invalid argument to unary operator")),
    };
    if op == Op::Subtract {
        match result.kind() {
            SexpType::Integer => {
                for value in values::ints_mut(result) {
                    if *value != NA_INTEGER {
                        *value = -*value;
                    }
                }
            }
            SexpType::Real => {
                for value in values::reals_mut(result) {
                    *value = -*value;
                }
            }
            SexpType::Complex => {
                for value in values::complexes_mut(result) {
                    value.r = -value.r;
                    value.i = -value.i;
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

/// The result attributes of a binary arithmetic operation: dims must agree, names come from the
/// operand as long as the result, and other attributes are copied from both, the first taking
/// precedence.
pub unsafe fn binary_attributes(left: Sexp, right: Sexp, result: Sexp, call: Sexp) -> R<()> {
    check_conformable(left, right, call)?;
    let result_length = length(result);
    let left_attributes = left.attrib() != nil();
    let right_attributes = right.attrib() != nil();
    if !left_attributes && !right_attributes {
        return Ok(());
    }
    if right_attributes && length(right) == result_length {
        values::copy_most_attributes(right, result, false)?;
    }
    if left_attributes && length(left) == result_length {
        values::copy_most_attributes(left, result, false)?;
    }
    copy_array_structure(left, right, result)
}

/// The attributes a comparison or a binary `&` and `|` keep, which GNU R limits to the
/// operands' names, dimensions and dimension names, plus a time series' `tsp` and class.
pub unsafe fn relational_attributes(left: Sexp, right: Sexp, result: Sexp, call: Sexp) -> R<()> {
    check_conformable(left, right, call)?;
    if left.attrib() == nil() && right.attrib() == nil() {
        return Ok(());
    }
    copy_array_structure(left, right, result)?;
    let result_length = length(result);
    for operand in [right, left] {
        let tsp = get_attr(operand, symbols().tsp);
        if tsp != nil() && length(operand) == result_length {
            values::set_attr(result, symbols().tsp, tsp)?;
            values::set_attr(result, symbols().class, get_attr(operand, symbols().class))?;
        }
    }
    Ok(())
}

unsafe fn check_conformable(left: Sexp, right: Sexp, call: Sexp) -> R<()> {
    let left_dims = get_attr(left, symbols().dim);
    let right_dims = get_attr(right, symbols().dim);
    if left_dims != nil() && right_dims != nil() {
        let equal = length(left_dims) == length(right_dims)
            && (0..length(left_dims)).all(|index| {
                crate::subset::numeric_element(left_dims, index)
                    == crate::subset::numeric_element(right_dims, index)
            });
        if !equal {
            return Err(crate::unwind::error_in(call, "non-conformable arrays"));
        }
        return Ok(());
    }
    for (array, dims, other) in [(left, left_dims, right), (right, right_dims, left)] {
        // A zero-length operand makes the result zero-length, so a zero-length array is never
        // too short.
        if dims != nil() && length(array) > 1 && length(other) > length(array) {
            return Err(crate::unwind::error_in(
                call,
                format!(
                    "dims [product {}] do not match the length of object [{}]",
                    length(array),
                    length(other)
                ),
            ));
        }
    }
    Ok(())
}

/// Dimensions, dimension names and names from the operand of the result's length, the left
/// one first.
unsafe fn copy_array_structure(left: Sexp, right: Sexp, result: Sexp) -> R<()> {
    let result_length = length(result);
    for symbol in [symbols().dim, symbols().dimnames] {
        let from_left = get_attr(left, symbol);
        let from_right = get_attr(right, symbol);
        if from_left != nil() && length(left) == result_length {
            values::set_attr(result, symbol, from_left)?;
        } else if from_right != nil() && length(right) == result_length {
            values::set_attr(result, symbol, from_right)?;
        }
    }
    // An array result takes no names, as in GNU R, which copies names only when neither
    // operand is an array.
    if get_attr(result, symbols().dim) != nil() {
        return Ok(());
    }
    let left_names = get_attr(left, symbols().names);
    let right_names = get_attr(right, symbols().names);
    if left_names != nil() && length(left) == result_length {
        values::set_attr(result, symbols().names, left_names)?;
    } else if right_names != nil() && length(right) == result_length {
        values::set_attr(result, symbols().names, right_names)?;
    }
    Ok(())
}

unsafe fn recycled_length(call: Sexp, left: usize, right: usize) -> R<usize> {
    if left == 0 || right == 0 {
        return Ok(0);
    }
    let longer = left.max(right);
    if longer % left.min(right) != 0 {
        conditions::warning_call(
            call,
            "longer object length is not a multiple of shorter object length",
        )?;
    }
    Ok(longer)
}

fn is_arithmetic_type(kind: SexpType) -> bool {
    matches!(
        kind,
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex
    )
}

pub unsafe fn binary_arithmetic(call: Sexp, op: Op, left: Sexp, right: Sexp) -> R<Sexp> {
    let left = values::materialize(left)?;
    let right = values::materialize(right)?;
    let left_kind = if left == nil() {
        SexpType::Integer
    } else {
        left.kind()
    };
    let right_kind = if right == nil() {
        SexpType::Integer
    } else {
        right.kind()
    };
    if !is_arithmetic_type(left_kind) || !is_arithmetic_type(right_kind) {
        return Err(crate::unwind::error_in(
            call,
            "non-numeric argument to binary operator",
        ));
    }
    let left_length = length(left);
    let right_length = length(right);
    let count = recycled_length(call, left_length, right_length)?;
    let result = if left_kind == SexpType::Complex || right_kind == SexpType::Complex {
        complex_arithmetic(op, left, right, count)?
    } else if left_kind == SexpType::Real
        || right_kind == SexpType::Real
        || matches!(op, Op::Divide | Op::Power)
    {
        real_arithmetic(op, left, right, count)?
    } else {
        integer_arithmetic(call, op, left, right, count)?
    };
    binary_attributes(left, right, result, call)?;
    Ok(result)
}

/// Element `index` of a numeric vector as a double.
#[inline]
pub unsafe fn real_at(vector: Sexp, index: usize) -> f64 {
    match vector.kind() {
        SexpType::Real => values::reals(vector)[index],
        SexpType::Integer | SexpType::Logical => {
            let value = values::ints(vector)[index];
            if value == NA_INTEGER {
                NA_REAL
            } else {
                value as f64
            }
        }
        _ => NA_REAL,
    }
}

unsafe fn real_arithmetic(op: Op, left: Sexp, right: Sexp, count: usize) -> R<Sexp> {
    let result = alloc(SexpType::Real, count)?;
    if count == 0 {
        return Ok(result);
    }
    let output = values::reals_mut(result);
    let as_real = |value: i32| {
        if value == NA_INTEGER {
            NA_REAL
        } else {
            value as f64
        }
    };
    match (
        left.kind() == SexpType::Real,
        right.kind() == SexpType::Real,
    ) {
        (true, true) => real_kernel(
            op,
            output,
            values::reals(left),
            values::reals(right),
            |x| x,
            |y| y,
        ),
        (true, false) => real_kernel(
            op,
            output,
            values::reals(left),
            values::ints(right),
            |x| x,
            as_real,
        ),
        (false, true) => real_kernel(
            op,
            output,
            values::ints(left),
            values::reals(right),
            as_real,
            |y| y,
        ),
        (false, false) => real_kernel(
            op,
            output,
            values::ints(left),
            values::ints(right),
            as_real,
            as_real,
        ),
    }
    Ok(result)
}

/// Matches the operator once, outside the element loop, so each arm compiles to its own loop
/// that the compiler can vectorize.
#[inline(always)]
fn real_kernel<A: Copy, B: Copy>(
    op: Op,
    output: &mut [f64],
    left: &[A],
    right: &[B],
    left_real: impl Fn(A) -> f64 + Copy,
    right_real: impl Fn(B) -> f64 + Copy,
) {
    match op {
        Op::Add => recycle(output, left, right, |x, y| left_real(x) + right_real(y)),
        Op::Subtract => recycle(output, left, right, |x, y| left_real(x) - right_real(y)),
        Op::Multiply => recycle(output, left, right, |x, y| left_real(x) * right_real(y)),
        Op::Divide => recycle(output, left, right, |x, y| left_real(x) / right_real(y)),
        Op::Power => recycle(output, left, right, |x, y| {
            power(left_real(x), right_real(y))
        }),
        Op::Modulo => recycle(output, left, right, |x, y| {
            real_modulo(left_real(x), right_real(y))
        }),
        Op::IntegerDivide => recycle(output, left, right, |x, y| {
            (left_real(x) / right_real(y)).floor()
        }),
    }
}

/// Fills `output` with `operation` applied to R's recycled pairs of `left` and `right`. The
/// common shapes (equal lengths, or one scalar operand) get loops without index arithmetic,
/// and the general case advances two wrapping indices instead of computing two remainders per
/// element. Both operands must be non-empty unless `output` is.
#[inline(always)]
fn recycle<A: Copy, B: Copy, T>(
    output: &mut [T],
    left: &[A],
    right: &[B],
    mut operation: impl FnMut(A, B) -> T,
) {
    let count = output.len();
    if left.len() == count && right.len() == count {
        for ((slot, &x), &y) in output.iter_mut().zip(left).zip(right) {
            *slot = operation(x, y);
        }
    } else if left.len() == count && right.len() == 1 {
        let y = right[0];
        for (slot, &x) in output.iter_mut().zip(left) {
            *slot = operation(x, y);
        }
    } else if left.len() == 1 && right.len() == count {
        let x = left[0];
        for (slot, &y) in output.iter_mut().zip(right) {
            *slot = operation(x, y);
        }
    } else {
        let (mut left_index, mut right_index) = (0, 0);
        for slot in output.iter_mut() {
            *slot = operation(left[left_index], right[right_index]);
            left_index += 1;
            if left_index == left.len() {
                left_index = 0;
            }
            right_index += 1;
            if right_index == right.len() {
                right_index = 0;
            }
        }
    }
}

#[inline]
pub fn real_op(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Subtract => a - b,
        Op::Multiply => a * b,
        Op::Divide => a / b,
        Op::Power => power(a, b),
        Op::Modulo => real_modulo(a, b),
        Op::IntegerDivide => (a / b).floor(),
    }
}

/// `^` with R's special cases, which C's `pow` leaves to the platform: `1^y` and `x^0` are 1
/// even for `NA`, and an infinite operand gives a defined limit or `NaN` (`(-Inf)^0.5`).
pub fn power(base: f64, exponent: f64) -> f64 {
    if base == 1.0 || exponent == 0.0 {
        return 1.0;
    }
    if base == 0.0 {
        return if exponent > 0.0 {
            0.0
        } else if exponent < 0.0 {
            f64::INFINITY
        } else {
            exponent
        };
    }
    if base.is_finite() && exponent.is_finite() {
        return if exponent == 2.0 { base * base } else { base.powf(exponent) };
    }
    if base.is_nan() || exponent.is_nan() {
        // GNU R's result carries the exponent's payload when both are NaN, so `NA^NaN` is
        // `NaN` and `NaN^NA` is `NA`. Choosing explicitly keeps that independent of which
        // payload the compiled addition happens to propagate.
        return if exponent.is_nan() { exponent } else { base };
    }
    if base.is_infinite() {
        if base > 0.0 {
            return if exponent < 0.0 { 0.0 } else { f64::INFINITY };
        }
        if exponent.is_finite() && exponent == exponent.floor() {
            return if exponent < 0.0 {
                0.0
            } else if exponent % 2.0 != 0.0 {
                base
            } else {
                -base
            };
        }
    }
    if exponent.is_infinite() && base >= 0.0 {
        let grows = if exponent > 0.0 { base >= 1.0 } else { base < 1.0 };
        return if grows { f64::INFINITY } else { 0.0 };
    }
    f64::NAN
}

pub fn real_modulo(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        return f64::NAN;
    }
    if is_na_real(a) || is_na_real(b) {
        return NA_REAL;
    }
    let quotient = (a / b).floor();
    let remainder = a - quotient * b;
    if remainder.abs() >= b.abs() && b.is_finite() {
        remainder % b
    } else if b.is_infinite() && a.is_finite() {
        if (a >= 0.0) == (b > 0.0) { a } else { b }
    } else {
        remainder
    }
}

unsafe fn integer_arithmetic(call: Sexp, op: Op, left: Sexp, right: Sexp, count: usize) -> R<Sexp> {
    let result = alloc(SexpType::Integer, count)?;
    let a = values::ints(left);
    let b = values::ints(right);
    let mut overflow = false;
    if count == 0 {
        return Ok(result);
    }
    let output = values::ints_mut(result);
    // An overflow is NA with a warning; R's own NA is the one value a checked result may not take.
    let mut checked = |value: Option<i32>| match value {
        Some(value) if value != NA_INTEGER => value,
        _ => {
            overflow = true;
            NA_INTEGER
        }
    };
    let either_na = |x: i32, y: i32| x == NA_INTEGER || y == NA_INTEGER;
    match op {
        Op::Add => recycle(output, a, b, |x, y| {
            if either_na(x, y) {
                NA_INTEGER
            } else {
                checked(x.checked_add(y))
            }
        }),
        Op::Subtract => recycle(output, a, b, |x, y| {
            if either_na(x, y) {
                NA_INTEGER
            } else {
                checked(x.checked_sub(y))
            }
        }),
        Op::Multiply => recycle(output, a, b, |x, y| {
            if either_na(x, y) {
                NA_INTEGER
            } else {
                checked(x.checked_mul(y))
            }
        }),
        Op::Modulo => recycle(output, a, b, |x, y| {
            if either_na(x, y) || y == 0 {
                NA_INTEGER
            } else {
                x.rem_euclid(y) + if y < 0 && x.rem_euclid(y) != 0 { y } else { 0 }
            }
        }),
        Op::IntegerDivide => recycle(output, a, b, |x, y| {
            if either_na(x, y) || y == 0 {
                NA_INTEGER
            } else {
                let value = (x as f64 / y as f64).floor() as i32;
                if value == NA_INTEGER {
                    NA_INTEGER
                } else {
                    value
                }
            }
        }),
        Op::Divide | Op::Power => unreachable!(),
    }
    if overflow {
        conditions::warning_call(call, "NAs produced by integer overflow")?;
    }
    Ok(result)
}

/// Smith's algorithm, which scales by the larger part of the divisor so that neither the squared
/// modulus nor the products overflow.
fn complex_divide(a: Rcomplex, b: Rcomplex) -> Rcomplex {
    if b.r.abs() >= b.i.abs() {
        let ratio = b.i / b.r;
        let denominator = b.r + ratio * b.i;
        Rcomplex {
            r: (a.r + a.i * ratio) / denominator,
            i: (a.i - a.r * ratio) / denominator,
        }
    } else {
        let ratio = b.r / b.i;
        let denominator = b.i + ratio * b.r;
        Rcomplex {
            r: (a.r * ratio + a.i) / denominator,
            i: (a.i * ratio - a.r) / denominator,
        }
    }
}

/// C99 complex multiplication, which GNU R uses: when the plain formula gives NaN in both parts
/// but an operand is infinite, the product is recovered as an infinity (C11 Annex G.5.1), so
/// `1i * complex(real = NaN, imaginary = Inf)` is `-Inf+NaNi`, not `NaN+NaNi`.
fn complex_multiply(x: Rcomplex, y: Rcomplex) -> Rcomplex {
    let (mut a, mut b, mut c, mut d) = (x.r, x.i, y.r, y.i);
    let (ac, bd, ad, bc) = (a * c, b * d, a * d, b * c);
    let real = ac - bd;
    let imaginary = ad + bc;
    if !(real.is_nan() && imaginary.is_nan()) {
        return Rcomplex { r: real, i: imaginary };
    }
    let box_infinity = |value: f64| (if value.is_infinite() { 1.0f64 } else { 0.0 }).copysign(value);
    let zero_nan = |value: f64| if value.is_nan() { 0.0f64.copysign(value) } else { value };
    let mut recalculate = false;
    if a.is_infinite() || b.is_infinite() {
        (a, b) = (box_infinity(a), box_infinity(b));
        (c, d) = (zero_nan(c), zero_nan(d));
        recalculate = true;
    }
    if c.is_infinite() || d.is_infinite() {
        (c, d) = (box_infinity(c), box_infinity(d));
        (a, b) = (zero_nan(a), zero_nan(b));
        recalculate = true;
    }
    if !recalculate && [ac, bd, ad, bc].iter().any(|product| product.is_infinite()) {
        (a, b, c, d) = (zero_nan(a), zero_nan(b), zero_nan(c), zero_nan(d));
        recalculate = true;
    }
    if !recalculate {
        return Rcomplex { r: real, i: imaginary };
    }
    Rcomplex {
        r: f64::INFINITY * (a * c - b * d),
        i: f64::INFINITY * (a * d + b * c),
    }
}

/// `x ^ y` with GNU R's special cases: real powers of zero and of non-negative reals stay real,
/// and a small integer power is repeated multiplication, so `(1+2i)^2` is exact.
fn complex_power(x: Rcomplex, y: Rcomplex) -> Rcomplex {
    if y.r == 0.0 && y.i == 0.0 {
        return Rcomplex { r: 1.0, i: 0.0 };
    }
    if y.i == 0.0 {
        if x.i == 0.0 && (x.r == 0.0 || x.r >= 0.0) {
            return Rcomplex {
                r: power(x.r, y.r),
                i: 0.0,
            };
        }
        if y.r == y.r.trunc() && y.r.abs() <= 65536.0 {
            let mut exponent = y.r.abs() as u32;
            let mut base = x;
            let mut result = Rcomplex { r: 1.0, i: 0.0 };
            while exponent > 0 {
                if exponent & 1 == 1 {
                    result = complex_multiply(result, base);
                }
                exponent >>= 1;
                if exponent > 0 {
                    base = complex_multiply(base, base);
                }
            }
            return if y.r < 0.0 {
                complex_divide(Rcomplex { r: 1.0, i: 0.0 }, result)
            } else {
                result
            };
        }
    }
    let log_modulus = x.r.hypot(x.i).ln();
    let argument = x.i.atan2(x.r);
    let real = y.r * log_modulus - y.i * argument;
    let imaginary = y.i * log_modulus + y.r * argument;
    let scale = real.exp();
    Rcomplex {
        r: scale * imaginary.cos(),
        i: scale * imaginary.sin(),
    }
}

unsafe fn complex_at(vector: Sexp, index: usize) -> Rcomplex {
    match vector.kind() {
        SexpType::Complex => values::complexes(vector)[index],
        _ => {
            let value = real_at(vector, index);
            if is_na_real(value) {
                Rcomplex {
                    r: NA_REAL,
                    i: NA_REAL,
                }
            } else {
                Rcomplex { r: value, i: 0.0 }
            }
        }
    }
}

unsafe fn complex_arithmetic(op: Op, left: Sexp, right: Sexp, count: usize) -> R<Sexp> {
    let result = alloc(SexpType::Complex, count)?;
    let left_length = length(left);
    let right_length = length(right);
    for index in 0..count {
        let a = complex_at(left, index % left_length);
        let b = complex_at(right, index % right_length);
        let value = match op {
            Op::Add => Rcomplex {
                r: a.r + b.r,
                i: a.i + b.i,
            },
            Op::Subtract => Rcomplex {
                r: a.r - b.r,
                i: a.i - b.i,
            },
            Op::Multiply => complex_multiply(a, b),
            Op::Divide => complex_divide(a, b),
            Op::Power => complex_power(a, b),
            _ => return Err(error("invalid operation on complex numbers")),
        };
        values::complexes_mut(result)[index] = value;
    }
    Ok(result)
}

// Comparison.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

impl Comparison {
    fn holds(self, ordering: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Comparison::Equal => ordering == Equal,
            Comparison::NotEqual => ordering != Equal,
            Comparison::Less => ordering == Less,
            Comparison::Greater => ordering == Greater,
            Comparison::LessEqual => ordering != Greater,
            Comparison::GreaterEqual => ordering != Less,
        }
    }
}

/// A language object as the string comparison sees it.
unsafe fn comparable(value: Sexp) -> R<Sexp> {
    match value.kind() {
        SexpType::Symbol => values::scalar_string_charsxp(value.car()),
        SexpType::Language => values::scalar_string(&crate::deparse::deparse_one_line(value)),
        _ => Ok(value),
    }
}

unsafe fn comparison(call: &Call<'_>, comparison: Comparison) -> R<Sexp> {
    if call.len() != 2 {
        return Err(call.error("comparison is possible only for atomic and list types"));
    }
    let left = values::materialize(comparable(call.arg(0))?)?;
    let right = values::materialize(comparable(call.arg(1))?)?;
    let allowed = |value: Sexp| value.kind().is_atomic() || value.kind() == SexpType::List || value == nil();
    if !allowed(left) || !allowed(right) {
        return Err(call.error(format!(
            "comparison ({}) is possible only for atomic and list types",
            call.name()
        )));
    }
    let left_length = length(left);
    let right_length = length(right);
    let count = recycled_length(call.call, left_length, right_length)?;
    let result = alloc(SexpType::Logical, count)?;
    // A list operand takes the other operand's type, element by element, as GNU R's `relop`
    // coerces it; two lists have no common type.
    let operand_kind = |value: Sexp| if value == nil() { SexpType::Logical } else { value.kind() };
    let kind = match (operand_kind(left), operand_kind(right)) {
        (SexpType::List, SexpType::List) => {
            return Err(call.error("comparison of these types is not implemented"));
        }
        (SexpType::List, other) | (other, SexpType::List) => other,
        (left, right) => coerce::higher_type(left, right),
    };
    let (left, right) = if left.kind() == SexpType::List || right.kind() == SexpType::List {
        (coerce_vector(left, kind)?, coerce_vector(right, kind)?)
    } else {
        (left, right)
    };
    match kind {
        SexpType::String => {
            let left = coerce_vector(left, SexpType::String)?;
            let right = coerce_vector(right, SexpType::String)?;
            for index in 0..count {
                let a = left.element(index % left_length);
                let b = right.element(index % right_length);
                values::ints_mut(result)[index] = if a == values::na_string()
                    || b == values::na_string()
                {
                    NA_LOGICAL
                } else if matches!(comparison, Comparison::Equal | Comparison::NotEqual) {
                    comparison.holds(if a == b {
                        std::cmp::Ordering::Equal
                    } else {
                        std::cmp::Ordering::Less
                    }) as i32
                } else {
                    let ordering = crate::sort::collate(&values::char_str(a), &values::char_str(b));
                    comparison.holds(ordering) as i32
                };
            }
        }
        SexpType::Complex => {
            if !matches!(comparison, Comparison::Equal | Comparison::NotEqual) {
                return Err(call.error("invalid comparison with complex values"));
            }
            for index in 0..count {
                let a = complex_at(left, index % left_length);
                let b = complex_at(right, index % right_length);
                values::ints_mut(result)[index] =
                    if a.r.is_nan() || a.i.is_nan() || b.r.is_nan() || b.i.is_nan() {
                        NA_LOGICAL
                    } else {
                        let equal = a.r == b.r && a.i == b.i;
                        (equal == (comparison == Comparison::Equal)) as i32
                    };
            }
        }
        SexpType::Raw if left.kind() == SexpType::Raw && right.kind() == SexpType::Raw => {
            for index in 0..count {
                let a = values::raws(left)[index % left_length];
                let b = values::raws(right)[index % right_length];
                values::ints_mut(result)[index] = comparison.holds(a.cmp(&b)) as i32;
            }
        }
        SexpType::Integer | SexpType::Logical
            if left.kind() != SexpType::Real && right.kind() != SexpType::Real =>
        {
            let left = coerce_vector(left, SexpType::Integer)?;
            let right = coerce_vector(right, SexpType::Integer)?;
            let a = values::ints(left);
            let b = values::ints(right);
            for index in 0..count {
                let x = a[index % left_length];
                let y = b[index % right_length];
                values::ints_mut(result)[index] = if x == NA_INTEGER || y == NA_INTEGER {
                    NA_LOGICAL
                } else {
                    comparison.holds(x.cmp(&y)) as i32
                };
            }
        }
        _ => {
            let left = coerce_vector(left, SexpType::Real)?;
            let right = coerce_vector(right, SexpType::Real)?;
            let a = values::reals(left);
            let b = values::reals(right);
            for index in 0..count {
                let x = a[index % left_length];
                let y = b[index % right_length];
                values::ints_mut(result)[index] = if x.is_nan() || y.is_nan() {
                    NA_LOGICAL
                } else {
                    comparison.holds(x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)) as i32
                };
            }
        }
    }
    relational_attributes(call.arg(0), call.arg(1), result, call.call)?;
    Ok(result)
}

// Logic.

unsafe fn logical_at(vector: Sexp, index: usize) -> i32 {
    match vector.kind() {
        SexpType::Logical => values::ints(vector)[index],
        SexpType::Integer => {
            let value = values::ints(vector)[index];
            if value == NA_INTEGER {
                NA_LOGICAL
            } else {
                (value != 0) as i32
            }
        }
        SexpType::Real => {
            let value = values::reals(vector)[index];
            if value.is_nan() {
                NA_LOGICAL
            } else {
                (value != 0.0) as i32
            }
        }
        SexpType::Complex => {
            let value = values::complexes(vector)[index];
            if value.r.is_nan() {
                NA_LOGICAL
            } else {
                (value.r != 0.0 || value.i != 0.0) as i32
            }
        }
        _ => NA_LOGICAL,
    }
}

unsafe fn logic(call: &Call<'_>, is_and: bool) -> R<Sexp> {
    let left = values::materialize(call.arg(0))?;
    let right = values::materialize(call.arg(1))?;
    if left.kind() == SexpType::Raw && right.kind() == SexpType::Raw {
        let left_length = length(left);
        let right_length = length(right);
        let count = recycled_length(call.call, left_length, right_length)?;
        let result = alloc(SexpType::Raw, count)?;
        for index in 0..count {
            let a = values::raws(left)[index % left_length];
            let b = values::raws(right)[index % right_length];
            values::raws_mut(result)[index] = if is_and { a & b } else { a | b };
        }
        return Ok(result);
    }
    let valid = |value: Sexp| {
        value == nil()
            || matches!(
                value.kind(),
                SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex
            )
    };
    if !valid(left) || !valid(right) {
        return Err(call.error(format!(
            "operations are possible only for numeric, logical or complex types"
        )));
    }
    let left_length = length(left);
    let right_length = length(right);
    let count = recycled_length(call.call, left_length, right_length)?;
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let a = logical_at(left, index % left_length);
        let b = logical_at(right, index % right_length);
        values::ints_mut(result)[index] = if is_and {
            if a == 0 || b == 0 {
                0
            } else if a == NA_LOGICAL || b == NA_LOGICAL {
                NA_LOGICAL
            } else {
                1
            }
        } else if a == 1 || b == 1 {
            1
        } else if a == NA_LOGICAL || b == NA_LOGICAL {
            NA_LOGICAL
        } else {
            0
        };
    }
    relational_attributes(left, right, result, call.call)?;
    Ok(result)
}

unsafe fn do_not(call: &Call<'_>) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    match operand.kind() {
        SexpType::Raw => {
            let result = alloc(SexpType::Raw, count)?;
            for index in 0..count {
                values::raws_mut(result)[index] = !values::raws(operand)[index];
            }
            values::copy_most_attributes(operand, result, true)?;
            Ok(result)
        }
        SexpType::Logical
        | SexpType::Integer
        | SexpType::Real
        | SexpType::Complex
        | SexpType::Nil => {
            let result = alloc(SexpType::Logical, count)?;
            for index in 0..count {
                let value = logical_at(operand, index);
                values::ints_mut(result)[index] = if value == NA_LOGICAL {
                    NA_LOGICAL
                } else {
                    1 - value
                };
            }
            for symbol in [symbols().names, symbols().dim, symbols().dimnames] {
                let value = get_attr(operand, symbol);
                if value != nil() {
                    values::set_attr(result, symbol, value)?;
                }
            }
            if operand.kind() == SexpType::Logical {
                values::copy_most_attributes(operand, result, true)?;
            }
            Ok(result)
        }
        _ => Err(call.error("invalid argument type")),
    }
}

// The Math group.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Math {
    Abs,
    Sign,
    Sqrt,
    Floor,
    Ceiling,
    Trunc,
    Exp,
    Expm1,
    Log1p,
    Log2,
    Log10,
    Cos,
    Sin,
    Tan,
    Cospi,
    Sinpi,
    Tanpi,
    Acos,
    Asin,
    Atan,
    Cosh,
    Sinh,
    Tanh,
    Acosh,
    Asinh,
    Atanh,
    Gamma,
    Lgamma,
    Digamma,
    Trigamma,
}

fn math_value(function: Math, x: f64) -> f64 {
    use std::f64::consts::PI;
    match function {
        Math::Abs => x.abs(),
        Math::Sign => {
            if x.is_nan() {
                x
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        Math::Sqrt => x.sqrt(),
        Math::Floor => x.floor(),
        Math::Ceiling => x.ceil(),
        Math::Trunc => x.trunc(),
        Math::Exp => x.exp(),
        // SAFETY: pure libm functions.
        Math::Expm1 => unsafe { expm1(x) },
        Math::Log1p => unsafe { log1p(x) },
        Math::Log2 => x.log2(),
        Math::Log10 => x.log10(),
        Math::Cos => x.cos(),
        Math::Sin => x.sin(),
        Math::Tan => x.tan(),
        Math::Cospi => {
            let reduced = (x.abs() % 2.0).abs();
            if reduced == 0.5 || reduced == 1.5 {
                0.0
            } else if reduced == 1.0 {
                -1.0
            } else if reduced == 0.0 {
                1.0
            } else {
                (PI * reduced).cos()
            }
        }
        Math::Sinpi => {
            let mut reduced = x % 2.0;
            if reduced <= -1.0 {
                reduced += 2.0;
            } else if reduced > 1.0 {
                reduced -= 2.0;
            }
            if reduced == 0.0 || reduced == 1.0 {
                0.0
            } else if reduced == 0.5 {
                1.0
            } else if reduced == -0.5 {
                -1.0
            } else {
                (PI * reduced).sin()
            }
        }
        Math::Tanpi => {
            // Reducing into (-1/2, 1/2] is exact, so values one period apart agree exactly.
            let mut reduced = x % 1.0;
            if reduced > 0.5 {
                reduced -= 1.0;
            } else if reduced <= -0.5 {
                reduced += 1.0;
            }
            if reduced == 0.0 {
                0.0
            } else if reduced == 0.5 {
                f64::NAN
            } else if reduced == 0.25 || reduced == -0.25 {
                reduced * 4.0
            } else {
                (PI * reduced).tan()
            }
        }
        Math::Acos => x.acos(),
        Math::Asin => x.asin(),
        Math::Atan => x.atan(),
        Math::Cosh => x.cosh(),
        Math::Sinh => x.sinh(),
        Math::Tanh => x.tanh(),
        Math::Acosh => x.acosh(),
        Math::Asinh => x.asinh(),
        Math::Atanh => x.atanh(),
        Math::Gamma => rmath::gammafn(x),
        Math::Lgamma => rmath::lgammafn(x),
        Math::Digamma => rmath::psigamma(x, 0.0),
        Math::Trigamma => rmath::psigamma(x, 1.0),
    }
}

unsafe fn math1(call: &Call<'_>, function: Math) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    match operand.kind() {
        SexpType::Integer | SexpType::Logical
            if matches!(function, Math::Abs) && operand.kind() == SexpType::Integer =>
        {
            let result = alloc(SexpType::Integer, count)?;
            for index in 0..count {
                let value = values::ints(operand)[index];
                values::ints_mut(result)[index] = if value == NA_INTEGER {
                    value
                } else {
                    value.abs()
                };
            }
            values::copy_most_attributes(operand, result, true)?;
            Ok(result)
        }
        SexpType::Integer | SexpType::Logical | SexpType::Real => {
            let result = alloc(SexpType::Real, count)?;
            let mut produced_nan = false;
            for index in 0..count {
                let x = real_at(operand, index);
                let value = if is_na_real(x) {
                    NA_REAL
                } else {
                    math_value(function, x)
                };
                if value.is_nan() && !x.is_nan() {
                    produced_nan = true;
                }
                values::reals_mut(result)[index] = value;
            }
            values::copy_most_attributes(operand, result, true)?;
            if produced_nan {
                conditions::warning_call(call.call, "NaNs produced")?;
            }
            Ok(result)
        }
        SexpType::Complex => complex_math(call, function, operand),
        _ => Err(call.error("non-numeric argument to mathematical function")),
    }
}

unsafe fn complex_math(call: &Call<'_>, function: Math, operand: Sexp) -> R<Sexp> {
    let count = length(operand);
    if function == Math::Abs {
        let result = alloc(SexpType::Real, count)?;
        for index in 0..count {
            let value = values::complexes(operand)[index];
            values::reals_mut(result)[index] = value.r.hypot(value.i);
        }
        return Ok(result);
    }
    let result = alloc(SexpType::Complex, count)?;
    for index in 0..count {
        let z = values::complexes(operand)[index];
        let value = match function {
            Math::Sqrt => csqrt(z),
            Math::Exp => cexp(z),
            Math::Cos => ccos(z),
            Math::Sin => csin(z),
            Math::Tan => complex_tan(z),
            Math::Acos => complex_acos(z),
            Math::Asin => complex_asin(z),
            Math::Atan => complex_atan(z),
            Math::Cosh => ccosh(z),
            Math::Sinh => csinh(z),
            Math::Tanh => times_minus_i(complex_tan(times_i(z))),
            Math::Acosh => times_i(complex_acos(z)),
            Math::Asinh => times_minus_i(complex_asin(times_i(z))),
            Math::Atanh => times_minus_i(complex_atan(times_i(z))),
            _ => return Err(call.error("unimplemented complex function")),
        };
        values::complexes_mut(result)[index] = value;
    }
    values::copy_most_attributes(operand, result, true)?;
    Ok(result)
}

// GNU R's corrections of the C99 complex functions, from its `complex.c`.

fn times_i(z: Rcomplex) -> Rcomplex {
    Rcomplex { r: -z.i, i: z.r }
}

fn times_minus_i(z: Rcomplex) -> Rcomplex {
    Rcomplex { r: z.i, i: -z.r }
}

/// Far from the real axis the tangent is `±i`, which some C libraries return as NaN.
unsafe fn complex_tan(z: Rcomplex) -> Rcomplex {
    let result = ctan(z);
    if z.i.is_finite() && z.i.abs() > 25.0 {
        return Rcomplex { r: 0.0, i: if z.i > 0.0 { 1.0 } else { -1.0 } };
    }
    result
}

/// On the real axis beyond ±1, continuous from below for `x >= 1` and from above for `x <= -1`.
unsafe fn complex_asin(z: Rcomplex) -> Rcomplex {
    if z.i == 0.0 && z.r.abs() > 1.0 {
        let x = z.r;
        let t1 = 0.5 * (x + 1.0).abs();
        let t2 = 0.5 * (x - 1.0).abs();
        let alpha = t1 + t2;
        let mut imaginary = (alpha + (alpha * alpha - 1.0).sqrt()).ln();
        if x > 1.0 {
            imaginary = -imaginary;
        }
        return Rcomplex { r: (t1 - t2).asin(), i: imaginary };
    }
    casin(z)
}

unsafe fn complex_acos(z: Rcomplex) -> Rcomplex {
    if z.i == 0.0 && z.r.abs() > 1.0 {
        let asin = complex_asin(z);
        return Rcomplex { r: std::f64::consts::FRAC_PI_2 - asin.r, i: -asin.i };
    }
    cacos(z)
}

unsafe fn complex_atan(z: Rcomplex) -> Rcomplex {
    if z.r == 0.0 && z.i.abs() > 1.0 {
        let y = z.i;
        let real = if y > 0.0 { std::f64::consts::FRAC_PI_2 } else { -std::f64::consts::FRAC_PI_2 };
        let imaginary = 0.25 * (((1.0 + y) * (1.0 + y)) / ((1.0 - y) * (1.0 - y))).ln();
        return Rcomplex { r: real, i: imaginary };
    }
    catan(z)
}

unsafe fn do_log(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "base"])?;
    let operand = present(matched[0])
        .ok_or_else(|| call.error("argument \"x\" is missing, with no default"))?;
    let base = present(matched[1]).map(|base| values::as_real_scalar(base));
    let operand = values::materialize(operand)?;
    match operand.kind() {
        SexpType::Integer | SexpType::Logical | SexpType::Real => {
            let count = length(operand);
            let result = alloc(SexpType::Real, count)?;
            let mut produced_nan = false;
            for index in 0..count {
                let x = real_at(operand, index);
                let value = if is_na_real(x) {
                    NA_REAL
                } else {
                    match base {
                        Some(base) if base == 10.0 => x.log10(),
                        Some(base) if base == 2.0 => x.log2(),
                        Some(base) => x.ln() / base.ln(),
                        None => x.ln(),
                    }
                };
                if value.is_nan() && !x.is_nan() {
                    produced_nan = true;
                }
                values::reals_mut(result)[index] = value;
            }
            values::copy_most_attributes(operand, result, true)?;
            if produced_nan {
                conditions::warning_call(call.call, "NaNs produced")?;
            }
            Ok(result)
        }
        SexpType::Complex => {
            let count = length(operand);
            let result = alloc(SexpType::Complex, count)?;
            for index in 0..count {
                let z = values::complexes(operand)[index];
                let mut value = Rcomplex {
                    r: z.r.hypot(z.i).ln(),
                    i: z.i.atan2(z.r),
                };
                if let Some(base) = base {
                    value.r /= base.ln();
                    value.i /= base.ln();
                }
                values::complexes_mut(result)[index] = value;
            }
            Ok(result)
        }
        _ => Err(call.error("non-numeric argument to mathematical function")),
    }
}

/// R's `round(x, digits)`: of the two candidates at `digits` decimals, the one closer to `x`,
/// and on a tie the one with an even last digit.
pub fn round_digits(x: f64, digits: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if digits > 308 {
        return x;
    }
    if digits == 0 {
        return x.round_ties_even();
    }
    let scale = 10f64.powi(digits);
    let scaled = x * scale;
    if !scaled.is_finite() {
        return x;
    }
    let lower = scaled.floor();
    let upper = scaled.ceil();
    if lower == upper {
        return x;
    }
    let lower_value = lower / scale;
    let upper_value = upper / scale;
    let lower_distance = (x - lower_value).abs();
    let upper_distance = (upper_value - x).abs();
    if lower_distance < upper_distance {
        lower_value
    } else if upper_distance < lower_distance {
        upper_value
    } else if lower % 2.0 == 0.0 {
        lower_value
    } else {
        upper_value
    }
}

pub fn signif_digits(x: f64, digits: i32) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let digits = digits.max(1);
    let text = format!("{:.*e}", (digits - 1) as usize, x);
    text.parse().unwrap_or(x)
}

unsafe fn round_like(call: &Call<'_>, significant: bool) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "digits"])?;
    let operand = present(matched[0])
        .ok_or_else(|| call.error("argument \"x\" is missing, with no default"))?;
    let digits = match present(matched[1]) {
        Some(digits) => {
            let value = values::as_real_scalar(digits);
            if value.is_nan() {
                return Err(call.error("invalid second argument of length 0"));
            }
            value.round() as i32
        }
        None => {
            if significant {
                6
            } else {
                0
            }
        }
    };
    let operand = values::materialize(operand)?;
    match operand.kind() {
        SexpType::Integer | SexpType::Logical if !significant || digits >= 10 => {
            if digits >= 0 || operand.kind() == SexpType::Logical {
                let result = coerce_vector(operand, SexpType::Integer)?;
                return Ok(result);
            }
            let result = coerce_vector(operand, SexpType::Real)?;
            let result = values::duplicate(result)?;
            for value in values::reals_mut(result) {
                *value = round_digits(*value, digits);
            }
            Ok(result)
        }
        SexpType::Integer | SexpType::Logical | SexpType::Real => {
            let count = length(operand);
            let result = alloc(SexpType::Real, count)?;
            for index in 0..count {
                let x = real_at(operand, index);
                values::reals_mut(result)[index] = if x.is_nan() {
                    x
                } else if significant {
                    signif_digits(x, digits)
                } else {
                    round_digits(x, digits)
                };
            }
            values::copy_most_attributes(operand, result, true)?;
            Ok(result)
        }
        SexpType::Complex => {
            let count = length(operand);
            let result = alloc(SexpType::Complex, count)?;
            for index in 0..count {
                let z = values::complexes(operand)[index];
                values::complexes_mut(result)[index] = if significant {
                    Rcomplex {
                        r: signif_digits(z.r, digits),
                        i: signif_digits(z.i, digits),
                    }
                } else {
                    Rcomplex {
                        r: round_digits(z.r, digits),
                        i: round_digits(z.i, digits),
                    }
                };
            }
            Ok(result)
        }
        _ => Err(call.error("non-numeric argument to mathematical function")),
    }
}

#[derive(Clone, Copy)]
enum Cumulative {
    Sum,
    Product,
    Max,
    Min,
}

unsafe fn cumulative(call: &Call<'_>, kind: Cumulative) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    let names = values::names(operand);
    let result = match (operand.kind(), kind) {
        (
            SexpType::Integer | SexpType::Logical,
            Cumulative::Sum | Cumulative::Max | Cumulative::Min,
        ) => {
            let result = alloc(SexpType::Integer, count)?;
            let mut accumulator: Option<i64> = None;
            let mut overflow = false;
            for index in 0..count {
                let value = values::ints(operand)[index];
                if value == NA_INTEGER {
                    for slot in &mut values::ints_mut(result)[index..] {
                        *slot = NA_INTEGER;
                    }
                    break;
                }
                let next = match (accumulator, kind) {
                    (None, _) => value as i64,
                    (Some(total), Cumulative::Sum) => total + value as i64,
                    (Some(total), Cumulative::Max) => total.max(value as i64),
                    (Some(total), _) => total.min(value as i64),
                };
                if next > i32::MAX as i64 || next < -(i32::MAX as i64) {
                    overflow = true;
                    for slot in &mut values::ints_mut(result)[index..] {
                        *slot = NA_INTEGER;
                    }
                    break;
                }
                accumulator = Some(next);
                values::ints_mut(result)[index] = next as i32;
            }
            if overflow {
                conditions::warning("integer overflow in 'cumsum'; use 'cumsum(as.numeric(.))'")?;
            }
            result
        }
        (SexpType::Integer | SexpType::Logical | SexpType::Real, _) => {
            let result = alloc(SexpType::Real, count)?;
            let mut accumulator = match kind {
                Cumulative::Sum => 0.0,
                Cumulative::Product => 1.0,
                Cumulative::Max => f64::NEG_INFINITY,
                Cumulative::Min => f64::INFINITY,
            };
            let mut na = false;
            for index in 0..count {
                let value = real_at(operand, index);
                if na {
                    values::reals_mut(result)[index] = NA_REAL;
                    continue;
                }
                if matches!(kind, Cumulative::Max | Cumulative::Min) && value.is_nan() {
                    na = true;
                    values::reals_mut(result)[index] =
                        if is_na_real(value) { NA_REAL } else { f64::NAN };
                    continue;
                }
                accumulator = match kind {
                    Cumulative::Sum => accumulator + value,
                    Cumulative::Product => accumulator * value,
                    Cumulative::Max => accumulator.max(value),
                    Cumulative::Min => accumulator.min(value),
                };
                values::reals_mut(result)[index] = accumulator;
            }
            result
        }
        (SexpType::Complex, Cumulative::Sum | Cumulative::Product) => {
            let result = alloc(SexpType::Complex, count)?;
            let mut accumulator = match kind {
                Cumulative::Sum => Rcomplex { r: 0.0, i: 0.0 },
                _ => Rcomplex { r: 1.0, i: 0.0 },
            };
            for index in 0..count {
                let z = values::complexes(operand)[index];
                accumulator = match kind {
                    Cumulative::Sum => Rcomplex {
                        r: accumulator.r + z.r,
                        i: accumulator.i + z.i,
                    },
                    _ => complex_multiply(accumulator, z),
                };
                values::complexes_mut(result)[index] = accumulator;
            }
            result
        }
        (SexpType::String, _) => {
            let converted = coerce_vector(operand, SexpType::Real)?;
            let args = [Arg {
                tag: nil(),
                value: converted,
            }];
            let inner = Call {
                call: call.call,
                op: call.op,
                args: &args,
                env: call.env,
            };
            return cumulative(&inner, kind);
        }
        (SexpType::Nil, _) => return alloc(SexpType::Real, 0),
        _ => {
            return Err(call.error(format!(
                "'{}' not defined for \"{}\" objects",
                call.name(),
                operand.kind().name()
            )));
        }
    };
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

// The Summary group.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Summary {
    Sum,
    Product,
    Max,
    Min,
}

/// The values a summary function reduces, and whether `na.rm` was set.
unsafe fn summary_args(call: &Call<'_>) -> R<(Vec<Sexp>, bool)> {
    let mut items = Vec::new();
    let mut na_rm = false;
    for arg in call.args {
        if arg.tag == symbols().na_rm {
            na_rm = values::as_logical_scalar(arg.value) == 1;
        } else if arg.value != nil() {
            // NULL holds no values, and reading its elements as integers would see garbage.
            items.push(values::materialize(arg.value)?);
        }
    }
    Ok((items, na_rm))
}

/// Neumaier's compensated sum, which tracks GNU R's extended-precision accumulator.
#[derive(Default)]
pub struct CompensatedSum {
    sum: f64,
    compensation: f64,
}

impl CompensatedSum {
    pub fn add(&mut self, value: f64) {
        let total = self.sum + value;
        if self.sum.abs() >= value.abs() {
            self.compensation += (self.sum - total) + value;
        } else {
            self.compensation += (value - total) + self.sum;
        }
        self.sum = total;
    }

    pub fn total(&self) -> f64 {
        if self.sum.is_finite() {
            self.sum + self.compensation
        } else {
            self.sum
        }
    }
}

unsafe fn summary(call: &Call<'_>, function: Summary) -> R<Sexp> {
    let (mut items, na_rm) = summary_args(call)?;
    // NULL contributes no values, and the typed loops below read vectors' payloads directly.
    items.retain(|item| *item != nil());
    for item in &items {
        if !(item.kind().is_atomic() || *item == nil())
            || (item.kind() == SexpType::String
                && matches!(function, Summary::Sum | Summary::Product))
        {
            return Err(call.error(format!(
                "invalid 'type' ({}) of argument",
                item.kind().name()
            )));
        }
    }
    // `NULL` contributes nothing, and it has no elements to read.
    let items: Vec<Sexp> = items.into_iter().filter(|item| *item != nil()).collect();
    let kind = items
        .iter()
        .fold(SexpType::Logical, |kind, item| coerce::higher_type(kind, item.kind()));
    match function {
        Summary::Sum => {
            if kind == SexpType::Complex {
                let mut total = Rcomplex { r: 0.0, i: 0.0 };
                for &item in &items {
                    for index in 0..length(item) {
                        let z = complex_at(item, index);
                        if na_rm && (z.r.is_nan() || z.i.is_nan()) {
                            continue;
                        }
                        total.r += z.r;
                        total.i += z.i;
                    }
                }
                return values::scalar_complex(total);
            }
            if kind == SexpType::Real {
                let mut total = CompensatedSum::default();
                let mut na = false;
                let mut add = |value: f64| {
                    if value.is_nan() {
                        if na_rm {
                            return;
                        }
                        if is_na_real(value) {
                            na = true;
                        }
                    }
                    total.add(value);
                };
                for &item in &items {
                    if item.kind() == SexpType::Real {
                        values::reals(item).iter().for_each(|&value| add(value));
                    } else {
                        (0..length(item)).for_each(|index| add(real_at(item, index)));
                    }
                }
                return scalar_real(if na { NA_REAL } else { total.total() });
            }
            let mut total: i64 = 0;
            for &item in &items {
                for &value in values::ints(item) {
                    if value == NA_INTEGER {
                        if na_rm {
                            continue;
                        }
                        return scalar_integer(NA_INTEGER);
                    }
                    total += value as i64;
                }
            }
            // An integer sum too large for an integer is a double, as in R 4.3.
            if total > i32::MAX as i64 || total < -(i32::MAX as i64) {
                return scalar_real(total as f64);
            }
            scalar_integer(total as i32)
        }
        Summary::Product => {
            let mut total = 1.0;
            for &item in &items {
                for index in 0..length(item) {
                    let value = real_at(item, index);
                    if na_rm && value.is_nan() {
                        continue;
                    }
                    total *= value;
                }
            }
            scalar_real(total)
        }
        Summary::Max | Summary::Min => {
            let is_max = function == Summary::Max;
            if kind == SexpType::String {
                let mut best: Option<String> = None;
                for &item in &items {
                    let strings = coerce_vector(item, SexpType::String)?;
                    for index in 0..length(strings) {
                        match values::string_at(strings, index) {
                            None => {
                                if !na_rm {
                                    return values::scalar_string_charsxp(values::na_string());
                                }
                            }
                            Some(text) => {
                                let better = match &best {
                                    None => true,
                                    Some(current) => {
                                        let ordering = crate::sort::collate(&text, current);
                                        if is_max {
                                            ordering.is_gt()
                                        } else {
                                            ordering.is_lt()
                                        }
                                    }
                                };
                                if better {
                                    best = Some(text.into_owned());
                                }
                            }
                        }
                    }
                }
                return match best {
                    Some(text) => values::scalar_string(&text),
                    None => Err(call.error(format!(
                        "no non-missing arguments to {}",
                        if is_max { "max" } else { "min" }
                    ))),
                };
            }
            if matches!(kind, SexpType::Integer | SexpType::Logical) {
                let mut best: Option<i32> = None;
                for &item in &items {
                    for &value in values::ints(item) {
                        if value == NA_INTEGER {
                            if na_rm {
                                continue;
                            }
                            return scalar_integer(NA_INTEGER);
                        }
                        best = Some(match best {
                            None => value,
                            Some(current) => {
                                if is_max {
                                    current.max(value)
                                } else {
                                    current.min(value)
                                }
                            }
                        });
                    }
                }
                if let Some(best) = best {
                    return scalar_integer(best);
                }
            }
            let mut best = if is_max {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            let mut any = false;
            let mut saw_nan = false;
            for &item in &items {
                for index in 0..length(item) {
                    let value = real_at(item, index);
                    if value.is_nan() {
                        if na_rm {
                            continue;
                        }
                        if is_na_real(value) {
                            return scalar_real(NA_REAL);
                        }
                        saw_nan = true;
                        continue;
                    }
                    any = true;
                    best = if is_max {
                        best.max(value)
                    } else {
                        best.min(value)
                    };
                }
            }
            if saw_nan {
                return scalar_real(f64::NAN);
            }
            if !any {
                conditions::warning_call(
                    call.call,
                    format!(
                        "no non-missing arguments to {}; returning {}",
                        if is_max { "max" } else { "min" },
                        if is_max { "-Inf" } else { "Inf" }
                    ),
                )?;
            }
            scalar_real(best)
        }
    }
}

unsafe fn do_range(call: &Call<'_>) -> R<Sexp> {
    let mut args: Vec<Arg> = Vec::new();
    let mut finite = false;
    for arg in call.args {
        if arg.tag != nil() && values::symbol_name(arg.tag) == "finite" {
            finite = values::as_logical_scalar(arg.value) == 1;
        } else {
            args.push(*arg);
        }
    }
    if finite {
        let mut filtered = Vec::new();
        for arg in &args {
            if arg.tag == symbols().na_rm {
                continue;
            }
            let value = values::materialize(arg.value)?;
            if matches!(value.kind(), SexpType::Real) {
                let kept: Vec<f64> = values::reals(value)
                    .iter()
                    .copied()
                    .filter(|value| value.is_finite())
                    .collect();
                filtered.push(Arg {
                    tag: nil(),
                    value: values::real_vector(&kept)?,
                });
            } else {
                filtered.push(*arg);
            }
        }
        filtered.push(Arg {
            tag: symbols().na_rm,
            value: values::scalar_bool(true)?,
        });
        args = filtered;
    }
    let inner = Call {
        call: call.call,
        op: call.op,
        args: &args,
        env: call.env,
    };
    let minimum = summary(&inner, Summary::Min)?;
    let maximum = summary(&inner, Summary::Max)?;
    crate::subset::combine(&[minimum, maximum])
}

unsafe fn all_any(call: &Call<'_>, is_all: bool) -> R<Sexp> {
    let (items, na_rm) = summary_args(call)?;
    let mut saw_na = false;
    for item in items {
        // GNU R skips an empty list as it skips `NULL`.
        if item == nil() || (item.kind() == SexpType::List && length(item) == 0) {
            continue;
        }
        if !matches!(
            item.kind(),
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::Complex
                | SexpType::Raw
                | SexpType::String
        ) {
            return Err(call.error(format!(
                "invalid 'type' ({}) of argument",
                item.kind().name()
            )));
        }
        if item.kind() == SexpType::String {
            return Err(call.error("invalid 'type' (character) of argument"));
        }
        if !matches!(item.kind(), SexpType::Logical | SexpType::Integer) && length(item) > 0 {
            conditions::warning_call(
                call.call,
                format!(
                    "coercing argument of type '{}' to logical",
                    item.kind().name()
                ),
            )?;
        }
        for index in 0..length(item) {
            let value = logical_at(item, index);
            if value == NA_LOGICAL {
                saw_na = true;
            } else if is_all && value == 0 {
                return scalar_bool(false);
            } else if !is_all && value == 1 {
                return scalar_bool(true);
            }
        }
    }
    if saw_na && !na_rm {
        return values::scalar_logical(NA_LOGICAL);
    }
    scalar_bool(is_all)
}

unsafe fn do_mean(call: &Call<'_>) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    match operand.kind() {
        SexpType::Logical | SexpType::Integer => {
            let mut total = 0.0;
            for &value in values::ints(operand) {
                if value == NA_INTEGER {
                    return scalar_real(NA_REAL);
                }
                total += value as f64;
            }
            scalar_real(total / count as f64)
        }
        SexpType::Real => {
            let data = values::reals(operand);
            let mut total = CompensatedSum::default();
            for &value in data {
                total.add(value);
            }
            let mean = total.total() / count as f64;
            if !mean.is_finite() {
                return scalar_real(mean);
            }
            let mut correction = CompensatedSum::default();
            for &value in data {
                correction.add(value - mean);
            }
            scalar_real(mean + correction.total() / count as f64)
        }
        SexpType::Complex => {
            let mut total = Rcomplex { r: 0.0, i: 0.0 };
            for &value in values::complexes(operand) {
                total.r += value.r;
                total.i += value.i;
            }
            values::scalar_complex(Rcomplex {
                r: total.r / count as f64,
                i: total.i / count as f64,
            })
        }
        _ => Err(call.error(format!(
            "invalid 'type' ({}) of argument",
            operand.kind().name()
        ))),
    }
}

// NA tests.

#[derive(Clone, Copy, PartialEq, Eq)]
enum NaTest {
    Na,
    NaN,
    Finite,
    Infinite,
}

unsafe fn na_test(call: &Call<'_>, test: NaTest) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    let result = alloc(SexpType::Logical, count)?;
    match operand.kind() {
        SexpType::Logical | SexpType::Integer => {
            for index in 0..count {
                let value = values::ints(operand)[index];
                values::ints_mut(result)[index] = match test {
                    NaTest::Na => (value == NA_INTEGER) as i32,
                    NaTest::NaN | NaTest::Infinite => 0,
                    NaTest::Finite => (value != NA_INTEGER) as i32,
                };
            }
        }
        SexpType::Real => {
            for index in 0..count {
                let value = values::reals(operand)[index];
                values::ints_mut(result)[index] = match test {
                    NaTest::Na => value.is_nan() as i32,
                    NaTest::NaN => (value.is_nan() && !is_na_real(value)) as i32,
                    NaTest::Finite => value.is_finite() as i32,
                    NaTest::Infinite => value.is_infinite() as i32,
                };
            }
        }
        SexpType::Complex => {
            for index in 0..count {
                let z = values::complexes(operand)[index];
                values::ints_mut(result)[index] = match test {
                    NaTest::Na => (z.r.is_nan() || z.i.is_nan()) as i32,
                    NaTest::NaN => {
                        ((z.r.is_nan() && !is_na_real(z.r)) || (z.i.is_nan() && !is_na_real(z.i)))
                            as i32
                    }
                    NaTest::Finite => (z.r.is_finite() && z.i.is_finite()) as i32,
                    NaTest::Infinite => (z.r.is_infinite() || z.i.is_infinite()) as i32,
                };
            }
        }
        SexpType::String => {
            for index in 0..count {
                values::ints_mut(result)[index] = match test {
                    NaTest::Na => (operand.element(index) == values::na_string()) as i32,
                    _ => 0,
                };
            }
        }
        SexpType::List => {
            for index in 0..count {
                let element = operand.element(index);
                values::ints_mut(result)[index] = match test {
                    NaTest::Na => {
                        (element.kind().is_atomic()
                            && length(element) == 1
                            && values::as_logical_scalar(is_na_scalar(element)?) == 1)
                            as i32
                    }
                    _ => 0,
                };
            }
        }
        SexpType::Raw | SexpType::Nil => {}
        _ => {
            if test == NaTest::Na {
                conditions::warning_call(
                    call.call,
                    format!(
                        "is.na() applied to non-(list or vector) of type '{}'",
                        operand.kind().name()
                    ),
                )?;
                return scalar_bool(false);
            }
            return Err(call.error(format!(
                "default method not implemented for type '{}'",
                operand.kind().name()
            )));
        }
    }
    for symbol in [symbols().names, symbols().dim, symbols().dimnames] {
        let value = get_attr(operand, symbol);
        if value != nil() {
            values::set_attr(result, symbol, value)?;
        }
    }
    Ok(result)
}

unsafe fn is_na_scalar(value: Sexp) -> R<Sexp> {
    let na = match value.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(value)[0] == NA_INTEGER,
        SexpType::Real => values::reals(value)[0].is_nan(),
        SexpType::String => value.element(0) == values::na_string(),
        SexpType::Complex => values::complexes(value)[0].r.is_nan(),
        _ => false,
    };
    scalar_bool(na)
}

unsafe fn do_any_na(call: &Call<'_>) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let recursive = call
        .args
        .get(1)
        .is_some_and(|arg| values::as_logical_scalar(arg.value) == 1);
    scalar_bool(any_na(operand, recursive)?)
}

unsafe fn any_na(operand: Sexp, recursive: bool) -> R<bool> {
    Ok(match operand.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(operand).contains(&NA_INTEGER),
        SexpType::Real => values::reals(operand).iter().any(|value| value.is_nan()),
        SexpType::Complex => values::complexes(operand)
            .iter()
            .any(|value| value.r.is_nan() || value.i.is_nan()),
        SexpType::String => {
            (0..length(operand)).any(|index| operand.element(index) == values::na_string())
        }
        SexpType::List => {
            for index in 0..length(operand) {
                let element = operand.element(index);
                let na = if recursive || (element.kind().is_atomic() && length(element) == 1) {
                    any_na(element, recursive)?
                } else {
                    false
                };
                if na {
                    return Ok(true);
                }
            }
            false
        }
        _ => false,
    })
}

// Sequences.

unsafe fn do_colon(call: &Call<'_>) -> R<Sexp> {
    let from = call.arg(0);
    let to = call.arg(1);
    if length(from) == 0 || length(to) == 0 {
        return Err(call.error("argument of length 0"));
    }
    if length(from) > 1 {
        conditions::warning_call(
            call.call,
            format!(
                "numerical expression has {} elements: only the first used",
                length(from)
            ),
        )?;
    }
    if length(to) > 1 {
        conditions::warning_call(
            call.call,
            format!(
                "numerical expression has {} elements: only the first used",
                length(to)
            ),
        )?;
    }
    let start = values::as_real_scalar(from);
    let end = values::as_real_scalar(to);
    if start.is_nan() || end.is_nan() {
        return Err(call.error("NA/NaN argument"));
    }
    colon(start, end)
}

/// `from:to`: integers when `from` is a whole number and the sequence fits, doubles otherwise.
pub unsafe fn colon(start: f64, end: f64) -> R<Sexp> {
    let count = ((end - start).abs() + 1e-10).floor() as usize + 1;
    let use_integer = start == start.floor()
        && start.abs() <= i32::MAX as f64
        && (start + (count as f64 - 1.0) * if start <= end { 1.0 } else { -1.0 }).abs()
            <= i32::MAX as f64;
    if use_integer {
        let result = alloc(SexpType::Integer, count)?;
        let start = start as i64;
        let output = values::ints_mut(result);
        if start <= end as i64 || end >= start as f64 {
            for (index, slot) in output.iter_mut().enumerate() {
                *slot = (start + index as i64) as i32;
            }
        } else {
            for (index, slot) in output.iter_mut().enumerate() {
                *slot = (start - index as i64) as i32;
            }
        }
        return Ok(result);
    }
    let result = alloc(SexpType::Real, count)?;
    let step = if start <= end { 1.0 } else { -1.0 };
    for (index, slot) in values::reals_mut(result).iter_mut().enumerate() {
        *slot = start + index as f64 * step;
    }
    Ok(result)
}

unsafe fn do_seq_len(call: &Call<'_>) -> R<Sexp> {
    let value = call.arg(0);
    if length(value) != 1 {
        if length(value) == 0 {
            return Err(call.error("argument of length 0"));
        }
        conditions::warning_call(call.call, "first element used of 'length.out' argument")?;
    }
    let count = values::as_real_scalar(value);
    if count.is_nan() || count < 0.0 {
        return Err(call.error("argument must be coercible to non-negative integer"));
    }
    let count = count as usize;
    let result = alloc(SexpType::Integer, count)?;
    for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
        *slot = index as i32 + 1;
    }
    Ok(result)
}

unsafe fn do_seq_along(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let count = if object.is_object() {
        crate::subset::dispatch_length(object, call.env)?
    } else {
        length(object)
    };
    let result = alloc(SexpType::Integer, count)?;
    for (index, slot) in values::ints_mut(result).iter_mut().enumerate() {
        *slot = index as i32 + 1;
    }
    Ok(result)
}

/// `seq.int(from, to, by, length.out, along.with, ...)`.
unsafe fn do_seq_int(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["from", "to", "by", "length.out", "along.with", "..."])?;
    let from = present(matched[0]);
    let to = present(matched[1]);
    let by = present(matched[2]);
    let length_out = present(matched[3]);
    let along = present(matched[4]);
    let one = |value: Option<Sexp>, name: &str| -> R<Option<f64>> {
        match value {
            None => Ok(None),
            Some(value) => {
                if length(value) != 1 {
                    return Err(call.error(format!("'{name}' must be of length 1")));
                }
                let number = values::as_real_scalar(value);
                if !number.is_finite() {
                    return Err(call.error(format!("'{name}' must be a finite number")));
                }
                Ok(Some(number))
            }
        }
    };
    let length_out = match along {
        Some(along) => Some(length(along) as f64),
        None => match length_out {
            Some(value) => {
                let number = values::as_real_scalar(value);
                if number.is_nan() || number < 0.0 {
                    return Err(call.error("'length.out' must be a non-negative number"));
                }
                Some(number.ceil())
            }
            None => None,
        },
    };
    let integer_like = |value: Option<Sexp>| {
        value.is_none_or(|value| matches!(value.kind(), SexpType::Integer | SexpType::Logical))
    };
    let all_integer = integer_like(from) && integer_like(to) && integer_like(by);
    if from.is_some() && to.is_none() && by.is_none() && length_out.is_none() {
        let value = from.unwrap_or(nil());
        if length(value) == 1 {
            let n = values::as_real_scalar(value);
            return colon(1.0, n);
        }
        return do_seq_along(&Call {
            call: call.call,
            op: call.op,
            args: &[Arg { tag: nil(), value }],
            env: call.env,
        });
    }
    let from_value = one(from, "from")?;
    let to_value = one(to, "to")?;
    let by_value = one(by, "by")?;
    match (from_value, to_value, by_value, length_out) {
        (from, to, None, None) => colon(from.unwrap_or(1.0), to.unwrap_or(1.0)),
        (from, Some(to), Some(by), None) => {
            let from = from.unwrap_or(1.0);
            let span = to - from;
            if span == 0.0 {
                return if all_integer {
                    values::integer_vector(&[from as i32])
                } else {
                    values::real_vector(&[from])
                };
            }
            if by == 0.0 {
                return Err(call.error("invalid '(to - from)/by' in seq(.)"));
            }
            if span / by < 0.0 {
                return Err(call.error("wrong sign in 'by' argument"));
            }
            let steps = (span / by + 1e-10).floor() as usize;
            if all_integer {
                let values: Vec<i32> = (0..=steps)
                    .map(|index| (from + index as f64 * by) as i32)
                    .collect();
                values::integer_vector(&values)
            } else {
                let values: Vec<f64> = (0..=steps).map(|index| from + index as f64 * by).collect();
                values::real_vector(&values)
            }
        }
        (from, to, by, Some(count)) => {
            let count = count as usize;
            match (from, to, by) {
                (Some(from), Some(to), None) => {
                    if count == 1 {
                        return values::real_vector(&[from]);
                    }
                    let by = (to - from) / (count as f64 - 1.0);
                    let values: Vec<f64> = (0..count)
                        .map(|index| {
                            if index == count - 1 {
                                to
                            } else {
                                from + index as f64 * by
                            }
                        })
                        .collect();
                    if all_integer && values.iter().all(|value| value.fract() == 0.0) {
                        let integers: Vec<i32> = values.iter().map(|&value| value as i32).collect();
                        return values::integer_vector(&integers);
                    }
                    values::real_vector(&values)
                }
                (from, None, by) => {
                    let from = from.unwrap_or(1.0);
                    let by = by.unwrap_or(1.0);
                    let values: Vec<f64> =
                        (0..count).map(|index| from + index as f64 * by).collect();
                    if (from_value.is_none() || all_integer)
                        && values
                            .iter()
                            .all(|value| value.fract() == 0.0 && value.abs() < i32::MAX as f64)
                    {
                        let integers: Vec<i32> = values.iter().map(|&value| value as i32).collect();
                        return values::integer_vector(&integers);
                    }
                    values::real_vector(&values)
                }
                (None, Some(to), by) => {
                    let by = by.unwrap_or(1.0);
                    let from = to - (count as f64 - 1.0) * by;
                    let values: Vec<f64> =
                        (0..count).map(|index| from + index as f64 * by).collect();
                    if all_integer && values.iter().all(|value| value.fract() == 0.0) {
                        let integers: Vec<i32> = values.iter().map(|&value| value as i32).collect();
                        return values::integer_vector(&integers);
                    }
                    values::real_vector(&values)
                }
                _ => Err(call.error("too many arguments")),
            }
        }
        _ => Err(call.error("invalid arguments")),
    }
}

/// `rep(x, times, each, length.out)`.
unsafe fn do_rep(call: &Call<'_>) -> R<Sexp> {
    let (matched, _) = call.match_args(&["x", "times", "length.out", "each", "..."])?;
    let object = present(matched[0]).unwrap_or(nil());
    let times = present(matched[1]);
    let length_out = present(matched[2])
        .map(|value| values::as_real_scalar(value))
        .filter(|value| !value.is_nan());
    let each = present(matched[3])
        .map(|value| values::as_integer_scalar(value))
        .filter(|&value| value != NA_INTEGER)
        .unwrap_or(1)
        .max(0) as usize;
    let object = values::materialize(object)?;
    let count = length(object);
    // An empty `x` returns before `times` is checked, as in GNU R, so `rep(NULL, c(1, 1))` is
    // NULL; only `length.out` can lengthen a non-NULL empty vector, with NAs.
    if object == nil() {
        if length_out.is_some_and(|out| out > 0.0) {
            crate::conditions::warning_call(call.call, "'x' is NULL so the result will be NULL")?;
        }
        return Ok(nil());
    }
    if count == 0 && length_out.is_none() {
        return values::duplicate(object);
    }
    // Expand `each` first.
    let mut indices: Vec<usize> = Vec::with_capacity(count * each);
    for index in 0..count {
        for _ in 0..each {
            indices.push(index);
        }
    }
    let base_count = indices.len();
    let indices: Vec<usize> = match (length_out, times) {
        (Some(out), _) => (0..out as usize)
            .map(|position| {
                if base_count == 0 {
                    usize::MAX
                } else {
                    indices[position % base_count]
                }
            })
            .collect(),
        (None, Some(times)) => {
            let times_count = length(times);
            if times_count == 1 {
                let repeat = values::as_real_scalar(times);
                if repeat.is_nan() || repeat < 0.0 {
                    return Err(call.error("invalid 'times' argument"));
                }
                let repeat = repeat as usize;
                let mut result = Vec::with_capacity(base_count * repeat);
                for _ in 0..repeat {
                    result.extend_from_slice(&indices);
                }
                result
            } else if times_count == base_count {
                let mut result = Vec::new();
                for (position, &index) in indices.iter().enumerate() {
                    let repeat = crate::subset::numeric_element(times, position);
                    if repeat.is_nan() || repeat < 0.0 {
                        return Err(call.error("invalid 'times' argument"));
                    }
                    for _ in 0..repeat as usize {
                        result.push(index);
                    }
                }
                result
            } else {
                return Err(call.error("invalid 'times' argument"));
            }
        }
        (None, None) => indices,
    };
    let result = crate::subset::select_elements(object, &indices)?;
    let names = values::names(object);
    if names != nil() {
        let new_names = crate::subset::select_elements(names, &indices)?;
        values::set_attr(result, symbols().names, new_names)?;
    }
    Ok(result)
}

unsafe fn do_rep_int(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let times = call.arg(1);
    let count = length(object);
    let mut indices = Vec::new();
    if length(times) == 1 {
        let repeat = values::as_real_scalar(times);
        if repeat.is_nan() || repeat < 0.0 {
            return Err(call.error("invalid 'times' value"));
        }
        for _ in 0..repeat as usize {
            indices.extend(0..count);
        }
    } else if length(times) == count {
        for index in 0..count {
            for _ in 0..crate::subset::numeric_element(times, index) as usize {
                indices.push(index);
            }
        }
    } else {
        return Err(call.error("invalid 'times' value"));
    }
    crate::subset::select_elements(object, &indices)
}

unsafe fn do_rep_len(call: &Call<'_>) -> R<Sexp> {
    let object = values::materialize(call.arg(0))?;
    let out = values::as_integer_scalar(call.arg(1));
    if out == NA_INTEGER || out < 0 {
        return Err(call.error("invalid 'length.out' value"));
    }
    let count = length(object);
    let indices: Vec<usize> = (0..out as usize)
        .map(|position| {
            if count == 0 {
                usize::MAX
            } else {
                position % count
            }
        })
        .collect();
    crate::subset::select_elements(object, &indices)
}

// Two-argument math.

unsafe fn math2(call: &Call<'_>, function: fn(f64, f64) -> f64) -> R<Sexp> {
    let left = values::materialize(call.arg(0))?;
    let right = values::materialize(call.arg(1))?;
    if !matches!(
        left.kind(),
        SexpType::Integer | SexpType::Real | SexpType::Logical
    ) || !matches!(
        right.kind(),
        SexpType::Integer | SexpType::Real | SexpType::Logical
    ) {
        return Err(call.error("non-numeric argument to mathematical function"));
    }
    let left_length = length(left);
    let right_length = length(right);
    let count = recycled_length(call.call, left_length, right_length)?;
    let result = alloc(SexpType::Real, count)?;
    for index in 0..count {
        let a = real_at(left, index % left_length);
        let b = real_at(right, index % right_length);
        values::reals_mut(result)[index] = if is_na_real(a) || is_na_real(b) {
            NA_REAL
        } else {
            function(a, b)
        };
    }
    binary_attributes(left, right, result, call.call)?;
    Ok(result)
}

unsafe fn bitwise(call: &Call<'_>, function: fn(i32, i32) -> i32) -> R<Sexp> {
    let left = coerce_vector(call.arg(0), SexpType::Integer)?;
    let right = coerce_vector(call.arg(1), SexpType::Integer)?;
    let left_length = length(left);
    let right_length = length(right);
    let count = if left_length == 0 || right_length == 0 {
        0
    } else {
        left_length.max(right_length)
    };
    let result = alloc(SexpType::Integer, count)?;
    for index in 0..count {
        let a = values::ints(left)[index % left_length];
        let b = values::ints(right)[index % right_length];
        values::ints_mut(result)[index] = if a == NA_INTEGER || b == NA_INTEGER {
            NA_INTEGER
        } else {
            function(a, b)
        };
    }
    Ok(result)
}

unsafe fn do_bitw_not(call: &Call<'_>) -> R<Sexp> {
    let operand = coerce_vector(call.arg(0), SexpType::Integer)?;
    let count = length(operand);
    let result = alloc(SexpType::Integer, count)?;
    for index in 0..count {
        let value = values::ints(operand)[index];
        values::ints_mut(result)[index] = if value == NA_INTEGER {
            NA_INTEGER
        } else {
            !value
        };
    }
    Ok(result)
}

unsafe fn complex_part(call: &Call<'_>, part: u8) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let count = length(operand);
    if !matches!(
        operand.kind(),
        SexpType::Complex | SexpType::Real | SexpType::Integer | SexpType::Logical
    ) {
        return Err(call.error("non-numeric argument to function"));
    }
    if part == 4 {
        if operand.kind() != SexpType::Complex {
            return Ok(operand);
        }
        let result = alloc(SexpType::Complex, count)?;
        for index in 0..count {
            let z = values::complexes(operand)[index];
            values::complexes_mut(result)[index] = Rcomplex { r: z.r, i: -z.i };
        }
        values::copy_most_attributes(operand, result, true)?;
        return Ok(result);
    }
    let result = alloc(SexpType::Real, count)?;
    for index in 0..count {
        let z = complex_at(operand, index);
        values::reals_mut(result)[index] = match part {
            0 => z.r,
            1 => z.i,
            2 => z.r.hypot(z.i),
            _ => z.i.atan2(z.r),
        };
    }
    values::copy_most_attributes(operand, result, true)?;
    Ok(result)
}

unsafe fn do_complex(call: &Call<'_>) -> R<Sexp> {
    let count = values::as_integer_scalar(call.arg(0)).max(0) as usize;
    let real = coerce_vector(call.arg(1), SexpType::Real)?;
    let imaginary = coerce_vector(call.arg(2), SexpType::Real)?;
    let count = count.max(length(real)).max(length(imaginary));
    let result = alloc(SexpType::Complex, count)?;
    for index in 0..count {
        let r = if length(real) > 0 {
            values::reals(real)[index % length(real)]
        } else {
            0.0
        };
        let i = if length(imaginary) > 0 {
            values::reals(imaginary)[index % length(imaginary)]
        } else {
            0.0
        };
        values::complexes_mut(result)[index] = Rcomplex { r, i };
    }
    Ok(result)
}

unsafe fn parallel_extreme(call: &Call<'_>, is_max: bool) -> R<Sexp> {
    let na_rm = values::as_logical_scalar(call.arg(0)) == 1;
    let items: Vec<Sexp> = call.args[1..]
        .iter()
        .map(|arg| values::materialize(arg.value))
        .collect::<R<_>>()?;
    if items.is_empty() {
        return Err(call.error("no arguments"));
    }
    let count = items.iter().map(|&item| length(item)).max().unwrap_or(0);
    if items.iter().any(|&item| length(item) == 0) {
        return alloc(items[0].kind(), 0);
    }
    let kind = items.iter().fold(SexpType::Logical, |kind, item| {
        coerce::higher_type(kind, item.kind())
    });
    if kind == SexpType::String {
        let result = alloc(SexpType::String, count)?;
        for index in 0..count {
            let mut best: Option<Sexp> = None;
            let mut na = false;
            for &item in &items {
                let strings = coerce_vector(item, SexpType::String)?;
                let value = strings.element(index % length(strings));
                if value == values::na_string() {
                    na = true;
                    continue;
                }
                best = Some(match best {
                    None => value,
                    Some(current) => {
                        let ordering = crate::sort::collate(
                            &values::char_str(value),
                            &values::char_str(current),
                        );
                        if (is_max && ordering.is_gt()) || (!is_max && ordering.is_lt()) {
                            value
                        } else {
                            current
                        }
                    }
                });
            }
            values::set_elt(
                result,
                index,
                if na && !na_rm {
                    values::na_string()
                } else {
                    best.unwrap_or(values::na_string())
                },
            );
        }
        return Ok(result);
    }
    let integer = matches!(kind, SexpType::Integer | SexpType::Logical);
    let result = alloc(
        if integer {
            SexpType::Integer
        } else {
            SexpType::Real
        },
        count,
    )?;
    for index in 0..count {
        let mut best: Option<f64> = None;
        let mut na = false;
        for &item in &items {
            let value = real_at(item, index % length(item));
            if value.is_nan() {
                na = true;
                continue;
            }
            best = Some(match best {
                None => value,
                Some(current) => {
                    if is_max {
                        current.max(value)
                    } else {
                        current.min(value)
                    }
                }
            });
        }
        let value = if na && !na_rm {
            NA_REAL
        } else {
            best.unwrap_or(NA_REAL)
        };
        if integer {
            values::ints_mut(result)[index] = if value.is_nan() {
                NA_INTEGER
            } else {
                value as i32
            };
        } else {
            values::reals_mut(result)[index] = value;
        }
    }
    values::copy_most_attributes(items[0], result, true)?;
    Ok(result)
}

unsafe fn do_tabulate(call: &Call<'_>) -> R<Sexp> {
    let bins = values::materialize(call.arg(0))?;
    let count = values::as_integer_scalar(call.arg(1)).max(0) as usize;
    let result = alloc(SexpType::Integer, count)?;
    for &value in values::ints(bins) {
        if value != NA_INTEGER && value >= 1 && (value as usize) <= count {
            values::ints_mut(result)[value as usize - 1] += 1;
        }
    }
    Ok(result)
}

unsafe fn do_which(call: &Call<'_>) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    if operand.kind() != SexpType::Logical {
        return Err(call.error("argument to 'which' is not logical"));
    }
    let positions: Vec<i32> = values::ints(operand)
        .iter()
        .enumerate()
        .filter(|(_, value)| **value == 1)
        .map(|(index, _)| index as i32 + 1)
        .collect();
    let result = values::integer_vector(&positions)?;
    let names = values::names(operand);
    if names != nil() {
        let indices: Vec<usize> = positions
            .iter()
            .map(|&position| position as usize - 1)
            .collect();
        let selected = crate::subset::select_elements(names, &indices)?;
        values::set_attr(result, symbols().names, selected)?;
    }
    Ok(result)
}

unsafe fn which_extreme(call: &Call<'_>, is_max: bool) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let mut best: Option<(usize, f64)> = None;
    for index in 0..length(operand) {
        let value = real_at(operand, index);
        if value.is_nan() {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, current)) => {
                if is_max {
                    value > current
                } else {
                    value < current
                }
            }
        };
        if better {
            best = Some((index, value));
        }
    }
    match best {
        None => alloc(SexpType::Integer, 0),
        Some((index, _)) => {
            let result = values::integer_vector(&[index as i32 + 1])?;
            let names = values::names(operand);
            if names != nil() {
                let name = crate::subset::select_elements(names, &[index])?;
                values::set_attr(result, symbols().names, name)?;
            }
            Ok(result)
        }
    }
}

unsafe fn do_is_unsorted(call: &Call<'_>) -> R<Sexp> {
    let operand = values::materialize(call.arg(0))?;
    let strictly = values::as_logical_scalar(call.arg(2)) == 1;
    let count = length(operand);
    for index in 1..count {
        let ordering = match operand.kind() {
            SexpType::String => crate::sort::collate(
                &values::char_str(operand.element(index - 1)),
                &values::char_str(operand.element(index)),
            ),
            _ => real_at(operand, index - 1)
                .partial_cmp(&real_at(operand, index))
                .unwrap_or(std::cmp::Ordering::Equal),
        };
        if ordering.is_gt() || (strictly && ordering.is_eq()) {
            return scalar_bool(true);
        }
    }
    scalar_bool(false)
}

unsafe fn do_pack_bits(call: &Call<'_>) -> R<Sexp> {
    let bits = call.arg(0);
    let kind = values::require_string(call.arg(1), "type")?;
    let width = match kind.as_str() {
        "raw" => 8,
        "integer" => 32,
        "double" => 64,
        _ => return Err(call.error("invalid 'type' argument")),
    };
    let count = length(bits);
    if count % width != 0 {
        return Err(call.error(format!("argument 'x' must be a multiple of {width} long")));
    }
    let mut flags = Vec::with_capacity(count);
    for index in 0..count {
        let bit = match bits.kind() {
            SexpType::Raw => values::raws(bits)[index] & 1,
            SexpType::Logical | SexpType::Integer => {
                let value = values::ints(bits)[index];
                if value == NA_INTEGER {
                    return Err(call.error("argument 'x' must not contain NAs"));
                }
                (value & 1) as u8
            }
            _ => return Err(call.error("argument 'x' must be raw, integer or logical")),
        };
        flags.push(bit as u64);
    }
    let packed: Vec<u64> = flags
        .chunks(width)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0u64, |word, (bit, flag)| word | (flag << bit))
        })
        .collect();
    match width {
        8 => {
            let result = alloc(SexpType::Raw, packed.len())?;
            for (index, word) in packed.iter().enumerate() {
                values::raws_mut(result)[index] = *word as u8;
            }
            Ok(result)
        }
        32 => {
            let result = alloc(SexpType::Integer, packed.len())?;
            for (index, word) in packed.iter().enumerate() {
                values::ints_mut(result)[index] = *word as u32 as i32;
            }
            Ok(result)
        }
        _ => {
            let result = alloc(SexpType::Real, packed.len())?;
            for (index, word) in packed.iter().enumerate() {
                values::reals_mut(result)[index] = f64::from_bits(*word);
            }
            Ok(result)
        }
    }
}

/// `polyroot(z)`: the roots of the polynomial with coefficients `z` in increasing order, by
/// Jenkins and Traub's three-stage algorithm (ACM TOMS 419), which GNU R also uses, so the roots
/// come out in the order it finds them: zeros at the origin first, then by deflation, roughly
/// smallest modulus first.
unsafe fn do_polyroot(call: &Call<'_>) -> R<Sexp> {
    let input = coerce_vector(call.arg(0), SexpType::Complex)?;
    let coefficients: Vec<Rcomplex> = values::complexes(input).to_vec();
    if coefficients.iter().any(|value| !value.r.is_finite() || !value.i.is_finite()) {
        return Err(call.error("invalid polynomial coefficient"));
    }
    let Some(degree) = coefficients.iter().rposition(|value| value.r != 0.0 || value.i != 0.0) else {
        return alloc(SexpType::Complex, 0);
    };
    let highest_first: Vec<(f64, f64)> =
        coefficients[..=degree].iter().rev().map(|value| (value.r, value.i)).collect();
    let roots = JenkinsTraub::roots(&highest_first).ok_or_else(|| call.error("root finding code failed"))?;
    let result = alloc(SexpType::Complex, roots.len())?;
    for (slot, root) in values::complexes_mut(result).iter_mut().zip(roots) {
        *slot = Rcomplex { r: root.0, i: root.1 };
    }
    Ok(result)
}

/// The state of the Jenkins-Traub iteration: the deflated polynomial `p` with the highest
/// coefficient first, the current H polynomial, the shift `s`, and the quotients that Horner
/// evaluation leaves behind.
struct JenkinsTraub {
    p: Vec<(f64, f64)>,
    h: Vec<(f64, f64)>,
    saved_h: Vec<(f64, f64)>,
    quotient_p: Vec<(f64, f64)>,
    quotient_h: Vec<(f64, f64)>,
    shift: (f64, f64),
    p_value: (f64, f64),
    t: (f64, f64),
}

const JT_ETA: f64 = f64::EPSILON;
const JT_ARE: f64 = JT_ETA;
const JT_MRE: f64 = 2.0 * std::f64::consts::SQRT_2 * JT_ETA;

impl JenkinsTraub {
    /// The zeros of the polynomial with `coefficients`, highest degree first, or `None` when the
    /// iteration fails to converge.
    fn roots(coefficients: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
        let degree = coefficients.len().checked_sub(1)?;
        let mut roots = Vec::with_capacity(degree);
        let mut count = coefficients.len();
        while count > 1 && coefficients[count - 1] == (0.0, 0.0) {
            roots.push((0.0, 0.0));
            count -= 1;
        }
        let mut state = JenkinsTraub {
            p: coefficients[..count].to_vec(),
            h: vec![(0.0, 0.0); count],
            saved_h: vec![(0.0, 0.0); count],
            quotient_p: vec![(0.0, 0.0); count],
            quotient_h: vec![(0.0, 0.0); count],
            shift: (0.0, 0.0),
            p_value: (0.0, 0.0),
            t: (0.0, 0.0),
        };
        let moduli: Vec<f64> = state.p.iter().map(|value| value.0.hypot(value.1)).collect();
        let factor = jt_scale(&moduli);
        if factor != 1.0 {
            for value in &mut state.p {
                *value = (value.0 * factor, value.1 * factor);
            }
        }
        // The shifts start at 45 degrees below the real axis and rotate by 94 degrees each time.
        let (cos_rotation, sin_rotation) = (-0.069_756_473_744_125_3_f64, 0.997_564_050_259_824_2_f64);
        let mut direction = (std::f64::consts::FRAC_1_SQRT_2, -std::f64::consts::FRAC_1_SQRT_2);
        while state.p.len() > 2 {
            let moduli: Vec<f64> = state.p.iter().map(|value| value.0.hypot(value.1)).collect();
            let bound = jt_cauchy(&moduli);
            let mut zero = None;
            'passes: for _ in 0..2 {
                state.no_shift(5);
                for attempt in 1..10 {
                    direction = (
                        cos_rotation * direction.0 - sin_rotation * direction.1,
                        sin_rotation * direction.0 + cos_rotation * direction.1,
                    );
                    state.shift = (bound * direction.0, bound * direction.1);
                    if let Some(found) = state.fixed_shift(10 * attempt) {
                        zero = Some(found);
                        break 'passes;
                    }
                }
            }
            roots.push(zero?);
            let remaining = state.p.len() - 1;
            state.p = state.quotient_p[..remaining].to_vec();
        }
        if let [first, second] = state.p[..] {
            roots.push(jt_divide((-second.0, -second.1), first));
        }
        Some(roots)
    }

    /// The first stage: H polynomials without a shift, starting from the derivative of P.
    fn no_shift(&mut self, iterations: usize) {
        let count = self.p.len();
        let n = count - 1;
        for index in 0..n {
            let weight = (count - index - 1) as f64;
            self.h[index] = (weight * self.p[index].0 / n as f64, weight * self.p[index].1 / n as f64);
        }
        for _ in 0..iterations {
            let last_h = self.h[n - 1];
            let last_p = self.p[n - 1];
            if last_h.0.hypot(last_h.1) <= JT_ETA * 10.0 * last_p.0.hypot(last_p.1) {
                for index in (1..n).rev() {
                    self.h[index] = self.h[index - 1];
                }
                self.h[0] = (0.0, 0.0);
            } else {
                let t = jt_divide((-self.p[count - 1].0, -self.p[count - 1].1), last_h);
                for index in (1..n).rev() {
                    let previous = self.h[index - 1];
                    self.h[index] = (
                        t.0 * previous.0 - t.1 * previous.1 + self.p[index].0,
                        t.0 * previous.1 + t.1 * previous.0 + self.p[index].1,
                    );
                }
                self.h[0] = self.p[0];
            }
        }
    }

    /// The second stage with a fixed shift, handing over to the third stage once the estimates
    /// settle. Returns the zero found.
    fn fixed_shift(&mut self, iterations: usize) -> Option<(f64, f64)> {
        let n = self.p.len() - 1;
        self.p_value = jt_evaluate(&self.p, self.shift, &mut self.quotient_p);
        let mut testing = true;
        let mut passed = false;
        let mut h_vanishes = self.calculate_t();
        let mut zero = self.shift;
        for step in 1..=iterations {
            let old_t = self.t;
            self.next_h(h_vanishes);
            h_vanishes = self.calculate_t();
            zero = (self.shift.0 + self.t.0, self.shift.1 + self.t.1);
            if h_vanishes || !testing || step == iterations {
                continue;
            }
            if (self.t.0 - old_t.0).hypot(self.t.1 - old_t.1) >= 0.5 * zero.0.hypot(zero.1) {
                passed = false;
                continue;
            }
            if !passed {
                passed = true;
                continue;
            }
            // The weak convergence test passed twice: try the third stage, keeping this H
            // polynomial and shift to come back to if it fails.
            self.saved_h[..n].copy_from_slice(&self.h[..n]);
            let saved_shift = self.shift;
            if let Some(found) = self.variable_shift(10, zero) {
                return Some(found);
            }
            testing = false;
            self.h[..n].copy_from_slice(&self.saved_h[..n]);
            self.shift = saved_shift;
            self.p_value = jt_evaluate(&self.p, self.shift, &mut self.quotient_p);
            h_vanishes = self.calculate_t();
        }
        self.variable_shift(10, zero)
    }

    /// The third stage: shifts that follow the estimate, until P is below its rounding error.
    fn variable_shift(&mut self, iterations: usize, start: (f64, f64)) -> Option<(f64, f64)> {
        let mut stalled = false;
        let mut old_modulus = 0.0;
        let mut relative_step: f64 = 0.0;
        self.shift = start;
        for step in 1..=iterations {
            self.p_value = jt_evaluate(&self.p, self.shift, &mut self.quotient_p);
            let modulus = self.p_value.0.hypot(self.p_value.1);
            let shift_modulus = self.shift.0.hypot(self.shift.1);
            if modulus <= 20.0 * jt_error_bound(&self.quotient_p, shift_modulus, modulus) {
                return Some(self.shift);
            }
            let mut take_fixed_steps = false;
            if step != 1 {
                if !stalled && modulus >= old_modulus && relative_step < 0.05 {
                    // The iteration has stalled, probably on a cluster of zeros: a few fixed
                    // shift steps into the cluster make one zero dominate.
                    let factor = relative_step.max(JT_ETA).sqrt();
                    stalled = true;
                    let real = self.shift.0 * (factor + 1.0) - self.shift.1 * factor;
                    self.shift.1 = self.shift.0 * factor + self.shift.1 * (factor + 1.0);
                    self.shift.0 = real;
                    self.p_value = jt_evaluate(&self.p, self.shift, &mut self.quotient_p);
                    for _ in 0..5 {
                        let vanishes = self.calculate_t();
                        self.next_h(vanishes);
                    }
                    old_modulus = f64::INFINITY;
                    take_fixed_steps = true;
                } else if modulus * 0.1 > old_modulus {
                    return None;
                }
            }
            if !take_fixed_steps {
                old_modulus = modulus;
            }
            let vanishes = self.calculate_t();
            self.next_h(vanishes);
            if self.calculate_t() {
                continue;
            }
            relative_step = self.t.0.hypot(self.t.1) / self.shift.0.hypot(self.shift.1);
            self.shift = (self.shift.0 + self.t.0, self.shift.1 + self.t.1);
        }
        None
    }

    /// `t = -P(s) / H(s)`, or zero when H(s) is negligible, which the result reports.
    fn calculate_t(&mut self) -> bool {
        let n = self.p.len() - 1;
        let h_value = jt_evaluate(&self.h[..n], self.shift, &mut self.quotient_h);
        let last = self.h[n - 1];
        let vanishes = h_value.0.hypot(h_value.1) <= JT_ARE * 10.0 * last.0.hypot(last.1);
        self.t = if vanishes {
            (0.0, 0.0)
        } else {
            jt_divide((-self.p_value.0, -self.p_value.1), h_value)
        };
        vanishes
    }

    /// The next H polynomial from the quotients of the last evaluations.
    fn next_h(&mut self, h_vanishes: bool) {
        let n = self.p.len() - 1;
        for index in 1..n {
            let previous = self.quotient_h[index - 1];
            self.h[index] = if h_vanishes {
                previous
            } else {
                (
                    self.t.0 * previous.0 - self.t.1 * previous.1 + self.quotient_p[index].0,
                    self.t.0 * previous.1 + self.t.1 * previous.0 + self.quotient_p[index].1,
                )
            };
        }
        self.h[0] = if h_vanishes { (0.0, 0.0) } else { self.quotient_p[0] };
    }
}

/// Horner evaluation of `polynomial` at `x`, leaving the partial sums in `partial`.
fn jt_evaluate(polynomial: &[(f64, f64)], x: (f64, f64), partial: &mut [(f64, f64)]) -> (f64, f64) {
    let mut value = polynomial[0];
    partial[0] = value;
    for (index, coefficient) in polynomial.iter().enumerate().skip(1) {
        value = (
            value.0 * x.0 - value.1 * x.1 + coefficient.0,
            value.0 * x.1 + value.1 * x.0 + coefficient.1,
        );
        partial[index] = value;
    }
    value
}

/// A bound on the rounding error of evaluating the polynomial whose Horner partial sums are
/// `partial`, at a point of modulus `modulus` where its value has modulus `value_modulus`.
fn jt_error_bound(partial: &[(f64, f64)], modulus: f64, value_modulus: f64) -> f64 {
    let mut bound = partial[0].0.hypot(partial[0].1) * JT_MRE / (JT_ARE + JT_MRE);
    for value in partial {
        bound = bound * modulus + value.0.hypot(value.1);
    }
    bound * (JT_ARE + JT_MRE) - value_modulus * JT_MRE
}

/// A lower bound on the moduli of the zeros, from the polynomial with coefficient `moduli`
/// whose constant term is negated (Cauchy's bound), refined by Newton's method.
fn jt_cauchy(moduli: &[f64]) -> f64 {
    let count = moduli.len();
    let n = count - 1;
    let mut terms = moduli.to_vec();
    terms[n] = -terms[n];
    let mut x = ((-terms[n]).ln() - terms[0].ln()) / n as f64;
    x = x.exp();
    if terms[n - 1] != 0.0 {
        x = x.min(-terms[n] / terms[n - 1]);
    }
    loop {
        let candidate = x * 0.1;
        let value = terms.iter().skip(1).fold(terms[0], |sum, term| sum * candidate + term);
        if value <= 0.0 {
            break;
        }
        x = candidate;
    }
    let mut step = x;
    let mut partial = vec![0.0; count];
    while (step / x).abs() > 0.005 {
        partial[0] = terms[0];
        for index in 1..count {
            partial[index] = partial[index - 1] * x + terms[index];
        }
        let value = partial[n];
        let derivative = partial[1..n].iter().fold(partial[0], |sum, term| sum * x + term);
        step = value / derivative;
        x -= step;
    }
    x
}

/// A power of two that brings coefficients with these `moduli` away from overflow and underflow,
/// or 1 when they are safe already.
fn jt_scale(moduli: &[f64]) -> f64 {
    let high = f64::MAX.sqrt();
    let low = f64::MIN_POSITIVE / JT_ETA;
    let largest = moduli.iter().copied().fold(0.0, f64::max);
    let smallest = moduli.iter().copied().filter(|value| *value != 0.0).fold(f64::MAX, f64::min);
    if smallest >= low && largest <= high {
        return 1.0;
    }
    let ratio = low / smallest;
    let factor = if ratio <= 1.0 {
        1.0 / (largest.sqrt() * smallest.sqrt())
    } else if f64::MAX / ratio > largest {
        1.0
    } else {
        ratio
    };
    let exponent = (factor.ln() / 2f64.ln() + 0.5) as i32;
    2f64.powi(exponent)
}

/// Complex division, infinite when the divisor is zero.
fn jt_divide(numerator: (f64, f64), divisor: (f64, f64)) -> (f64, f64) {
    let (a, b) = numerator;
    let (c, d) = divisor;
    if c == 0.0 && d == 0.0 {
        return (f64::MAX, f64::MAX);
    }
    if c.abs() < d.abs() {
        let ratio = c / d;
        let denominator = d + ratio * c;
        ((a * ratio + b) / denominator, (b * ratio - a) / denominator)
    } else {
        let ratio = d / c;
        let denominator = c + ratio * d;
        ((a + b * ratio) / denominator, (b - a * ratio) / denominator)
    }
}
