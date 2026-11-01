//! A software model of the x87 80-bit `long double` that GNU R accumulates sums and means in.
//!
//! GNU R's `sum`, `mean`, `rowSums`, `cov` and friends add doubles into a `long double`, whose
//! 64-bit significand rounds differently from both plain doubles and compensated summation.
//! Matching it bit for bit is what makes `mean(c(1:9, -1e300, 1e300))` exactly 0, as in GNU R,
//! rather than the more accurate 45/11 that an error-tracking sum gives.

use std::ops::{Add, AddAssign, Div, Mul, Sub};

/// A value with a 64-bit significand and an unbounded exponent, rounded to nearest even after
/// every operation the way x87 extended precision is. Infinities and NaN keep their double.
#[derive(Clone, Copy, Debug)]
pub struct LongDouble(Repr);

#[derive(Clone, Copy, Debug)]
enum Repr {
    /// `significand * 2^exponent`, with the top bit of a nonzero significand set.
    Finite {
        negative: bool,
        significand: u64,
        exponent: i32,
    },
    Special(f64),
}

/// Guard bits kept below an aligned significand, enough for a correctly rounded sum.
const GUARD: u32 = 62;

impl LongDouble {
    pub const ZERO: LongDouble = LongDouble(Repr::Finite {
        negative: false,
        significand: 0,
        exponent: 0,
    });

    pub fn to_f64(self) -> f64 {
        match self.0 {
            Repr::Special(value) => value,
            Repr::Finite {
                negative,
                significand,
                exponent,
            } => {
                let magnitude = if significand == 0 {
                    0.0
                } else {
                    significand_to_f64(significand, exponent)
                };
                if negative { -magnitude } else { magnitude }
            }
        }
    }

    pub fn is_finite(self) -> bool {
        self.to_f64().is_finite()
    }

    /// The correctly rounded square root, as `sqrtl` gives it.
    pub fn sqrt(self) -> LongDouble {
        let Repr::Finite {
            negative,
            significand,
            exponent,
        } = self.0
        else {
            return LongDouble(Repr::Special(self.to_f64().sqrt()));
        };
        if significand == 0 {
            return self;
        }
        if negative {
            return LongDouble(Repr::Special(f64::NAN));
        }
        // Widen to 127 or 128 bits with an even exponent left over, so the integer root has
        // exactly 64 bits.
        let shift = if (exponent - 63) % 2 == 0 { 63 } else { 64 };
        let radicand = (significand as u128) << shift;
        let mut root = (radicand as f64).sqrt() as u128 + (1 << 13);
        loop {
            let next = (root + radicand / root) / 2;
            if next >= root {
                break;
            }
            root = next;
        }
        // An integer radicand is never exactly halfway between two roots, so rounding to
        // nearest only asks whether it passes `root^2 + root`.
        let rounded = if radicand - root * root > root {
            root + 1
        } else {
            root
        };
        round_wide(false, rounded, (exponent - shift) / 2)
    }

    /// The largest integer not above the value, as `floorl` gives it.
    pub fn floor(self) -> LongDouble {
        let Repr::Finite {
            negative,
            significand,
            exponent,
        } = self.0
        else {
            return self;
        };
        if significand == 0 || exponent >= 0 {
            return self;
        }
        let fraction_bits = exponent.unsigned_abs();
        let (whole, has_fraction) = if fraction_bits >= 64 {
            (0, true)
        } else {
            (
                significand >> fraction_bits,
                significand & ((1u64 << fraction_bits) - 1) != 0,
            )
        };
        let whole = if negative && has_fraction {
            whole as u128 + 1
        } else {
            whole as u128
        };
        if whole == 0 {
            return LongDouble(Repr::Finite {
                negative,
                significand: 0,
                exponent: 0,
            });
        }
        round_wide(negative, whole, 0)
    }

