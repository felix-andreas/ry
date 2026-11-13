//! The native routines of R's stats package, other than the distribution functions, which the
//! interpreter keeps because base R's own code calls them: least squares and the GLM helpers,
//! model formulas, the optimizers and integrators, smoothers, time series and the exact tests.
//! Most are ports of the C and Fortran sources GNU R ships, kept in the original operation order
//! so results agree with GNU R to the last printed digit.
//!
//! The crate sits above the interpreter. A host calls [`install`] after `interp::install`, and
//! the interpreter then resolves the stats package's `.Call`, `.External` and `.C` routines here.

// The code dereferences raw handles everywhere, so an `unsafe` block inside each `unsafe fn`
// would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of the
// interpreter's heap.
#![allow(clippy::missing_safety_doc)]
// The numeric ports keep the reference algorithms' index-based loops and literal constants so
// they stay comparable line by line with the C and Fortran originals.
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

pub mod arima;
pub mod cluster;
pub mod deriv;
pub mod hypothesis_tests;
pub mod lbfgsb;
pub mod loess;
pub mod models;
pub mod nlm;
pub mod optimize;
pub mod port;
pub mod port_minimizer;
pub mod routines;
pub mod spline;

/// Registers the stats package's routines with the interpreter, which must be installed.
pub unsafe fn install() {
    interp::builtin_libraries::register_routines("stats", routines::ROUTINES);
}
