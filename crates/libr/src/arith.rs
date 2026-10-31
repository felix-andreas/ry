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
use heap::object::{NA_INTEGER, SexpType, is_na_real};
use std::ffi::c_int;

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

    /// A standard normal deviate by inversion, GNU R's default: two uniforms make one with
    /// more than 32 random bits, since a single 32-bit uniform leaves gaps in the far tails.
    pub fn norm_rand(&mut self) -> f64 {
        const BIG: f64 = 134217728.0;
        let mut u = self.unif_rand();
        u = (BIG * u).trunc() + self.unif_rand();
        crate::rmath::qnorm(u / BIG, 0.0, 1.0, true, false)
    }

    /// A standard exponential deviate by Ahrens and Dieter's algorithm SA (1972).
    pub fn exp_rand(&mut self) -> f64 {
        // q[k] = sum_{i=1}^{k+1} log(2)^i / i!, truncated to sixteen digits as the paper
        // tabulates them.
        const Q: [f64; 16] = [
            0.6931471805599453,
            0.9333736875190459,
            0.9888777961838675,
            0.9984959252914960040,
            0.9998292811061389,
            0.9999833164100727,
            0.9999985691438767,
            0.9999998906925558,
            0.9999999924734159,
            0.9999999995283275,
            0.9999999999728814,
            0.9999999999985598,
            0.9999999999999289,
            0.9999999999999968,
            0.9999999999999999,
            1.0000000000000000,
        ];
        let mut a = 0.0;
        let mut u = self.unif_rand();
        while u <= 0.0 || u >= 1.0 {
            u = self.unif_rand();
        }
        loop {
            u += u;
            if u > 1.0 {
                break;
            }
            a += Q[0];
        }
        u -= 1.0;
        if u <= Q[0] {
            return a + u;
        }
        let mut i = 0;
        let mut umin = self.unif_rand();
        loop {
            let ustar = self.unif_rand();
            if umin > ustar {
                umin = ustar;
            }
            i += 1;
            if u <= Q[i] {
                break;
            }
        }
        a + umin * Q[0]
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

unsafe fn read_seed() -> ApiResult<()> {
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

unsafe fn write_seed() -> ApiResult<()> {
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
pub unsafe extern "C" fn norm_rand() -> f64 {
    runtime().rng.norm_rand()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn exp_rand() -> f64 {
    runtime().rng.exp_rand()
}

/// `Sampletype`'s `REJECTION`, the only discrete-uniform method ry implements.
#[unsafe(no_mangle)]
pub extern "C" fn R_sample_kind() -> c_int {
    1
}

/// `Binomtype`'s `BUGGY_BTPE`: `rbinom` reproduces the stream of GNU R before the BTPE fix.
#[unsafe(no_mangle)]
pub extern "C" fn R_binom_kind() -> c_int {
    0
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

/// R's sort order for doubles: ascending, with `NA` and `NaN` last.
fn real_order(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// R's sort order for integers: ascending, with `NA` last.
fn int_order(left: i32, right: i32) -> std::cmp::Ordering {
    match (left == NA_INTEGER, right == NA_INTEGER) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.cmp(&right),
    }
}

unsafe fn slice<'a, T>(values: *mut T, count: c_int) -> &'a mut [T] {
    if values.is_null() || count <= 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(values, count as usize)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isort(values: *mut c_int, count: c_int) {
    slice(values, count).sort_by(|left, right| int_order(*left, *right));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_rsort(values: *mut f64, count: c_int) {
    slice(values, count).sort_by(|left, right| real_order(*left, *right));
}

/// Sorts `values` ascending and permutes `index` the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rsort_with_index(values: *mut f64, index: *mut c_int, count: c_int) {
    let values = slice(values, count);
    let index = slice(index, count);
    let mut pairs: Vec<(f64, c_int)> = values.iter().copied().zip(index.iter().copied()).collect();
    pairs.sort_by(|left, right| real_order(left.0, right.0));
    for (position, (value, original)) in pairs.into_iter().enumerate() {
        values[position] = value;
        index[position] = original;
    }
}

/// Sorts `values` descending and permutes `index` the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn revsort(values: *mut f64, index: *mut c_int, count: c_int) {
    let values = slice(values, count);
    let index = slice(index, count);
    let mut pairs: Vec<(f64, c_int)> = values.iter().copied().zip(index.iter().copied()).collect();
    pairs.sort_by(|left, right| real_order(right.0, left.0));
    for (position, (value, original)) in pairs.into_iter().enumerate() {
        values[position] = value;
        index[position] = original;
    }
}

/// Partial sort: element `k` ends up where a full sort would put it, smaller ones before it and
/// larger ones after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_iPsort(values: *mut c_int, count: c_int, k: c_int) {
    let values = slice(values, count);
    if (0..values.len() as c_int).contains(&k) {
        values.select_nth_unstable_by(k as usize, |left, right| int_order(*left, *right));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_rPsort(values: *mut f64, count: c_int, k: c_int) {
    let values = slice(values, count);
    if (0..values.len() as c_int).contains(&k) {
        values.select_nth_unstable_by(k as usize, |left, right| real_order(*left, *right));
    }
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