    fn negate(self) -> LongDouble {
        match self.0 {
            Repr::Special(value) => LongDouble(Repr::Special(-value)),
            Repr::Finite {
                negative,
                significand,
                exponent,
            } => LongDouble(Repr::Finite {
                negative: !negative,
                significand,
                exponent,
            }),
        }
    }
}

impl From<f64> for LongDouble {
    fn from(value: f64) -> LongDouble {
        if !value.is_finite() {
            return LongDouble(Repr::Special(value));
        }
        let negative = value.is_sign_negative();
        if value == 0.0 {
            return LongDouble(Repr::Finite {
                negative,
                significand: 0,
                exponent: 0,
            });
        }
        let bits = value.to_bits();
        let biased = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1 << 52) - 1);
        let (raw, exponent) = if biased == 0 {
            (fraction, -1074)
        } else {
            (fraction | (1 << 52), biased - 1075)
        };
        let shift = raw.leading_zeros();
        LongDouble(Repr::Finite {
            negative,
            significand: raw << shift,
            exponent: exponent - shift as i32,
        })
    }
}

impl Add for LongDouble {
    type Output = LongDouble;

    fn add(self, other: LongDouble) -> LongDouble {
        let (
            Repr::Finite {
                negative: left_negative,
                significand: left,
                exponent: left_exponent,
            },
            Repr::Finite {
                negative: right_negative,
                significand: right,
                exponent: right_exponent,
            },
        ) = (self.0, other.0)
        else {
            return LongDouble(Repr::Special(self.to_f64() + other.to_f64()));
        };
        if right == 0 {
            if left == 0 {
                // Only `-0 + -0` is negative zero under round to nearest.
                return LongDouble(Repr::Finite {
                    negative: left_negative && right_negative,
                    significand: 0,
                    exponent: 0,
                });
            }
            return self;
        }
        if left == 0 {
            return other;
        }
        let ((big_negative, big, big_exponent), (small_negative, small, small_exponent)) =
            if (left_exponent, left) >= (right_exponent, right) {
                (
                    (left_negative, left, left_exponent),
                    (right_negative, right, right_exponent),
                )
            } else {
                (
                    (right_negative, right, right_exponent),
                    (left_negative, left, left_exponent),
                )
            };
        let big_wide = (big as u128) << GUARD;
        let small_wide = (small as u128) << GUARD;
        let distance = (big_exponent - small_exponent) as u32;
        // Bits shifted out of the smaller operand survive as a sticky low bit, which is all the
        // rounding step needs to know about them.
        let small_aligned = if distance >= 127 {
            1
        } else {
            let shifted = small_wide >> distance;
            let lost = small_wide & ((1u128 << distance) - 1);
            shifted | u128::from(lost != 0)
        };
        let total = if big_negative == small_negative {
            big_wide + small_aligned
        } else {
            big_wide - small_aligned
        };
        if total == 0 {
            return LongDouble::ZERO;
        }
        round_wide(big_negative, total, big_exponent - GUARD as i32)
    }
}

impl Sub for LongDouble {
    type Output = LongDouble;

    fn sub(self, other: LongDouble) -> LongDouble {
        self + other.negate()
    }
}

impl Mul for LongDouble {
    type Output = LongDouble;

    fn mul(self, other: LongDouble) -> LongDouble {
        let (
            Repr::Finite {
                negative: left_negative,
                significand: left,
                exponent: left_exponent,
            },
            Repr::Finite {
                negative: right_negative,
                significand: right,
                exponent: right_exponent,
            },
        ) = (self.0, other.0)
        else {
            return LongDouble(Repr::Special(self.to_f64() * other.to_f64()));
        };
        let negative = left_negative != right_negative;
        if left == 0 || right == 0 {
            return LongDouble(Repr::Finite {
                negative,
                significand: 0,
                exponent: 0,
            });
        }
        round_wide(
            negative,
            left as u128 * right as u128,
            left_exponent + right_exponent,
        )
    }
}

impl AddAssign<f64> for LongDouble {
    fn add_assign(&mut self, value: f64) {
        *self = *self + LongDouble::from(value);
    }
}

