//! Native values of the typed core: strings, lists and dicts, their reference counts and their
//! allocator, and the kernels compiled code calls by address.
//!
//! Every `ry_*` function is part of a fixed ABI with the code generator. Values are immutable to
//! the program and acyclic, so a reference count is exact and nothing here traces. The compiler
//! inserts the dups and drops (Perceus style), and an operation handed an owned, uniquely held
//! buffer writes its result into that buffer instead of allocating.
//!
//! Every function runs on the R interpreter's thread, so counts are plain integers and the
//! allocator state is thread-local.
//!
//! Ownership flags: a nonzero `owned` argument passes the caller's reference to the function,
//! which either reuses the buffer or drops it, failure included. A zero flag borrows it. Results
//! are always owned. A function that fails returns null (or its documented status) after
//! consuming its owned arguments, so the caller reruns the whole operation in the interpreter.

// Every function dereferences raw pointers handed over by compiled code, so the module is an
// FFI-style unsafe layer and an `unsafe` block inside each `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` has the contract the module documentation states: each pointer is a live value
// of the kind the function expects, and each length and index is in bounds.
#![allow(clippy::missing_safety_doc)]

use {
    heap::object::{NA_INTEGER, NA_LOGICAL, NA_REAL, NA_REAL_BITS, is_na_real},
    interp::{
        arith::{
            long_double_mean, long_double_mean_from_total, modulus_loses_accuracy, power,
            real_integer_divide, real_modulo,
        },
        format::format_real_alone,
        long_double::{LongDouble, add_all},
        rmath::fround,
    },
    std::{
        alloc::{self, Layout},
        cell::Cell,
        fmt::{self, Write},
        hash::{BuildHasher, Hasher},
        ptr::{self, null_mut},
        slice,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[cfg(not(target_endian = "little"))]
compile_error!("the typed value layout assumes a little-endian target");

/// A UTF-8 string in two words. Up to 15 bytes live inline, with `0x80 | length` in the top byte
/// of `hi` and zeros in the unused bytes; a longer string has `lo` pointing at a counted buffer
/// and `hi` holding the byte length. The form is canonical, so two equal strings have equal `hi`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Str {
    pub lo: u64,
    pub hi: u64,
}

/// A list header. The compiler reads `length` and `data` at their offsets and updates `count`
/// inline, so the layout is fixed: count 0, kind 4, flags 5, length 8, capacity 16, data 24.
#[repr(C)]
pub struct List {
    pub count: u32,
    pub kind: u8,
    pub flags: u8,
    pub _pad: u16,
    pub length: u64,
    pub capacity: u64,
    pub data: *mut u8,
}

pub const INT: u8 = 0;
pub const DBL: u8 = 1;
pub const LGL: u8 = 2;
pub const STR: u8 = 3;
pub const DICT: u8 = 16;

/// The list's items belong to someone else (an R vector the compiler borrows).
pub const VIEW: u8 = 1;

/// A count that is never changed: a view, a static, or a value whose count saturated.
pub const STATIC_COUNT: u32 = u32::MAX;

pub const ADD: u32 = 0;
pub const SUB: u32 = 1;
pub const MUL: u32 = 2;
pub const DIV: u32 = 3;
pub const POW: u32 = 4;
pub const MOD: u32 = 5;
pub const IDIV: u32 = 6;
pub const MIN2: u32 = 7;
pub const MAX2: u32 = 8;

pub const EQ: u32 = 0;
pub const NE: u32 = 1;
pub const LT: u32 = 2;
pub const LE: u32 = 3;
pub const GT: u32 = 4;
pub const GE: u32 = 5;

pub const AND: u32 = 0;
pub const OR: u32 = 1;

pub const NEG: u32 = 0;
pub const EXP: u32 = 1;
pub const LOG: u32 = 2;
pub const SQRT: u32 = 3;
pub const ABS: u32 = 4;
pub const SIN: u32 = 5;
pub const COS: u32 = 6;
pub const FLOOR: u32 = 7;
pub const CEILING: u32 = 8;
pub const TRUNC: u32 = 9;
pub const LOG2: u32 = 10;
pub const LOG10: u32 = 11;
pub const TAN: u32 = 12;
pub const ROUND0: u32 = 13;
pub const SIGN: u32 = 14;

impl Str {
    pub const EMPTY: Str = Str {
        lo: 0,
        hi: INLINE_FLAG,
    };

    pub fn is_inline(self) -> bool {
        self.hi & INLINE_FLAG != 0
    }

    pub fn len(self) -> usize {
        if self.is_inline() {
            (self.hi >> 56) as usize & 0x7F
        } else {
            self.hi as usize
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The string's bytes; `self` must be a live string.
    pub unsafe fn bytes(&self) -> &[u8] {
        if self.is_inline() {
            slice::from_raw_parts(self as *const Str as *const u8, self.len())
        } else {
            slice::from_raw_parts((self.lo as *const u8).add(STR_HEADER), self.hi as usize)
        }
    }

    /// The string's text; `self` must be a live string.
    pub unsafe fn as_str(&self) -> &str {
        std::str::from_utf8_unchecked(self.bytes())
    }
}

/// Fixes the seed of dicts created from now on. Each dict keeps the seed it was created with, so
/// changing it never disturbs a live dict.
pub fn set_hash_seed(seed: u64) {
    HASH_SEED.store(seed, Ordering::Relaxed);
    HASH_SEEDED.store(true, Ordering::Relaxed);
}

// Counting and freeing.

/// Frees a list or a dict whose count reached zero, dropping its items.
pub unsafe extern "C" fn ry_list_free(list: *mut List) {
    if list.is_null() {
        return;
    }
    if (*list).kind == DICT {
        dict_free(list as *mut Dict);
        return;
    }
    if (*list).flags & VIEW != 0 {
        block_free(list as *mut u8, HEADER_BYTES);
        return;
    }
    if (*list).kind == STR {
        for &item in items::<Str>(list) {
            str_drop(item);
        }
    }
    list_release_storage(list);
}

/// Frees a heap string buffer whose count reached zero.
pub unsafe extern "C" fn ry_str_free(buffer: *mut u8) {
    let capacity = (*(buffer as *mut StrBuffer)).capacity as usize;
    block_free(buffer, STR_HEADER + capacity);
    live_add(-1);
}

pub unsafe extern "C" fn ry_list_dup(list: *mut List) {
    if !list.is_null() {
        count_add(&mut (*list).count, 1);
    }
}

pub unsafe extern "C" fn ry_list_drop(list: *mut List) {
    if list.is_null() {
        return;
    }
    let count = (*list).count;
    if count == STATIC_COUNT {
        return;
    }
    if count <= 1 {
        ry_list_free(list);
    } else {
        (*list).count = count - 1;
    }
}

pub unsafe extern "C" fn ry_str_dup(lo: u64, hi: u64) {
    str_dup(Str { lo, hi });
}

pub unsafe extern "C" fn ry_str_drop(lo: u64, hi: u64) {
    str_drop(Str { lo, hi });
}

/// The number of counted lists, dicts and heap string buffers alive on this thread.
pub unsafe extern "C" fn ry_live_objects() -> i64 {
    LIVE_OBJECTS.with(Cell::get)
}

// Construction.

/// `n` zeros, `FALSE`s, or empty strings.
pub unsafe extern "C" fn ry_list_new(kind: u32, length: u64) -> *mut List {
    let Some(kind) = list_kind(kind) else {
        return null_mut();
    };
    let length = length as usize;
    let list = list_alloc(kind, length, length);
    if kind == STR {
        slice_mut::<Str>((*list).data, length).fill(Str::EMPTY);
    } else {
        ptr::write_bytes((*list).data, 0, length * item_size(kind));
    }
    list
}

/// `from:to` as R counts it: up by one, or down by one when `to < from`.
/// `seq_len(count)`: the integers from 1 to `count`, which is not negative.
pub unsafe extern "C" fn ry_list_seq_len(count: u64) -> *mut List {
    let length = count as usize;
    let list = list_alloc(INT, length, length);
    for (offset, slot) in slice_mut::<i32>((*list).data, length)
        .iter_mut()
        .enumerate()
    {
        *slot = offset as i32 + 1;
    }
    list
}

pub unsafe extern "C" fn ry_list_seq(from: i32, to: i32) -> *mut List {
    let length = (i64::from(to) - i64::from(from)).unsigned_abs() as usize + 1;
    let list = list_alloc(INT, length, length);
    let target = slice_mut::<i32>((*list).data, length);
    if to >= from {
        for (offset, slot) in target.iter_mut().enumerate() {
            *slot = from.wrapping_add(offset as i32);
        }
    } else {
        for (offset, slot) in target.iter_mut().enumerate() {
            *slot = from.wrapping_sub(offset as i32);
        }
    }
    list
}

/// One item repeated `times` times. A string item is borrowed and gains one count per copy.
pub unsafe extern "C" fn ry_list_rep(kind: u32, word0: u64, word1: u64, times: u64) -> *mut List {
    let Some(kind) = list_kind(kind) else {
        return null_mut();
    };
    let length = times as usize;
    let list = list_alloc(kind, length, length);
    match kind {
        DBL => slice_mut::<f64>((*list).data, length).fill(f64::from_bits(word0)),
        STR => {
            let item = Str {
                lo: word0,
                hi: word1,
            };
            if !item.is_inline() && length > 0 {
                count_add(&mut (*(item.lo as *mut StrBuffer)).count, times);
            }
            slice_mut::<Str>((*list).data, length).fill(item);
        }
        _ => slice_mut::<i32>((*list).data, length).fill(word_int(word0)),
    }
    list
}

/// A view header over foreign items. Its count is static, so drops leave it alone; only
/// `ry_view_release` frees it, and never the items.
pub unsafe extern "C" fn ry_list_view(kind: u32, data: *mut u8, length: u64) -> *mut List {
    let Some(kind) = list_kind(kind) else {
        return null_mut();
    };
    let list = block_alloc(HEADER_BYTES) as *mut List;
    list.write(List {
        count: STATIC_COUNT,
        kind,
        flags: VIEW,
        _pad: 0,
        length,
        capacity: length,
        data,
    });
    list
}

pub unsafe extern "C" fn ry_view_release(list: *mut List) {
    if !list.is_null() {
        block_free(list as *mut u8, HEADER_BYTES);
    }
}

/// A list with count one the caller may write: `list` itself when it is owned and unique, else
/// a copy.
pub unsafe extern "C" fn ry_list_unshare(list: *mut List, owned: u32) -> *mut List {
    if owned != 0 && is_unique(list) {
        return list;
    }
    let length = (*list).length as usize;
    let copy = list_copy(list, length);
    consume(list, owned);
    copy
}

/// Appends one item, moving a string item in. The capacity doubles when it runs out, so a loop
/// of pushes on a unique list is amortized constant time.
pub unsafe extern "C" fn ry_list_push(
    list: *mut List,
    owned: u32,
    word0: u64,
    word1: u64,
) -> *mut List {
    let length = (*list).length as usize;
    let target = if owned != 0 && is_unique(list) {
        list_reserve(list, length + 1)
    } else {
        let copy = list_copy(list, grown_capacity(length + 1, (*list).capacity as usize));
        consume(list, owned);
        copy
    };
    write_item((*target).data, (*target).kind, length, word0, word1);
    (*target).length = length as u64 + 1;
    target
}

/// `as.double`, `as.integer` of a logical, or `as.character` of a number or logical. Null for
/// any other pair, and for `as.character` of an `NA`, since `Str` has no `NA`.
pub unsafe extern "C" fn ry_list_coerce(list: *mut List, owned: u32, kind: u32) -> *mut List {
    let Some(target) = list_kind(kind) else {
        consume(list, owned);
        return null_mut();
    };
    let source = (*list).kind;
    if source == target {
        return pass_through(list, owned);
    }
    let length = (*list).length as usize;
    let result = match (source, target) {
        (INT | LGL, DBL) => {
            let result = list_alloc(DBL, length, length);
            for (slot, &value) in slice_mut::<f64>((*result).data, length)
                .iter_mut()
                .zip(items::<i32>(list))
            {
                *slot = int_to_dbl(value);
            }
            result
        }
        (LGL, INT) => {
            if owned != 0 && is_unique(list) {
                (*list).kind = INT;
                return list;
            }
            let result = list_alloc(INT, length, length);
            ptr::copy_nonoverlapping((*list).data, (*result).data, length * 4);
            result
        }
        (INT, STR) => {
            let source = items::<i32>(list);
            if source.contains(&NA_INTEGER) {
                null_mut()
            } else {
                let result = list_alloc(STR, length, length);
                for (slot, &value) in slice_mut::<Str>((*result).data, length)
                    .iter_mut()
                    .zip(source)
                {
                    *slot = format_int(value);
                }
                result
            }
        }
        (LGL, STR) => {
            let source = items::<i32>(list);
            if source.contains(&NA_LOGICAL) {
                null_mut()
            } else {
                let result = list_alloc(STR, length, length);
                for (slot, &value) in slice_mut::<Str>((*result).data, length)
                    .iter_mut()
                    .zip(source)
                {
                    *slot = str_from_bytes(if value != 0 { b"TRUE" } else { b"FALSE" });
                }
                result
            }
        }
        (DBL, STR) => {
            let source = items::<f64>(list);
            if source.iter().any(|&value| is_na_real(value)) {
                null_mut()
            } else {
                let result = list_alloc(STR, length, length);
                for (slot, &value) in slice_mut::<Str>((*result).data, length)
                    .iter_mut()
                    .zip(source)
                {
                    *slot = ry_format_dbl(value);
                }
                result
            }
        }
        _ => null_mut(),
    };
    consume(list, owned);
    result
}

// Arithmetic, comparison and logic.

/// `a op b` elementwise for two lists of `kind`, with R's recycling of a length-one operand.
/// Null for lengths that differ otherwise, for integer overflow (R warns there), and for an
/// unsupported kind or operator.
pub unsafe extern "C" fn ry_arith(
    op: u32,
    kind: u32,
    a: *mut List,
    a_owned: u32,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    arith(op, kind, list_side(a, a_owned), list_side(b, b_owned))
}

pub unsafe extern "C" fn ry_arith_vs(
    op: u32,
    kind: u32,
    a: *mut List,
    a_owned: u32,
    b: u64,
) -> *mut List {
    let mut scalar = [b, 0];
    arith(op, kind, list_side(a, a_owned), scalar_side(&mut scalar))
}

pub unsafe extern "C" fn ry_arith_sv(
    op: u32,
    kind: u32,
    a: u64,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    let mut scalar = [a, 0];
    arith(op, kind, scalar_side(&mut scalar), list_side(b, b_owned))
}

/// `a op b` elementwise as a logical list; `NA` or `NaN` on either side gives `NA`. Strings
/// support only `==` and `!=`.
pub unsafe extern "C" fn ry_compare(
    op: u32,
    kind: u32,
    a: *mut List,
    a_owned: u32,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    compare(op, kind, list_side(a, a_owned), list_side(b, b_owned))
}

/// As `ry_compare` with a scalar right operand, given as its words (a string takes both).
pub unsafe extern "C" fn ry_compare_vs(
    op: u32,
    kind: u32,
    a: *mut List,
    a_owned: u32,
    b0: u64,
    b1: u64,
) -> *mut List {
    let mut scalar = [b0, b1];
    compare(op, kind, list_side(a, a_owned), scalar_side(&mut scalar))
}

pub unsafe extern "C" fn ry_compare_sv(
    op: u32,
    kind: u32,
    a0: u64,
    a1: u64,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    let mut scalar = [a0, a1];
    compare(op, kind, scalar_side(&mut scalar), list_side(b, b_owned))
}

/// `a & b` or `a | b` on logical lists, with R's three-valued logic.
pub unsafe extern "C" fn ry_logic(
    op: u32,
    a: *mut List,
    a_owned: u32,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    logic(op, list_side(a, a_owned), list_side(b, b_owned))
}

pub unsafe extern "C" fn ry_logic_vs(op: u32, a: *mut List, a_owned: u32, b: u64) -> *mut List {
    let mut scalar = [b, 0];
    logic(op, list_side(a, a_owned), scalar_side(&mut scalar))
}

pub unsafe extern "C" fn ry_logic_sv(op: u32, a: u64, b: *mut List, b_owned: u32) -> *mut List {
    let mut scalar = [a, 0];
    logic(op, scalar_side(&mut scalar), list_side(b, b_owned))
}

pub unsafe extern "C" fn ry_not(a: *mut List, a_owned: u32) -> *mut List {
    let side = list_side(a, a_owned);
    if !matches!((*a).kind, LGL | INT) {
        release(side);
        return null_mut();
    }
    unary::<i32, i32>(side, LGL, |x| {
        if x == NA_LOGICAL {
            NA_LOGICAL
        } else {
            (x == 0) as i32
        }
    })
}

/// An elementwise math function of a double list. `NEG` and `ABS` also take an integer list and
/// keep it integer. Null when a value that is not `NaN` gives `NaN`, where R warns.
pub unsafe extern "C" fn ry_math(op: u32, a: *mut List, a_owned: u32) -> *mut List {
    let side = list_side(a, a_owned);
    match (*a).kind {
        INT | LGL if op == NEG => {
            unary::<i32, i32>(side, INT, |x| if x == NA_INTEGER { x } else { -x })
        }
        INT | LGL if op == ABS => {
            unary::<i32, i32>(side, INT, |x| if x == NA_INTEGER { x } else { x.abs() })
        }
        DBL => {
            let mut produced_nan = false;
            let result = match op {
                NEG => unary::<f64, f64>(side, DBL, |x| -x),
                ABS => unary::<f64, f64>(side, DBL, f64::abs),
                FLOOR => unary::<f64, f64>(side, DBL, f64::floor),
                CEILING => unary::<f64, f64>(side, DBL, f64::ceil),
                TRUNC => unary::<f64, f64>(side, DBL, f64::trunc),
                ROUND0 => unary::<f64, f64>(side, DBL, f64::round_ties_even),
                SIGN => unary::<f64, f64>(side, DBL, |x| {
                    if x.is_nan() {
                        x
                    } else if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                }),
                EXP => math_checked(side, &mut produced_nan, f64::exp),
                LOG => math_checked(side, &mut produced_nan, f64::ln),
                SQRT => math_checked(side, &mut produced_nan, f64::sqrt),
                SIN => math_checked(side, &mut produced_nan, f64::sin),
                COS => math_checked(side, &mut produced_nan, f64::cos),
                TAN => math_checked(side, &mut produced_nan, f64::tan),
                LOG2 => math_checked(side, &mut produced_nan, f64::log2),
                LOG10 => math_checked(side, &mut produced_nan, f64::log10),
                _ => {
                    release(side);
                    null_mut()
                }
            };
            if produced_nan {
                ry_list_drop(result);
                return null_mut();
            }
            result
        }
        _ => {
            release(side);
            null_mut()
        }
    }
}

/// `round(x, digits)` through `fround`, the function R's `round` uses. An integer or logical
/// list gives a double list, as ry's `round` does.
pub unsafe extern "C" fn ry_round(a: *mut List, a_owned: u32, digits: i32) -> *mut List {
    let side = list_side(a, a_owned);
    let digits = f64::from(digits);
    match (*a).kind {
        DBL => unary::<f64, f64>(side, DBL, |x| {
            if is_na_real(x) {
                NA_REAL
            } else {
                fround(x, digits)
            }
        }),
        INT | LGL => unary::<i32, f64>(side, DBL, |x| {
            if x == NA_INTEGER {
                NA_REAL
            } else {
                fround(f64::from(x), digits)
            }
        }),
        _ => {
            release(side);
            null_mut()
        }
    }
}

// Reductions. They borrow their argument.

/// `sum` of a double list, accumulated in x87 extended precision as GNU R does.
pub unsafe extern "C" fn ry_sum_dbl(a: *mut List) -> f64 {
    let data = items::<f64>(a);
    if let Some(total) = add_all(LongDouble::ZERO, data).finite() {
        return 0.0 + total.to_f64();
    }
    // The first NaN wins, as GNU R's long double loop gives; a sum that only overflowed is an
    // infinity.
    let sum = match data.iter().position(|value| value.is_nan()) {
        Some(at) => data[at],
        None => add_all(LongDouble::ZERO, data).to_f64(),
    };
    if is_na_real(sum) { NA_REAL } else { 0.0 + sum }
}

/// `sum` of an integer or logical list. Status 0 with the total, 1 with `NA` for an `NA` item,
/// and 2 when the total does not fit an integer (R warns there).
pub unsafe extern "C" fn ry_sum_int(a: *mut List, out: *mut i32) -> u32 {
    let mut total: i64 = 0;
    let mut na = false;
    for &value in items::<i32>(a) {
        na |= value == NA_INTEGER;
        total += i64::from(value);
    }
    if na {
        *out = NA_INTEGER;
        return 1;
    }
    if total > i64::from(i32::MAX) || total < -i64::from(i32::MAX) {
        return 2;
    }
    *out = total as i32;
    0
}

/// `prod` of a double list in extended precision; the first `NaN` wins.
pub unsafe extern "C" fn ry_prod_dbl(a: *mut List) -> f64 {
    let mut product = LongDouble::from(1.0);
    for &value in items::<f64>(a) {
        if value.is_nan() {
            return value;
        }
        product = product * LongDouble::from(value);
    }
    product.to_f64()
}

/// `max` of a double list: `NA` if any item is `NA`, else `NaN` if any is `NaN`. Status 2 for an
/// empty list, where R warns and gives `-Inf`.
pub unsafe extern "C" fn ry_max_dbl(a: *mut List, out: *mut f64) -> u32 {
    extreme_dbl(a, out, true)
}

pub unsafe extern "C" fn ry_min_dbl(a: *mut List, out: *mut f64) -> u32 {
    extreme_dbl(a, out, false)
}

/// `max` of an integer or logical list: `NA` if any item is `NA`. Status 2 for an empty list.
pub unsafe extern "C" fn ry_max_int(a: *mut List, out: *mut i32) -> u32 {
    extreme_int(a, out, true)
}

pub unsafe extern "C" fn ry_min_int(a: *mut List, out: *mut i32) -> u32 {
    extreme_int(a, out, false)
}

/// `mean`, as GNU R computes it: the extended-precision total over the count, refined by the
/// mean of the deviations from it. An integer or logical list divides its exact total once.
pub unsafe extern "C" fn ry_mean_dbl(a: *mut List) -> f64 {
    if matches!((*a).kind, INT | LGL) {
        let mut total: i64 = 0;
        for &value in items::<i32>(a) {
            if value == NA_INTEGER {
                return NA_REAL;
            }
            total += i64::from(value);
        }
        return (LongDouble::from_i64(total) / (*a).length as usize).to_f64();
    }
    let data = items::<f64>(a);
    if let Some(total) = add_all(LongDouble::ZERO, data).finite() {
        return long_double_mean_from_total(data, total).to_f64();
    }
    if data.iter().any(|&value| is_na_real(value)) {
        return NA_REAL;
    }
    long_double_mean(data).to_f64()
}

/// `any` of a logical list: `TRUE` if any item is, else `NA` if any is `NA`, else `FALSE`.
pub unsafe extern "C" fn ry_any(a: *mut List, out: *mut i32) -> u32 {
    let data = items::<i32>(a);
    let mut any = false;
    let mut na = false;
    for &value in data {
        any |= value != 0 && value != NA_LOGICAL;
        na |= value == NA_LOGICAL;
    }
    *out = if any {
        1
    } else if na {
        NA_LOGICAL
    } else {
        0
    };
    0
}

/// `all` of a logical list: `FALSE` if any item is, else `NA` if any is `NA`, else `TRUE`.
pub unsafe extern "C" fn ry_all(a: *mut List, out: *mut i32) -> u32 {
    let data = items::<i32>(a);
    let mut none_false = true;
    let mut na = false;
    for &value in data {
        none_false &= value != 0;
        na |= value == NA_LOGICAL;
    }
    *out = if !none_false {
        0
    } else if na {
        NA_LOGICAL
    } else {
        1
    };
    0
}

/// `identical` of two lists: same kind, same length and equal items, where `0` equals `-0` and
/// `NA` differs from `NaN`, as `identical`'s defaults have it.
pub unsafe extern "C" fn ry_list_equal(a: *mut List, b: *mut List) -> u32 {
    if a == b {
        return 1;
    }
    if (*a).kind != (*b).kind || (*a).length != (*b).length {
        return 0;
    }
    let equal = match (*a).kind {
        DBL => items::<f64>(a).iter().zip(items::<f64>(b)).all(|(&x, &y)| {
            if x.is_nan() || y.is_nan() {
                x.is_nan() && y.is_nan() && is_na_real(x) == is_na_real(y)
            } else {
                x == y
            }
        }),
        STR => items::<Str>(a)
            .iter()
            .zip(items::<Str>(b))
            .all(|(x, y)| str_equal(x, y)),
        _ => items::<i32>(a) == items::<i32>(b),
    };
    equal as u32
}

// Strings.

/// A new string holding a copy of `length` bytes of valid UTF-8.
pub unsafe extern "C" fn ry_str_from_bytes(bytes: *const u8, length: u64) -> Str {
    str_from_bytes(raw_slice(bytes, length as usize))
}

/// As `ry_str_from_bytes`, but a heap string gets a static count: it is never counted or freed,
/// so the compiler can bake it into code.
pub unsafe extern "C" fn ry_str_static(bytes: *const u8, length: u64) -> Str {
    let bytes = raw_slice(bytes, length as usize);
    if bytes.len() <= INLINE_CAPACITY {
        return str_from_bytes(bytes);
    }
    let buffer = str_buffer_alloc(bytes.len());
    (*(buffer as *mut StrBuffer)).count = STATIC_COUNT;
    live_add(-1);
    ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.add(STR_HEADER), bytes.len());
    Str {
        lo: buffer as u64,
        hi: bytes.len() as u64,
    }
}

/// `paste(..., sep = separator)` of scalar strings, which are borrowed.
pub unsafe extern "C" fn ry_str_concat(
    parts: *const Str,
    count: u64,
    separator_lo: u64,
    separator_hi: u64,
) -> Str {
    let parts = raw_slice(parts, count as usize);
    let separator = Str {
        lo: separator_lo,
        hi: separator_hi,
    };
    let separator = separator.bytes();
    let total = parts.iter().map(|part| part.len()).sum::<usize>()
        + separator.len() * parts.len().saturating_sub(1);
    str_build(total, |cursor| {
        for (at, part) in parts.iter().enumerate() {
            if at > 0 {
                cursor.push(separator);
            }
            cursor.push(part.bytes());
        }
    })
}

/// `paste(x, collapse = separator)` of a string list, which is borrowed.
pub unsafe extern "C" fn ry_list_join(
    list: *mut List,
    separator_lo: u64,
    separator_hi: u64,
) -> Str {
    let parts = items::<Str>(list);
    ry_str_concat(
        parts.as_ptr(),
        parts.len() as u64,
        separator_lo,
        separator_hi,
    )
}

/// Elementwise `paste(..., sep = separator)` of string lists, which are borrowed. Shorter
/// operands recycle as in R, which does not warn for `paste`, and a zero-length operand reads as
/// `""`; the result is empty only when every operand is.
pub unsafe extern "C" fn ry_list_paste(
    parts: *const *mut List,
    count: u64,
    separator_lo: u64,
    separator_hi: u64,
) -> *mut List {
    let parts = raw_slice(parts, count as usize);
    let separator = Str {
        lo: separator_lo,
        hi: separator_hi,
    };
    let separator = separator.bytes();
    let length = parts
        .iter()
        .map(|&part| (*part).length as usize)
        .max()
        .unwrap_or(0);
    let result = list_alloc(STR, length, length);
    let target = slice_mut::<Str>((*result).data, length);
    let piece = |part: *mut List, row: usize| -> &[u8] {
        let part = items::<Str>(part);
        match part.len() {
            0 => &[],
            1 => part[0].bytes(),
            count => part[row % count].bytes(),
        }
    };
    for (row, slot) in target.iter_mut().enumerate() {
        let total = parts
            .iter()
            .map(|&part| piece(part, row).len())
            .sum::<usize>()
            + separator.len() * parts.len().saturating_sub(1);
        *slot = str_build(total, |cursor| {
            for (at, &part) in parts.iter().enumerate() {
                if at > 0 {
                    cursor.push(separator);
                }
                cursor.push(piece(part, row));
            }
        });
    }
    result
}

/// `nchar(type = "chars")`: the number of Unicode scalar values.
pub unsafe extern "C" fn ry_str_nchar(lo: u64, hi: u64) -> i32 {
    char_count(Str { lo, hi }.bytes()) as i32
}

pub unsafe extern "C" fn ry_list_nchar(list: *mut List) -> *mut List {
    let source = items::<Str>(list);
    let result = list_alloc(INT, source.len(), source.len());
    for (slot, item) in slice_mut::<i32>((*result).data, source.len())
        .iter_mut()
        .zip(source)
    {
        *slot = char_count(item.bytes()) as i32;
    }
    result
}

pub unsafe extern "C" fn ry_str_toupper(lo: u64, hi: u64, owned: u32) -> Str {
    str_map_case(Str { lo, hi }, owned != 0, true)
}

pub unsafe extern "C" fn ry_str_tolower(lo: u64, hi: u64, owned: u32) -> Str {
    str_map_case(Str { lo, hi }, owned != 0, false)
}

pub unsafe extern "C" fn ry_list_toupper(list: *mut List, owned: u32) -> *mut List {
    list_map_case(list, owned != 0, true)
}

pub unsafe extern "C" fn ry_list_tolower(list: *mut List, owned: u32) -> *mut List {
    list_map_case(list, owned != 0, false)
}

pub unsafe extern "C" fn ry_str_starts_with(
    lo: u64,
    hi: u64,
    prefix_lo: u64,
    prefix_hi: u64,
) -> i32 {
    let prefix = Str {
        lo: prefix_lo,
        hi: prefix_hi,
    };
    Str { lo, hi }.bytes().starts_with(prefix.bytes()) as i32
}

pub unsafe extern "C" fn ry_str_ends_with(lo: u64, hi: u64, suffix_lo: u64, suffix_hi: u64) -> i32 {
    let suffix = Str {
        lo: suffix_lo,
        hi: suffix_hi,
    };
    Str { lo, hi }.bytes().ends_with(suffix.bytes()) as i32
}

/// `startsWith(x, prefix)` of each item of a string list, as a logical list.
pub unsafe extern "C" fn ry_list_starts_with(
    list: *mut List,
    prefix_lo: u64,
    prefix_hi: u64,
) -> *mut List {
    let prefix = Str {
        lo: prefix_lo,
        hi: prefix_hi,
    };
    list_test(list, |bytes| bytes.starts_with(prefix.bytes()))
}

pub unsafe extern "C" fn ry_list_ends_with(
    list: *mut List,
    suffix_lo: u64,
    suffix_hi: u64,
) -> *mut List {
    let suffix = Str {
        lo: suffix_lo,
        hi: suffix_hi,
    };
    list_test(list, |bytes| bytes.ends_with(suffix.bytes()))
}

pub unsafe extern "C" fn ry_str_equal(a_lo: u64, a_hi: u64, b_lo: u64, b_hi: u64) -> i32 {
    str_equal(&Str { lo: a_lo, hi: a_hi }, &Str { lo: b_lo, hi: b_hi }) as i32
}

/// Bytewise order, -1, 0 or 1: the C locale's collation.
pub unsafe extern "C" fn ry_str_compare(a_lo: u64, a_hi: u64, b_lo: u64, b_hi: u64) -> i32 {
    let a = Str { lo: a_lo, hi: a_hi };
    let b = Str { lo: b_lo, hi: b_hi };
    a.bytes().cmp(b.bytes()) as i32
}

/// `substr(x, start, stop)` counted in characters, from 1: a start below 1 reads as 1, a stop
/// past the end as the end, and `start > stop` gives `""`.
pub unsafe extern "C" fn ry_str_substr(lo: u64, hi: u64, start: i32, stop: i32) -> Str {
    let string = Str { lo, hi };
    let bytes = string.bytes();
    let start = start.max(1) as usize;
    if stop < 0 || start > stop as usize {
        return Str::EMPTY;
    }
    let stop = stop as usize;
    let mut begin = bytes.len();
    let mut end = bytes.len();
    let mut character = 0usize;
    for (offset, &byte) in bytes.iter().enumerate() {
        if !is_continuation(byte) {
            character += 1;
            if character == start {
                begin = offset;
            }
            if character == stop + 1 {
                end = offset;
                break;
            }
        }
    }
    if begin >= end {
        return Str::EMPTY;
    }
    str_from_bytes(&bytes[begin..end])
}

/// An integer in decimal. The compiler never passes `NA`.
pub unsafe extern "C" fn ry_format_int(value: i32) -> Str {
    format_int(value)
}

/// A double as `as.character` and `paste` spell it: 15 significant digits, and `NA`, `NaN`,
/// `Inf` and `-Inf`.
pub unsafe extern "C" fn ry_format_dbl(value: f64) -> Str {
    str_from_bytes(format_real_alone(value, 15).as_bytes())
}

/// `sprintf("%.Nf", value)` as ry's `sprintf` writes it: the exact binary value rounded half to
/// even, with the sign of a negative zero kept. A negative `decimals` means C's default of 6.
pub unsafe extern "C" fn ry_format_fixed(value: f64, decimals: i32) -> Str {
    if is_na_real(value) {
        return str_from_bytes(b"NA");
    }
    if value.is_nan() {
        return str_from_bytes(b"NaN");
    }
    if value.is_infinite() {
        return str_from_bytes(if value < 0.0 { b"-Inf" } else { b"Inf" });
    }
    let precision = if decimals < 0 { 6 } else { decimals as usize };
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let mut text = StackText::new();
    if write!(text, "{sign}{:.*}", precision, value.abs()).is_ok() {
        return str_from_bytes(text.as_bytes());
    }
    str_from_bytes(format!("{sign}{:.*}", precision, value.abs()).as_bytes())
}

/// `formatC(value, width = width, flag = "0")` when `zero` is nonzero, else with spaces. A
/// negative width left-justifies with spaces, and `NA` pads with spaces.
pub unsafe extern "C" fn ry_format_padded_int(value: i32, width: i32, zero: u32) -> Str {
    let mut digits = [0u8; 12];
    let digits = if value == NA_INTEGER {
        b"NA".as_slice()
    } else {
        decimal_digits(value, &mut digits)
    };
    let length = digits.len();
    let left = width < 0;
    let pad = (width.unsigned_abs() as usize).saturating_sub(length);
    str_build(length + pad, |cursor| {
        if pad == 0 {
            cursor.push(digits);
        } else if left {
            cursor.push(digits);
            cursor.fill(b' ', pad);
        } else if zero != 0 && value != NA_INTEGER {
            let (sign, rest) = match digits.split_first() {
                Some((b'-', rest)) => (b"-".as_slice(), rest),
                _ => (b"".as_slice(), digits),
            };
            cursor.push(sign);
            cursor.fill(b'0', pad);
            cursor.push(rest);
        } else {
            cursor.fill(b' ', pad);
            cursor.push(digits);
        }
    })
}

/// `Str.to_integer`: Rust's integer syntax (an optional sign and decimal digits, nothing
/// trimmed), in range, and not the bit pattern of `NA`. Status 0 on success, 1 otherwise.
pub unsafe extern "C" fn ry_str_to_int(lo: u64, hi: u64, out: *mut i32) -> u32 {
    match (Str { lo, hi }).as_str().parse::<i32>() {
        Ok(value) if value != NA_INTEGER => {
            *out = value;
            0
        }
        _ => 1,
    }
}

/// `Str.to_double`: Rust's float syntax, refusing `NaN`. Status 0 on success, 1 otherwise.
pub unsafe extern "C" fn ry_str_to_dbl(lo: u64, hi: u64, out: *mut f64) -> u32 {
    match (Str { lo, hi }).as_str().parse::<f64>() {
        Ok(value) if !value.is_nan() => {
            *out = value;
            0
        }
        _ => 1,
    }
}

/// `Str.split_on`: the pieces between occurrences of the separator, empty pieces included, and
/// the whole string as the one piece when the separator is empty.
pub unsafe extern "C" fn ry_str_split(
    lo: u64,
    hi: u64,
    separator_lo: u64,
    separator_hi: u64,
) -> *mut List {
    let string = Str { lo, hi };
    let separator = Str {
        lo: separator_lo,
        hi: separator_hi,
    };
    let separator = separator.as_str();
    if separator.is_empty() {
        let list = list_alloc(STR, 1, 1);
        str_dup(string);
        slice_mut::<Str>((*list).data, 1)[0] = string;
        return list;
    }
    let text = string.as_str();
    let length = text.split(separator).count();
    let list = list_alloc(STR, length, length);
    for (slot, piece) in slice_mut::<Str>((*list).data, length)
        .iter_mut()
        .zip(text.split(separator))
    {
        *slot = str_from_bytes(piece.as_bytes());
    }
    list
}

// Lists.

/// The string at a 0-based index, with a count of its own.
pub unsafe extern "C" fn ry_list_get_str(list: *mut List, index: u64) -> Str {
    let item = *((*list).data as *const Str).add(index as usize);
    str_dup(item);
    item
}

/// Stores an item at a 0-based index of a list it unshares first. A string item is moved in,
/// and the item it replaces is dropped.
pub unsafe extern "C" fn ry_list_set(
    list: *mut List,
    owned: u32,
    index: u64,
    word0: u64,
    word1: u64,
) -> *mut List {
    let target = ry_list_unshare(list, owned);
    let index = index as usize;
    if (*target).kind == STR {
        str_drop(*((*target).data as *const Str).add(index));
    }
    write_item((*target).data, (*target).kind, index, word0, word1);
    target
}

/// `c(a, b)` of two lists of one kind. A unique owned `a` grows in place, and a unique owned `b`
/// gives its strings up instead of having them counted again.
pub unsafe extern "C" fn ry_list_concat(
    a: *mut List,
    a_owned: u32,
    b: *mut List,
    b_owned: u32,
) -> *mut List {
    if (*a).kind != (*b).kind {
        consume(a, a_owned);
        consume(b, b_owned);
        return null_mut();
    }
    let kind = (*a).kind;
    let width = item_size(kind);
    let a_length = (*a).length as usize;
    let b_length = (*b).length as usize;
    let length = a_length + b_length;
    let target = if a_owned != 0 && is_unique(a) && a != b {
        list_reserve(a, length)
    } else {
        let target = list_alloc(kind, length, length);
        ptr::copy_nonoverlapping((*a).data, (*target).data, a_length * width);
        if kind == STR {
            for &item in items::<Str>(a) {
                str_dup(item);
            }
        }
        consume(a, a_owned);
        target
    };
    let b_moves = b_owned != 0 && is_unique(b) && b != target;
    ptr::copy_nonoverlapping(
        (*b).data,
        (*target).data.add(a_length * width),
        b_length * width,
    );
    if b_moves {
        list_release_storage(b);
    } else {
        if kind == STR {
            for &item in items::<Str>(b) {
                str_dup(item);
            }
        }
        consume(b, b_owned);
    }
    (*target).length = length as u64;
    target
}

pub unsafe extern "C" fn ry_list_reverse(a: *mut List, a_owned: u32) -> *mut List {
    if a_owned != 0 && is_unique(a) {
        let length = (*a).length as usize;
        match item_size((*a).kind) {
            4 => slice_mut::<i32>((*a).data, length).reverse(),
            8 => slice_mut::<f64>((*a).data, length).reverse(),
            _ => slice_mut::<Str>((*a).data, length).reverse(),
        }
        return a;
    }
    let length = (*a).length as usize;
    let result = list_alloc((*a).kind, length, length);
    match item_size((*a).kind) {
        4 => reverse_into(items::<i32>(a), slice_mut::<i32>((*result).data, length)),
        8 => reverse_into(items::<f64>(a), slice_mut::<f64>((*result).data, length)),
        _ => {
            reverse_into(items::<Str>(a), slice_mut::<Str>((*result).data, length));
            for &item in items::<Str>(a) {
                str_dup(item);
            }
        }
    }
    consume(a, a_owned);
    result
}

/// `sort(x)` of a numeric list: `NA` and `NaN` dropped, ascending (or descending), and stable,
/// so `0` and `-0` keep their order.
pub unsafe extern "C" fn ry_list_sort(a: *mut List, a_owned: u32, decreasing: u32) -> *mut List {
    let kind = (*a).kind;
    if !matches!(kind, INT | LGL | DBL) {
        consume(a, a_owned);
        return null_mut();
    }
    let length = (*a).length as usize;
    let target = if a_owned != 0 && is_unique(a) {
        a
    } else {
        let copy = list_copy(a, length);
        consume(a, a_owned);
        copy
    };
    let kept = if kind == DBL {
        let data = slice_mut::<f64>((*target).data, length);
        let kept = compact(data, |value| !value.is_nan());
        let data = &mut data[..kept];
        if decreasing != 0 {
            data.sort_by(|x, y| y.partial_cmp(x).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            data.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        }
        kept
    } else {
        let data = slice_mut::<i32>((*target).data, length);
        let kept = compact(data, |value| value != NA_INTEGER);
        let data = &mut data[..kept];
        if decreasing != 0 {
            data.sort_unstable_by(|x, y| y.cmp(x));
        } else {
            data.sort_unstable();
        }
        kept
    };
    (*target).length = kept as u64;
    target
}

/// `unique(x)` in first-occurrence order. Doubles compare with `0 == -0` and `NA` distinct from
/// `NaN`.
pub unsafe extern "C" fn ry_list_unique(a: *mut List, a_owned: u32) -> *mut List {
    let kind = (*a).kind;
    let length = (*a).length as usize;
    let in_place = a_owned != 0 && is_unique(a);
    let target = if in_place {
        a
    } else {
        list_alloc(kind, length, length)
    };
    let kept = match kind {
        DBL => unique_into(
            (*a).data as *const f64,
            (*target).data as *mut f64,
            length,
            |value| hash_word(UNIQUE_SEED, double_key(*value)),
            |x, y| double_key(*x) == double_key(*y),
            |_| {},
            |_| {},
        ),
        STR => unique_into(
            (*a).data as *const Str,
            (*target).data as *mut Str,
            length,
            |value| hash_bytes(UNIQUE_SEED, value.bytes()),
            |x, y| str_equal(x, y),
            |item| {
                if in_place {
                    str_drop(item);
                }
            },
            |item| {
                if !in_place {
                    str_dup(*item);
                }
            },
        ),
        _ => unique_into(
            (*a).data as *const i32,
            (*target).data as *mut i32,
            length,
            |value| hash_word(UNIQUE_SEED, *value as u32 as u64),
            |x, y| x == y,
            |_| {},
            |_| {},
        ),
    };
    (*target).length = kept as u64;
    if !in_place {
        consume(a, a_owned);
    }
    target
}

/// `count` items from the 0-based `start`. Null when the range is out of bounds.
pub unsafe extern "C" fn ry_list_slice(
    a: *mut List,
    a_owned: u32,
    start: u64,
    count: u64,
) -> *mut List {
    let length = (*a).length as usize;
    let start = start as usize;
    let count = count as usize;
    let Some(end) = start.checked_add(count).filter(|&end| end <= length) else {
        consume(a, a_owned);
        return null_mut();
    };
    let kind = (*a).kind;
    let width = item_size(kind);
    if a_owned != 0 && is_unique(a) {
        if kind == STR {
            let all = items::<Str>(a);
            for &item in all[..start].iter().chain(&all[end..]) {
                str_drop(item);
            }
        }
        ptr::copy((*a).data.add(start * width), (*a).data, count * width);
        (*a).length = count as u64;
        return a;
    }
    let result = list_alloc(kind, count, count);
    ptr::copy_nonoverlapping((*a).data.add(start * width), (*result).data, count * width);
    if kind == STR {
        for &item in items::<Str>(result) {
            str_dup(item);
        }
    }
    consume(a, a_owned);
    result
}

// Dicts.

/// An empty dict. Keys are `INT`, `DBL` or `STR`, values any list kind.
pub unsafe extern "C" fn ry_dict_new(key_kind: u32, value_kind: u32) -> *mut u8 {
    let (Some(key_kind), Some(value_kind)) = (list_kind(key_kind), list_kind(value_kind)) else {
        return null_mut();
    };
    if key_kind == LGL {
        return null_mut();
    }
    dict_alloc(key_kind, value_kind, dict_seed()) as *mut u8
}

/// The dict with `key` mapped to the value. The key is borrowed (a string key is counted when
/// stored) and the value is moved in. A new key goes last, and an existing key keeps its place.
/// Null past 2^25 - 1 entries, which needs wider index entries.
pub unsafe extern "C" fn ry_dict_insert(
    dict: *mut u8,
    owned: u32,
    key0: u64,
    key1: u64,
    value0: u64,
    value1: u64,
) -> *mut u8 {
    let dict = dict_unshare(dict as *mut Dict, owned != 0);
    let key = [key0, key1];
    let key = key.as_ptr() as *const u8;
    let hash = key_hash(dict, key);
    if let Probe::Found { position, .. } = probe(dict, key, hash)
        && !is_removed(dict, position)
    {
        let value = value_slot(dict, position);
        if (*dict).value_kind == STR {
            str_drop(*(value as *const Str));
        }
        write_item(value, (*dict).value_kind, 0, value0, value1);
        return dict as *mut u8;
    }
    if (*dict).entries == (*dict).capacity {
        let live = (*dict).entries - (*dict).removed;
        let capacity = if (*dict).capacity == 0 {
            DICT_MIN_CAPACITY
        } else if live * 2 <= (*dict).capacity {
            (*dict).capacity
        } else {
            (*dict).capacity * 2
        };
        dict_rebuild(dict, capacity);
    }
    if (*dict).entries >= MAX_ENTRIES {
        if (*dict).value_kind == STR {
            str_drop(Str {
                lo: value0,
                hi: value1,
            });
        }
        ry_list_drop(dict as *mut List);
        return null_mut();
    }
    let position = (*dict).entries;
    let key_kind = (*dict).key_kind;
    ptr::copy_nonoverlapping(key, key_slot(dict, position), item_size(key_kind));
    if key_kind == STR {
        str_dup(*(key as *const Str));
    }
    write_item(
        value_slot(dict, position),
        (*dict).value_kind,
        0,
        value0,
        value1,
    );
    (*dict).entries = position + 1;
    let slot = match probe(dict, key, hash) {
        Probe::Found { slot, .. } | Probe::Vacant { slot } => slot,
    };
    *(*dict).index.add(slot) = index_entry(hash, position);
    dict as *mut u8
}

/// 1 when `key` is present, with the value's words in `out[0]` and `out[1]` (a string value
/// counted for the caller); 0 otherwise.
pub unsafe extern "C" fn ry_dict_get(dict: *mut u8, key0: u64, key1: u64, out: *mut u64) -> u32 {
    let dict = dict as *mut Dict;
    let Some(position) = dict_find(dict, key0, key1) else {
        return 0;
    };
    let value = value_slot(dict, position);
    match (*dict).value_kind {
        DBL => {
            *out = (*(value as *const f64)).to_bits();
            *out.add(1) = 0;
        }
        STR => {
            let item = *(value as *const Str);
            str_dup(item);
            *out = item.lo;
            *out.add(1) = item.hi;
        }
        _ => {
            *out = *(value as *const i32) as u32 as u64;
            *out.add(1) = 0;
        }
    }
    1
}

/// The dict without `key`. The removed entry keeps its column position until half the entries
/// are removed, when the columns are compacted, so iteration order never changes.
pub unsafe extern "C" fn ry_dict_remove(
    dict: *mut u8,
    owned: u32,
    key0: u64,
    key1: u64,
) -> *mut u8 {
    let original = dict as *mut Dict;
    if dict_find(original, key0, key1).is_none() {
        if owned == 0 {
            ry_list_dup(original as *mut List);
        }
        return dict;
    }
    let dict = dict_unshare(original, owned != 0);
    let Some(position) = dict_find(dict, key0, key1) else {
        return dict as *mut u8;
    };
    if (*dict).value_kind == STR {
        let value = value_slot(dict, position) as *mut Str;
        str_drop(*value);
        *value = Str::EMPTY;
    }
    *(*dict).removed_bits.add(position / 64) |= 1 << (position % 64);
    (*dict).removed += 1;
    if (*dict).removed * 2 >= (*dict).entries {
        dict_rebuild(dict, (*dict).capacity);
    }
    dict as *mut u8
}

pub unsafe extern "C" fn ry_dict_len(dict: *mut u8) -> u64 {
    let dict = dict as *mut Dict;
    ((*dict).entries - (*dict).removed) as u64
}

pub unsafe extern "C" fn ry_dict_contains(dict: *mut u8, key0: u64, key1: u64) -> u32 {
    dict_find(dict as *mut Dict, key0, key1).is_some() as u32
}

/// The keys in insertion order, as a list with its own counts.
pub unsafe extern "C" fn ry_dict_keys(dict: *mut u8) -> *mut List {
    let dict = dict as *mut Dict;
    dict_column(dict, (*dict).key_kind, (*dict).keys)
}

/// The values in insertion order, as a list with its own counts.
pub unsafe extern "C" fn ry_dict_values(dict: *mut u8) -> *mut List {
    let dict = dict as *mut Dict;
    dict_column(dict, (*dict).value_kind, (*dict).values)
}

// Private types and constants.

/// A heap string's buffer header; the bytes follow at offset 8.
#[repr(C)]
struct StrBuffer {
    count: u32,
    capacity: u32,
}

/// A dict: entries in insertion order as a key column and a value column, an open-addressing
/// index of 32-bit entries (a 25-bit column position under 7 bits of the key's hash), and a
/// bitset of removed positions. The first word matches `List`'s, so counts and `ry_list_free`
/// treat both alike.
#[repr(C)]
struct Dict {
    count: u32,
    kind: u8,
    key_kind: u8,
    value_kind: u8,
    _pad: u8,
    seed: u64,
    /// Column positions in use, removed ones included.
    entries: usize,
    removed: usize,
    capacity: usize,
    index_mask: usize,
    keys: *mut u8,
    values: *mut u8,
    index: *mut u32,
    removed_bits: *mut u64,
}

enum Probe {
    Found { slot: usize, position: usize },
    Vacant { slot: usize },
}

/// One operand of an elementwise kernel: a list, or a scalar whose words sit on the caller's
/// stack (`list` null).
#[derive(Clone, Copy)]
struct Side {
    list: *mut List,
    owned: bool,
    data: *mut u8,
    length: usize,
}

struct FreeBlock {
    next: *mut FreeBlock,
}

/// Text formatted without touching the heap, for the formatting functions' common sizes.
struct StackText {
    bytes: [u8; 128],
    length: usize,
}

/// Writes bytes into a freshly allocated string's storage.
struct Cursor<'a> {
    target: &'a mut [u8],
    at: usize,
}

const INLINE_FLAG: u64 = 1 << 63;
const INLINE_CAPACITY: usize = 15;
const STR_HEADER: usize = 8;
const HEADER_BYTES: usize = 32;

const MIN_BLOCK_SHIFT: u32 = 5;
const MAX_BLOCK: usize = 1 << 16;
const CLASS_COUNT: usize = 12;
const SLAB_BYTES: usize = 1 << 16;
const SLAB_ALIGN: usize = 64;
const SYSTEM_ALIGN: usize = 16;

const DICT_MIN_CAPACITY: usize = 8;
const POSITION_BITS: u32 = 25;
const POSITION_MASK: u32 = (1 << POSITION_BITS) - 1;
const EMPTY_SLOT: u32 = u32::MAX;
/// The largest position whose index entry can never equal `EMPTY_SLOT` is one below the mask.
const MAX_ENTRIES: usize = POSITION_MASK as usize;

const HASH_K0: u64 = 0xa076_1d64_78bd_642f;
const HASH_K1: u64 = 0xe703_7ed1_a0b4_28db;
/// `unique` only groups equal items, and its output order is first occurrence, so its table
/// needs no per-process seed.
const UNIQUE_SEED: u64 = 0x2d35_8dcc_aa6c_78a5;

static HASH_SEED: AtomicU64 = AtomicU64::new(0);
static HASH_SEEDED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static FREE_STACKS: [Cell<*mut FreeBlock>; CLASS_COUNT] =
        const { [const { Cell::new(null_mut()) }; CLASS_COUNT] };
    static LIVE_OBJECTS: Cell<i64> = const { Cell::new(0) };
}

// Allocation.

/// A block of at least `size` bytes, 16-byte aligned: a pooled power of two up to 64 KiB, else
/// straight from the system allocator.
unsafe fn block_alloc(size: usize) -> *mut u8 {
    if size > MAX_BLOCK {
        let layout = system_layout(size);
        let block = alloc::alloc(layout);
        if block.is_null() {
            alloc::handle_alloc_error(layout);
        }
        return block;
    }
    let class = size_class(size);
    FREE_STACKS.with(|stacks| {
        let stack = &stacks[class];
        let mut top = stack.get();
        if top.is_null() {
            top = refill(class);
        }
        stack.set((*top).next);
        top as *mut u8
    })
}

/// Returns a block from `block_alloc(size)`; `size` must be the size it was allocated with.
unsafe fn block_free(block: *mut u8, size: usize) {
    if size > MAX_BLOCK {
        alloc::dealloc(block, system_layout(size));
        return;
    }
    let class = size_class(size);
    let block = block as *mut FreeBlock;
    FREE_STACKS.with(|stacks| {
        let stack = &stacks[class];
        (*block).next = stack.get();
        stack.set(block);
    });
}

/// Moves a block to a new size, keeping the first `min(old, new)` bytes.
unsafe fn block_realloc(block: *mut u8, old_size: usize, new_size: usize) -> *mut u8 {
    if old_size > MAX_BLOCK && new_size > MAX_BLOCK {
        let moved = alloc::realloc(block, system_layout(old_size), new_size);
        if moved.is_null() {
            alloc::handle_alloc_error(system_layout(new_size));
        }
        return moved;
    }
    let moved = block_alloc(new_size);
    ptr::copy_nonoverlapping(block, moved, old_size.min(new_size));
    block_free(block, old_size);
    moved
}

/// The size a pooled request really gets, which callers turn into spare capacity.
fn pooled_size(size: usize) -> usize {
    if size > MAX_BLOCK {
        size
    } else {
        size.max(1 << MIN_BLOCK_SHIFT).next_power_of_two()
    }
}

fn size_class(size: usize) -> usize {
    (pooled_size(size).trailing_zeros() - MIN_BLOCK_SHIFT) as usize
}

/// Carves a fresh slab into blocks of one class and returns the first; the rest go on the free
/// stack. Slabs are never returned, so a steady loop reuses warm memory.
#[cold]
unsafe fn refill(class: usize) -> *mut FreeBlock {
    let block = 1usize << (class as u32 + MIN_BLOCK_SHIFT);
    let bytes = SLAB_BYTES.max(block * 8);
    let layout = match Layout::from_size_align(bytes, SLAB_ALIGN) {
        Ok(layout) => layout,
        Err(error) => panic!("typed value slab of {bytes} bytes: {error}"),
    };
    let slab = alloc::alloc(layout);
    if slab.is_null() {
        alloc::handle_alloc_error(layout);
    }
    let count = bytes / block;
    let mut next: *mut FreeBlock = null_mut();
    for index in (1..count).rev() {
        let free = slab.add(index * block) as *mut FreeBlock;
        (*free).next = next;
        next = free;
    }
    let first = slab as *mut FreeBlock;
    (*first).next = next;
    first
}

fn system_layout(size: usize) -> Layout {
    match Layout::from_size_align(size, SYSTEM_ALIGN) {
        Ok(layout) => layout,
        Err(error) => panic!("typed value allocation of {size} bytes: {error}"),
    }
}

fn live_add(delta: i64) {
    LIVE_OBJECTS.with(|live| live.set(live.get() + delta));
}

/// Adds `amount` to a count, saturating at the static count: a value whose count would
/// overflow is leaked rather than freed early.
unsafe fn count_add(count: *mut u32, amount: u64) {
    let current = *count;
    if current != STATIC_COUNT {
        *count = (u64::from(current) + amount).min(u64::from(STATIC_COUNT)) as u32;
    }
}

// List storage.

/// A list with count one, `length` uninitialized items, and room for at least `capacity`. Items
/// share the header's block while they fit a pooled block, and get their own system block
/// beyond that, so a growing large list reallocates only its items.
unsafe fn list_alloc(kind: u8, length: usize, capacity: usize) -> *mut List {
    let width = item_size(kind);
    let bytes = capacity
        .checked_mul(width)
        .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
        .unwrap_or_else(|| panic!("typed list of {capacity} items overflows memory"));
    let (list, data, capacity) = if bytes <= MAX_BLOCK {
        let block = block_alloc(bytes);
        (
            block as *mut List,
            block.add(HEADER_BYTES),
            (pooled_size(bytes) - HEADER_BYTES) / width,
        )
    } else {
        let header = block_alloc(HEADER_BYTES);
        (header as *mut List, block_alloc(capacity * width), capacity)
    };
    list.write(List {
        count: 1,
        kind,
        flags: 0,
        _pad: 0,
        length: length as u64,
        capacity: capacity as u64,
        data,
    });
    live_add(1);
    list
}

/// Frees a list's blocks without touching its items.
unsafe fn list_release_storage(list: *mut List) {
    let width = item_size((*list).kind);
    let capacity = (*list).capacity as usize;
    let header = list as *mut u8;
    if (*list).data == header.add(HEADER_BYTES) {
        block_free(header, HEADER_BYTES + capacity * width);
    } else {
        block_free((*list).data, capacity * width);
        block_free(header, HEADER_BYTES);
    }
    live_add(-1);
}

/// A copy of `list` with count one and room for `capacity` items, its strings counted again.
unsafe fn list_copy(list: *mut List, capacity: usize) -> *mut List {
    let kind = (*list).kind;
    let length = (*list).length as usize;
    let copy = list_alloc(kind, length, capacity.max(length));
    ptr::copy_nonoverlapping((*list).data, (*copy).data, length * item_size(kind));
    if kind == STR {
        for &item in items::<Str>(copy) {
            str_dup(item);
        }
    }
    copy
}

/// A unique list with room for `needed` items, which may have moved.
unsafe fn list_reserve(list: *mut List, needed: usize) -> *mut List {
    let capacity = (*list).capacity as usize;
    if needed <= capacity {
        return list;
    }
    let kind = (*list).kind;
    let width = item_size(kind);
    let grown = grown_capacity(needed, capacity);
    let header = list as *mut u8;
    if (*list).data != header.add(HEADER_BYTES) {
        (*list).data = block_realloc((*list).data, capacity * width, grown * width);
        (*list).capacity = grown as u64;
        return list;
    }
    let moved = list_alloc(kind, (*list).length as usize, grown);
    ptr::copy_nonoverlapping((*list).data, (*moved).data, (*list).length as usize * width);
    list_release_storage(list);
    moved
}

fn grown_capacity(needed: usize, capacity: usize) -> usize {
    needed.max(capacity.saturating_mul(2)).max(4)
}

unsafe fn is_unique(list: *mut List) -> bool {
    (*list).count == 1 && (*list).flags & VIEW == 0
}

/// Drops `list` when the caller passed ownership.
unsafe fn consume(list: *mut List, owned: u32) {
    if owned != 0 {
        ry_list_drop(list);
    }
}

/// `list` returned unchanged as an owned result: moved when owned, counted again when
/// borrowed, and copied when it is a view, whose header the compiler releases on its own.
unsafe fn pass_through(list: *mut List, owned: u32) -> *mut List {
    if (*list).flags & VIEW != 0 {
        return list_copy(list, (*list).length as usize);
    }
    if owned == 0 {
        ry_list_dup(list);
    }
    list
}

unsafe fn items<'a, T>(list: *const List) -> &'a [T] {
    raw_slice((*list).data as *const T, (*list).length as usize)
}

unsafe fn raw_slice<'a, T>(data: *const T, length: usize) -> &'a [T] {
    if length == 0 {
        &[]
    } else {
        slice::from_raw_parts(data, length)
    }
}

unsafe fn slice_mut<'a, T>(data: *mut u8, length: usize) -> &'a mut [T] {
    if length == 0 {
        &mut []
    } else {
        slice::from_raw_parts_mut(data as *mut T, length)
    }
}

/// Writes one item, given as its words, at `index`; a string item is moved in.
unsafe fn write_item(data: *mut u8, kind: u8, index: usize, word0: u64, word1: u64) {
    match kind {
        DBL => *(data as *mut f64).add(index) = f64::from_bits(word0),
        STR => {
            *(data as *mut Str).add(index) = Str {
                lo: word0,
                hi: word1,
            }
        }
        _ => *(data as *mut i32).add(index) = word_int(word0),
    }
}

fn list_kind(kind: u32) -> Option<u8> {
    match kind {
        0..=3 => Some(kind as u8),
        _ => None,
    }
}

fn item_size(kind: u8) -> usize {
    match kind {
        DBL => 8,
        STR => 16,
        _ => 4,
    }
}

fn word_int(word: u64) -> i32 {
    word as u32 as i32
}

fn int_to_dbl(value: i32) -> f64 {
    if value == NA_INTEGER {
        NA_REAL
    } else {
        f64::from(value)
    }
}

// Elementwise kernels.

unsafe fn list_side(list: *mut List, owned: u32) -> Side {
    Side {
        list,
        owned: owned != 0,
        data: (*list).data,
        length: (*list).length as usize,
    }
}

fn scalar_side(words: &mut [u64; 2]) -> Side {
    Side {
        list: null_mut(),
        owned: false,
        data: words.as_mut_ptr() as *mut u8,
        length: 1,
    }
}

/// Whether the side's list can hold the result: owned, unique, owning its items, of the
/// result's kind and length. The kind check also guarantees the items have the result's type.
unsafe fn reusable(side: Side, kind: u8, length: usize) -> bool {
    !side.list.is_null()
        && side.owned
        && is_unique(side.list)
        && (*side.list).kind == kind
        && side.length == length
}

unsafe fn release(side: Side) {
    if side.owned && !side.list.is_null() {
        ry_list_drop(side.list);
    }
}

/// Whether the side's items have `kind`'s layout. A scalar always does.
unsafe fn side_has(side: Side, kind: u8) -> bool {
    side.list.is_null()
        || item_size((*side.list).kind) == item_size(kind)
            && ((*side.list).kind == STR) == (kind == STR)
}

/// The result length of two operands, or `None` for lengths R would recycle with a warning.
fn result_length(a: usize, b: usize) -> Option<usize> {
    if a == 0 || b == 0 {
        Some(0)
    } else if a == b || b == 1 {
        Some(a)
    } else if a == 1 {
        Some(b)
    } else {
        None
    }
}

/// `f` of each pair of items into a list of `kind`, written over an operand's items when that
/// operand is reusable. Each operand shape gets its own loop over plain slices so the common
/// kernels vectorize.
unsafe fn binary<T: Copy, O: Copy>(
    a: Side,
    b: Side,
    kind: u8,
    length: usize,
    mut f: impl FnMut(T, T) -> O,
) -> *mut List {
    if length > 0 && reusable(a, kind, length) && a.list != b.list {
        let target = a.data as *mut O;
        let source = a.data as *const T;
        let right = raw_slice(b.data as *const T, b.length);
        if let [y] = right {
            for index in 0..length {
                *target.add(index) = f(*source.add(index), *y);
            }
        } else {
            for (index, &y) in right.iter().enumerate() {
                *target.add(index) = f(*source.add(index), y);
            }
        }
        release(b);
        return a.list;
    }
    if length > 0 && reusable(b, kind, length) && a.list != b.list {
        let target = b.data as *mut O;
        let source = b.data as *const T;
        let left = raw_slice(a.data as *const T, a.length);
        if let [x] = left {
            for index in 0..length {
                *target.add(index) = f(*x, *source.add(index));
            }
        } else {
            for (index, &x) in left.iter().enumerate() {
                *target.add(index) = f(x, *source.add(index));
            }
        }
        release(a);
        return b.list;
    }
    let result = list_alloc(kind, length, length);
    if length > 0 {
        let out = slice_mut::<O>((*result).data, length);
        let left = raw_slice(a.data as *const T, a.length);
        let right = raw_slice(b.data as *const T, b.length);
        match (left, right) {
            ([x], [y]) => {
                if let [slot] = out {
                    *slot = f(*x, *y);
                }
            }
            (left, [y]) => {
                for (slot, &x) in out.iter_mut().zip(left) {
                    *slot = f(x, *y);
                }
            }
            ([x], right) => {
                for (slot, &y) in out.iter_mut().zip(right) {
                    *slot = f(*x, y);
                }
            }
            (left, right) => {
                for ((slot, &x), &y) in out.iter_mut().zip(left).zip(right) {
                    *slot = f(x, y);
                }
            }
        }
    }
    release(a);
    release(b);
    result
}

/// `f` of each item into a list of `kind`, in place when the operand is reusable.
unsafe fn unary<T: Copy, O: Copy>(a: Side, kind: u8, mut f: impl FnMut(T) -> O) -> *mut List {
    let length = a.length;
    if reusable(a, kind, length) {
        let target = a.data as *mut O;
        let source = a.data as *const T;
        for index in 0..length {
            *target.add(index) = f(*source.add(index));
        }
        return a.list;
    }
    let result = list_alloc(kind, length, length);
    for (slot, &x) in slice_mut::<O>((*result).data, length)
        .iter_mut()
        .zip(raw_slice(a.data as *const T, length))
    {
        *slot = f(x);
    }
    release(a);
    result
}

/// A math function that keeps `NA` and flags a `NaN` made from a value that was not `NaN`.
unsafe fn math_checked(a: Side, produced_nan: &mut bool, f: impl Fn(f64) -> f64) -> *mut List {
    unary::<f64, f64>(a, DBL, |x| {
        let value = if is_na_real(x) { NA_REAL } else { f(x) };
        *produced_nan |= value.is_nan() & !x.is_nan();
        value
    })
}

unsafe fn arith(op: u32, kind: u32, a: Side, b: Side) -> *mut List {
    let kind = match list_kind(kind) {
        Some(LGL) => Some(INT),
        Some(STR) | None => None,
        other => other,
    };
    let length = result_length(a.length, b.length);
    let (Some(kind), Some(length)) = (kind, length) else {
        release(a);
        release(b);
        return null_mut();
    };
    if !side_has(a, kind) || !side_has(b, kind) || op > MAX2 {
        release(a);
        release(b);
        return null_mut();
    }
    if kind == DBL {
        return match op {
            ADD => binary::<f64, f64>(a, b, DBL, length, |x, y| x + y),
            SUB => binary::<f64, f64>(a, b, DBL, length, |x, y| x - y),
            MUL => binary::<f64, f64>(a, b, DBL, length, |x, y| x * y),
            DIV => binary::<f64, f64>(a, b, DBL, length, |x, y| x / y),
            POW => binary::<f64, f64>(a, b, DBL, length, power),
            MOD => {
                // A quotient too large for the remainder to mean anything makes R warn, which
                // only the interpreter can do.
                let mut lost = false;
                let result = binary::<f64, f64>(a, b, DBL, length, |x, y| {
                    lost |= modulus_loses_accuracy(x, y);
                    real_modulo(x, y)
                });
                if lost {
                    ry_list_drop(result);
                    return null_mut();
                }
                result
            }
            IDIV => binary::<f64, f64>(a, b, DBL, length, real_integer_divide),
            // GNU R's `pmin` keeps the first of two equal values (so `pmin(-0, 0)` is `-0`), and
            // a NaN on the right outranks one on the left.
            MIN2 => binary::<f64, f64>(a, b, DBL, length, |x, y| {
                if y.is_nan() || (!x.is_nan() && y < x) {
                    y
                } else {
                    x
                }
            }),
            _ => binary::<f64, f64>(a, b, DBL, length, |x, y| {
                if y.is_nan() || (!x.is_nan() && y > x) {
                    y
                } else {
                    x
                }
            }),
        };
    }
    let na = |x: i32, y: i32| x == NA_INTEGER || y == NA_INTEGER;
    let mut overflow = false;
    let result = match op {
        ADD => int_checked(a, b, length, &mut overflow, |x, y| x + y),
        SUB => int_checked(a, b, length, &mut overflow, |x, y| x - y),
        MUL => int_checked(a, b, length, &mut overflow, |x, y| x * y),
        DIV => binary::<i32, f64>(a, b, DBL, length, |x, y| int_to_dbl(x) / int_to_dbl(y)),
        POW => binary::<i32, f64>(a, b, DBL, length, |x, y| {
            power(int_to_dbl(x), int_to_dbl(y))
        }),
        MOD => binary::<i32, i32>(a, b, INT, length, |x, y| {
            if na(x, y) || y == 0 {
                NA_INTEGER
            } else {
                let remainder = x.rem_euclid(y);
                if y < 0 && remainder != 0 {
                    remainder + y
                } else {
                    remainder
                }
            }
        }),
        IDIV => binary::<i32, i32>(a, b, INT, length, |x, y| {
            if na(x, y) || y == 0 {
                NA_INTEGER
            } else {
                (f64::from(x) / f64::from(y)).floor() as i32
            }
        }),
        MIN2 => binary::<i32, i32>(a, b, INT, length, |x, y| {
            if na(x, y) { NA_INTEGER } else { x.min(y) }
        }),
        _ => binary::<i32, i32>(a, b, INT, length, |x, y| {
            if na(x, y) { NA_INTEGER } else { x.max(y) }
        }),
    };
    if overflow {
        ry_list_drop(result);
        return null_mut();
    }
    result
}

/// Integer `+`, `-` or `*` computed in 64 bits, flagging a result outside the integer range or
/// equal to the `NA` bit pattern, where R gives `NA` with a warning.
unsafe fn int_checked(
    a: Side,
    b: Side,
    length: usize,
    overflow: &mut bool,
    combine: impl Fn(i64, i64) -> i64,
) -> *mut List {
    binary::<i32, i32>(a, b, INT, length, |x, y| {
        let value = combine(i64::from(x), i64::from(y));
        let missing = x == NA_INTEGER || y == NA_INTEGER;
        *overflow |= !missing & (value > i64::from(i32::MAX) || value <= i64::from(i32::MIN));
        if missing { NA_INTEGER } else { value as i32 }
    })
}

unsafe fn compare(op: u32, kind: u32, a: Side, b: Side) -> *mut List {
    let kind = list_kind(kind);
    let length = result_length(a.length, b.length);
    let (Some(kind), Some(length)) = (kind, length) else {
        release(a);
        release(b);
        return null_mut();
    };
    if !side_has(a, kind) || !side_has(b, kind) || op > GE || (kind == STR && op > NE) {
        release(a);
        release(b);
        return null_mut();
    }
    match kind {
        DBL => {
            let test = |x: f64, y: f64| -> bool {
                match op {
                    EQ => x == y,
                    NE => x != y,
                    LT => x < y,
                    LE => x <= y,
                    GT => x > y,
                    _ => x >= y,
                }
            };
            binary::<f64, i32>(a, b, LGL, length, |x, y| {
                if x.is_nan() || y.is_nan() {
                    NA_LOGICAL
                } else {
                    test(x, y) as i32
                }
            })
        }
        STR => {
            let equal = op == EQ;
            binary::<Str, i32>(a, b, LGL, length, |x, y| {
                (str_equal(&x, &y) == equal) as i32
            })
        }
        _ => {
            let test = |x: i32, y: i32| -> bool {
                match op {
                    EQ => x == y,
                    NE => x != y,
                    LT => x < y,
                    LE => x <= y,
                    GT => x > y,
                    _ => x >= y,
                }
            };
            binary::<i32, i32>(a, b, LGL, length, |x, y| {
                if x == NA_INTEGER || y == NA_INTEGER {
                    NA_LOGICAL
                } else {
                    test(x, y) as i32
                }
            })
        }
    }
}

unsafe fn logic(op: u32, a: Side, b: Side) -> *mut List {
    let Some(length) = result_length(a.length, b.length)
        .filter(|_| op <= OR && side_has(a, LGL) && side_has(b, LGL))
    else {
        release(a);
        release(b);
        return null_mut();
    };
    if op == AND {
        binary::<i32, i32>(a, b, LGL, length, |x, y| {
            if x == 0 || y == 0 {
                0
            } else if x == NA_LOGICAL || y == NA_LOGICAL {
                NA_LOGICAL
            } else {
                1
            }
        })
    } else {
        binary::<i32, i32>(a, b, LGL, length, |x, y| {
            if (x != 0 && x != NA_LOGICAL) || (y != 0 && y != NA_LOGICAL) {
                1
            } else if x == NA_LOGICAL || y == NA_LOGICAL {
                NA_LOGICAL
            } else {
                0
            }
        })
    }
}

unsafe fn extreme_dbl(a: *mut List, out: *mut f64, is_max: bool) -> u32 {
    let data = items::<f64>(a);
    let mut best = if is_max {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    let mut saw_nan = false;
    for &value in data {
        if value.is_nan() {
            if is_na_real(value) {
                *out = NA_REAL;
                return 0;
            }
            saw_nan = true;
        } else {
            best = if is_max {
                best.max(value)
            } else {
                best.min(value)
            };
        }
    }
    *out = if saw_nan { f64::NAN } else { best };
    if data.is_empty() { 2 } else { 0 }
}

unsafe fn extreme_int(a: *mut List, out: *mut i32, is_max: bool) -> u32 {
    let data = items::<i32>(a);
    if data.is_empty() {
        return 2;
    }
    if data.contains(&NA_INTEGER) {
        *out = NA_INTEGER;
        return 0;
    }
    let best = if is_max {
        data.iter().copied().max()
    } else {
        data.iter().copied().min()
    };
    *out = best.unwrap_or(NA_INTEGER);
    0
}

/// Moves the items `keep` accepts to the front, in order, and returns how many there are.
fn compact<T: Copy>(data: &mut [T], keep: impl Fn(T) -> bool) -> usize {
    let mut kept = 0;
    for read in 0..data.len() {
        let value = data[read];
        if keep(value) {
            data[kept] = value;
            kept += 1;
        }
    }
    kept
}

fn reverse_into<T: Copy>(source: &[T], target: &mut [T]) {
    for (slot, &value) in target.iter_mut().zip(source.iter().rev()) {
        *slot = value;
    }
}

/// The key `unique` and dicts group doubles by: `0` and `-0` alike, every `NA` alike, and every
/// other NaN alike.
fn double_key(value: f64) -> u64 {
    if value == 0.0 {
        0
    } else if is_na_real(value) {
        NA_REAL_BITS
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

/// Writes the first occurrence of each distinct item of `source` to `target`, which may be
/// `source` itself, since an item is only written at or before the position it is read from.
/// Returns how many were kept. A small input is scanned directly; a larger one uses one
/// open-addressing table of target positions for the whole call.
#[allow(clippy::too_many_arguments)]
unsafe fn unique_into<T: Copy>(
    source: *const T,
    target: *mut T,
    length: usize,
    hash: impl Fn(&T) -> u64,
    equal: impl Fn(&T, &T) -> bool,
    mut on_duplicate: impl FnMut(T),
    mut on_keep: impl FnMut(&T),
) -> usize {
    let mut kept = 0usize;
    if length <= 16 {
        for read in 0..length {
            let item = *source.add(read);
            if (0..kept).any(|at| equal(&*target.add(at), &item)) {
                on_duplicate(item);
            } else {
                on_keep(&item);
                *target.add(kept) = item;
                kept += 1;
            }
        }
        return kept;
    }
    let slots = (length * 2).next_power_of_two();
    let mask = slots - 1;
    let mut table = vec![u32::MAX; slots];
    for read in 0..length {
        let item = *source.add(read);
        let mut slot = hash(&item) as usize & mask;
        let duplicate = loop {
            let entry = table[slot];
            if entry == u32::MAX {
                break false;
            }
            if equal(&*target.add(entry as usize), &item) {
                break true;
            }
            slot = (slot + 1) & mask;
        };
        if duplicate {
            on_duplicate(item);
        } else {
            on_keep(&item);
            *target.add(kept) = item;
            table[slot] = kept as u32;
            kept += 1;
        }
    }
    kept
}

// String storage and helpers.

/// A heap string buffer for `length` bytes with count one, its spare block bytes as capacity.
unsafe fn str_buffer_alloc(length: usize) -> *mut u8 {
    let size = STR_HEADER
        .checked_add(length)
        .unwrap_or_else(|| panic!("typed string of {length} bytes overflows memory"));
    let capacity = pooled_size(size) - STR_HEADER;
    let Ok(capacity) = u32::try_from(capacity) else {
        panic!("typed string of {length} bytes is longer than a string buffer holds");
    };
    let buffer = block_alloc(size);
    (buffer as *mut StrBuffer).write(StrBuffer { count: 1, capacity });
    live_add(1);
    buffer
}

unsafe fn str_from_bytes(bytes: &[u8]) -> Str {
    str_build(bytes.len(), |cursor| cursor.push(bytes))
}

/// A string of exactly `length` bytes, which `fill` writes through the cursor.
unsafe fn str_build(length: usize, fill: impl FnOnce(&mut Cursor)) -> Str {
    if length <= INLINE_CAPACITY {
        let mut bytes = [0u8; 16];
        fill(&mut Cursor {
            target: &mut bytes[..length],
            at: 0,
        });
        bytes[15] = 0x80 | length as u8;
        let [lo, hi] = std::mem::transmute::<[u8; 16], [u64; 2]>(bytes);
        return Str { lo, hi };
    }
    let buffer = str_buffer_alloc(length);
    fill(&mut Cursor {
        target: slice::from_raw_parts_mut(buffer.add(STR_HEADER), length),
        at: 0,
    });
    Str {
        lo: buffer as u64,
        hi: length as u64,
    }
}

unsafe fn str_dup(string: Str) {
    if !string.is_inline() {
        count_add(&mut (*(string.lo as *mut StrBuffer)).count, 1);
    }
}

unsafe fn str_drop(string: Str) {
    if string.is_inline() {
        return;
    }
    let buffer = string.lo as *mut StrBuffer;
    let count = (*buffer).count;
    if count == STATIC_COUNT {
        return;
    }
    if count <= 1 {
        ry_str_free(buffer as *mut u8);
    } else {
        (*buffer).count = count - 1;
    }
}

unsafe fn str_is_unique(string: Str) -> bool {
    !string.is_inline() && (*(string.lo as *mut StrBuffer)).count == 1
}

/// Canonical form makes `hi` decide most comparisons: it differs for different lengths and for
/// inline against heap, and holds an inline string's last bytes.
unsafe fn str_equal(a: &Str, b: &Str) -> bool {
    if a.hi != b.hi {
        return false;
    }
    if a.is_inline() || a.lo == b.lo {
        return a.lo == b.lo;
    }
    a.bytes() == b.bytes()
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

fn char_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| !is_continuation(byte)).count()
}

/// R's `toupper` and `tolower` map one character to one character, so a mapping that expands
/// (`ß` to `SS`) keeps the character.
fn map_char(character: char, upper: bool) -> char {
    let mapped = if upper {
        let mut mapped = character.to_uppercase();
        (mapped.next(), mapped.next())
    } else {
        let mut mapped = character.to_lowercase();
        (mapped.next(), mapped.next())
    };
    match mapped {
        (Some(single), None) => single,
        _ => character,
    }
}

/// The case-mapped string, written over an owned unique buffer when every character keeps its
/// width.
unsafe fn str_map_case(string: Str, owned: bool, upper: bool) -> Str {
    let bytes = string.bytes();
    let reuse = owned && str_is_unique(string);
    if bytes.is_ascii() {
        if reuse {
            let target =
                slice::from_raw_parts_mut((string.lo as *mut u8).add(STR_HEADER), bytes.len());
            if upper {
                target.make_ascii_uppercase();
            } else {
                target.make_ascii_lowercase();
            }
            return string;
        }
        let mapped = str_build(bytes.len(), |cursor| {
            for &byte in bytes {
                cursor.push(&[if upper {
                    byte.to_ascii_uppercase()
                } else {
                    byte.to_ascii_lowercase()
                }]);
            }
        });
        if owned {
            str_drop(string);
        }
        return mapped;
    }
    let text = string.as_str();
    let mut length = 0;
    let mut same_widths = true;
    for character in text.chars() {
        let width = map_char(character, upper).len_utf8();
        same_widths &= width == character.len_utf8();
        length += width;
    }
    if reuse && same_widths {
        let target = slice::from_raw_parts_mut((string.lo as *mut u8).add(STR_HEADER), bytes.len());
        let mut at = 0;
        while at < target.len() {
            let width = utf8_width(target[at]);
            let mapped = std::str::from_utf8_unchecked(&target[at..at + width])
                .chars()
                .next()
                .map(|character| map_char(character, upper));
            if let Some(mapped) = mapped {
                mapped.encode_utf8(&mut target[at..at + width]);
            }
            at += width;
        }
        return string;
    }
    let mapped = str_build(length, |cursor| {
        let mut buffer = [0u8; 4];
        for character in text.chars() {
            cursor.push(
                map_char(character, upper)
                    .encode_utf8(&mut buffer)
                    .as_bytes(),
            );
        }
    });
    if owned {
        str_drop(string);
    }
    mapped
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

unsafe fn list_map_case(list: *mut List, owned: bool, upper: bool) -> *mut List {
    if (*list).kind != STR {
        consume(list, owned as u32);
        return null_mut();
    }
    let length = (*list).length as usize;
    if owned && is_unique(list) {
        for item in slice_mut::<Str>((*list).data, length) {
            *item = str_map_case(*item, true, upper);
        }
        return list;
    }
    let result = list_alloc(STR, length, length);
    for (slot, &item) in slice_mut::<Str>((*result).data, length)
        .iter_mut()
        .zip(items::<Str>(list))
    {
        *slot = str_map_case(item, false, upper);
    }
    consume(list, owned as u32);
    result
}

unsafe fn list_test(list: *mut List, test: impl Fn(&[u8]) -> bool) -> *mut List {
    let source = items::<Str>(list);
    let result = list_alloc(LGL, source.len(), source.len());
    for (slot, item) in slice_mut::<i32>((*result).data, source.len())
        .iter_mut()
        .zip(source)
    {
        *slot = test(item.bytes()) as i32;
    }
    result
}

/// The decimal digits of `value`, with its sign, written at the end of `buffer`.
fn decimal_digits(value: i32, buffer: &mut [u8; 12]) -> &[u8] {
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

/// An integer's decimal text, always inline since it has at most 11 bytes.
unsafe fn format_int(value: i32) -> Str {
    let mut buffer = [0u8; 12];
    str_from_bytes(decimal_digits(value, &mut buffer))
}

impl Cursor<'_> {
    fn push(&mut self, bytes: &[u8]) {
        let end = self.at + bytes.len();
        if let Some(target) = self.target.get_mut(self.at..end) {
            target.copy_from_slice(bytes);
            self.at = end;
        }
    }

    fn fill(&mut self, byte: u8, count: usize) {
        let end = self.at + count;
        if let Some(target) = self.target.get_mut(self.at..end) {
            target.fill(byte);
            self.at = end;
        }
    }
}

impl StackText {
    fn new() -> StackText {
        StackText {
            bytes: [0; 128],
            length: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

impl Write for StackText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length + text.len();
        let target = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        target.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

// Dict storage and helpers.

/// The seed for a new dict: the one tests fixed, or one drawn from the OS once per process.
fn dict_seed() -> u64 {
    if !HASH_SEEDED.load(Ordering::Relaxed) {
        let seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        HASH_SEED.store(seed, Ordering::Relaxed);
        HASH_SEEDED.store(true, Ordering::Relaxed);
    }
    HASH_SEED.load(Ordering::Relaxed)
}

unsafe fn dict_alloc(key_kind: u8, value_kind: u8, seed: u64) -> *mut Dict {
    let dict = block_alloc(size_of::<Dict>()) as *mut Dict;
    dict.write(Dict {
        count: 1,
        kind: DICT,
        key_kind,
        value_kind,
        _pad: 0,
        seed,
        entries: 0,
        removed: 0,
        capacity: 0,
        index_mask: 0,
        keys: null_mut(),
        values: null_mut(),
        index: null_mut(),
        removed_bits: null_mut(),
    });
    live_add(1);
    dict
}

unsafe fn dict_free(dict: *mut Dict) {
    // Removed entries keep their keys until compaction, and their string values are already
    // replaced by the empty string, so every slot below the entry count drops safely.
    if (*dict).key_kind == STR {
        for position in 0..(*dict).entries {
            str_drop(*(key_slot(dict, position) as *const Str));
        }
    }
    if (*dict).value_kind == STR {
        for position in 0..(*dict).entries {
            str_drop(*(value_slot(dict, position) as *const Str));
        }
    }
    dict_free_columns(dict);
    block_free(dict as *mut u8, size_of::<Dict>());
    live_add(-1);
}

/// Gives `dict` empty columns with room for `capacity` entries.
unsafe fn dict_alloc_columns(dict: *mut Dict, capacity: usize) {
    (*dict).entries = 0;
    (*dict).removed = 0;
    (*dict).capacity = capacity;
    if capacity == 0 {
        (*dict).index_mask = 0;
        (*dict).keys = null_mut();
        (*dict).values = null_mut();
        (*dict).index = null_mut();
        (*dict).removed_bits = null_mut();
        return;
    }
    let slots = (capacity * 2).next_power_of_two().max(16);
    (*dict).index_mask = slots - 1;
    (*dict).keys = block_alloc(capacity * item_size((*dict).key_kind));
    (*dict).values = block_alloc(capacity * item_size((*dict).value_kind));
    (*dict).index = block_alloc(slots * 4) as *mut u32;
    ptr::write_bytes((*dict).index, 0xFF, slots);
    let words = capacity.div_ceil(64);
    (*dict).removed_bits = block_alloc(words * 8) as *mut u64;
    ptr::write_bytes((*dict).removed_bits, 0, words);
}

unsafe fn dict_free_columns(dict: *mut Dict) {
    let capacity = (*dict).capacity;
    if capacity == 0 {
        return;
    }
    block_free((*dict).keys, capacity * item_size((*dict).key_kind));
    block_free((*dict).values, capacity * item_size((*dict).value_kind));
    block_free((*dict).index as *mut u8, ((*dict).index_mask + 1) * 4);
    block_free((*dict).removed_bits as *mut u8, capacity.div_ceil(64) * 8);
}

/// Appends every live entry of `source` to `target`, which has room, and indexes it. With
/// `duplicate` the strings gain a count each; without it they move.
unsafe fn dict_copy_live(target: *mut Dict, source: *const Dict, duplicate: bool) {
    let key_width = item_size((*source).key_kind);
    let value_width = item_size((*source).value_kind);
    for position in 0..(*source).entries {
        if is_removed(source, position) {
            continue;
        }
        let at = (*target).entries;
        let key = key_slot(source, position);
        ptr::copy_nonoverlapping(key, key_slot(target, at), key_width);
        ptr::copy_nonoverlapping(
            value_slot(source, position),
            value_slot(target, at),
            value_width,
        );
        if duplicate {
            if (*source).key_kind == STR {
                str_dup(*(key as *const Str));
            }
            if (*source).value_kind == STR {
                str_dup(*(value_slot(source, position) as *const Str));
            }
        }
        let hash = key_hash(target, key);
        if let Probe::Vacant { slot } = probe(target, key, hash) {
            *(*target).index.add(slot) = index_entry(hash, at);
        }
        (*target).entries = at + 1;
    }
}

/// Moves the live entries into fresh columns of `capacity`, dropping the keys removed entries
/// kept, so the columns are dense again.
unsafe fn dict_rebuild(dict: *mut Dict, capacity: usize) {
    let mut fresh = Dict { ..*dict };
    dict_alloc_columns(&mut fresh, capacity);
    dict_copy_live(&mut fresh, dict, false);
    if (*dict).key_kind == STR {
        for position in 0..(*dict).entries {
            if is_removed(dict, position) {
                str_drop(*(key_slot(dict, position) as *const Str));
            }
        }
    }
    dict_free_columns(dict);
    (*dict).entries = fresh.entries;
    (*dict).removed = 0;
    (*dict).capacity = fresh.capacity;
    (*dict).index_mask = fresh.index_mask;
    (*dict).keys = fresh.keys;
    (*dict).values = fresh.values;
    (*dict).index = fresh.index;
    (*dict).removed_bits = fresh.removed_bits;
}

/// `dict` itself when owned and unique, else a dense copy with its own counts.
unsafe fn dict_unshare(dict: *mut Dict, owned: bool) -> *mut Dict {
    if owned && (*dict).count == 1 {
        return dict;
    }
    let copy = dict_alloc((*dict).key_kind, (*dict).value_kind, (*dict).seed);
    dict_alloc_columns(copy, (*dict).capacity);
    dict_copy_live(copy, dict, true);
    if owned {
        ry_list_drop(dict as *mut List);
    }
    copy
}

/// The column position of a live entry for the key given as words.
unsafe fn dict_find(dict: *const Dict, key0: u64, key1: u64) -> Option<usize> {
    let key = [key0, key1];
    let key = key.as_ptr() as *const u8;
    match probe(dict, key, key_hash(dict, key)) {
        Probe::Found { position, .. } if !is_removed(dict, position) => Some(position),
        _ => None,
    }
}

/// Finds the index slot holding `key`, or the empty slot where it would go. A key has at most
/// one slot: re-inserting a removed key points its slot at the new position.
unsafe fn probe(dict: *const Dict, key: *const u8, hash: u64) -> Probe {
    if (*dict).index.is_null() {
        return Probe::Vacant { slot: 0 };
    }
    let fragment = (hash >> (64 - (32 - POSITION_BITS))) as u32;
    let mask = (*dict).index_mask;
    let mut slot = hash as usize & mask;
    loop {
        let entry = *(*dict).index.add(slot);
        if entry == EMPTY_SLOT {
            return Probe::Vacant { slot };
        }
        if entry >> POSITION_BITS == fragment {
            let position = (entry & POSITION_MASK) as usize;
            if key_equal((*dict).key_kind, key, key_slot(dict, position)) {
                return Probe::Found { slot, position };
            }
        }
        slot = (slot + 1) & mask;
    }
}

fn index_entry(hash: u64, position: usize) -> u32 {
    let fragment = (hash >> (64 - (32 - POSITION_BITS))) as u32;
    (fragment << POSITION_BITS) | position as u32
}

unsafe fn is_removed(dict: *const Dict, position: usize) -> bool {
    *(*dict).removed_bits.add(position / 64) & (1 << (position % 64)) != 0
}

unsafe fn key_slot(dict: *const Dict, position: usize) -> *mut u8 {
    (*dict).keys.add(position * item_size((*dict).key_kind))
}

unsafe fn value_slot(dict: *const Dict, position: usize) -> *mut u8 {
    (*dict).values.add(position * item_size((*dict).value_kind))
}

unsafe fn key_hash(dict: *const Dict, key: *const u8) -> u64 {
    let seed = (*dict).seed;
    match (*dict).key_kind {
        STR => hash_bytes(seed, (*(key as *const Str)).bytes()),
        DBL => hash_word(seed, double_key(*(key as *const f64))),
        _ => hash_word(seed, *(key as *const i32) as u32 as u64),
    }
}

unsafe fn key_equal(kind: u8, a: *const u8, b: *const u8) -> bool {
    match kind {
        STR => str_equal(&*(a as *const Str), &*(b as *const Str)),
        DBL => double_key(*(a as *const f64)) == double_key(*(b as *const f64)),
        _ => *(a as *const i32) == *(b as *const i32),
    }
}

/// A key or value column of the live entries as a list, its strings counted again.
unsafe fn dict_column(dict: *const Dict, kind: u8, column: *mut u8) -> *mut List {
    let length = (*dict).entries - (*dict).removed;
    let list = list_alloc(kind, length, length);
    let width = item_size(kind);
    let mut at = 0;
    for position in 0..(*dict).entries {
        if is_removed(dict, position) {
            continue;
        }
        ptr::copy_nonoverlapping(
            column.add(position * width),
            (*list).data.add(at * width),
            width,
        );
        at += 1;
    }
    if kind == STR {
        for &item in items::<Str>(list) {
            str_dup(item);
        }
    }
    list
}

/// The folded 128-bit product wyhash builds on.
fn fold_multiply(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    (product as u64) ^ ((product >> 64) as u64)
}

fn hash_word(seed: u64, word: u64) -> u64 {
    fold_multiply(word ^ seed ^ HASH_K0, HASH_K1 ^ seed.rotate_left(32))
}

fn hash_bytes(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed ^ HASH_K0 ^ bytes.len() as u64;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for chunk in chunks {
        hash = fold_multiply(hash ^ u64::from_le_bytes(*chunk), HASH_K1);
    }
    if !rest.is_empty() {
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        hash = fold_multiply(hash ^ u64::from_le_bytes(word), HASH_K1);
    }
    fold_multiply(hash, HASH_K0 ^ seed)
}
