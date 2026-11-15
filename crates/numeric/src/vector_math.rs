//! `exp` and `log` four elements at a time, bit for bit the glibc routines GNU R calls.
//!
//! GNU R's `exp(x)` and `log(x)` call glibc's scalar `exp` and `log`, which on a CPU with FMA
//! and AVX2 dispatch to `__exp_fma` and `__log_fma`: table-driven routines (ARM's optimized
//! routines) whose result depends on which steps the compiler fused into one rounding. The
//! kernels here run the same steps on four lanes, with the same tables and the same fused
//! multiply-adds in the same places, read from the disassembly of glibc 2.39's `libm.so.6`.
//! A lane outside the routine's main range (zero, a tiny or huge argument, a negative or
//! subnormal one for `log`, `NA`, NaN, infinities) goes to the caller's scalar function, which
//! calls glibc itself, so every element comes out as the scalar routine would compute it.
//!
//! Before the first use, [`available`] checks the CPU and compares the kernels against glibc
//! on inputs that cover every table entry and both of `log`'s paths. Any difference (another
//! libm, another glibc algorithm, a CPU without FMA where glibc picks its plain build) turns
//! the kernels off for the process, and callers keep their scalar loop.

use std::sync::atomic::{AtomicU8, Ordering};

/// `exp` of each element of `source` (or of `output` itself, when `source` is `None`) into
/// `output`. Lanes outside the routine's main range go through `scalar`. Returns `false`,
/// writing nothing, when the kernels are not available, and the caller runs its own loop.
pub fn exp(source: Option<&[f64]>, output: &mut [f64], scalar: &mut dyn FnMut(f64) -> f64) -> bool {
    run(Function::Exp, source, output, scalar)
}

/// `log` of each element, as [`exp`].
pub fn log(source: Option<&[f64]>, output: &mut [f64], scalar: &mut dyn FnMut(f64) -> f64) -> bool {
    run(Function::Log, source, output, scalar)
}