impl Div for LongDouble {
    type Output = LongDouble;

    fn div(self, other: LongDouble) -> LongDouble {
        let (
            Repr::Finite {
                negative: left_negative,
                significand: left,
                exponent: left_exponent,
            },
            Repr::Finite {
                negative: right_negative,
                significand: right,
                exponent: right_exponent,
            },
        ) = (self.0, other.0)
        else {
            return LongDouble(Repr::Special(self.to_f64() / other.to_f64()));
        };
        if right == 0 {
            return LongDouble(Repr::Special(self.to_f64() / other.to_f64()));
        }
        let negative = left_negative != right_negative;
        if left == 0 {
            return LongDouble(Repr::Finite {
                negative,
                significand: 0,
                exponent: 0,
            });
        }
        // Both significands have their top bit set, so the first quotient has at least 64
        // bits; two more bits and a sticky bit make the rounding exact.
        let numerator = (left as u128) << 64;
        let divisor = right as u128;
        let (quotient, remainder) = (numerator / divisor, numerator % divisor);
        let extra = (remainder << 2) / divisor;
        let sticky = u128::from((remainder << 2) % divisor != 0);
        round_wide(
            negative,
            (quotient << 2) | extra | sticky,
            left_exponent - right_exponent - 66,
        )
    }
}

/// Division by a count, which is how every GNU R mean finishes.
impl Div<usize> for LongDouble {
    type Output = LongDouble;

    fn div(self, count: usize) -> LongDouble {
        let Repr::Finite {
            negative,
            significand,
            exponent,
        } = self.0
        else {
            return LongDouble(Repr::Special(self.to_f64() / count as f64));
        };
        if count == 0 {
            return LongDouble(Repr::Special(self.to_f64() / 0.0));
        }
        if significand == 0 {
            return self;
        }
        let numerator = (significand as u128) << 64;
        let divisor = count as u128;
        let quotient = numerator / divisor;
        let sticky = u128::from(numerator % divisor != 0);
        round_wide(negative, quotient | sticky, exponent - 64)
    }
}

/// The long double sum of `values` from zero, or `None` when any value is infinite or NaN,
/// whose handling (the first NaN wins, or NaN is skipped) depends on the caller.
///
/// A sum of doubles never overflows the extended exponent range, so the total is finite exactly
/// when every term is, which lets the kernel decide after the loop instead of testing each term.
pub fn finite_sum(values: &[f64]) -> Option<LongDouble> {
    #[cfg(target_arch = "x86_64")]
    let total = x87::sum(values);
    #[cfg(not(target_arch = "x86_64"))]
    let total = values.iter().fold(LongDouble::ZERO, |total, &value| {
        total + LongDouble::from(value)
    });
    total.is_finite_exact().then_some(total)
}

/// The long double sum of `value - center` over finite `values` and a finite `center`, the
/// refinement step of GNU R's mean, or `None` when a term is not finite.
pub fn finite_deviation_sum(values: &[f64], center: LongDouble) -> Option<LongDouble> {
    #[cfg(target_arch = "x86_64")]
    let total = x87::deviation_sum(values, center.to_extended()?);
    #[cfg(not(target_arch = "x86_64"))]
    let total = values.iter().fold(LongDouble::ZERO, |total, &value| {
        total + (LongDouble::from(value) - center)
    });
    total.is_finite_exact().then_some(total)
}

/// The long double sum of `(x - x_center) * (y - y_center)`, the numerator of GNU R's
/// covariance, or `None` when a term is not finite. `x` and `y` have the same length.
pub fn finite_cross_deviation_sum(
    x: &[f64],
    y: &[f64],
    x_center: LongDouble,
    y_center: LongDouble,
) -> Option<LongDouble> {
    if x.len() != y.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    let total = x87::cross_deviation_sum(x, y, x_center.to_extended()?, y_center.to_extended()?);
    #[cfg(not(target_arch = "x86_64"))]
    let total = x.iter().zip(y).fold(LongDouble::ZERO, |total, (&x, &y)| {
        total + (LongDouble::from(x) - x_center) * (LongDouble::from(y) - y_center)
    });
    total.is_finite_exact().then_some(total)
}

