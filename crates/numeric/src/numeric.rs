//! The numeric kernels of ry's R runtime: R's nmath library (`rmath`), x87 long double arithmetic
//! (`long_double`), the vectorized elementary functions (`vector_math`), Singleton's FFT (`fft`),
//! LINPACK (`linpack`) and C's `printf` number formats (`c_format`).
//!
//! Nothing here reads the heap or the interpreter's state: every function takes and returns
//! plain numbers. That is what lets the interpreter sit above this crate, so an edit to the
//! evaluator recompiles none of it.
//!
//! GNU R changed a few numeric results between the releases ry emulates, so some kernels ask
//! which release is emulated ([`r_at_least`]). The interpreter answers that from the base R it
//! loads, through [`set_emulated_r_provider`].

// The kernels are called with numbers the caller already validated, so an `unsafe` block inside
// each `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]
// The ports keep the reference algorithms' index-based loops and literal constants so they stay
// comparable line by line with the C and Fortran originals.
#![allow(
    clippy::needless_range_loop,
    clippy::manual_memcpy,
    clippy::excessive_precision,
    clippy::explicit_counter_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::int_plus_one,
    clippy::manual_range_contains,
    clippy::assign_op_pattern,
    clippy::type_complexity
)]

pub mod c_format;
pub mod fft;
pub mod linpack;
pub mod long_double;
pub mod rmath;
pub mod vector_math;

use std::sync::OnceLock;

/// The newest GNU R release whose behavior ry emulates, which is the base R ry bundles.
pub const NEWEST_EMULATED_R: (u32, u32, u32) = (4, 6, 2);

/// Whether the emulated R is `major.minor` or later. Behavior that GNU R changed between
/// supported releases follows the release in `R_HOME`, so the base packages' R code and the
/// internals it calls always agree.
#[inline]
pub fn r_at_least(major: u32, minor: u32) -> bool {
    let (emulated_major, emulated_minor, _) = emulated_r();
    (emulated_major, emulated_minor) >= (major, minor)
}

/// Whether the emulated R is `major.minor.patch` or later, for the rare change GNU R made in a
/// patch release.
#[inline]
pub fn r_at_least_release(major: u32, minor: u32, patch: u32) -> bool {
    emulated_r() >= (major, minor, patch)
}

/// Names the function that reads the emulated release from the base R the session runs. It is
/// called once, on the first question about the release, because finding the R home reads the
/// file system.
pub fn set_emulated_r_provider(provider: fn() -> (u32, u32, u32)) {
    // A second registration names the same function, since one process runs one interpreter.
    PROVIDER.get_or_init(|| provider);
}

static PROVIDER: OnceLock<fn() -> (u32, u32, u32)> = OnceLock::new();
static EMULATED_R: OnceLock<(u32, u32, u32)> = OnceLock::new();

/// The emulated release. Code that runs with no interpreter installed, such as a unit test of a
/// kernel, sees the newest release, and that answer is not kept, so a later installation still
/// gets asked.
#[inline]
fn emulated_r() -> (u32, u32, u32) {
    if let Some(version) = EMULATED_R.get() {
        return *version;
    }
    match PROVIDER.get() {
        Some(provider) => *EMULATED_R.get_or_init(provider),
        None => NEWEST_EMULATED_R,
    }
}
