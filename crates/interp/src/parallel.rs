//! The parallel package's native routines. The L'Ecuyer-CMRG stream jumps behind
//! `nextRNGStream` and `nextRNGSubStream` are implemented. Forking (`mcfork` and the routines
//! that talk to forked children) is not, so `mclapply` and `mcparallel` work only on the paths
//! that run in the current process, such as `mc.cores = 1`.

use crate::{
    builtin_libraries::{NativeCall, NativeFn},
    coerce::coerce_vector,
    unwind::R,
    values::{self, nil, scalar_bool},
};
use heap::object::{Sexp, SexpType};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("nextStream", |call| unsafe { advance(call, &A1P127, &A2P127) }),
    ("nextSubStream", |call| unsafe { advance(call, &A1P76, &A2P76) }),
    ("mc_is_child", |_| unsafe { scalar_bool(false) }),
    ("mc_children", |_| unsafe { values::integer_vector(&[]) }),
    ("mc_fds", |_| unsafe { values::integer_vector(&[]) }),
    ("mc_master_fd", |_| unsafe { values::scalar_integer(-1) }),
    ("mc_interactive", |_| Ok(nil())),
    ("mc_affinity", |_| Ok(nil())),
    ("mc_prepare_cleanup", |_| Ok(nil())),
    ("mc_cleanup", |_| Ok(nil())),
];

const M1: u64 = 4_294_967_087;
const M2: u64 = 4_294_944_443;

/// The RngStreams jump matrices: `A1P127` and `A2P127` advance a seed by 2^127 steps (to the
/// next stream), `A1P76` and `A2P76` by 2^76 (to the next substream).
const A1P127: [[u64; 3]; 3] = [
    [2_427_906_178, 3_580_155_704, 949_770_784],
    [226_153_695, 1_230_515_664, 3_580_155_704],
    [1_988_835_001, 986_791_581, 1_230_515_664],
];
const A2P127: [[u64; 3]; 3] = [
    [1_464_411_153, 277_697_599, 1_610_723_613],
    [32_183_930, 1_464_411_153, 1_022_607_788],
    [2_824_425_944, 32_183_930, 2_093_834_863],
];
const A1P76: [[u64; 3]; 3] = [
    [82_758_667, 1_871_391_091, 4_127_413_238],
    [3_672_831_523, 69_195_019, 1_871_391_091],
    [3_672_091_415, 3_528_743_235, 69_195_019],
];
const A2P76: [[u64; 3]; 3] = [
    [1_511_326_704, 3_759_209_742, 1_610_795_712],
    [4_292_754_251, 1_511_326_704, 3_889_917_532],
    [3_859_662_829, 4_292_754_251, 3_708_466_080],
];

/// Multiplies the two halves of a `.Random.seed` (the kind code, then six seed words) by the
/// jump matrices. The arithmetic wraps in 64 bits exactly as GNU R's C code does, so the
/// streams match GNU R's bit for bit.
unsafe fn advance(call: &NativeCall<'_>, first: &[[u64; 3]; 3], second: &[[u64; 3]; 3]) -> R<Sexp> {
    let seed = coerce_vector(call.arg(0), SexpType::Integer)?;
    let seed = values::ints(seed);
    if seed.len() < 7 {
        return Err(call.error("invalid seed: a L'Ecuyer-CMRG seed has 7 integers"));
    }
    let word = |index: usize| u64::from(seed[index + 1] as u32);
    let mut next = [0i32; 7];
    next[0] = seed[0];
    for (half, (matrix, modulus)) in [(first, M1), (second, M2)].into_iter().enumerate() {
        for (row, coefficients) in matrix.iter().enumerate() {
            let mut sum: u64 = 0;
            for (column, coefficient) in coefficients.iter().enumerate() {
                sum = sum.wrapping_add(coefficient.wrapping_mul(word(half * 3 + column)));
                sum %= modulus;
            }
            next[1 + half * 3 + row] = sum as u32 as i32;
        }
    }
    values::integer_vector(&next)
}