/// GNU R's `rowSums` (`divisor` 1) or `rowMeans` (`divisor` the column count) of a column-major
/// matrix with `rows` rows: each row's long double total, added column by column, divided by
/// `divisor` in long double and rounded once to double into `out`. Returns which rows were
/// written; a row holding an infinite or NaN value is left for the caller, whose `na.rm`
/// decides it.
pub fn finite_row_means(values: &[f64], rows: usize, divisor: usize, out: &mut [f64]) -> Vec<bool> {
    let rows = rows.min(out.len());
    if rows == 0 {
        return Vec::new();
    }
    #[cfg(target_arch = "x86_64")]
    {
        let columns = values.len() / rows;
        let blocked = rows - rows % x87::BLOCK;
        let mut totals = vec![[0u8; 16]; rows];
        for (block, totals) in totals[..blocked].chunks_exact_mut(x87::BLOCK).enumerate() {
            x87::block_row_sums(values, rows, columns, block * x87::BLOCK, totals);
        }
        for column in values.chunks_exact(rows) {
            x87::add_to_each(&mut totals[blocked..], &column[blocked..]);
        }
        x87::divide_each(&totals, divisor as u64, &mut out[..rows]);
        totals
            .iter()
            .map(|total| u16::from_le_bytes([total[8], total[9]]) & 0x7fff != 0x7fff)
            .collect()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut totals = vec![LongDouble::ZERO; rows];
        for column in values.chunks_exact(rows) {
            for (total, &value) in totals.iter_mut().zip(column) {
                *total += value;
            }
        }
        totals
            .into_iter()
            .zip(out.iter_mut())
            .map(|(total, out)| {
                *out = if divisor == 1 {
                    total.to_f64()
                } else {
                    (total / divisor).to_f64()
                };
                total.is_finite_exact()
            })
            .collect()
    }
}

impl LongDouble {
    /// Whether the value is finite before any rounding to double: a finite total too large for
    /// a double still counts, as it does in x87 registers.
    fn is_finite_exact(self) -> bool {
        matches!(self.0, Repr::Finite { .. })
    }

    /// From an integer, exactly when it fits 64 significand bits, as an x87 `fild` loads it.
    pub fn from_i64(value: i64) -> LongDouble {
        let magnitude = value.unsigned_abs();
        if magnitude == 0 {
            return LongDouble::ZERO;
        }
        let shift = magnitude.leading_zeros();
        LongDouble(Repr::Finite {
            negative: value < 0,
            significand: magnitude << shift,
            exponent: -(shift as i32),
        })
    }

    /// The x87 extended encoding: the 64-bit significand with its explicit integer bit, then
    /// the sign and a 15-bit exponent biased by 16383, padded to 16 bytes. `None` for a value
    /// outside the normal extended range, or not finite.
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    fn to_extended(self) -> Option<[u8; 16]> {
        let Repr::Finite {
            negative,
            significand,
            exponent,
        } = self.0
        else {
            return None;
        };
        let mut bytes = [0u8; 16];
        let sign = u16::from(negative) << 15;
        if significand == 0 {
            bytes[8..10].copy_from_slice(&sign.to_le_bytes());
            return Some(bytes);
        }
        let biased = exponent + EXTENDED_OFFSET;
        if !(1..0x7fff).contains(&biased) {
            return None;
        }
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..10].copy_from_slice(&(sign | biased as u16).to_le_bytes());
        Some(bytes)
    }

    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    fn from_extended(bytes: &[u8; 16]) -> LongDouble {
        let mut significand_bytes = [0u8; 8];
        significand_bytes.copy_from_slice(&bytes[..8]);
        let significand = u64::from_le_bytes(significand_bytes);
        let top = u16::from_le_bytes([bytes[8], bytes[9]]);
        let negative = top >> 15 == 1;
        let biased = i32::from(top & 0x7fff);
        if biased == 0x7fff {
            // Only reached for a non-finite total, which every caller replaces.
            let value = if significand << 1 == 0 {
                f64::INFINITY
            } else {
                f64::NAN
            };
            return LongDouble(Repr::Special(if negative { -value } else { value }));
        }
        if significand == 0 {
            return LongDouble(Repr::Finite {
                negative,
                significand: 0,
                exponent: 0,
            });
        }
        // A denormal extended value has a zero exponent field but the exponent of field 1.
        let shift = significand.leading_zeros();
        LongDouble(Repr::Finite {
            negative,
            significand: significand << shift,
            exponent: biased.max(1) - EXTENDED_OFFSET - shift as i32,
        })
    }
}

