//! ry's tables as R values: the `DataFrame` a session works with (`frame`), its expression and
//! lazy-query API (`dataframe`), and the data viewer behind `View()` and the editor's variable
//! pane (`viewer`). The table engine itself is the `table` crate, which is backed by Polars and
//! is the reason this code is not part of the interpreter: nothing else in the runtime depends
//! on Polars.
//!
//! The crate sits above the interpreter. A host calls [`install`] after `interp::install`; the
//! interpreter then reaches tables through `interp::extensions`.

// The code dereferences raw handles everywhere, so an `unsafe` block inside each `unsafe fn`
// would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of the
// interpreter's heap.
#![allow(clippy::missing_safety_doc)]

pub mod dataframe;
pub mod frame;
pub mod viewer;

use interp::{
    define_builtins, define_own_builtins,
    extensions::{Tables, Viewer},
    interp,
};

/// Registers the tables' builtins with the interpreter, which must be installed, and hands it
/// the table operations and the viewer.
pub unsafe fn install() {
    define_own_builtins(frame::BUILTINS);
    define_builtins(dataframe::BUILTINS);
    let extensions = &mut interp().extensions;
    extensions.tables = Tables {
        is_frame: frame::is_frame,
        is_table_value: dataframe::is_table_value,
        subset: frame::run,
        package: &[frame::PACKAGE, dataframe::PACKAGE],
        methods: &[frame::METHODS, dataframe::METHODS],
        release_worker_memory: frame::release_worker_memory,
    };
    extensions.viewer = Viewer {
        open: viewer::open,
        request: viewer::request,
        forget_orders: viewer::forget_orders,
        globals: viewer::globals,
    };
}
