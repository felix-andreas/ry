//! Arithmetic helpers, sorting and the random number generator.
//!
//! The generator is R's default, the Mersenne Twister with R's seeding and its fix-up into the
//! open interval (0, 1), and its state lives where R keeps it, in `.Random.seed` in the global
//! environment, so `GetRNGstate` and `PutRNGstate` exchange it with R code exactly as in GNU R.

use crate::{
    boundary::{ApiResult, enter, error},
    environments::frame_value,
    globals::R_SeedsSymbol,
    runtime,
};
use heap::object::{NA_INTEGER, Rcomplex, SexpType, is_na_real};
use std::{cmp::Ordering, ffi::c_int};

const STATE_WORDS: usize = 624;
/// `.Random.seed[1]` for the default kinds: Mersenne Twister, inversion for normals, and
/// rejection sampling.
const DEFAULT_KIND: i32 = 10403;
const I2_32M1: f64 = 2.328306437080797e-10;

pub struct Rng {
    /// `dummy[0]` is the position in the state, `dummy[1..]` the state itself, as R lays out
    /// the seed vector.
    dummy: [u32; STATE_WORDS + 1],
}

impl Default for Rng {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u32 ^ std::process::id());
        let mut rng = Rng {
            dummy: [0; STATE_WORDS + 1],
        };
        rng.seed(seed);
        rng
    }
}

impl Rng {
    /// `set.seed(seed)`: R scrambles the seed with a linear congruential generator before it
    /// fills the state.
    pub fn seed(&mut self, seed: u32) {
        let mut seed = seed;
        for _ in 0..50 {
            seed = seed.wrapping_mul(69069).wrapping_add(1);
        }
        for word in self.dummy.iter_mut() {
            seed = seed.wrapping_mul(69069).wrapping_add(1);
            *word = seed;
        }
        self.dummy[0] = STATE_WORDS as u32;
    }

    pub fn unif_rand(&mut self) -> f64 {
        let value = self.genrand();
        if value <= 0.0 {
            0.5 * I2_32M1
        } else if 1.0 - value <= 0.0 {
            1.0 - 0.5 * I2_32M1
        } else {
            value
        }
    }

    fn genrand(&mut self) -> f64 {
        const UPPER_MASK: u32 = 0x8000_0000;
        const LOWER_MASK: u32 = 0x7fff_ffff;
        const MATRIX_A: u32 = 0x9908_b0df;
        let magnitude = |y: u32| if y & 1 == 1 { MATRIX_A } else { 0 };
        let mut position = self.dummy[0] as usize;
        let state = &mut self.dummy[1..];
        if position >= STATE_WORDS {
            if position == STATE_WORDS + 1 {
                seed_genrand(state, 4357);
            }
            for index in 0..STATE_WORDS - 397 {
                let y = (state[index] & UPPER_MASK) | (state[index + 1] & LOWER_MASK);
                state[index] = state[index + 397] ^ (y >> 1) ^ magnitude(y);
            }
            for index in STATE_WORDS - 397..STATE_WORDS - 1 {
                let y = (state[index] & UPPER_MASK) | (state[index + 1] & LOWER_MASK);
                state[index] = state[index + 397 - STATE_WORDS] ^ (y >> 1) ^ magnitude(y);
            }
            let y = (state[STATE_WORDS - 1] & UPPER_MASK) | (state[0] & LOWER_MASK);
            state[STATE_WORDS - 1] = state[396] ^ (y >> 1) ^ magnitude(y);
            position = 0;
        }
        let mut y = state[position];
        position += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        self.dummy[0] = position as u32;
        y as f64 * 2.3283064365386963e-10
    }

    /// `R_unif_index` under rejection sampling: uniform on `0..n` without the bias of
    /// `floor(n * unif_rand())`.
    pub fn unif_index(&mut self, n: f64) -> f64 {
        if n <= 0.0 {
            return 0.0;
        }
        let bits = n.log2().ceil() as u32;
        loop {
            let value = self.random_bits(bits);
            if n > value {
                return value;
            }
        }
    }

    fn random_bits(&mut self, bits: u32) -> f64 {
        let mut value: i64 = 0;
        let mut drawn = 0;
        while drawn <= bits {
            let chunk = (self.unif_rand() * 65536.0).floor() as i64;
            value = value.wrapping_mul(65536).wrapping_add(chunk);
            drawn += 16;
        }
        let mask = if bits >= 63 {
            i64::MAX
        } else {
            (1i64 << bits) - 1
        };
        (value & mask) as f64
    }
}

