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

/// `total` plus every value in order, rounding after each addition, as a C loop of
/// `total += values[i]` on a `long double` does.
pub fn add_all(total: LongDouble, values: &[f64]) -> LongDouble {
    #[cfg(target_arch = "x86_64")]
    if let Some(start) = to_x87(total) {
        return from_x87(x87::add_all(start, values));
    }
    values
        .iter()
        .fold(total, |total, &value| total + LongDouble::from(value))
}

/// `total` plus every `value - offset` in order, rounding after each subtraction and addition,
/// as GNU R's second pass over the deviations from a mean does.
pub fn add_differences(total: LongDouble, values: &[f64], offset: LongDouble) -> LongDouble {
    #[cfg(target_arch = "x86_64")]
    if let (Some(start), Some(offset)) = (to_x87(total), to_x87(offset)) {
        return from_x87(x87::add_differences(start, values, offset));
    }
    values.iter().fold(total, |total, &value| {
        total + (LongDouble::from(value) - offset)
    })
}

/// The x87 extended format's bytes: the 64-bit significand with its explicit integer bit, then
/// the sign and a 15-bit exponent biased by 16383. `None` when the exponent leaves its range,
/// which only products and quotients of extreme values reach.
#[cfg(target_arch = "x86_64")]
fn to_x87(value: LongDouble) -> Option<[u8; 10]> {
    let (negative, significand, biased) = match value.0 {
        Repr::Special(special) => {
            let bits = special.to_bits();
            let negative = bits >> 63 == 1;
            // An infinity has only the integer bit; a NaN keeps its payload below it.
            let payload = (bits & ((1 << 52) - 1)) << 11;
            (negative, (1 << 63) | payload, 0x7fff)
        }
        Repr::Finite {
            negative,
            significand: 0,
            ..
        } => (negative, 0, 0),
        Repr::Finite {
            negative,
            significand,
            exponent,
        } => {
            let biased = exponent + 63 + 16383;
            if !(1..0x7fff).contains(&biased) {
                return None;
            }
            (negative, significand, biased as u16)
        }
    };
    let mut bytes = [0u8; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    let top = biased | if negative { 0x8000 } else { 0 };
    bytes[8..].copy_from_slice(&top.to_le_bytes());
    Some(bytes)
}

#[cfg(target_arch = "x86_64")]
fn from_x87(bytes: [u8; 10]) -> LongDouble {
    let mut low = [0u8; 8];
    low.copy_from_slice(&bytes[..8]);
    let significand = u64::from_le_bytes(low);
    let top = u16::from_le_bytes([bytes[8], bytes[9]]);
    let negative = top & 0x8000 != 0;
    let biased = i32::from(top & 0x7fff);
    if biased == 0x7fff {
        let fraction = (significand << 1) >> 12;
        let magnitude = if fraction == 0 {
            f64::INFINITY
        } else {
            f64::from_bits((0x7ff << 52) | fraction | (1 << 51))
        };
        return LongDouble(Repr::Special(if negative { -magnitude } else { magnitude }));
    }
    if significand == 0 {
        return LongDouble(Repr::Finite {
            negative,
            significand: 0,
            exponent: 0,
        });
    }
    // A denormal's exponent field is 0 but means the same scale as 1.
    let shift = significand.leading_zeros();
    LongDouble(Repr::Finite {
        negative,
        significand: significand << shift,
        exponent: biased.max(1) - 16383 - 63 - shift as i32,
    })
}

/// The loops that need speed run on the x87 unit itself, which rounds exactly like the model:
/// System V leaves its precision control at the full 64-bit significand.
#[cfg(target_arch = "x86_64")]
mod x87 {
    use std::arch::asm;

    pub fn add_all(mut total: [u8; 10], values: &[f64]) -> [u8; 10] {
        // SAFETY: the loop reads `values.len()` doubles from the slice and writes `total` back;
        // the x87 stack starts empty under the ABI and is left empty.
        unsafe {
            asm!(
                "fld tbyte ptr [{total}]",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fadd qword ptr [{data}]",
                "add {data}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{total}]",
                total = in(reg) total.as_mut_ptr(),
                data = inout(reg) values.as_ptr() => _,
                count = inout(reg) values.len() => _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        total
    }

    pub fn add_differences(mut total: [u8; 10], values: &[f64], offset: [u8; 10]) -> [u8; 10] {
        // SAFETY: as in `add_all`; `offset` is only read.
        unsafe {
            asm!(
                "fld tbyte ptr [{offset}]",
                "fld tbyte ptr [{total}]",
                "test {count}, {count}",
                "jz 3f",
                "2:",
                "fld qword ptr [{data}]",
                "fsub st(0), st(2)",
                "faddp st(1), st(0)",
                "add {data}, 8",
                "dec {count}",
                "jnz 2b",
                "3:",
                "fstp tbyte ptr [{total}]",
                "fstp st(0)",
                total = in(reg) total.as_mut_ptr(),
                offset = in(reg) offset.as_ptr(),
                data = inout(reg) values.as_ptr() => _,
                count = inout(reg) values.len() => _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        total
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
    fn slice_sums_match_the_model() {
        let cases: [&[f64]; 6] = [
            &[],
            &[1.0, 2.0, 3.0],
            &[1e300, 1e300, -1e300, 0.1, 5e-324],
            &[f64::MAX, f64::MAX, -f64::MAX],
            &[1.0, f64::INFINITY, -f64::INFINITY],
            &[-0.0, -0.0],
        ];
        for values in cases {
            let model = values.iter().fold(LongDouble::ZERO, |total, &value| {
                total + LongDouble::from(value)
            });
            let fast = add_all(LongDouble::ZERO, values);
            assert_eq!(format!("{fast:?}"), format!("{model:?}"), "{values:?}");
            let third = LongDouble::from(1.0) / 3;
            let model = values.iter().fold(third, |total, &value| {
                total + (LongDouble::from(value) - third)
            });
            let fast = add_differences(third, values, third);
            assert_eq!(format!("{fast:?}"), format!("{model:?}"), "{values:?}");
        }
        let pseudo: Vec<f64> = (1..2000u32)
            .map(|index| f64::from(index.wrapping_mul(2654435761) % 10007) * 1.37e-3 - 6.0)
            .collect();
        let model = pseudo.iter().fold(LongDouble::ZERO, |total, &value| {
            total + LongDouble::from(value)
        });
        assert_eq!(
            format!("{:?}", add_all(LongDouble::ZERO, &pseudo)),
            format!("{model:?}")
        );
    }

    #[test]
    fn overflow_is_only_seen_on_conversion() {
        let total = LongDouble::from(f64::MAX) + LongDouble::from(f64::MAX);
        assert_eq!(total.to_f64(), f64::INFINITY);
        assert_eq!((total / 2).to_f64(), f64::MAX);
    }
}