/// `biased exponent - EXTENDED_OFFSET` is the power of two that scales the 64-bit integer
/// significand: 16383 of bias plus 63 fraction bits.
const EXTENDED_OFFSET: i32 = 16383 + 63;

/// Summation loops on the x87 unit itself, which is what GNU R's `long double` compiles to on
/// x86-64 and runs several times faster than the software model. Each loop sets the control word
/// to 64-bit significands and round to nearest, the Linux default, and restores the caller's.
/// Every loop leaves the x87 register stack empty, as the ABI requires.
#[cfg(target_arch = "x86_64")]
mod x87 {
    use {super::LongDouble, std::arch::asm};

    /// Extended precision, round to nearest, every exception masked.
    static CONTROL: u16 = 0x037f;

    pub(super) fn sum(values: &[f64]) -> LongDouble {
        let mut total = [0u8; 16];
        let mut saved: u16 = 0;
        // SAFETY: the loop reads `values.len()` doubles from `values` and writes ten bytes of
        // `total`; the clobbered x87 registers are declared.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "fldz",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fadd qword ptr [{pointer}]",
                "add {pointer}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{total}]",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                pointer = inout(reg) values.as_ptr() => _,
                count = inout(reg) values.len() => _,
                total = in(reg) total.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        LongDouble::from_extended(&total)
    }

    pub(super) fn deviation_sum(values: &[f64], center: [u8; 16]) -> LongDouble {
        let mut total = [0u8; 16];
        let mut saved: u16 = 0;
        // SAFETY: as in `sum`; the stack holds the center below the running total.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "fld tbyte ptr [{center}]",
                "fldz",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fld qword ptr [{pointer}]",
                "fsub st(0), st(2)",
                "faddp st(1), st(0)",
                "add {pointer}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{total}]",
                "fstp st(0)",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                center = in(reg) center.as_ptr(),
                pointer = inout(reg) values.as_ptr() => _,
                count = inout(reg) values.len() => _,
                total = in(reg) total.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        LongDouble::from_extended(&total)
    }

    /// `x` and `y` have the same length, which the caller checks.
    pub(super) fn cross_deviation_sum(
        x: &[f64],
        y: &[f64],
        x_center: [u8; 16],
        y_center: [u8; 16],
    ) -> LongDouble {
        let mut total = [0u8; 16];
        let mut saved: u16 = 0;
        let count = x.len().min(y.len());
        // SAFETY: as in `sum`, reading `count` doubles from each of `x` and `y`; the stack holds
        // the running total over the x center over the y center.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "fld tbyte ptr [{y_center}]",
                "fld tbyte ptr [{x_center}]",
                "fldz",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fld qword ptr [{x}]",
                "fsub st(0), st(2)",
                "fld qword ptr [{y}]",
                "fsub st(0), st(4)",
                "fmulp st(1), st(0)",
                "faddp st(1), st(0)",
                "add {x}, 8",
                "add {y}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{total}]",
                "fstp st(0)",
                "fstp st(0)",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                x_center = in(reg) x_center.as_ptr(),
                y_center = in(reg) y_center.as_ptr(),
                x = inout(reg) x.as_ptr() => _,
                y = inout(reg) y.as_ptr() => _,
                count = inout(reg) count => _,
                total = in(reg) total.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        LongDouble::from_extended(&total)
    }