fn seed_genrand(state: &mut [u32], seed: u32) {
    let mut seed = seed;
    for word in state.iter_mut() {
        *word = seed & 0xffff_0000;
        seed = seed.wrapping_mul(69069).wrapping_add(1);
        *word |= (seed & 0xffff_0000) >> 16;
        seed = seed.wrapping_mul(69069).wrapping_add(1);
    }
}

pub(crate) unsafe fn read_seed() -> ApiResult<()> {
    let runtime = runtime();
    let known = *runtime.heap.known();
    let seed = frame_value(known.global_env, R_SeedsSymbol.get());
    if seed == known.unbound {
        return Ok(());
    }
    if seed.kind() != SexpType::Integer || seed.length() != (STATE_WORDS + 2) as isize {
        return Err(error("'.Random.seed' is not a valid integer vector"));
    }
    let values = seed.slice::<i32>();
    if values[0] != DEFAULT_KIND {
        return Err(error(
            "ry supports only the default RNG kinds (Mersenne-Twister, Inversion, Rejection)",
        ));
    }
    for (word, &value) in runtime.rng.dummy.iter_mut().zip(&values[1..]) {
        *word = value as u32;
    }
    Ok(())
}

pub(crate) unsafe fn write_seed() -> ApiResult<()> {
    let runtime = runtime();
    let seed = runtime
        .heap
        .alloc_vector(SexpType::Integer, (STATE_WORDS + 2) as isize)?;
    runtime.heap.root(seed);
    let values = seed.slice_mut::<i32>();
    values[0] = DEFAULT_KIND;
    for (value, &word) in values[1..].iter_mut().zip(runtime.rng.dummy.iter()) {
        *value = word as i32;
    }
    let global = runtime.heap.known().global_env;
    runtime.heap.define(global, R_SeedsSymbol.get(), seed)?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GetRNGstate() {
    enter(|| read_seed())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PutRNGstate() {
    enter(|| write_seed())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn unif_rand() -> f64 {
    runtime().rng.unif_rand()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_unif_index(n: f64) -> f64 {
    runtime().rng.unif_index(n)
}

#[unsafe(no_mangle)]
pub extern "C" fn R_IsNA(value: f64) -> c_int {
    is_na_real(value) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_IsNaN(value: f64) -> c_int {
    (value.is_nan() && !is_na_real(value)) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_finite(value: f64) -> c_int {
    value.is_finite() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn R_isnancpp(value: f64) -> c_int {
    value.is_nan() as c_int
}

// Sorting, as `R_ext/Utils.h` declares it. Each routine reproduces the algorithm GNU R documents
// for it (Shell sort for the `R_*sort` family, Singleton's quicksort for `R_qsort*`, heapsort
// for `revsort`, Hoare's selection for the partial sorts), because the order these leave ties,
// `NA` and `NaN` in, and the companion index vectors they permute, are observable.

/// R's sort order for doubles: ascending, with `NA` and `NaN` last and equal to each other.
pub fn real_order(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
    }
}

/// R's sort order for integers: ascending, with `NA` last.
pub fn int_order(left: i32, right: i32) -> Ordering {
    match (left == NA_INTEGER, right == NA_INTEGER) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left.cmp(&right),
    }
}

/// R's sort order for complex numbers: by real part, then by imaginary part. Two values whose
/// real parts are both `NaN` are equal whatever their imaginary parts.
pub fn complex_order(left: Rcomplex, right: Rcomplex) -> Ordering {
    match (left.r.is_nan(), right.r.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => match left.r.partial_cmp(&right.r) {
            Some(Ordering::Equal) | None => real_order(left.i, right.i),
            Some(order) => order,
        },
    }
}

unsafe fn slice<'a, T>(values: *mut T, count: c_int) -> &'a mut [T] {
    if values.is_null() || count <= 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(values, count as usize)
    }
}

/// Shell sort with Knuth's increments 1, 4, 13, 40, ..., moving `index` alongside when given.
fn shell_sort<T: Copy>(
    values: &mut [T],
    mut index: Option<&mut [c_int]>,
    order: impl Fn(T, T) -> Ordering,
) {
    let count = values.len();
    let mut gap = 1;
    while gap <= count / 9 {
        gap = 3 * gap + 1;
    }
    while gap > 0 {
        for start in gap..count {
            let value = values[start];
            let carried = index.as_deref().map(|index| index[start]);
            let mut position = start;
            while position >= gap && order(values[position - gap], value) == Ordering::Greater {
                values[position] = values[position - gap];
                if let Some(index) = index.as_deref_mut() {
                    index[position] = index[position - gap];
                }
                position -= gap;
            }
            values[position] = value;
            if let (Some(index), Some(carried)) = (index.as_deref_mut(), carried) {
                index[position] = carried;
            }
        }
        gap /= 3;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isort(values: *mut c_int, count: c_int) {
    shell_sort(slice(values, count), None, int_order);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_rsort(values: *mut f64, count: c_int) {
    shell_sort(slice(values, count), None, real_order);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_csort(values: *mut Rcomplex, count: c_int) {
    shell_sort(slice(values, count), None, complex_order);
}

/// Sorts `values` ascending and permutes `index` the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rsort_with_index(values: *mut f64, index: *mut c_int, count: c_int) {
    shell_sort(slice(values, count), Some(slice(index, count)), real_order);
}

/// Sorts `values` into descending order by heapsort and permutes `index` the same way. The
/// comparisons are plain `<` and `>`, so a `NaN` lands wherever the heap puts it, as in GNU R.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_revsort(values: *mut f64, index: *mut c_int, count: c_int) {
    let values = slice(values, count);
    let index = slice(index, count);
    let count = values.len();
    if count <= 1 {
        return;
    }
    // The heap is 1-based, as in the textbook formulation the order of ties depends on.
    let mut left = count / 2 + 1;
    let mut right = count;
    loop {
        let (value, carried);
        if left > 1 {
            left -= 1;
            value = values[left - 1];
            carried = index[left - 1];
        } else {
            value = values[right - 1];
            carried = index[right - 1];
            values[right - 1] = values[0];
            index[right - 1] = index[0];
            right -= 1;
            if right == 1 {
                values[0] = value;
                index[0] = carried;
                return;
            }
        }
        let mut parent = left;
        let mut child = left * 2;
        while child <= right {
            if child < right && values[child - 1] > values[child] {
                child += 1;
            }
            if value > values[child - 1] {
                values[parent - 1] = values[child - 1];
                index[parent - 1] = index[child - 1];
                parent = child;
                child += parent;
            } else {
                child = right + 1;
            }
        }
        values[parent - 1] = value;
        index[parent - 1] = carried;
    }
}

/// Hoare's selection: afterwards `values[k]` holds what a full sort would put there, with no
/// larger value before it and no smaller one after it.
fn partial_sort<T: Copy>(values: &mut [T], k: c_int, order: impl Fn(T, T) -> Ordering) {
    let count = values.len() as isize;
    let k = k as isize;
    if !(0..count).contains(&k) {
        return;
    }
    let (mut low, mut high) = (0isize, count - 1);
    while low < high {
        let pivot = values[k as usize];
        let (mut i, mut j) = (low, high);
        while i <= j {
            while order(values[i as usize], pivot) == Ordering::Less {
                i += 1;
            }
            while order(pivot, values[j as usize]) == Ordering::Less {
                j -= 1;
            }
            if i <= j {
                values.swap(i as usize, j as usize);
                i += 1;
                j -= 1;
            }
        }
        if j < k {
            low = i;
        }
        if k < i {
            high = j;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_iPsort(values: *mut c_int, count: c_int, k: c_int) {
    partial_sort(slice(values, count), k, int_order);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_rPsort(values: *mut f64, count: c_int, k: c_int) {
    partial_sort(slice(values, count), k, real_order);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_cPsort(values: *mut Rcomplex, count: c_int, k: c_int) {
    partial_sort(slice(values, count), k, complex_order);
}

/// Singleton's quicksort (CACM Algorithm 347) on the 1-based inclusive range `first..=last`,
/// moving `index` alongside when given. Comparisons are plain `<` and `>`; GNU R leaves `NaN`
/// input undefined, and here the scans are bounded so it at worst leaves the range unsorted.
fn singleton_sort<T: Copy + PartialOrd>(
    values: &mut [T],
    mut index: Option<&mut [c_int]>,
    first: usize,
    last: usize,
) {
    if first < 1 || last > values.len() || first >= last {
        return;
    }
    // Positions are 1-based, matching the published algorithm's index arithmetic.
    let at = |values: &[T], position: usize| values[position - 1];
    let mut ratio = 0.375f64;
    let mut lower_stack = [0usize; 64];
    let mut upper_stack = [0usize; 64];
    let mut depth = 1usize;
    let (mut i, mut j) = (first, last);
    let mut partition = true;
    ratio = next_ratio(ratio);
    loop {
        if partition {
            // Median of three around a position that drifts with `ratio`.
            let mut k = i;
            let middle = i + ((j - i) as f64 * ratio) as usize;
            if at(values, i) > at(values, middle) {
                swap_with_index(values, &mut index, i, middle);
            }
            let mut l = j;
            if at(values, j) < at(values, middle) {
                swap_with_index(values, &mut index, j, middle);
                if at(values, i) > at(values, middle) {
                    swap_with_index(values, &mut index, i, middle);
                }
            }
            let pivot = at(values, middle);
            loop {
                l -= 1;
                while l > i && at(values, l) > pivot {
                    l -= 1;
                }
                k += 1;
                while k < j && at(values, k) < pivot {
                    k += 1;
                }
                if k > l {
                    break;
                }
                swap_with_index(values, &mut index, k, l);
            }
            depth += 1;
            if depth >= lower_stack.len() {
                return;
            }
            if l - i <= j - k {
                lower_stack[depth] = k;
                upper_stack[depth] = j;
                j = l;
            } else {
                lower_stack[depth] = i;
                upper_stack[depth] = l;
                i = k;
            }
        } else {
            if depth == 1 {
                return;
            }
            i = lower_stack[depth];
            j = upper_stack[depth];
            depth -= 1;
        }
        if j > i && j - i > 10 {
            partition = true;
            continue;
        }
        if i == first {
            partition = i < j;
            if partition {
                ratio = next_ratio(ratio);
            }
            continue;
        }
        // Insertion sort. The segment's left neighbour is no larger than anything in it, which
        // is what stops the inner scan in the published algorithm.
        let mut position = i - 1;
        loop {
            position += 1;
            if position >= j {
                break;
            }
            let value = at(values, position + 1);
            if at(values, position) > value {
                let carried = index.as_deref().map(|index| index[position]);
                let mut k = position;
                loop {
                    values[k] = values[k - 1];
                    if let Some(index) = index.as_deref_mut() {
                        index[k] = index[k - 1];
                    }
                    k -= 1;
                    if k < first || value.partial_cmp(&at(values, k)) != Some(Ordering::Less) {
                        break;
                    }
                }
                values[k] = value;
                if let (Some(index), Some(carried)) = (index.as_deref_mut(), carried) {
                    index[k] = carried;
                }
            }
        }
        partition = false;
    }
}

fn swap_with_index<T>(values: &mut [T], index: &mut Option<&mut [c_int]>, a: usize, b: usize) {
    values.swap(a - 1, b - 1);
    if let Some(index) = index.as_deref_mut() {
        index.swap(a - 1, b - 1);
    }
}

fn next_ratio(ratio: f64) -> f64 {
    if ratio < 0.5898437 {
        ratio + 0.0390625
    } else {
        ratio - 0.21875
    }
}

unsafe fn prefix<'a, T>(values: *mut T, last: usize) -> &'a mut [T] {
    if values.is_null() {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(values, last)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_qsort(values: *mut f64, first: usize, last: usize) {
    singleton_sort(prefix(values, last), None, first, last);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_qsort_I(values: *mut f64, index: *mut c_int, first: c_int, last: c_int) {
    if first < 1 || last <= first {
        return;
    }
    let last = last as usize;
    singleton_sort(
        prefix(values, last),
        Some(prefix(index, last)),
        first as usize,
        last,
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_qsort_int(values: *mut c_int, first: usize, last: usize) {
    singleton_sort(prefix(values, last), None, first, last);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_qsort_int_I(
    values: *mut c_int,
    index: *mut c_int,
    first: c_int,
    last: c_int,
) {
    if first < 1 || last <= first {
        return;
    }
    let last = last as usize;
    singleton_sort(
        prefix(values, last),
        Some(prefix(index, last)),
        first as usize,
        last,
    );
}

/// `qsort3` for Fortran callers: `R_qsort` with the bounds passed by reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qsort3_(values: *mut f64, first: *const c_int, last: *const c_int) {
    if *first >= 1 && *last > *first {
        R_qsort(values, *first as usize, *last as usize);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn qsort4_(
    values: *mut f64,
    index: *mut c_int,
    first: *const c_int,
    last: *const c_int,
) {
    R_qsort_I(values, index, *first, *last);
}

/// The C stack is the host's, and Rust's own guard pages catch an overflow, so there is nothing
/// to check yet.
#[unsafe(no_mangle)]
pub extern "C" fn R_CheckStack() {}

#[unsafe(no_mangle)]
pub extern "C" fn R_CheckStack2(_extra: usize) {}

/// Seeds the generator as `set.seed(seed)` does and stores `.Random.seed`. Not GNU R API; the
/// test harness compares sequences against GNU R with it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_set_seed(seed: c_int) {
    runtime().rng.seed(seed as u32);
    enter(|| write_seed());
}
