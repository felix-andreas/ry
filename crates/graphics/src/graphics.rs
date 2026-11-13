//! R's graphics: the graphics engine with its device table (`engine`), the native routines of
//! the grDevices and graphics packages together with the built-in devices (`routines`),
//! mathematical annotation (`plotmath`), and the renderer that turns what a device recorded
//! into PDF, PostScript, SVG and raster files (`render`).
//!
//! The crate sits above the interpreter. A host calls [`install`] after `interp::install`; the
//! interpreter then resolves the two packages' native routines here and reaches the engine
//! through `interp::extensions::Graphics` when a session starts, ends, saves its image or
//! collects garbage.

// The code dereferences raw handles everywhere, so an `unsafe` block inside each `unsafe fn`
// would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of the
// interpreter's heap.
#![allow(clippy::missing_safety_doc)]
// The contour and perspective ports keep the reference algorithms' index-based loops and literal
// constants so they stay comparable line by line with the C originals.
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

pub mod engine;
pub mod plotmath;
pub mod render;
pub mod routines;
pub mod testing;

use interp::{
    builtin_libraries::register_routines,
    builtins::{BuiltinDef, builtin},
    extensions::Graphics,
    interp,
    native::{foreign_call, foreign_context},
    register_builtins,
};

/// Registers the graphics builtins and the routines of grDevices and graphics with the
/// interpreter, which must be installed, and hands it the engine.
pub unsafe fn install() {
    register_builtins(engine::BUILTINS);
    register_builtins(FOREIGN_BUILTINS);
    register_routines("grDevices", routines::GRDEVICES_ROUTINES);
    register_routines("graphics", routines::GRAPHICS_ROUTINES);
    interp().extensions.graphics = Graphics {
        trace: engine::trace,
        define_device_variables: routines::define_device_variables,
        kill_all_devices: engine::kill_all_devices,
        has_open_device: engine::has_open_device,
        current_plot_stamp: engine::current_plot_stamp,
    };
}

/// `.Call.graphics` and `.External.graphics`: a foreign call that the current device records in
/// its display list.
const FOREIGN_BUILTINS: &[BuiltinDef] = &[
    builtin(".Call.graphics", -1, |call| unsafe {
        foreign_context(call.call, || {
            engine::graphics_call(call, || foreign_call(call, None))
        })
    }),
    builtin(".External.graphics", -1, |call| unsafe {
        foreign_context(call.call, || {
            engine::graphics_call(call, || foreign_call(call, None))
        })
    }),
];