/// Whether the kernels run on this machine and agree with its glibc.
pub fn available() -> bool {
    match STATE.load(Ordering::Relaxed) {
        ON => true,
        OFF => false,
        _ => {
            let on = detect();
            STATE.store(if on { ON } else { OFF }, Ordering::Relaxed);
            on
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Function {
    Exp,
    Log,
}

const UNKNOWN: u8 = 0;
const ON: u8 = 1;
const OFF: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

fn run(
    function: Function,
    source: Option<&[f64]>,
    output: &mut [f64],
    scalar: &mut dyn FnMut(f64) -> f64,
) -> bool {
    if !available() {
        return false;
    }
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
    {
        let count = output.len();
        let input = match source {
            Some(source) => source[..count].as_ptr(),
            None => output.as_ptr(),
        };
        // SAFETY: `available` confirmed AVX2 and FMA; `input` and `output` hold `count`
        // elements, and each block of lanes is read before it is written, so they may be the
        // same buffer.
        unsafe { avx2::map(function, input, output.as_mut_ptr(), count, scalar) };
        true
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
    {
        // `detect` is false here, so `available` never lets a call get this far.
        unreachable!(
            "{function:?} {} {} {}",
            source.is_some(),
            output.len(),
            scalar(0.0)
        )
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn detect() -> bool {
    if !(std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")) {
        return false;
    }
    let probes = probes();
    let mut output = vec![0.0; probes.len()];
    for (function, reference) in [
        (Function::Exp, f64::exp as fn(f64) -> f64),
        (Function::Log, f64::ln),
    ] {
        // A lane the kernel hands back to the scalar function is marked, so the check sees the
        // kernel's own lanes only.
        let mut scalar = |_: f64| f64::from_bits(MARK);
        // SAFETY: the CPU has AVX2 and FMA, and both buffers hold `probes.len()` elements.
        unsafe {
            avx2::map(
                function,
                probes.as_ptr(),
                output.as_mut_ptr(),
                probes.len(),
                &mut scalar,
            );
        }
        let agrees = probes.iter().zip(&output).all(|(&probe, &value)| {
            value.to_bits() == MARK || value.to_bits() == reference(probe).to_bits()
        });
        if !agrees {
            return false;
        }
    }
    true
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn detect() -> bool {
    false
}

/// A NaN payload no kernel lane produces, marking a lane left to the scalar function.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
const MARK: u64 = 0x7ff8_dead_beef_0001;

/// Inputs that reach every table entry of both routines, `log`'s path near 1, and the edges
/// of each main range.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn probes() -> Vec<f64> {
    let mut probes = Vec::with_capacity(1100);
    // `exp` indexes its table by `round(x * 128 / ln 2) % 128`.
    for step in -600..600 {
        probes.push(f64::from(step) * std::f64::consts::LN_2 / 128.0 * 1.37 + 0.001);
    }
    // `log` indexes its table by the top mantissa bits of `x / 0.6875`'s binade.
    for step in 0..256 {
        probes.push(0.5 + f64::from(step) / 256.0 + 1e-7);
    }
    probes.extend([
        1.0,
        0.9375,
        1.0646,
        0.99,
        1.01,
        2.0f64.powi(-54),
        511.9,
        -511.9,
        700.0,
        1e-300,
        1e300,
        f64::MIN_POSITIVE,
        f64::MAX,
        3.0,
    ]);
    while probes.len() % 4 != 0 {
        probes.push(1.5);
    }
    probes
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
mod avx2 {
    use {
        super::Function,
        std::arch::x86_64::{
            __m256d, __m256i, _CMP_GE_OQ, _CMP_LE_OQ, _CMP_LT_OQ, _mm_loadu_pd, _mm256_add_epi64,
            _mm256_add_pd, _mm256_and_pd, _mm256_and_si256, _mm256_andnot_pd, _mm256_blendv_pd,
            _mm256_castpd_si256, _mm256_castpd128_pd256, _mm256_castsi256_pd, _mm256_cmp_pd,
            _mm256_extract_epi64, _mm256_fmadd_pd, _mm256_fnmadd_pd, _mm256_insertf128_pd,
            _mm256_loadu_pd, _mm256_movemask_pd, _mm256_mul_pd, _mm256_or_si256,
            _mm256_set1_epi64x, _mm256_set1_pd, _mm256_slli_epi64, _mm256_srli_epi64,
            _mm256_storeu_pd, _mm256_sub_epi64, _mm256_sub_pd, _mm256_unpackhi_pd,
            _mm256_unpacklo_pd, _mm256_xor_si256,
        },
    };

    const LANES: usize = 4;
    const ALL: i32 = 0b1111;

    /// Applies `function` to `count` elements from `input` into `output`, which may be the
    /// same buffer.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn map(
        function: Function,
        input: *const f64,
        output: *mut f64,
        count: usize,
        scalar: &mut dyn FnMut(f64) -> f64,
    ) {
        let mut index = 0;
        while index + LANES <= count {
            // SAFETY: `index + LANES <= count`, and the caller guarantees `count` elements.
            let x = unsafe { _mm256_loadu_pd(input.add(index)) };
            let (value, fast) = match function {
                Function::Exp => exp(x),
                Function::Log => log(x),
            };
            if fast == ALL {
                // SAFETY: as for the load.
                unsafe { _mm256_storeu_pd(output.add(index), value) };
            } else {
                let mut lanes = [0.0; LANES];
                let mut arguments = [0.0; LANES];
                // SAFETY: both arrays hold `LANES` elements.
                unsafe {
                    _mm256_storeu_pd(lanes.as_mut_ptr(), value);
                    _mm256_storeu_pd(arguments.as_mut_ptr(), x);
                }
                for lane in 0..LANES {
                    let result = if fast & (1 << lane) != 0 {
                        lanes[lane]
                    } else {
                        scalar(arguments[lane])
                    };
                    // SAFETY: `index + lane < count`.
                    unsafe { output.add(index + lane).write(result) };
                }
            }
            index += LANES;
        }
        while index < count {
            // SAFETY: `index < count`; the element is read before it is written.
            unsafe { output.add(index).write(scalar(input.add(index).read())) };
            index += 1;
        }
    }

    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn constant(bits: u64) -> __m256d {
        _mm256_set1_pd(f64::from_bits(bits))
    }

    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn integer(bits: u64) -> __m256i {
        _mm256_set1_epi64x(bits as i64)
    }

    /// The entries `table[slot]` and `table[slot + 1]` of each lane's even `slot`, as two
    /// vectors. Four 16-byte loads and two shuffles, which outrun two gathers: a gather issues
    /// one load per lane anyway, and the microcode that mitigates Gather Data Sampling makes
    /// it several times slower again.
    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn pairs(table: &[u64; 256], slot: __m256i) -> (__m256d, __m256d) {
        let at = |lane: usize| {
            let index = match lane {
                0 => _mm256_extract_epi64::<0>(slot),
                1 => _mm256_extract_epi64::<1>(slot),
                2 => _mm256_extract_epi64::<2>(slot),
                _ => _mm256_extract_epi64::<3>(slot),
            } as usize
                & 0xfe;
            // SAFETY: `index` is even and at most 254, so both entries lie in the table.
            unsafe { _mm_loadu_pd(table.as_ptr().add(index).cast()) }
        };
        let even = _mm256_insertf128_pd::<1>(_mm256_castpd128_pd256(at(0)), at(2));
        let odd = _mm256_insertf128_pd::<1>(_mm256_castpd128_pd256(at(1)), at(3));
        (_mm256_unpacklo_pd(even, odd), _mm256_unpackhi_pd(even, odd))
    }

    /// `__exp_fma`'s main path on the lanes with `2^-54 <= |x| < 512`, and which lanes those are.
    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn exp(x: __m256d) -> (__m256d, i32) {
        let magnitude = _mm256_andnot_pd(_mm256_set1_pd(-0.0), x);
        let fast = _mm256_and_pd(
            _mm256_cmp_pd::<_CMP_GE_OQ>(magnitude, _mm256_set1_pd(f64::from_bits(0x3c9 << 52))),
            _mm256_cmp_pd::<_CMP_LT_OQ>(magnitude, _mm256_set1_pd(512.0)),
        );
        let shift = constant(EXP_SHIFT);
        // x = k ln2/128 + r, with the integer k in the low bits of `shifted`.
        let shifted = _mm256_fmadd_pd(x, constant(EXP_INV_LN2_N), shift);
        let k = _mm256_castpd_si256(shifted);
        let kd = _mm256_sub_pd(shifted, shift);
        let r = _mm256_fmadd_pd(kd, constant(EXP_NEG_LN2_HI_N), x);
        let r = _mm256_fmadd_pd(kd, constant(EXP_NEG_LN2_LO_N), r);
        // 2^(k/128) = scale * (1 + tail), scale's exponent carrying k / 128.
        let slot = _mm256_slli_epi64::<1>(_mm256_and_si256(k, integer(0x7f)));
        let top = _mm256_slli_epi64::<45>(k);
        let (tail, scale_bits) = pairs(&EXP_TABLE, slot);
        let scale = _mm256_castsi256_pd(_mm256_add_epi64(_mm256_castpd_si256(scale_bits), top));
        let low = _mm256_fmadd_pd(r, constant(EXP_C3), constant(EXP_C2));
        let sum = _mm256_add_pd(r, tail);
        let r2 = _mm256_mul_pd(r, r);
        let high = _mm256_fmadd_pd(r, constant(EXP_C5), constant(EXP_C4));
        let partial = _mm256_fmadd_pd(low, r2, sum);
        let r4 = _mm256_mul_pd(r2, r2);
        let tmp = _mm256_fmadd_pd(r4, high, partial);
        (_mm256_fmadd_pd(scale, tmp, scale), _mm256_movemask_pd(fast))
    }

    /// `__log_fma` on the lanes holding a positive normal finite `x`, and which lanes those are.
    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn log(x: __m256d) -> (__m256d, i32) {
        let normal = _mm256_and_pd(
            _mm256_cmp_pd::<_CMP_GE_OQ>(x, _mm256_set1_pd(f64::MIN_POSITIVE)),
            _mm256_cmp_pd::<_CMP_LE_OQ>(x, _mm256_set1_pd(f64::MAX)),
        );
        let near = _mm256_and_pd(
            _mm256_cmp_pd::<_CMP_GE_OQ>(x, _mm256_set1_pd(f64::from_bits(NEAR_LOW))),
            _mm256_cmp_pd::<_CMP_LT_OQ>(x, _mm256_set1_pd(f64::from_bits(NEAR_HIGH))),
        );
        let near_lanes = _mm256_movemask_pd(near);
        let main = log_main(x);
        let value = if near_lanes == 0 {
            main
        } else {
            _mm256_blendv_pd(main, log_near(x), near)
        };
        (value, _mm256_movemask_pd(normal))
    }

    /// `log` away from 1: x = 2^k z, log(x) = k ln2 + log(c) + log1p(z/c - 1), for the table
    /// entry c nearest z.
    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn log_main(x: __m256d) -> __m256d {
        let bits = _mm256_castpd_si256(x);
        let tmp = _mm256_sub_epi64(bits, integer(LOG_OFFSET));
        let slot = _mm256_slli_epi64::<1>(_mm256_and_si256(
            _mm256_srli_epi64::<45>(tmp),
            integer(0x7f),
        ));
        // k is the sign-extended top 12 bits of `tmp`, made a double through the 2^52 trick.
        let biased = _mm256_xor_si256(_mm256_srli_epi64::<52>(tmp), integer(0x800));
        let kd = _mm256_sub_pd(
            _mm256_castsi256_pd(_mm256_or_si256(biased, integer(0x4330_0000_0000_0000))),
            // 2^52 + 2048.
            constant(0x4330_0000_0000_0800),
        );
        let z = _mm256_castsi256_pd(_mm256_sub_epi64(
            bits,
            _mm256_and_si256(tmp, integer(0xfff0_0000_0000_0000)),
        ));
        let (inverse, logarithm) = pairs(&LOG_TABLE, slot);
        let w = _mm256_fmadd_pd(kd, constant(LOG_LN2_HI), logarithm);
        let r = _mm256_fmadd_pd(z, inverse, _mm256_set1_pd(-1.0));
        let p12 = _mm256_fmadd_pd(r, constant(LOG_A2), constant(LOG_A1));
        let hi = _mm256_add_pd(r, w);
        let r2 = _mm256_mul_pd(r, r);
        let lo = _mm256_add_pd(_mm256_sub_pd(w, hi), r);
        let lo = _mm256_fmadd_pd(kd, constant(LOG_LN2_LO), lo);
        let r3 = _mm256_mul_pd(r, r2);
        let p34 = _mm256_fmadd_pd(r, constant(LOG_A4), constant(LOG_A3));
        let lo = _mm256_fmadd_pd(r2, constant(LOG_A0), lo);
        let q = _mm256_fmadd_pd(p34, r2, p12);
        let y = _mm256_fmadd_pd(r3, q, lo);
        _mm256_add_pd(y, hi)
    }

    /// `log` for `1 - 2^-4 <= x < 1 + 0x1.09p-4`, a polynomial in `r = x - 1` with its
    /// leading terms in extra precision.
    #[inline]
    #[target_feature(enable = "avx2,fma")]
    fn log_near(x: __m256d) -> __m256d {
        let r = _mm256_sub_pd(x, _mm256_set1_pd(1.0));
        let a = _mm256_fmadd_pd(r, constant(LOG_B2), constant(LOG_B1));
        let b = _mm256_fmadd_pd(r, constant(LOG_B5), constant(LOG_B4));
        let c = _mm256_fmadd_pd(r, constant(LOG_B8), constant(LOG_B7));
        let r2 = _mm256_mul_pd(r, r);
        let a = _mm256_fmadd_pd(r2, constant(LOG_B3), a);
        let b = _mm256_fmadd_pd(r2, constant(LOG_B6), b);
        let r3 = _mm256_mul_pd(r, r2);
        let c = _mm256_fmadd_pd(r2, constant(LOG_B9), c);
        let c = _mm256_fmadd_pd(r3, constant(LOG_B10), c);
        let c = _mm256_fmadd_pd(c, r3, b);
        let p = _mm256_fmadd_pd(c, r3, a);
        let split = _mm256_set1_pd(134_217_728.0);
        let rounded = _mm256_fmadd_pd(r, split, r);
        let r_hi = _mm256_fnmadd_pd(split, r, rounded);
        let r_hi2 = _mm256_mul_pd(r_hi, r_hi);
        let r_lo = _mm256_sub_pd(r, r_hi);
        let half = constant(LOG_B0);
        let hi = _mm256_fmadd_pd(r_hi2, half, r);
        let difference = _mm256_sub_pd(r, hi);
        let sum = _mm256_add_pd(r, r_hi);
        let lo = _mm256_fmadd_pd(r_hi2, half, difference);
        let lo = _mm256_fmadd_pd(_mm256_mul_pd(half, r_lo), sum, lo);
        let y = _mm256_fmadd_pd(p, r3, lo);
        _mm256_add_pd(hi, y)
    }

    // The constants of glibc's `__exp_data` and `__log_data`, as bit patterns.

    const EXP_INV_LN2_N: u64 = 0x4067_1547_652b_82fe;
    const EXP_SHIFT: u64 = 0x4338_0000_0000_0000;
    const EXP_NEG_LN2_HI_N: u64 = 0xbf76_2e42_fefa_0000;
    const EXP_NEG_LN2_LO_N: u64 = 0xbd0c_f79a_bc9e_3b3a;
    const EXP_C2: u64 = 0x3fdf_ffff_ffff_fdbd;
    const EXP_C3: u64 = 0x3fc5_5555_5555_543c;
    const EXP_C4: u64 = 0x3fa5_5555_cf17_2b91;
    const EXP_C5: u64 = 0x3f81_1111_67a4_d017;

    /// `1 - 2^-4` and `1 + 0x1.09p-4`, the bounds of `log`'s path near 1.
    const NEAR_LOW: u64 = 0x3fee_0000_0000_0000;
    const NEAR_HIGH: u64 = 0x3ff1_0900_0000_0000;
    /// `0x1.6p-1`: the table's subintervals split `[0x1.6p-1, 0x1.6p0)`.
    const LOG_OFFSET: u64 = 0x3fe6_0000_0000_0000;
    const LOG_LN2_HI: u64 = 0x3fe6_2e42_fefa_3800;
    const LOG_LN2_LO: u64 = 0x3d2e_f357_93c7_6730;
    const LOG_A0: u64 = 0xbfe0_0000_0000_0001;
    const LOG_A1: u64 = 0x3fd5_5555_5551_305b;
    const LOG_A2: u64 = 0xbfcf_ffff_ffeb_4590;
    const LOG_A3: u64 = 0x3fc9_99b3_24f1_0111;
    const LOG_A4: u64 = 0xbfc5_5575_e506_c89f;
    const LOG_B0: u64 = 0xbfe0_0000_0000_0000;
    const LOG_B1: u64 = 0x3fd5_5555_5555_5577;
    const LOG_B2: u64 = 0xbfcf_ffff_ffff_fdcb;
    const LOG_B3: u64 = 0x3fc9_9999_9995_dd0c;
    const LOG_B4: u64 = 0xbfc5_5555_5567_45a7;
    const LOG_B5: u64 = 0x3fc2_4924_a344_de30;
    const LOG_B6: u64 = 0xbfbf_ffff_a442_3d65;
    const LOG_B7: u64 = 0x3fbc_7184_282a_d6ca;
    const LOG_B8: u64 = 0xbfb9_99eb_43b0_68ff;
    const LOG_B9: u64 = 0x3fb7_8182_f7af_d085;
    const LOG_B10: u64 = 0xbfb5_5213_75d1_45cd;

    /// `__exp_data.tab`: for each of the 128 subintervals, the tail and the scale's bits.
    static EXP_TABLE: [u64; 256] = [
        0x0000000000000000,
        0x3ff0000000000000,
        0x3c9b3b4f1a88bf6e,
        0x3feff63da9fb3335,
        0xbc7160139cd8dc5d,
        0x3fefec9a3e778061,
        0xbc905e7a108766d1,
        0x3fefe315e86e7f85,
        0x3c8cd2523567f613,
        0x3fefd9b0d3158574,
        0xbc8bce8023f98efa,
        0x3fefd06b29ddf6de,
        0x3c60f74e61e6c861,
        0x3fefc74518759bc8,
        0x3c90a3e45b33d399,
        0x3fefbe3ecac6f383,
        0x3c979aa65d837b6d,
        0x3fefb5586cf9890f,
        0x3c8eb51a92fdeffc,
        0x3fefac922b7247f7,
        0x3c3ebe3d702f9cd1,
        0x3fefa3ec32d3d1a2,
        0xbc6a033489906e0b,
        0x3fef9b66affed31b,
        0xbc9556522a2fbd0e,
        0x3fef9301d0125b51,
        0xbc5080ef8c4eea55,
        0x3fef8abdc06c31cc,
        0xbc91c923b9d5f416,
        0x3fef829aaea92de0,
        0x3c80d3e3e95c55af,
        0x3fef7a98c8a58e51,
        0xbc801b15eaa59348,
        0x3fef72b83c7d517b,
        0xbc8f1ff055de323d,
        0x3fef6af9388c8dea,
        0x3c8b898c3f1353bf,
        0x3fef635beb6fcb75,
        0xbc96d99c7611eb26,
        0x3fef5be084045cd4,
        0x3c9aecf73e3a2f60,
        0x3fef54873168b9aa,
        0xbc8fe782cb86389d,
        0x3fef4d5022fcd91d,
        0x3c8a6f4144a6c38d,
        0x3fef463b88628cd6,
        0x3c807a05b0e4047d,
        0x3fef3f49917ddc96,
        0x3c968efde3a8a894,
        0x3fef387a6e756238,
        0x3c875e18f274487d,
        0x3fef31ce4fb2a63f,
        0x3c80472b981fe7f2,
        0x3fef2b4565e27cdd,
        0xbc96b87b3f71085e,
        0x3fef24dfe1f56381,
        0x3c82f7e16d09ab31,
        0x3fef1e9df51fdee1,
        0xbc3d219b1a6fbffa,
        0x3fef187fd0dad990,
        0x3c8b3782720c0ab4,
        0x3fef1285a6e4030b,
        0x3c6e149289cecb8f,
        0x3fef0cafa93e2f56,
        0x3c834d754db0abb6,
        0x3fef06fe0a31b715,
        0x3c864201e2ac744c,
        0x3fef0170fc4cd831,
        0x3c8fdd395dd3f84a,
        0x3feefc08b26416ff,
        0xbc86a3803b8e5b04,
        0x3feef6c55f929ff1,
        0xbc924aedcc4b5068,
        0x3feef1a7373aa9cb,
        0xbc9907f81b512d8e,
        0x3feeecae6d05d866,
        0xbc71d1e83e9436d2,
        0x3feee7db34e59ff7,
        0xbc991919b3ce1b15,
        0x3feee32dc313a8e5,
        0x3c859f48a72a4c6d,
        0x3feedea64c123422,
        0xbc9312607a28698a,
        0x3feeda4504ac801c,
        0xbc58a78f4817895b,
        0x3feed60a21f72e2a,
        0xbc7c2c9b67499a1b,
        0x3feed1f5d950a897,
        0x3c4363ed60c2ac11,
        0x3feece086061892d,
        0x3c9666093b0664ef,
        0x3feeca41ed1d0057,
        0x3c6ecce1daa10379,
        0x3feec6a2b5c13cd0,
        0x3c93ff8e3f0f1230,
        0x3feec32af0d7d3de,
        0x3c7690cebb7aafb0,
        0x3feebfdad5362a27,
        0x3c931dbdeb54e077,
        0x3feebcb299fddd0d,
        0xbc8f94340071a38e,
        0x3feeb9b2769d2ca7,
        0xbc87deccdc93a349,
        0x3feeb6daa2cf6642,
        0xbc78dec6bd0f385f,
        0x3feeb42b569d4f82,
        0xbc861246ec7b5cf6,
        0x3feeb1a4ca5d920f,
        0x3c93350518fdd78e,
        0x3feeaf4736b527da,
        0x3c7b98b72f8a9b05,
        0x3feead12d497c7fd,
        0x3c9063e1e21c5409,
        0x3feeab07dd485429,
        0x3c34c7855019c6ea,
        0x3feea9268a5946b7,
        0x3c9432e62b64c035,
        0x3feea76f15ad2148,
        0xbc8ce44a6199769f,
        0x3feea5e1b976dc09,
        0xbc8c33c53bef4da8,
        0x3feea47eb03a5585,
        0xbc845378892be9ae,
        0x3feea34634ccc320,
        0xbc93cedd78565858,
        0x3feea23882552225,
        0x3c5710aa807e1964,
        0x3feea155d44ca973,
        0xbc93b3efbf5e2228,
        0x3feea09e667f3bcd,
        0xbc6a12ad8734b982,
        0x3feea012750bdabf,
        0xbc6367efb86da9ee,
        0x3fee9fb23c651a2f,
        0xbc80dc3d54e08851,
        0x3fee9f7df9519484,
        0xbc781f647e5a3ecf,
        0x3fee9f75e8ec5f74,
        0xbc86ee4ac08b7db0,
        0x3fee9f9a48a58174,
        0xbc8619321e55e68a,
        0x3fee9feb564267c9,
        0x3c909ccb5e09d4d3,
        0x3feea0694fde5d3f,
        0xbc7b32dcb94da51d,
        0x3feea11473eb0187,
        0x3c94ecfd5467c06b,
        0x3feea1ed0130c132,
        0x3c65ebe1abd66c55,
        0x3feea2f336cf4e62,
        0xbc88a1c52fb3cf42,
        0x3feea427543e1a12,
        0xbc9369b6f13b3734,
        0x3feea589994cce13,
        0xbc805e843a19ff1e,
        0x3feea71a4623c7ad,
        0xbc94d450d872576e,
        0x3feea8d99b4492ed,
        0x3c90ad675b0e8a00,
        0x3feeaac7d98a6699,
        0x3c8db72fc1f0eab4,
        0x3feeace5422aa0db,
        0xbc65b6609cc5e7ff,
        0x3feeaf3216b5448c,
        0x3c7bf68359f35f44,
        0x3feeb1ae99157736,
        0xbc93091fa71e3d83,
        0x3feeb45b0b91ffc6,
        0xbc5da9b88b6c1e29,
        0x3feeb737b0cdc5e5,
        0xbc6c23f97c90b959,
        0x3feeba44cbc8520f,
        0xbc92434322f4f9aa,
        0x3feebd829fde4e50,
        0xbc85ca6cd7668e4b,
        0x3feec0f170ca07ba,
        0x3c71affc2b91ce27,
        0x3feec49182a3f090,
        0x3c6dd235e10a73bb,
        0x3feec86319e32323,
        0xbc87c50422622263,
        0x3feecc667b5de565,
        0x3c8b1c86e3e231d5,
        0x3feed09bec4a2d33,
        0xbc91bbd1d3bcbb15,
        0x3feed503b23e255d,
        0x3c90cc319cee31d2,
        0x3feed99e1330b358,
        0x3c8469846e735ab3,
        0x3feede6b5579fdbf,
        0xbc82dfcd978e9db4,
        0x3feee36bbfd3f37a,
        0x3c8c1a7792cb3387,
        0x3feee89f995ad3ad,
        0xbc907b8f4ad1d9fa,
        0x3feeee07298db666,
        0xbc55c3d956dcaeba,
        0x3feef3a2b84f15fb,
        0xbc90a40e3da6f640,
        0x3feef9728de5593a,
        0xbc68d6f438ad9334,
        0x3feeff76f2fb5e47,
        0xbc91eee26b588a35,
        0x3fef05b030a1064a,
        0x3c74ffd70a5fddcd,
        0x3fef0c1e904bc1d2,
        0xbc91bdfbfa9298ac,
        0x3fef12c25bd71e09,
        0x3c736eae30af0cb3,
        0x3fef199bdd85529c,
        0x3c8ee3325c9ffd94,
        0x3fef20ab5fffd07a,
        0x3c84e08fd10959ac,
        0x3fef27f12e57d14b,
        0x3c63cdaf384e1a67,
        0x3fef2f6d9406e7b5,
        0x3c676b2c6c921968,
        0x3fef3720dcef9069,
        0xbc808a1883ccb5d2,
        0x3fef3f0b555dc3fa,
        0xbc8fad5d3ffffa6f,
        0x3fef472d4a07897c,
        0xbc900dae3875a949,
        0x3fef4f87080d89f2,
        0x3c74a385a63d07a7,
        0x3fef5818dcfba487,
        0xbc82919e2040220f,
        0x3fef60e316c98398,
        0x3c8e5a50d5c192ac,
        0x3fef69e603db3285,
        0x3c843a59ac016b4b,
        0x3fef7321f301b460,
        0xbc82d52107b43e1f,
        0x3fef7c97337b9b5f,
        0xbc892ab93b470dc9,
        0x3fef864614f5a129,
        0x3c74b604603a88d3,
        0x3fef902ee78b3ff6,
        0x3c83c5ec519d7271,
        0x3fef9a51fbc74c83,
        0xbc8ff7128fd391f0,
        0x3fefa4afa2a490da,
        0xbc8dae98e223747d,
        0x3fefaf482d8e67f1,
        0x3c8ec3bc41aa2008,
        0x3fefba1bee615a27,
        0x3c842b94c3a9eb32,
        0x3fefc52b376bba97,
        0x3c8a64a931d185ee,
        0x3fefd0765b6e4540,
        0xbc8e37bae43be3ed,
        0x3fefdbfdad9cbe14,
        0x3c77893b4d91cd9d,
        0x3fefe7c1819e90d8,
        0x3c5305c14160cc89,
        0x3feff3c22b8f71f1,
    ];

    /// `__log_data.tab`: for each of the 128 subintervals, `1/c` and `log(c)` as bit patterns.
    static LOG_TABLE: [u64; 256] = [
        0x3ff734f0c3e0de9f,
        0xbfd7cc7f79e69000,
        0x3ff713786a2ce91f,
        0xbfd76feec20d0000,
        0x3ff6f26008fab5a0,
        0xbfd713e31351e000,
        0x3ff6d1a61f138c7d,
        0xbfd6b85b38287800,
        0x3ff6b1490bc5b4d1,
        0xbfd65d5590807800,
        0x3ff69147332f0cba,
        0xbfd602d076180000,
        0x3ff6719f18224223,
        0xbfd5a8ca86909000,
        0x3ff6524f99a51ed9,
        0xbfd54f4356035000,
        0x3ff63356aa8f24c4,
        0xbfd4f637c36b4000,
        0x3ff614b36b9ddc14,
        0xbfd49da7fda85000,
        0x3ff5f66452c65c4c,
        0xbfd445923989a800,
        0x3ff5d867b5912c4f,
        0xbfd3edf439b0b800,
        0x3ff5babccb5b90de,
        0xbfd396ce448f7000,
        0x3ff59d61f2d91a78,
        0xbfd3401e17bda000,
        0x3ff5805612465687,
        0xbfd2e9e2ef468000,
        0x3ff56397cee76bd3,
        0xbfd2941b3830e000,
        0x3ff54725e2a77f93,
        0xbfd23ec58cda8800,
        0x3ff52aff42064583,
        0xbfd1e9e129279000,
        0x3ff50f22dbb2bddf,
        0xbfd1956d2b48f800,
        0x3ff4f38f4734ded7,
        0xbfd141679ab9f800,
        0x3ff4d843cfde2840,
        0xbfd0edd094ef9800,
        0x3ff4bd3ec078a3c8,
        0xbfd09aa518db1000,
        0x3ff4a27fc3e0258a,
        0xbfd047e65263b800,
        0x3ff4880524d48434,
        0xbfcfeb224586f000,
        0x3ff46dce1b192d0b,
        0xbfcf474a7517b000,
        0x3ff453d9d3391854,
        0xbfcea4443d103000,
        0x3ff43a2744b4845a,
        0xbfce020d44e9b000,
        0x3ff420b54115f8fb,
        0xbfcd60a22977f000,
        0x3ff40782da3ef4b1,
        0xbfccc00104959000,
        0x3ff3ee8f5d57fe8f,
        0xbfcc202956891000,
        0x3ff3d5d9a00b4ce9,
        0xbfcb81178d811000,
        0x3ff3bd60c010c12b,
        0xbfcae2c9ccd3d000,
        0x3ff3a5242b75dab8,
        0xbfca45402e129000,
        0x3ff38d22cd9fd002,
        0xbfc9a877681df000,
        0x3ff3755bc5847a1c,
        0xbfc90c6d69483000,
        0x3ff35dce49ad36e2,
        0xbfc87120a645c000,
        0x3ff34679984dd440,
        0xbfc7d68fb4143000,
        0x3ff32f5cceffcb24,
        0xbfc73cb83c627000,
        0x3ff3187775a10d49,
        0xbfc6a39a9b376000,
        0x3ff301c8373e3990,
        0xbfc60b3154b7a000,
        0x3ff2eb4ebb95f841,
        0xbfc5737d76243000,
        0x3ff2d50a0219a9d1,
        0xbfc4dc7b8fc23000,
        0x3ff2bef9a8b7fd2a,
        0xbfc4462c51d20000,
        0x3ff2a91c7a0c1bab,
        0xbfc3b08abc830000,
        0x3ff293726014b530,
        0xbfc31b996b490000,
        0x3ff27dfa5757a1f5,
        0xbfc2875490a44000,
        0x3ff268b39b1d3bbf,
        0xbfc1f3b9f879a000,
        0x3ff2539d838ff5bd,
        0xbfc160c8252ca000,
        0x3ff23eb7aac9083b,
        0xbfc0ce7f57f72000,
        0x3ff22a012ba940b6,
        0xbfc03cdc49fea000,
        0x3ff2157996cc4132,
        0xbfbf57bdbc4b8000,
        0x3ff201201dd2fc9b,
        0xbfbe370896404000,
        0x3ff1ecf4494d480b,
        0xbfbd17983ef94000,
        0x3ff1d8f5528f6569,
        0xbfbbf9674ed8a000,
        0x3ff1c52311577e7c,
        0xbfbadc79202f6000,
        0x3ff1b17c74cb26e9,
        0xbfb9c0c3e7288000,
        0x3ff19e010c2c1ab6,
        0xbfb8a646b372c000,
        0x3ff18ab07bb670bd,
        0xbfb78d01b3ac0000,
        0x3ff1778a25efbcb6,
        0xbfb674f145380000,
        0x3ff1648d354c31da,
        0xbfb55e0e6d878000,
        0x3ff151b990275fdd,
        0xbfb4485cdea1e000,
        0x3ff13f0ea432d24c,
        0xbfb333d94d6aa000,
        0x3ff12c8b7210f9da,
        0xbfb22079f8c56000,
        0x3ff11a3028ecb531,
        0xbfb10e4698622000,
        0x3ff107fbda8434af,
        0xbfaffa6c6ad20000,
        0x3ff0f5ee0f4e6bb3,
        0xbfadda8d4a774000,
        0x3ff0e4065d2a9fce,
        0xbfabbcece4850000,
        0x3ff0d244632ca521,
        0xbfa9a1894012c000,
        0x3ff0c0a77ce2981a,
        0xbfa788583302c000,
        0x3ff0af2f83c636d1,
        0xbfa5715e67d68000,
        0x3ff09ddb98a01339,
        0xbfa35c8a49658000,
        0x3ff08cabaf52e7df,
        0xbfa149e364154000,
        0x3ff07b9f2f4e28fb,
        0xbf9e72c082eb8000,
        0x3ff06ab58c358f19,
        0xbf9a55f152528000,
        0x3ff059eea5ecf92c,
        0xbf963d62cf818000,
        0x3ff04949cdd12c90,
        0xbf9228fb8caa0000,
        0x3ff038c6c6f0ada9,
        0xbf8c317b20f90000,
        0x3ff02865137932a9,
        0xbf8419355daa0000,
        0x3ff0182427ea7348,
        0xbf781203c2ec0000,
        0x3ff008040614b195,
        0xbf60040979240000,
        0x3fefe01ff726fa1a,
        0x3f6feff384900000,
        0x3fefa11cc261ea74,
        0x3f87dc41353d0000,
        0x3fef6310b081992e,
        0x3f93cea3c4c28000,
        0x3fef25f63ceeadcd,
        0x3f9b9fc114890000,
        0x3feee9c8039113e7,
        0x3fa1b0d8ce110000,
        0x3feeae8078cbb1ab,
        0x3fa58a5bd001c000,
        0x3fee741aa29d0c9b,
        0x3fa95c8340d88000,
        0x3fee3a91830a99b5,
        0x3fad276aef578000,
        0x3fee01e009609a56,
        0x3fb07598e598c000,
        0x3fedca01e577bb98,
        0x3fb253f5e30d2000,
        0x3fed92f20b7c9103,
        0x3fb42edd8b380000,
        0x3fed5cac66fb5cce,
        0x3fb606598757c000,
        0x3fed272caa5ede9d,
        0x3fb7da76356a0000,
        0x3fecf26e3e6b2ccd,
        0x3fb9ab434e1c6000,
        0x3fecbe6da2a77902,
        0x3fbb78c7bb0d6000,
        0x3fec8b266d37086d,
        0x3fbd431332e72000,
        0x3fec5894bd5d5804,
        0x3fbf0a3171de6000,
        0x3fec26b533bb9f8c,
        0x3fc067152b914000,
        0x3febf583eeece73f,
        0x3fc147858292b000,
        0x3febc4fd75db96c1,
        0x3fc2266ecdca3000,
        0x3feb951e0c864a28,
        0x3fc303d7a6c55000,
        0x3feb65e2c5ef3e2c,
        0x3fc3dfc33c331000,
        0x3feb374867c9888b,
        0x3fc4ba366b7a8000,
        0x3feb094b211d304a,
        0x3fc5933928d1f000,
        0x3feadbe885f2ef7e,
        0x3fc66acd2418f000,
        0x3feaaf1d31603da2,
        0x3fc740f8ec669000,
        0x3fea82e63fd358a7,
        0x3fc815c0f51af000,
        0x3fea5740ef09738b,
        0x3fc8e92954f68000,
        0x3fea2c2a90ab4b27,
        0x3fc9bb3602f84000,
        0x3fea01a01393f2d1,
        0x3fca8bed1c2c0000,
        0x3fe9d79f24db3c1b,
        0x3fcb5b515c01d000,
        0x3fe9ae2505c7b190,
        0x3fcc2967ccbcc000,
        0x3fe9852ef297ce2f,
        0x3fccf635d5486000,
        0x3fe95cbaeea44b75,
        0x3fcdc1bd3446c000,
        0x3fe934c69de74838,
        0x3fce8c01b8cfe000,
        0x3fe90d4f2f6752e6,
        0x3fcf5509c0179000,
        0x3fe8e6528effd79d,
        0x3fd00e6c121fb800,
        0x3fe8bfce9fcc007c,
        0x3fd071b80e93d000,
        0x3fe899c0dabec30e,
        0x3fd0d46b9e867000,
        0x3fe87427aa2317fb,
        0x3fd13687334bd000,
        0x3fe84f00acb39a08,
        0x3fd1980d67234800,
        0x3fe82a49e8653e55,
        0x3fd1f8ffe0cc8000,
        0x3fe8060195f40260,
        0x3fd2595fd7636800,
        0x3fe7e22563e0a329,
        0x3fd2b9300914a800,
        0x3fe7beb377dcb5ad,
        0x3fd3187210436000,
        0x3fe79baa679725c2,
        0x3fd377266dec1800,
        0x3fe77907f2170657,
        0x3fd3d54ffbaf3000,
        0x3fe756cadbd6130c,
        0x3fd432eee32fe000,
    ];
}

#[cfg(test)]
mod tests {
    use super::{available, exp, log};

    /// A seeded stream of doubles spread over every exponent, so that each lane class and
    /// table entry is hit many times.
    fn inputs(seed: u64, count: usize) -> Vec<f64> {
        let mut state = seed;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let bits = state ^ (state >> 29);
                match bits % 4 {
                    0 => f64::from_bits(bits),
                    1 => (bits >> 11) as f64 / (1u64 << 53) as f64 * 1400.0 - 700.0,
                    2 => 0.9 + (bits >> 11) as f64 / (1u64 << 53) as f64 * 0.2,
                    _ => (bits >> 11) as f64 / (1u64 << 53) as f64 * 40.0 - 20.0,
                }
            })
            .collect()
    }

    #[test]
    fn kernels_agree_with_glibc_bit_for_bit() {
        // The kernels are meant to run on every x86-64 glibc machine with AVX2 and FMA, so
        // detection turning them off there is a failure, not a skip.
        #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            assert!(
                available(),
                "the kernels disagree with this glibc on the probes"
            );
        }
        if !available() {
            return;
        }
        let values = inputs(0x5eed, 1 << 20);
        for (kernel, reference) in [
            (
                exp as fn(Option<&[f64]>, &mut [f64], &mut dyn FnMut(f64) -> f64) -> bool,
                f64::exp as fn(f64) -> f64,
            ),
            (log, f64::ln),
        ] {
            let mut output = vec![0.0; values.len()];
            assert!(kernel(Some(&values), &mut output, &mut |value| reference(
                value
            )));
            for (&value, &result) in values.iter().zip(&output) {
                assert_eq!(
                    result.to_bits(),
                    reference(value).to_bits(),
                    "input {value:e} ({:#x})",
                    value.to_bits()
                );
            }
            // In place, with a length that leaves a scalar tail.
            let mut buffer = values[..1027].to_vec();
            assert!(kernel(None, &mut buffer, &mut |value| reference(value)));
            for (&value, &result) in values.iter().zip(&buffer) {
                assert_eq!(result.to_bits(), reference(value).to_bits());
            }
        }
    }
}
