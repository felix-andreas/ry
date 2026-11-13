//! What the crates above the interpreter provide to it. The graphics system, ry's tables, the
//! data viewer and the HTTP client are implemented in their own crates, which depend on this
//! one, so the interpreter reaches them through the function tables here. Each crate's `install`
//! fills its table after [`crate::install`]. Until then every entry is the default a host
//! without that crate runs with: a question answers that nothing is there (no device is open,
//! no object is a table), and an operation that needs the crate is an error.

use {
    crate::{
        http::{FetchError, Fetched},
        unwind::{R, error},
    },
    heap::{Marker, object::Sexp},
    serde_json::Value,
    std::time::Duration,
};

#[derive(Clone, Copy)]
pub struct Extensions {
    pub graphics: Graphics,
    pub tables: Tables,
    pub viewer: Viewer,
    pub http: Transport,
}

/// The graphics engine, as far as a session's start, its end and the collector need it.
#[derive(Clone, Copy)]
pub struct Graphics {
    /// Marks the objects the graphics devices hold.
    pub trace: unsafe fn(&mut Marker<'_>),
    /// Binds the variables that list the devices (`.Devices`, `.Device`) and registers the base
    /// graphics system, when a session defines its process variables.
    pub define_device_variables: unsafe fn() -> R<()>,
    /// Closes every device, which writes the files they were drawing.
    pub kill_all_devices: unsafe fn() -> R<()>,
    pub has_open_device: fn() -> bool,
    /// A number that identifies what the current device has drawn, which changes with every
    /// drawing operation. `None` with no device open or nothing drawn yet, and an error when the
    /// current device is the null device.
    pub current_plot_stamp: unsafe fn() -> Result<Option<u64>, String>,
}

/// ry's tables (`DataFrame` and the values of its API).
#[derive(Clone, Copy)]
pub struct Tables {
    /// Whether an object is a table.
    pub is_frame: unsafe fn(Sexp) -> bool,
    /// Whether an object is one of the table API's own values: a `DataFrame`, a `LazyFrame`, a
    /// `GroupBy` or an expression. Typed code holds one without looking inside it.
    pub is_table_value: unsafe fn(Sexp) -> bool,
    /// Runs `[` on a table: the call, the table and the calling environment.
    pub subset: unsafe fn(Sexp, Sexp, Sexp) -> R<Sexp>,
    /// R source evaluated in the `ry` namespace when `package:ry` is attached, which defines the
    /// tables' user-facing functions.
    pub package: &'static [&'static str],
    /// R source evaluated in the global environment after `package:ry` is attached, which
    /// registers the tables' S3 methods for other packages' generics.
    pub methods: &'static [&'static str],
    /// Returns the memory the table engine's worker threads hold free to the system.
    pub release_worker_memory: fn(),
}

/// The data viewer behind `View()` and the editor's variable pane.
#[derive(Clone, Copy)]
pub struct Viewer {
    /// Opens a view of an object under a title, and gives the view's description.
    pub open: unsafe fn(Sexp, &str) -> Result<Value, String>,
    /// Answers a view request (`ryTable`, `ryRelease`): the command and its arguments.
    pub request: unsafe fn(&str, &Value) -> Result<Value, String>,
    /// Drops the row orders the open views keep.
    pub forget_orders: fn(),
    /// The global environment's variables, for completion and hover.
    pub globals: unsafe fn() -> Value,
}

/// The HTTP and HTTPS client behind `url()`, `download.file()` and `curlGetHeaders()`. The
/// functions of [`crate::http`] call it and document each entry.
#[derive(Clone, Copy)]
pub struct Transport {
    pub fetch: fn(&str, &[String]) -> Result<Fetched, FetchError>,
    pub header_lines: fn(&str, bool, Option<Duration>) -> Result<(Vec<String>, u16), FetchError>,
    pub normalized: fn(&str) -> String,
    pub host: fn(&str) -> Option<String>,
}

impl Default for Extensions {
    fn default() -> Self {
        Extensions {
            graphics: Graphics {
                trace: |_| {},
                define_device_variables: || Ok(()),
                kill_all_devices: || Ok(()),
                has_open_device: || false,
                current_plot_stamp: || Ok(None),
            },
            tables: Tables {
                is_frame: |_| false,
                is_table_value: |_| false,
                subset: |_, _, _| Err(error("this host runs without ry's tables")),
                package: &[],
                methods: &[],
                release_worker_memory: || {},
            },
            viewer: Viewer {
                open: |_, _| Err("this host runs without the data viewer".to_owned()),
                request: |_, _| Err("this host runs without the data viewer".to_owned()),
                forget_orders: || {},
                globals: || Value::Array(Vec::new()),
            },
            http: Transport {
                fetch: |_, _| Err(no_transport()),
                header_lines: |_, _, _| Err(no_transport()),
                normalized: str::to_owned,
                host: |_| None,
            },
        }
    }
}

/// The failure of every transfer on a host without an HTTP client, in libcurl's words.
fn no_transport() -> FetchError {
    FetchError::Transport {
        code: 1,
        message: "Unsupported protocol",
        detail: "this host runs without an HTTP client".to_owned(),
    }
}