    /// The rows `block_row_sums` keeps in registers at once: seven totals and one loaded term
    /// fill the eight-register stack.
    pub(super) const BLOCK: usize = 7;

    /// The totals of rows `first..first + BLOCK` of a column-major `rows` by `columns` matrix,
    /// each added column by column, written to `out`. The seven independent totals keep the
    /// adder busy where a single running total would wait on each addition.
    pub(super) fn block_row_sums(
        values: &[f64],
        rows: usize,
        columns: usize,
        first: usize,
        out: &mut [[u8; 16]],
    ) {
        assert!(out.len() == BLOCK && first + BLOCK <= rows && rows * columns <= values.len());
        let mut saved: u16 = 0;
        // SAFETY: the asserts keep every read `first + row + column * rows` inside `values` and
        // the seven ten-byte stores inside `out`.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "fldz", "fldz", "fldz", "fldz", "fldz", "fldz", "fldz",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fld qword ptr [{pointer} + 48]",
                "faddp st(7), st(0)",
                "fld qword ptr [{pointer} + 40]",
                "faddp st(6), st(0)",
                "fld qword ptr [{pointer} + 32]",
                "faddp st(5), st(0)",
                "fld qword ptr [{pointer} + 24]",
                "faddp st(4), st(0)",
                "fld qword ptr [{pointer} + 16]",
                "faddp st(3), st(0)",
                "fld qword ptr [{pointer} + 8]",
                "faddp st(2), st(0)",
                "fld qword ptr [{pointer}]",
                "faddp st(1), st(0)",
                "add {pointer}, {stride}",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{out}]",
                "fstp tbyte ptr [{out} + 16]",
                "fstp tbyte ptr [{out} + 32]",
                "fstp tbyte ptr [{out} + 48]",
                "fstp tbyte ptr [{out} + 64]",
                "fstp tbyte ptr [{out} + 80]",
                "fstp tbyte ptr [{out} + 96]",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                pointer = inout(reg) values.as_ptr().add(first) => _,
                stride = in(reg) rows * 8,
                count = inout(reg) columns => _,
                out = in(reg) out.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    /// `out[i] = totals[i] / divisor`, divided in extended precision and rounded once to
    /// double, for the shorter of the two. A divisor of 1 only rounds.
    pub(super) fn divide_each(totals: &[[u8; 16]], divisor: u64, out: &mut [f64]) {
        let mut saved: u16 = 0;
        let count = totals.len().min(out.len());
        let divisor = divisor.min(i64::MAX as u64);
        // SAFETY: reads `count` totals and writes `count` doubles.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "fild qword ptr [{divisor}]",
                "test {count}, {count}",
                "jz 3f",
                "cmp qword ptr [{divisor}], 1",
                "je 4f",
                "2:",
                "fld tbyte ptr [{totals}]",
                "fdiv st(0), st(1)",
                "fstp qword ptr [{out}]",
                "add {totals}, 16",
                "add {out}, 8",
                "dec {count}",
                "jnz 2b",
                "jmp 3f",
                "4:",
                "fld tbyte ptr [{totals}]",
                "fstp qword ptr [{out}]",
                "add {totals}, 16",
                "add {out}, 8",
                "dec {count}",
                "jnz 4b",
                "3:",
                "fstp st(0)",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                divisor = in(reg) &divisor,
                totals = inout(reg) totals.as_ptr() => _,
                out = inout(reg) out.as_mut_ptr() => _,
                count = inout(reg) count => _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    /// Adds `values[i]` to the extended total in `totals[i]`, for the shorter of the two.
    pub(super) fn add_to_each(totals: &mut [[u8; 16]], values: &[f64]) {
        let mut saved: u16 = 0;
        let count = totals.len().min(values.len());
        // SAFETY: reads `count` doubles and rewrites the first ten bytes of `count` totals.
        unsafe {
            asm!(
                "fnstcw word ptr [{saved}]",
                "fldcw word ptr [{control}]",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fld tbyte ptr [{totals}]",
                "fadd qword ptr [{pointer}]",
                "fstp tbyte ptr [{totals}]",
                "add {totals}, 16",
                "add {pointer}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fldcw word ptr [{saved}]",
                saved = in(reg) &mut saved,
                control = in(reg) &CONTROL,
                totals = inout(reg) totals.as_mut_ptr() => _,
                pointer = inout(reg) values.as_ptr() => _,
                count = inout(reg) count => _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }
}

/// Rounds `wide * 2^exponent` to a 64-bit significand, ties to even.
fn round_wide(negative: bool, wide: u128, exponent: i32) -> LongDouble {
    let width = 128 - wide.leading_zeros();
    let (significand, exponent) = if width <= 64 {
        let shift = 64 - width;
        ((wide << shift) as u64, exponent - shift as i32)
    } else {
        let shift = width - 64;
        let kept = wide >> shift;
        let rest = wide & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        let rounded = if rest > half || (rest == half && kept & 1 == 1) {
            kept + 1
        } else {
            kept
        };
        if rounded >> 64 != 0 {
            ((rounded >> 1) as u64, exponent + shift as i32 + 1)
        } else {
            (rounded as u64, exponent + shift as i32)
        }
    };
    LongDouble(Repr::Finite {
        negative,
        significand,
        exponent,
    })
}

/// The double nearest `significand * 2^exponent`, ties to even, with gradual underflow.
fn significand_to_f64(significand: u64, exponent: i32) -> f64 {
    // The value lies in [2^top, 2^(top + 1)).
    let top = exponent + 63;
    if top > 1023 {
        return f64::INFINITY;
    }
    // A normal double keeps 53 bits; a subnormal one keeps fewer, down to its 2^-1074 unit.
    let dropped = if top >= -1022 {
        11
    } else {
        11 + (-1022 - top) as u32
    };
    if dropped > 65 {
        return 0.0;
    }
    let wide = significand as u128;
    let kept = wide >> dropped;
    let rest = wide & ((1u128 << dropped) - 1);
    let half = 1u128 << (dropped - 1);
    let rounded = if rest > half || (rest == half && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    };
    scale(rounded as f64, exponent + dropped as i32)
}

/// `value * 2^power`, exact whenever the result is representable.
fn scale(mut value: f64, mut power: i32) -> f64 {
    let step = 2f64.powi(600);
    let inverse = 2f64.powi(-600);
    while power > 600 {
        value *= step;
        power -= 600;
    }
    while power < -600 {
        value *= inverse;
        power += 600;
    }
    value * 2f64.powi(power)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(values: &[f64]) -> f64 {
        let mut total = LongDouble::ZERO;
        for &value in values {
            total += value;
        }
        total.to_f64()
    }

    #[test]
    fn huge_terms_swallow_small_ones_like_x87() {
        let values: Vec<f64> = (1..=9).map(f64::from).chain([-1e300, 1e300]).collect();
        assert_eq!(sum(&values), 0.0);
    }

    #[test]
    fn extra_bits_survive_until_the_final_rounding() {
        // 2^53 + 1 is not a double, but the 64-bit significand holds it until 1 more arrives.
        let two_53 = 2f64.powi(53);
        assert_eq!(sum(&[two_53, 1.0, 1.0]), two_53 + 2.0);
        assert_eq!(two_53 + 1.0 + 1.0, two_53);
    }

    #[test]
    fn round_trips_doubles_and_subnormals() {
        for value in [1.0, -0.1, 1e-310, f64::MIN_POSITIVE, f64::MAX, 5e-324] {
            assert_eq!(LongDouble::from(value).to_f64(), value);
        }
    }

    #[test]
    fn division_rounds_once_to_the_double() {
        let third = LongDouble::from(1.0) / 3;
        assert_eq!(third.to_f64(), 1.0 / 3.0);
        let mean = (LongDouble::from(0.1) + LongDouble::from(0.2) + LongDouble::from(0.3)) / 3;
        assert_eq!(mean.to_f64(), 0.2);
    }

    #[test]
    fn products_round_to_the_extended_significand() {
        let third = LongDouble::from(1.0) / 3;
        let product = third * LongDouble::from(3.0);
        assert_eq!(product.to_f64(), 1.0);
        assert_eq!(
            (LongDouble::from(-2.0) * LongDouble::from(0.75)).to_f64(),
            -1.5
        );
    }

    #[test]
    fn square_roots_are_correctly_rounded() {
        assert_eq!(LongDouble::from(2.0).sqrt().to_f64(), 2f64.sqrt());
        assert_eq!(LongDouble::from(0.25).sqrt().to_f64(), 0.5);
        assert_eq!(LongDouble::from(1e-300).sqrt().to_f64(), 1e-150);
        assert!(LongDouble::from(-1.0).sqrt().to_f64().is_nan());
    }

    #[test]
    fn kernels_match_the_software_model() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut draw = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mantissa = (state >> 11) as f64 / (1u64 << 53) as f64;
            let scale = [1e-300, 1e-8, 1.0, 1e8, 1e300][(state % 5) as usize];
            if state % 3 == 0 {
                -mantissa * scale
            } else {
                mantissa * scale
            }
        };
        let values: Vec<f64> = (0..997).map(|_| draw()).collect();
        let other: Vec<f64> = (0..997).map(|_| draw()).collect();
        let bits = |value: LongDouble| value.to_f64().to_bits();
        let software = values.iter().fold(LongDouble::ZERO, |total, &value| {
            total + LongDouble::from(value)
        });
        assert_eq!(finite_sum(&values).map(bits), Some(bits(software)));
        let center = software / values.len();
        let deviations = values.iter().fold(LongDouble::ZERO, |total, &value| {
            total + (LongDouble::from(value) - center)
        });
        assert_eq!(
            finite_deviation_sum(&values, center).map(bits),
            Some(bits(deviations))
        );
        let other_center = LongDouble::from(0.5);
        let products = values
            .iter()
            .zip(&other)
            .fold(LongDouble::ZERO, |total, (&x, &y)| {
                total + (LongDouble::from(x) - center) * (LongDouble::from(y) - other_center)
            });
        assert_eq!(
            finite_cross_deviation_sum(&values, &other, center, other_center).map(bits),
            Some(bits(products))
        );
        for divisor in [1, 10] {
            let mut out = vec![0.0; 99];
            let written = finite_row_means(&values[..990], 99, divisor, &mut out);
            assert_eq!(written, vec![true; 99]);
            for (row, &value) in out.iter().enumerate() {
                let total = (0..10).fold(LongDouble::ZERO, |total, column| {
                    total + LongDouble::from(values[row + column * 99])
                });
                assert_eq!(value.to_bits(), (total / divisor).to_f64().to_bits());
            }
        }
        let mut out = vec![0.0; 2];
        assert_eq!(
            finite_row_means(&[1.0, f64::NAN, 2.0, 3.0], 2, 2, &mut out),
            vec![true, false]
        );
        assert_eq!(out[0], 1.5);
        assert_eq!(finite_sum(&[1.0, f64::NAN]).map(bits), None);
        assert_eq!(finite_sum(&[f64::INFINITY, 1.0]).map(bits), None);
        assert_eq!(
            finite_sum(&[f64::MAX, f64::MAX]).map(|total| total.to_f64()),
            Some(f64::INFINITY)
        );
    }

    #[test]
    fn overflow_is_only_seen_on_conversion() {
        let total = LongDouble::from(f64::MAX) + LongDouble::from(f64::MAX);
        assert_eq!(total.to_f64(), f64::INFINITY);
        assert_eq!((total / 2).to_f64(), f64::MAX);
    }
}
