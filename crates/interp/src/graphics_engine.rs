//! R's graphics engine: the device table, the graphics systems registered with every device,
//! and the drawing calls that sit between a system (base graphics, grid) and a device.
//!
//! The structures below are laid out exactly as `R_ext/GraphicsEngine.h` and
//! `R_ext/GraphicsDevice.h` declare them, because compiled packages read and write them
//! directly: grid flips `dd->recordGraphics`, and a device package such as ragg fills in a
//! `DevDesc` of its own and hands it to the engine. Every device, built in or not, is a
//! `DevDesc` with C callbacks, so base graphics and grid draw through one path.
//!
//! A device callback may be native code that raises an R error with a `longjmp`. Every call
//! into a device or a system therefore goes through the host's `guard_native` hook, which runs
//! it inside a native context and turns such an error into an unwind. A device written in Rust
//! cannot unwind through `extern "C"`, so it reports a failure with [`fail`] instead.

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        eval, interp,
        unwind::{R, Unwind, error},
        values::{self, length, nil, scalar_integer, scalar_string},
    },
    heap::{
        Marker,
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType},
    },
    std::{
        borrow::Cow,
        cell::RefCell,
        ffi::{CString, c_char, c_int, c_uint, c_void},
        ptr::{self, null_mut},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[internal("recordGraphics", 3, |call| unsafe {
    record_graphics(call)
})];

/// `R_GE_version`: the graphics engine version ry presents, R 4.6's.
pub const ENGINE_VERSION: c_int = 17;

/// The oldest engine version a package may be compiled against. Every version from here on
/// shares the `DevDesc` layout, which grew only at its end and in its reserved space.
pub const OLDEST_ENGINE_VERSION: c_int = 14;

/// How many devices can be open at once, counting the null device in slot 0.
pub const MAX_DEVICES: usize = 64;

pub const MAX_SYSTEMS: usize = 24;

/// `R_GE_definitions`, `R_GE_deviceClip`, `R_GE_group` and `R_GE_glyphs`: the device versions
/// that introduced patterns, device clipping, groups and paths, and glyphs.
const VERSION_DEFINITIONS: c_int = 13;
const VERSION_DEVICE_CLIP: c_int = 14;
pub const VERSION_GROUP: c_int = 15;
pub const VERSION_GLYPHS: c_int = 16;

pub const NULL_DEVICE_NAME: &str = "null device";

pub type Rboolean = c_int;
pub type Colour = u32;

/// A colour with zero alpha, which devices treat as no colour at all.
pub const TRANSPARENT_WHITE: c_int = 0x00FF_FFFF;

/// `LTY_BLANK`: a line type that draws nothing.
pub const LTY_BLANK: c_int = -1;

/// The graphics state every drawing call carries (`R_GE_gcontext`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GEContext {
    pub col: c_int,
    pub fill: c_int,
    pub gamma: f64,
    pub lwd: f64,
    pub lty: c_int,
    pub lend: c_int,
    pub ljoin: c_int,
    pub lmitre: f64,
    pub cex: f64,
    pub ps: f64,
    pub lineheight: f64,
    pub fontface: c_int,
    pub fontfamily: [c_char; 201],
    pub pattern_fill: Option<Sexp>,
}

impl GEContext {
    /// A context with GNU R's defaults: black lines one unit wide, no fill, 12 points.
    pub fn new() -> Self {
        GEContext {
            col: 0xFF00_0000_u32 as c_int,
            fill: TRANSPARENT_WHITE,
            gamma: 1.0,
            lwd: 1.0,
            lty: 0,
            lend: 1,
            ljoin: 1,
            lmitre: 10.0,
            cex: 1.0,
            ps: 12.0,
            lineheight: 1.0,
            fontface: 1,
            fontfamily: [0; 201],
            pattern_fill: None,
        }
    }

    /// The font family as text, up to its terminating NUL.
    pub fn family(&self) -> String {
        let bytes: Vec<u8> = self
            .fontfamily
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn set_family(&mut self, family: &str) {
        self.fontfamily = [0; 201];
        for (slot, byte) in self.fontfamily.iter_mut().zip(family.bytes().take(200)) {
            *slot = byte as c_char;
        }
    }
}

impl Default for GEContext {
    fn default() -> Self {
        GEContext::new()
    }
}

type Dev = *mut DevDesc;
type Gc = *const GEContext;

/// A device's description of itself (`DevDesc`): its extent and character metrics, what it
/// can do, and the callbacks that draw.
#[repr(C)]
pub struct DevDesc {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
    pub clip_left: f64,
    pub clip_right: f64,
    pub clip_bottom: f64,
    pub clip_top: f64,
    pub x_char_offset: f64,
    pub y_char_offset: f64,
    pub y_line_bias: f64,
    pub ipr: [f64; 2],
    pub cra: [f64; 2],
    pub gamma: f64,
    pub can_clip: Rboolean,
    pub can_change_gamma: Rboolean,
    pub can_hadj: c_int,
    pub startps: f64,
    pub startcol: c_int,
    pub startfill: c_int,
    pub startlty: c_int,
    pub startfont: c_int,
    pub startgamma: f64,
    pub device_specific: *mut c_void,
    pub display_list_on: Rboolean,
    pub can_gen_mouse_down: Rboolean,
    pub can_gen_mouse_move: Rboolean,
    pub can_gen_mouse_up: Rboolean,
    pub can_gen_keybd: Rboolean,
    pub can_gen_idle: Rboolean,
    pub getting_event: Rboolean,
    pub activate: Option<unsafe extern "C" fn(Dev)>,
    pub circle: Option<unsafe extern "C" fn(f64, f64, f64, Gc, Dev)>,
    pub clip: Option<unsafe extern "C" fn(f64, f64, f64, f64, Dev)>,
    pub close: Option<unsafe extern "C" fn(Dev)>,
    pub deactivate: Option<unsafe extern "C" fn(Dev)>,
    pub locator: Option<unsafe extern "C" fn(*mut f64, *mut f64, Dev) -> Rboolean>,
    pub line: Option<unsafe extern "C" fn(f64, f64, f64, f64, Gc, Dev)>,
    pub metric_info: Option<unsafe extern "C" fn(c_int, Gc, *mut f64, *mut f64, *mut f64, Dev)>,
    pub mode: Option<unsafe extern "C" fn(c_int, Dev)>,
    pub new_page: Option<unsafe extern "C" fn(Gc, Dev)>,
    pub polygon: Option<unsafe extern "C" fn(c_int, *mut f64, *mut f64, Gc, Dev)>,
    pub polyline: Option<unsafe extern "C" fn(c_int, *mut f64, *mut f64, Gc, Dev)>,
    pub rect: Option<unsafe extern "C" fn(f64, f64, f64, f64, Gc, Dev)>,
    pub path:
        Option<unsafe extern "C" fn(*mut f64, *mut f64, c_int, *mut c_int, Rboolean, Gc, Dev)>,
    #[allow(clippy::type_complexity)]
    pub raster: Option<
        unsafe extern "C" fn(*mut c_uint, c_int, c_int, f64, f64, f64, f64, f64, Rboolean, Gc, Dev),
    >,
    pub cap: Option<unsafe extern "C" fn(Dev) -> Option<Sexp>>,
    pub size: Option<unsafe extern "C" fn(*mut f64, *mut f64, *mut f64, *mut f64, Dev)>,
    pub str_width: Option<unsafe extern "C" fn(*const c_char, Gc, Dev) -> f64>,
    pub text: Option<unsafe extern "C" fn(f64, f64, *const c_char, f64, f64, Gc, Dev)>,
    pub on_exit: Option<unsafe extern "C" fn(Dev)>,
    pub get_event: Option<unsafe extern "C" fn(Sexp, *const c_char) -> Option<Sexp>>,
    pub new_frame_confirm: Option<unsafe extern "C" fn(Dev) -> Rboolean>,
    pub has_text_utf8: Rboolean,
    pub text_utf8: Option<unsafe extern "C" fn(f64, f64, *const c_char, f64, f64, Gc, Dev)>,
    pub str_width_utf8: Option<unsafe extern "C" fn(*const c_char, Gc, Dev) -> f64>,
    pub want_symbol_utf8: Rboolean,
    pub use_rotated_text_in_contour: Rboolean,
    pub event_env: Option<Sexp>,
    pub event_helper: Option<unsafe extern "C" fn(Dev, c_int)>,
    pub holdflush: Option<unsafe extern "C" fn(Dev, c_int) -> c_int>,
    pub have_transparency: c_int,
    pub have_transparent_bg: c_int,
    pub have_raster: c_int,
    pub have_capture: c_int,
    pub have_locator: c_int,
    pub set_pattern: Option<unsafe extern "C" fn(Sexp, Dev) -> Option<Sexp>>,
    pub release_pattern: Option<unsafe extern "C" fn(Sexp, Dev)>,
    pub set_clip_path: Option<unsafe extern "C" fn(Sexp, Sexp, Dev) -> Option<Sexp>>,
    pub release_clip_path: Option<unsafe extern "C" fn(Sexp, Dev)>,
    pub set_mask: Option<unsafe extern "C" fn(Sexp, Sexp, Dev) -> Option<Sexp>>,
    pub release_mask: Option<unsafe extern "C" fn(Sexp, Dev)>,
    pub device_version: c_int,
    pub device_clip: Rboolean,
    pub define_group: Option<unsafe extern "C" fn(Sexp, c_int, Sexp, Dev) -> Option<Sexp>>,
    pub use_group: Option<unsafe extern "C" fn(Sexp, Sexp, Dev)>,
    pub release_group: Option<unsafe extern "C" fn(Sexp, Dev)>,
    pub stroke: Option<unsafe extern "C" fn(Sexp, Gc, Dev)>,
    pub fill: Option<unsafe extern "C" fn(Sexp, c_int, Gc, Dev)>,
    pub fill_stroke: Option<unsafe extern "C" fn(Sexp, c_int, Gc, Dev)>,
    pub capabilities: Option<unsafe extern "C" fn(Sexp) -> Option<Sexp>>,
    #[allow(clippy::type_complexity)]
    pub glyph: Option<
        unsafe extern "C" fn(c_int, *mut c_int, *mut f64, *mut f64, Sexp, f64, c_int, f64, Dev),
    >,
    pub reserved: [c_char; 64],
}

/// What a system's callback is asked to do (`GEevent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Event {
    InitState = 0,
    FinaliseState = 1,
    SaveState = 2,
    CopyState = 3,
    SaveSnapshotState = 4,
    RestoreSnapshotState = 5,
    RestoreState = 6,
    CheckPlot = 7,
    ScalePS = 8,
}

pub type SystemCallback = unsafe extern "C" fn(c_int, *mut GEDevDesc, Sexp) -> Option<Sexp>;

/// One system's state on one device (`GESystemDesc`).
#[repr(C)]
pub struct GESystemDesc {
    pub system_specific: *mut c_void,
    pub callback: Option<SystemCallback>,
}

/// The engine's view of a device (`GEDevDesc`).
///
/// GNU R 4.3's layout has no `lock`, so in a package compiled against it `gesd[i]` falls on
/// this layout's `gesd[i - 1]` and `ask` on `gesd[23]`. The engine therefore hands systems odd
/// indices only and stores each system's descriptor in both slots of its pair, and it mirrors
/// `ask` into the last slot, which no system uses.
#[repr(C)]
pub struct GEDevDesc {
    pub dev: *mut DevDesc,
    pub display_list_on: Rboolean,
    /// Always `NULL`: the display list lives in [`EngineDevice`], which no package reads.
    pub display_list: Sexp,
    pub dl_last_elt: Sexp,
    pub saved_snapshot: Sexp,
    pub dirty: Rboolean,
    pub record_graphics: Rboolean,
    pub lock: Rboolean,
    pub gesd: [*mut GESystemDesc; MAX_SYSTEMS],
    pub ask: Rboolean,
    pub appending: Rboolean,
}

/// A device the engine owns: the `GEDevDesc` packages see, then what only the engine reads.
/// `GEcreateDevDesc` hands out a pointer to the first field, which is a pointer to the whole.
#[repr(C)]
pub struct EngineDevice {
    pub ge: GEDevDesc,
    /// The name `.Devices` lists, and the file it records as the name's `filepath`.
    name: String,
    file: Option<String>,
    /// The display list, one entry per recorded graphics operation: the primitive called and
    /// its argument pairlist.
    ops: Vec<Sexp>,
    args: Vec<Sexp>,
}

struct Engine {
    devices: [*mut EngineDevice; MAX_DEVICES],
    current: usize,
    /// The length of `.Devices`, which never shrinks: a closed device leaves `""`.
    listed: usize,
    systems: [Option<SystemCallback>; MAX_SYSTEMS],
}

thread_local! {
    static ENGINE: RefCell<Engine> = const {
        RefCell::new(Engine {
            devices: [null_mut(); MAX_DEVICES],
            current: 0,
            listed: 1,
            systems: [None; MAX_SYSTEMS],
        })
    };
    /// A failure a device written in Rust reported from inside a callback.
    static FAILURE: RefCell<Option<Unwind>> = const { RefCell::new(None) };
}

fn with_engine<T>(action: impl FnOnce(&mut Engine) -> T) -> T {
    ENGINE.with(|engine| action(&mut engine.borrow_mut()))
}

/// Reports an error from a device callback written in Rust. The engine raises it once the
/// callback returns.
pub fn fail(message: impl Into<String>) {
    fail_with(error(message));
}

/// Like [`fail`], for a callback that has an R error to pass on.
pub fn fail_with(unwind: Unwind) {
    FAILURE.with(|failure| {
        failure.borrow_mut().get_or_insert(unwind);
    });
}

/// Runs a call into a device or a system under the host's native boundary.
pub(crate) unsafe fn guarded<T>(body: impl FnOnce() -> T) -> R<T> {
    let mut body = Some(body);
    let mut result = None;
    (interp().hooks.guard_native)(&mut || {
        if let Some(body) = body.take() {
            result = Some(body());
        }
    })?;
    if let Some(unwind) = FAILURE.with(|failure| failure.borrow_mut().take()) {
        return Err(unwind);
    }
    result.ok_or_else(|| error("a graphics device callback did not run"))
}

/// Like [`guarded`] for a callback that returns an R object, which is rooted at once because
/// the native boundary releases the handles that kept it alive.
pub(crate) unsafe fn guarded_sexp(body: impl FnOnce() -> Option<Sexp>) -> R<Sexp> {
    let value = guarded(body)?.unwrap_or_else(nil);
    crate::heap().root(value);
    Ok(value)
}

/// Marks what the engine keeps alive: every device's display list and event environment.
pub unsafe fn trace(marker: &mut Marker<'_>) {
    ENGINE.with(|engine| {
        let Ok(engine) = engine.try_borrow() else {
            return;
        };
        for &device in engine.devices.iter().filter(|device| !device.is_null()) {
            for &object in (*device).ops.iter().chain(&(*device).args) {
                marker.mark(object);
            }
            let dev = (*device).ge.dev;
            if !dev.is_null()
                && let Some(env) = (*dev).event_env
            {
                marker.mark(env);
            }
        }
    });
}

// The device table.

/// `GEcurrentDevice`: the current device, opening `getOption("device")` when only the null
/// device exists.
pub unsafe fn current_device() -> R<*mut GEDevDesc> {
    if no_devices() {
        open_default_device()?;
    }
    let device = with_engine(|engine| engine.devices[engine.current]);
    if device.is_null() {
        return Err(error("no active or default device"));
    }
    Ok(device.cast())
}

/// `GEgetDevice`: the device in slot `number` (counting the null device as 0), or null.
pub fn device(number: usize) -> *mut GEDevDesc {
    with_engine(|engine| engine.devices.get(number).copied().unwrap_or(null_mut())).cast()
}

/// `GEdeviceNumber`: the slot of `dd`, or 0 when it is not in the table.
pub fn device_number(dd: *const GEDevDesc) -> usize {
    with_engine(|engine| {
        (1..MAX_DEVICES)
            .find(|slot| ptr::eq(engine.devices[*slot].cast::<GEDevDesc>(), dd))
            .unwrap_or(0)
    })
}

/// `Rf_desc2GEDesc`: the engine device that wraps `dev`, or the first device when none does.
pub fn desc_to_device(dev: *const DevDesc) -> *mut GEDevDesc {
    with_engine(|engine| {
        let found = engine.devices.iter().find(|device| {
            // SAFETY: the table holds live devices.
            !device.is_null() && ptr::eq(unsafe { (***device).ge.dev }, dev)
        });
        found
            .copied()
            .unwrap_or(engine.devices[1])
            .cast::<GEDevDesc>()
    })
    .cast()
}

pub fn current_number() -> usize {
    with_engine(|engine| engine.current)
}

pub fn no_devices() -> bool {
    with_engine(|engine| engine.devices[1..].iter().all(|device| device.is_null()))
}

/// `Rf_NumDevices`: the open devices, counting the null device.
pub fn num_devices() -> usize {
    with_engine(|engine| {
        1 + engine.devices[1..]
            .iter()
            .filter(|device| !device.is_null())
            .count()
    })
}

/// The next open device after slot `from`, wrapping around, or the null device.
pub fn next_device(from: usize) -> usize {
    with_engine(|engine| {
        // The wrap includes `from` itself, so a lone open device is its own successor.
        (from + 1..MAX_DEVICES)
            .chain(1..(from + 1).min(MAX_DEVICES))
            .find(|slot| !engine.devices[*slot].is_null())
            .unwrap_or(0)
    })
}

pub fn previous_device(from: usize) -> usize {
    with_engine(|engine| {
        (1..from.min(MAX_DEVICES))
            .rev()
            .chain((from.max(1)..MAX_DEVICES).rev())
            .find(|slot| !engine.devices[*slot].is_null())
            .unwrap_or(0)
    })
}

/// `R_CheckDeviceAvailable`: fails when the table is full, before a device allocates anything.
pub fn check_device_available() -> R<()> {
    if device_available() {
        Ok(())
    } else {
        Err(error("too many open devices"))
    }
}

pub fn device_available() -> bool {
    num_devices() < MAX_DEVICES - 1
}

/// Calls `getOption("device")`, which is a device function or the name of one.
unsafe fn open_default_device() -> R<()> {
    let option = crate::envir::get_option("device");
    let function = if values::is_function(option) {
        option
    } else if option.kind() == SexpType::String
        && let Some(name) = values::first_string(option)
    {
        let lookup = values::call(values::install("match.fun")?, &[scalar_string(&name)?])?;
        crate::heap().root(lookup);
        let found = eval::eval(lookup, values::global_env())?;
        if !values::is_function(found) {
            return Err(error(
                "no active device and default getOption(\"device\") is invalid",
            ));
        }
        found
    } else {
        return Err(error("no active or default device"));
    };
    crate::heap().root(function);
    eval::apply(nil(), function, &[], values::global_env())?;
    if no_devices() {
        return Err(error("no active or default device"));
    }
    Ok(())
}

/// `GEcreateDevDesc`: wraps a device description in an engine device that is not yet in the
/// table.
pub unsafe fn create_device(dev: *mut DevDesc) -> R<*mut GEDevDesc> {
    let ask = values::as_logical_scalar(crate::envir::get_option("device.ask.default")) == 1;
    (*dev).event_env = None;
    let device = Box::new(EngineDevice {
        ge: GEDevDesc {
            dev,
            display_list_on: (*dev).display_list_on,
            display_list: nil(),
            dl_last_elt: nil(),
            saved_snapshot: nil(),
            dirty: 0,
            record_graphics: 1,
            lock: 0,
            gesd: [null_mut(); MAX_SYSTEMS],
            ask: c_int::from(ask),
            appending: 0,
        },
        name: String::new(),
        file: None,
        ops: Vec::new(),
        args: Vec::new(),
    });
    let dd = Box::into_raw(device);
    sync_ask(dd.cast());
    Ok(dd.cast())
}

/// Mirrors `ask` where a package compiled against GNU R 4.3 reads it.
unsafe fn sync_ask(dd: *mut GEDevDesc) {
    let slot = ptr::addr_of_mut!((*dd).gesd[MAX_SYSTEMS - 1]).cast::<c_int>();
    slot.write((*dd).ask);
}

pub unsafe fn set_ask(dd: *mut GEDevDesc, ask: bool) {
    (*dd).ask = c_int::from(ask);
    sync_ask(dd);
}

/// Reads `ask`, taking a package's write through either layout into account.
pub unsafe fn ask(dd: *mut GEDevDesc) -> bool {
    let mirror = ptr::addr_of!((*dd).gesd[MAX_SYSTEMS - 1])
        .cast::<c_int>()
        .read();
    if mirror != (*dd).ask {
        (*dd).ask = mirror;
    }
    (*dd).ask != 0
}

/// `GEaddDevice2f`: adds `dd` to the table as `name`, opened on `file`, makes it current, and
/// starts its display list.
pub unsafe fn add_device_named(dd: *mut GEDevDesc, name: &str, file: Option<&str>) -> R<()> {
    let device = dd.cast::<EngineDevice>();
    (*device).name = name.to_owned();
    (*device).file = file.map(str::to_owned);
    add_device(dd)?;
    init_display_list(dd)
}

/// `GEaddDevice`: adds `dd` under the name `.Device` holds, which an older device sets itself
/// before calling this.
pub unsafe fn add_device_from_dot_device(dd: *mut GEDevDesc) -> R<()> {
    let value = eval::find_var(values::symbols().dot_device, values::base_env())?;
    let device = dd.cast::<EngineDevice>();
    if (&(*device).name).is_empty() {
        (*device).name =
            values::first_string(value).map_or_else(String::new, |name| name.into_owned());
        let path = values::get_attr(value, values::install("filepath")?);
        (*device).file = values::first_string(path).map(|file| file.into_owned());
    }
    add_device(dd)
}

unsafe fn add_device(dd: *mut GEDevDesc) -> R<()> {
    if !no_devices() {
        let old = device(current_number());
        let deactivate = (*(*old).dev).deactivate;
        if let Some(deactivate) = deactivate {
            guarded(|| deactivate((*old).dev))?;
        }
    }
    let slot = with_engine(|engine| {
        let slot = (1..MAX_DEVICES).find(|slot| engine.devices[*slot].is_null())?;
        engine.devices[slot] = dd.cast();
        engine.current = slot;
        engine.listed = engine.listed.max(slot + 1);
        Some(slot)
    });
    let Some(slot) = slot else {
        destroy_device(dd)?;
        return Err(error("too many open devices"));
    };
    register_with_device(dd)?;
    if let Some(activate) = (*(*dd).dev).activate {
        guarded(|| activate((*dd).dev))?;
    }
    publish_devices()?;
    if slot == MAX_DEVICES - 1 {
        kill_device(slot)?;
        return Err(error("too many open devices"));
    }
    Ok(())
}

/// `selectDevice`: makes `number` current, or the next open device after it when it is not
/// open.
pub unsafe fn select_device(number: usize) -> R<usize> {
    let open = number < MAX_DEVICES && !device(number).is_null();
    if !open {
        let next = next_device(number);
        if next == 0 {
            with_engine(|engine| engine.current = 0);
            publish_devices()?;
            // GNU R asks for the current device here, which opens the default one, and still
            // reports the null device as selected.
            current_device()?;
            return Ok(0);
        }
        return select_device(next);
    }
    if !no_devices() {
        let old = device(current_number());
        if let Some(deactivate) = (*(*old).dev).deactivate {
            guarded(|| deactivate((*old).dev))?;
        }
    }
    with_engine(|engine| engine.current = number);
    publish_devices()?;
    let dd = device(number);
    if let Some(activate) = (*(*dd).dev).activate {
        guarded(|| activate((*dd).dev))?;
    }
    Ok(number)
}

/// `killDevice`: closes the device in `number`; the next open device becomes current when it
/// was.
pub unsafe fn kill_device(number: usize) -> R<()> {
    let dd = device(number);
    if number == 0 || dd.is_null() {
        return Ok(());
    }
    if (*dd).lock != 0 {
        return crate::conditions::warning("can't shut down a locked device");
    }
    remove_device(number, true)
}

/// `GEkillDevice`.
pub unsafe fn kill(dd: *mut GEDevDesc) -> R<()> {
    if !dd.is_null() && (*dd).lock != 0 {
        return crate::conditions::warning("can't shut down a locked device");
    }
    remove_device(device_number(dd), true)
}

unsafe fn remove_device(number: usize, find_next: bool) -> R<()> {
    let dd = device(number);
    if number == 0 || dd.is_null() {
        return Ok(());
    }
    let was_current = with_engine(|engine| {
        engine.devices[number] = null_mut();
        let was_current = engine.current == number;
        if was_current && find_next {
            engine.current = (number + 1..MAX_DEVICES)
                .chain(1..number)
                .find(|slot| !engine.devices[*slot].is_null())
                .unwrap_or(0);
        } else if was_current {
            engine.current = 0;
        }
        was_current
    });
    publish_devices()?;
    let closing = (|| -> R<()> {
        if was_current && find_next && !no_devices() {
            let next = device(current_number());
            if let Some(activate) = (*(*next).dev).activate {
                guarded(|| activate((*next).dev))?;
            }
        }
        if let Some(close) = (*(*dd).dev).close {
            guarded(|| close((*dd).dev))?;
        }
        Ok(())
    })();
    destroy_device(dd)?;
    closing
}

/// `KillAllDevices`: closes every device, the highest slot first.
pub unsafe fn kill_all_devices() -> R<()> {
    let mut first_failure = None;
    for number in (1..MAX_DEVICES).rev() {
        if let Err(unwind) = remove_device(number, false)
            && first_failure.is_none()
        {
            first_failure = Some(unwind);
        }
    }
    with_engine(|engine| engine.current = 0);
    publish_devices()?;
    first_failure.map_or(Ok(()), Err)
}

/// `GEdestroyDevDesc`: lets every system free its state, then frees the device.
pub unsafe fn destroy_device(dd: *mut GEDevDesc) -> R<()> {
    if dd.is_null() {
        return Ok(());
    }
    let mut outcome = Ok(());
    for index in system_indices() {
        let descriptor = (*dd).gesd[index];
        if descriptor.is_null() {
            continue;
        }
        if let Some(callback) = (*descriptor).callback {
            let result = guarded(|| callback(Event::FinaliseState as c_int, dd, nil()));
            if outcome.is_ok()
                && let Err(unwind) = result
            {
                outcome = Err(unwind);
            }
        }
        libc::free(descriptor.cast());
        (*dd).gesd[index] = null_mut();
        (*dd).gesd[index - 1] = null_mut();
    }
    libc::free((*dd).dev.cast());
    drop(Box::from_raw(dd.cast::<EngineDevice>()));
    outcome
}

/// Rebinds `.Devices` and `.Device` in base. `.Devices` is a pairlist of device names, and the
/// name of a device opened on a file carries the file as its `filepath` attribute.
pub unsafe fn publish_devices() -> R<()> {
    let (entries, current) = with_engine(|engine| {
        let entries: Vec<(String, Option<String>)> = (0..engine.listed)
            .map(|slot| {
                let device = engine.devices[slot];
                if slot == 0 {
                    (NULL_DEVICE_NAME.to_owned(), None)
                } else if device.is_null() {
                    (String::new(), None)
                } else {
                    // SAFETY: the table holds live devices.
                    unsafe { ((*device).name.clone(), (*device).file.clone()) }
                }
            })
            .collect();
        (entries, engine.current)
    });
    let mut list = nil();
    let mut current_entry = nil();
    for (slot, (name, file)) in entries.iter().enumerate().rev() {
        let entry = scalar_string(name)?;
        crate::heap().root(entry);
        if let Some(file) = file {
            values::set_attr(entry, values::install("filepath")?, scalar_string(file)?)?;
        }
        if slot == current {
            current_entry = entry;
        }
        list = values::cons(entry, list)?;
        crate::heap().root(list);
    }
    if current_entry == nil() {
        current_entry = scalar_string(NULL_DEVICE_NAME)?;
    }
    let base = values::base_env();
    eval::define(values::install(".Devices")?, list, base)?;
    eval::define(values::symbols().dot_device, current_entry, base)?;
    Ok(())
}

// Graphics systems.

/// The indices systems may hold: odd ones, below the slot that mirrors `ask`.
fn system_indices() -> impl Iterator<Item = usize> {
    (1..MAX_SYSTEMS - 1).step_by(2)
}

fn registered_systems() -> Vec<(usize, SystemCallback)> {
    with_engine(|engine| {
        system_indices()
            .filter_map(|index| engine.systems[index].map(|callback| (index, callback)))
            .collect()
    })
}

unsafe fn register_one(dd: *mut GEDevDesc, index: usize, callback: SystemCallback) -> R<()> {
    let descriptor = libc::calloc(1, size_of::<GESystemDesc>()).cast::<GESystemDesc>();
    if descriptor.is_null() {
        return Err(error("unable to allocate memory (in GEregister)"));
    }
    (*descriptor).callback = Some(callback);
    (*dd).gesd[index] = descriptor;
    (*dd).gesd[index - 1] = descriptor;
    let state = guarded(|| callback(Event::InitState as c_int, dd, nil()))?;
    if state.is_none_or(|state| state == nil()) {
        libc::free(descriptor.cast());
        (*dd).gesd[index] = null_mut();
        (*dd).gesd[index - 1] = null_mut();
        return Err(error("unable to allocate memory (in GEregister)"));
    }
    Ok(())
}

pub unsafe fn register_with_device(dd: *mut GEDevDesc) -> R<()> {
    for (index, callback) in registered_systems() {
        register_one(dd, index, callback)?;
    }
    Ok(())
}

fn open_devices() -> Vec<*mut GEDevDesc> {
    with_engine(|engine| {
        engine.devices[1..]
            .iter()
            .filter(|device| !device.is_null())
            .map(|device| device.cast())
            .collect()
    })
}

/// `GEregisterSystem`: registers `callback` with every open device and with each device opened
/// later, and returns its index.
pub unsafe fn register_system(callback: SystemCallback) -> R<usize> {
    let index =
        with_engine(|engine| system_indices().find(|index| engine.systems[*index].is_none()))
            .ok_or_else(|| error("too many graphics systems registered"))?;
    for dd in open_devices() {
        register_one(dd, index, callback)?;
    }
    with_engine(|engine| engine.systems[index] = Some(callback));
    Ok(index)
}

/// `GEunregisterSystem`.
pub unsafe fn unregister_system(index: c_int) -> R<()> {
    let Ok(index) = usize::try_from(index) else {
        return Ok(());
    };
    if registered_systems().is_empty() {
        return crate::conditions::warning("no graphics system to unregister");
    }
    if index >= MAX_SYSTEMS || index == 0 {
        return Ok(());
    }
    for dd in open_devices() {
        let descriptor = (*dd).gesd[index];
        if descriptor.is_null() {
            continue;
        }
        if let Some(callback) = (*descriptor).callback {
            guarded(|| callback(Event::FinaliseState as c_int, dd, nil()))?;
        }
        libc::free(descriptor.cast());
        (*dd).gesd[index] = null_mut();
        (*dd).gesd[index - 1] = null_mut();
    }
    with_engine(|engine| engine.systems[index] = None);
    Ok(())
}

/// `GEsystemState`: the state system `index` keeps on `dd`.
pub unsafe fn system_state(dd: *mut GEDevDesc, index: usize) -> *mut c_void {
    let descriptor = (*dd).gesd.get(index).copied().unwrap_or(null_mut());
    if descriptor.is_null() {
        null_mut()
    } else {
        (*descriptor).system_specific
    }
}

/// Calls every system registered with `dd` about `event`, returning each result in index order.
unsafe fn notify_systems(dd: *mut GEDevDesc, event: Event, data: Sexp) -> R<Vec<Sexp>> {
    let mut results = Vec::new();
    for index in system_indices() {
        let descriptor = (*dd).gesd[index];
        if descriptor.is_null() {
            continue;
        }
        if let Some(callback) = (*descriptor).callback {
            results.push(guarded_sexp(|| callback(event as c_int, dd, data))?);
        }
    }
    Ok(results)
}

/// `GEhandleEvent`: passes `event` to every system on the device that `dev` describes.
pub unsafe fn handle_event(event: c_int, dev: *mut DevDesc, data: Sexp) -> R<Sexp> {
    let dd = desc_to_device(dev);
    if dd.is_null() {
        return Ok(nil());
    }
    for index in system_indices() {
        let descriptor = (*dd).gesd[index];
        if !descriptor.is_null()
            && let Some(callback) = (*descriptor).callback
        {
            guarded_sexp(|| callback(event, dd, data))?;
        }
    }
    Ok(nil())
}

// Units.

/// `GEUnit`: device units, the device as the unit square, inches, and centimetres.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Device,
    Ndc,
    Inches,
    Cm,
}

impl Unit {
    pub fn from_code(code: c_int) -> Unit {
        match code {
            1 => Unit::Ndc,
            2 => Unit::Inches,
            3 => Unit::Cm,
            _ => Unit::Device,
        }
    }
}

unsafe fn extent(dd: *mut GEDevDesc, vertical: bool) -> (f64, f64, f64) {
    let dev = &*(*dd).dev;
    if vertical {
        (dev.bottom, dev.top, dev.ipr[1])
    } else {
        (dev.left, dev.right, dev.ipr[0])
    }
}

unsafe fn from_device_position(value: f64, to: Unit, dd: *mut GEDevDesc, vertical: bool) -> f64 {
    let (low, high, ipr) = extent(dd, vertical);
    let fraction = (value - low) / (high - low);
    match to {
        Unit::Device => value,
        Unit::Ndc => fraction,
        Unit::Inches => fraction * (high - low).abs() * ipr,
        Unit::Cm => fraction * (high - low).abs() * ipr * 2.54,
    }
}

unsafe fn to_device_position(value: f64, from: Unit, dd: *mut GEDevDesc, vertical: bool) -> f64 {
    let (low, high, ipr) = extent(dd, vertical);
    let ndc = match from {
        Unit::Device => return value,
        Unit::Ndc => value,
        Unit::Inches => value / ipr / (high - low).abs(),
        Unit::Cm => value / 2.54 / ipr / (high - low).abs(),
    };
    low + ndc * (high - low)
}

unsafe fn from_device_size(value: f64, to: Unit, dd: *mut GEDevDesc, vertical: bool) -> f64 {
    let (low, high, ipr) = extent(dd, vertical);
    let ndc = value / (high - low);
    match to {
        Unit::Device => value,
        Unit::Ndc => ndc,
        Unit::Inches => ndc * (high - low).abs() * ipr,
        Unit::Cm => ndc * (high - low).abs() * ipr * 2.54,
    }
}

unsafe fn to_device_size(value: f64, from: Unit, dd: *mut GEDevDesc, vertical: bool) -> f64 {
    let (low, high, ipr) = extent(dd, vertical);
    let ndc = match from {
        Unit::Device => return value,
        Unit::Ndc => value,
        Unit::Inches => value / ipr / (high - low).abs(),
        Unit::Cm => value / 2.54 / ipr / (high - low).abs(),
    };
    ndc * (high - low)
}

pub unsafe fn from_device_x(value: f64, to: Unit, dd: *mut GEDevDesc) -> f64 {
    from_device_position(value, to, dd, false)
}

pub unsafe fn from_device_y(value: f64, to: Unit, dd: *mut GEDevDesc) -> f64 {
    from_device_position(value, to, dd, true)
}

pub unsafe fn to_device_x(value: f64, from: Unit, dd: *mut GEDevDesc) -> f64 {
    to_device_position(value, from, dd, false)
}

pub unsafe fn to_device_y(value: f64, from: Unit, dd: *mut GEDevDesc) -> f64 {
    to_device_position(value, from, dd, true)
}

pub unsafe fn from_device_width(value: f64, to: Unit, dd: *mut GEDevDesc) -> f64 {
    from_device_size(value, to, dd, false)
}

pub unsafe fn from_device_height(value: f64, to: Unit, dd: *mut GEDevDesc) -> f64 {
    from_device_size(value, to, dd, true)
}

pub unsafe fn to_device_width(value: f64, from: Unit, dd: *mut GEDevDesc) -> f64 {
    to_device_size(value, from, dd, false)
}

pub unsafe fn to_device_height(value: f64, from: Unit, dd: *mut GEDevDesc) -> f64 {
    to_device_size(value, from, dd, true)
}

// Line parameters.

/// The line type names and their dash patterns: four bits per dash length, the first dash in
/// the low bits.
pub const LINE_TYPES: [(&str, c_int); 7] = [
    ("blank", LTY_BLANK),
    ("solid", 0),
    ("dashed", 0x44),
    ("dotted", 0x31),
    ("dotdash", 0x3431),
    ("longdash", 0x37),
    ("twodash", 0x2262),
];

/// The line end and join names with their engine codes. A number picks from the first two
/// only, because GNU R counts its tables without their last entry.
const LINE_ENDS: [(&str, c_int); 3] = [("round", 1), ("butt", 2), ("square", 3)];
const LINE_JOINS: [(&str, c_int); 3] = [("round", 1), ("mitre", 2), ("bevel", 3)];

/// Reads element `index` of a parameter that names a table entry or numbers one, wrapping
/// numbers past the first `count` entries back to 1.
unsafe fn table_par(
    value: Sexp,
    index: usize,
    table: &[(&str, c_int)],
    count: usize,
    message: &str,
) -> R<Option<c_int>> {
    let code = match value.kind() {
        SexpType::String => {
            let name = values::string_at(value, index).unwrap_or(Cow::Borrowed("NA"));
            return Ok(table
                .iter()
                .find(|(entry, _)| *entry == name)
                .map(|(_, code)| *code));
        }
        SexpType::Integer => {
            let code = values::ints(value)
                .get(index)
                .copied()
                .unwrap_or(NA_INTEGER);
            if code == NA_INTEGER || code < 0 {
                return Err(error(message));
            }
            code as usize
        }
        SexpType::Real => {
            let code = values::reals(value).get(index).copied().unwrap_or(f64::NAN);
            if !code.is_finite() || code < 0.0 {
                return Err(error(message));
            }
            code as usize
        }
        _ => return Err(error(message)),
    };
    let slot = if code > 0 { (code - 1) % count + 1 } else { 0 };
    Ok(table.get(slot).map(|(_, code)| *code))
}

/// `GE_LENDpar`.
pub unsafe fn lend_par(value: Sexp, index: usize) -> R<c_int> {
    table_par(value, index, &LINE_ENDS, 2, "invalid line end")?
        .ok_or_else(|| error("invalid line end"))
}

/// `GE_LJOINpar`.
pub unsafe fn ljoin_par(value: Sexp, index: usize) -> R<c_int> {
    table_par(value, index, &LINE_JOINS, 2, "invalid line join")?
        .ok_or_else(|| error("invalid line join"))
}

pub fn lend_name(lend: c_int) -> R<&'static str> {
    LINE_ENDS
        .iter()
        .find(|(_, code)| *code == lend)
        .map(|(name, _)| *name)
        .ok_or_else(|| error("invalid line end"))
}

pub fn ljoin_name(ljoin: c_int) -> R<&'static str> {
    LINE_JOINS
        .iter()
        .find(|(_, code)| *code == ljoin)
        .map(|(name, _)| *name)
        .ok_or_else(|| error("invalid line join"))
}

/// `GE_LTYpar`: a line type name, a string of two to eight hex dash lengths, or a number
/// that picks a named type.
pub unsafe fn lty_par(value: Sexp, index: usize) -> R<c_int> {
    if value.kind() == SexpType::String {
        let text = values::string_at(value, index).unwrap_or(Cow::Borrowed("NA"));
        if let Some((_, code)) = LINE_TYPES.iter().find(|(name, _)| **name == *text) {
            return Ok(*code);
        }
        return lty_from_hex(&text);
    }
    table_par(value, index, &LINE_TYPES, 6, "invalid line type")?
        .ok_or_else(|| error("invalid line type"))
}

fn lty_from_hex(text: &str) -> R<c_int> {
    if !(2..=8).contains(&text.len()) || text.len() % 2 == 1 {
        return Err(error("invalid line type: must be length 2, 4, 6 or 8"));
    }
    let mut code: u32 = 0;
    for (position, character) in text.chars().enumerate() {
        let digit = character
            .to_digit(16)
            .ok_or_else(|| error("invalid hex digit in 'color' or 'lty'"))?;
        if digit == 0 {
            return Err(error("invalid line type: zeroes are not allowed"));
        }
        code |= digit << (4 * position);
    }
    Ok(code as c_int)
}

/// `GE_LTYget`: the name of a line type, or its dash lengths as hex digits.
pub fn lty_name(lty: c_int) -> String {
    if let Some((name, _)) = LINE_TYPES.iter().find(|(_, code)| *code == lty) {
        return (*name).to_owned();
    }
    let mut dashes = lty as u32;
    let mut text = String::new();
    while text.len() < 8 && dashes & 15 != 0 {
        text.push(
            char::from_digit(dashes & 15, 16)
                .unwrap_or('0')
                .to_ascii_uppercase(),
        );
        dashes >>= 4;
    }
    text
}

/// `GEstring_to_pch`: a one-character plotting symbol as its code, a non-ASCII character as
/// its negated code point.
pub fn string_to_pch(text: Option<&str>) -> R<c_int> {
    let Some(text) = text else {
        return Ok(NA_INTEGER);
    };
    let Some(first) = text.chars().next() else {
        return Ok(NA_INTEGER);
    };
    if first.is_ascii() {
        Ok(first as c_int)
    } else {
        Ok(-(first as c_int))
    }
}

// Clipping.

#[derive(Clone, Copy)]
struct ClipRect {
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

/// The clipping region the last `GESetClip` set, in increasing order.
unsafe fn clip_rect(dd: *mut GEDevDesc) -> ClipRect {
    let dev = &*(*dd).dev;
    ClipRect {
        xmin: dev.clip_left.min(dev.clip_right),
        xmax: dev.clip_left.max(dev.clip_right),
        ymin: dev.clip_bottom.min(dev.clip_top),
        ymax: dev.clip_bottom.max(dev.clip_top),
    }
}

/// The device extent grown by four times its larger side: clipping at the real edge leaves
/// artefacts, and a device that clips itself only needs coordinates kept within reason.
unsafe fn device_rect(dd: *mut GEDevDesc) -> ClipRect {
    let dev = &*(*dd).dev;
    let (xmin, xmax) = (dev.left.min(dev.right), dev.left.max(dev.right));
    let (ymin, ymax) = (dev.bottom.min(dev.top), dev.bottom.max(dev.top));
    let margin = (4.0 * (xmax - xmin)).max(4.0 * (ymax - ymin));
    ClipRect {
        xmin: xmin - margin,
        xmax: xmax + margin,
        ymin: ymin - margin,
        ymax: ymax + margin,
    }
}

unsafe fn region(to_device: bool, dd: *mut GEDevDesc) -> ClipRect {
    if to_device {
        device_rect(dd)
    } else {
        clip_rect(dd)
    }
}

/// `GESetClip`: clips to the rectangle, trimmed to the device.
pub unsafe fn set_clip(
    mut x1: f64,
    mut y1: f64,
    mut x2: f64,
    mut y2: f64,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    let (left, right, bottom, top) = ((*dev).left, (*dev).right, (*dev).bottom, (*dev).top);
    if left <= right {
        (x1, x2) = (x1.max(left), x2.min(right));
    } else {
        (x1, x2) = (x1.min(left), x2.max(right));
    }
    if bottom <= top {
        (y1, y2) = (y1.max(bottom), y2.min(top));
    } else {
        (y1, y2) = (y1.min(bottom), y2.max(top));
    }
    if let Some(clip) = (*dev).clip {
        guarded(|| clip(x1, x2, y1, y2, dev))?;
    }
    (*dev).clip_left = x1.min(x2);
    (*dev).clip_right = x1.max(x2);
    (*dev).clip_top = y1.max(y2);
    (*dev).clip_bottom = y1.min(y2);
    Ok(())
}

const CLIP_BOTTOM: u8 = 1;
const CLIP_LEFT: u8 = 2;
const CLIP_TOP: u8 = 4;
const CLIP_RIGHT: u8 = 8;

fn clip_code(x: f64, y: f64, rect: &ClipRect) -> u8 {
    let mut code = 0;
    if x < rect.xmin {
        code |= CLIP_LEFT;
    } else if x > rect.xmax {
        code |= CLIP_RIGHT;
    }
    if y < rect.ymin {
        code |= CLIP_BOTTOM;
    } else if y > rect.ymax {
        code |= CLIP_TOP;
    }
    code
}

/// Cohen-Sutherland clipping of one segment. Returns whether any of it is visible, and which
/// ends were moved.
fn clip_segment(segment: &mut [f64; 4], rect: &ClipRect) -> Option<(bool, bool)> {
    let [x1, y1, x2, y2] = segment;
    let mut code1 = clip_code(*x1, *y1, rect);
    let mut code2 = clip_code(*x2, *y2, rect);
    let (mut moved1, mut moved2) = (false, false);
    while code1 != 0 || code2 != 0 {
        if code1 & code2 != 0 {
            return None;
        }
        let code = if code1 != 0 { code1 } else { code2 };
        let (x, y) = if code & CLIP_LEFT != 0 {
            (
                rect.xmin,
                *y1 + (*y2 - *y1) * (rect.xmin - *x1) / (*x2 - *x1),
            )
        } else if code & CLIP_RIGHT != 0 {
            (
                rect.xmax,
                *y1 + (*y2 - *y1) * (rect.xmax - *x1) / (*x2 - *x1),
            )
        } else if code & CLIP_BOTTOM != 0 {
            (
                *x1 + (*x2 - *x1) * (rect.ymin - *y1) / (*y2 - *y1),
                rect.ymin,
            )
        } else {
            (
                *x1 + (*x2 - *x1) * (rect.ymax - *y1) / (*y2 - *y1),
                rect.ymax,
            )
        };
        if code == code1 {
            (*x1, *y1, moved1) = (x, y, true);
            code1 = clip_code(x, y, rect);
        } else {
            (*x2, *y2, moved2) = (x, y, true);
            code2 = clip_code(x, y, rect);
        }
    }
    Some((moved1, moved2))
}

/// Clips a polyline segment by segment and draws each visible run as its own polyline.
unsafe fn clip_polyline(
    x: &[f64],
    y: &[f64],
    gc: &GEContext,
    to_device: bool,
    dd: *mut GEDevDesc,
) -> R<()> {
    let rect = region(to_device, dd);
    let Some(polyline) = (*(*dd).dev).polyline else {
        return Ok(());
    };
    let dev = (*dd).dev;
    let n = x.len().min(y.len());
    if n == 0 {
        return Ok(());
    }
    let mut run_x = vec![x[0]];
    let mut run_y = vec![y[0]];
    let draw = |run_x: &mut Vec<f64>, run_y: &mut Vec<f64>| -> R<()> {
        let count = run_x.len() as c_int;
        guarded(|| polyline(count, run_x.as_mut_ptr(), run_y.as_mut_ptr(), gc, dev))
    };
    for index in 1..n {
        let mut segment = [x[index - 1], y[index - 1], x[index], y[index]];
        if let Some((moved1, moved2)) = clip_segment(&mut segment, &rect) {
            let [x1, y1, x2, y2] = segment;
            if moved1 && moved2 {
                draw(&mut vec![x1, x2], &mut vec![y1, y2])?;
            } else if moved1 {
                run_x = vec![x1, x2];
                run_y = vec![y1, y2];
                if index == n - 1 {
                    draw(&mut run_x, &mut run_y)?;
                }
            } else if moved2 {
                run_x.push(x2);
                run_y.push(y2);
                if run_x.len() > 1 {
                    draw(&mut run_x, &mut run_y)?;
                }
            } else {
                run_x.push(x2);
                run_y.push(y2);
                if index == n - 1 && run_x.len() > 1 {
                    draw(&mut run_x, &mut run_y)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Edge {
    Left,
    Right,
    Bottom,
    Top,
}

const EDGES: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Bottom, Edge::Top];

#[derive(Clone, Copy, Default)]
struct EdgeState {
    first: Option<(f64, f64)>,
    last: (f64, f64),
}

fn inside(edge: Edge, x: f64, y: f64, rect: &ClipRect) -> bool {
    match edge {
        Edge::Left => x >= rect.xmin,
        Edge::Right => x <= rect.xmax,
        Edge::Bottom => y >= rect.ymin,
        Edge::Top => y <= rect.ymax,
    }
}

fn intersect(edge: Edge, x1: f64, y1: f64, x2: f64, y2: f64, rect: &ClipRect) -> (f64, f64) {
    let slope = if x1 != x2 { (y1 - y2) / (x1 - x2) } else { 0.0 };
    match edge {
        Edge::Left => (rect.xmin, y2 + (rect.xmin - x2) * slope),
        Edge::Right => (rect.xmax, y2 + (rect.xmax - x2) * slope),
        Edge::Bottom => (
            if x1 != x2 {
                x2 + (rect.ymin - y2) / slope
            } else {
                x2
            },
            rect.ymin,
        ),
        Edge::Top => (
            if x1 != x2 {
                x2 + (rect.ymax - y2) / slope
            } else {
                x2
            },
            rect.ymax,
        ),
    }
}

/// One step of Sutherland-Hodgman clipping: passes the point through edge `edge` and on to
/// the next edges.
fn clip_point(
    edge: usize,
    x: f64,
    y: f64,
    out: &mut Vec<(f64, f64)>,
    rect: &ClipRect,
    states: &mut [EdgeState; 4],
) {
    let side = EDGES[edge];
    match states[edge].first {
        None => states[edge].first = Some((x, y)),
        Some(_) => {
            let (lx, ly) = states[edge].last;
            if inside(side, x, y, rect) != inside(side, lx, ly, rect) {
                let (ix, iy) = intersect(side, x, y, lx, ly, rect);
                if edge < 3 {
                    clip_point(edge + 1, ix, iy, out, rect, states);
                } else {
                    out.push((ix, iy));
                }
            }
        }
    }
    states[edge].last = (x, y);
    if inside(side, x, y, rect) {
        if edge < 3 {
            clip_point(edge + 1, x, y, out, rect, states);
        } else {
            out.push((x, y));
        }
    }
}

fn close_clip(out: &mut Vec<(f64, f64)>, rect: &ClipRect, states: &mut [EdgeState; 4]) {
    for edge in 0..4 {
        let side = EDGES[edge];
        let Some((fx, fy)) = states[edge].first else {
            continue;
        };
        let (lx, ly) = states[edge].last;
        if inside(side, lx, ly, rect) != inside(side, fx, fy, rect) {
            let (ix, iy) = intersect(side, lx, ly, fx, fy, rect);
            if edge < 3 {
                clip_point(edge + 1, ix, iy, out, rect, states);
            } else {
                out.push((ix, iy));
            }
        }
    }
}

fn clip_poly(x: &[f64], y: &[f64], rect: &ClipRect) -> (Vec<f64>, Vec<f64>) {
    let mut states = [EdgeState::default(); 4];
    let mut out = Vec::new();
    for (&px, &py) in x.iter().zip(y) {
        clip_point(0, px, py, &mut out, rect, &mut states);
    }
    close_clip(&mut out, rect, &mut states);
    out.into_iter().unzip()
}

fn bounds(x: &[f64], y: &[f64]) -> ClipRect {
    // GNU R starts the maxima at the smallest positive double, not at minus infinity.
    let mut rect = ClipRect {
        xmin: f64::MAX,
        xmax: f64::MIN_POSITIVE,
        ymin: f64::MAX,
        ymax: f64::MIN_POSITIVE,
    };
    for (&px, &py) in x.iter().zip(y) {
        rect.xmin = rect.xmin.min(px);
        rect.xmax = rect.xmax.max(px);
        rect.ymin = rect.ymin.min(py);
        rect.ymax = rect.ymax.max(py);
    }
    rect
}

fn must_clip(shape: &ClipRect, rect: &ClipRect) -> bool {
    rect.xmin > shape.xmin
        || rect.xmax < shape.xmax
        || rect.ymin > shape.ymin
        || rect.ymax < shape.ymax
}

/// Rotates a closed outline (whose last point repeats the first) so that it starts outside
/// the clipping region, which keeps the clipped outline from breaking at its first vertex.
unsafe fn reorder_vertices(x: &mut [f64], y: &mut [f64], dd: *mut GEDevDesc) -> R<()> {
    let rect = clip_rect(dd);
    let n = x.len() - 1;
    let within = |index: usize| {
        x[index] >= rect.xmin
            && x[index] <= rect.xmax
            && y[index] >= rect.ymin
            && y[index] <= rect.ymax
    };
    if n < 2 || !within(0) {
        return Ok(());
    }
    let start = (1..n)
        .find(|index| !within(*index))
        .ok_or_else(|| error("Clipping polygon that does not need clipping"))?;
    let (old_x, old_y) = (x[..n].to_vec(), y[..n].to_vec());
    for index in 0..=n {
        x[index] = old_x[(start + index) % n];
        y[index] = old_y[(start + index) % n];
    }
    Ok(())
}

unsafe fn device_polygon(
    x: &mut [f64],
    y: &mut [f64],
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    if let Some(polygon) = (*dev).polygon {
        let count = x.len() as c_int;
        guarded(|| polygon(count, x.as_mut_ptr(), y.as_mut_ptr(), gc, dev))?;
    }
    Ok(())
}

unsafe fn clip_polygon(
    x: &[f64],
    y: &[f64],
    gc: &mut GEContext,
    to_device: bool,
    dd: *mut GEDevDesc,
) -> R<()> {
    let mut closed_x: Vec<f64> = x.iter().copied().chain(x.first().copied()).collect();
    let mut closed_y: Vec<f64> = y.iter().copied().chain(y.first().copied()).collect();
    let n = x.len();
    let transparent_fill =
        (gc.fill as u32) >> 24 == 0 && gc.pattern_fill.is_none_or(|fill| fill == nil());
    if transparent_fill && !to_device {
        // Without a fill, an outline clipped as a polygon would run along the clip border,
        // so it is drawn as a closed polyline instead.
        if must_clip(&bounds(x, y), &region(to_device, dd)) {
            reorder_vertices(&mut closed_x, &mut closed_y, dd)?;
            polyline(&closed_x, &closed_y, gc, dd)
        } else {
            device_polygon(&mut closed_x[..n], &mut closed_y[..n], gc, dd)
        }
    } else if to_device {
        let (mut clipped_x, mut clipped_y) = clip_poly(x, y, &region(true, dd));
        if clipped_x.len() > 1 {
            device_polygon(&mut clipped_x, &mut clipped_y, gc, dd)?;
        }
        Ok(())
    } else if must_clip(&bounds(x, y), &region(false, dd)) {
        let colour = gc.col;
        gc.col = TRANSPARENT_WHITE;
        let (mut clipped_x, mut clipped_y) = clip_poly(x, y, &region(false, dd));
        if clipped_x.len() > 1 {
            device_polygon(&mut clipped_x, &mut clipped_y, gc, dd)?;
        }
        gc.col = colour;
        gc.fill = TRANSPARENT_WHITE;
        reorder_vertices(&mut closed_x, &mut closed_y, dd)?;
        polyline(&closed_x, &closed_y, gc, dd)
    } else {
        device_polygon(&mut closed_x[..n], &mut closed_y[..n], gc, dd)
    }
}

// Drawing.

unsafe fn check_lwd(gc: &GEContext) -> R<()> {
    if gc.lwd == f64::INFINITY || gc.lwd < 0.0 {
        return Err(error("'lwd' must be non-negative and finite"));
    }
    Ok(())
}

fn invisible_line(gc: &GEContext) -> bool {
    gc.lwd.is_nan() || gc.lty == LTY_BLANK
}

unsafe fn device_clips(dd: *mut GEDevDesc) -> bool {
    let dev = &*(*dd).dev;
    dev.device_version >= VERSION_DEVICE_CLIP && dev.device_clip != 0
}

unsafe fn can_clip(dd: *mut GEDevDesc) -> bool {
    (*(*dd).dev).can_clip != 0
}

/// `GELine`.
pub unsafe fn line(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    check_lwd(gc)?;
    if invisible_line(gc) {
        return Ok(());
    }
    let dev = (*dd).dev;
    let Some(draw) = (*dev).line else {
        return Ok(());
    };
    let mut segment = [x1, y1, x2, y2];
    if !device_clips(dd) && clip_segment(&mut segment, &region(can_clip(dd), dd)).is_none() {
        return Ok(());
    }
    let [x1, y1, x2, y2] = segment;
    guarded(|| draw(x1, y1, x2, y2, gc, dev))
}

/// `GEPolyline`.
pub unsafe fn polyline(x: &[f64], y: &[f64], gc: &GEContext, dd: *mut GEDevDesc) -> R<()> {
    check_lwd(gc)?;
    if invisible_line(gc) {
        return Ok(());
    }
    if device_clips(dd) {
        let dev = (*dd).dev;
        if let Some(draw) = (*dev).polyline {
            let (mut x, mut y) = (x.to_vec(), y.to_vec());
            let count = x.len().min(y.len()) as c_int;
            guarded(|| draw(count, x.as_mut_ptr(), y.as_mut_ptr(), gc, dev))?;
        }
        return Ok(());
    }
    clip_polyline(x, y, gc, can_clip(dd), dd)
}

/// `GEPolygon`.
pub unsafe fn polygon(x: &[f64], y: &[f64], gc: &mut GEContext, dd: *mut GEDevDesc) -> R<()> {
    check_lwd(gc)?;
    if invisible_line(gc) {
        gc.col = TRANSPARENT_WHITE;
    }
    let n = x.len().min(y.len());
    let (x, y) = (&x[..n], &y[..n]);
    if device_clips(dd) {
        return device_polygon(&mut x.to_vec(), &mut y.to_vec(), gc, dd);
    }
    clip_polygon(x, y, gc, can_clip(dd), dd)
}

/// How a circle meets a region: wholly inside, wholly outside, or crossing it, in which case
/// the number of vertices of a polygon that stands in for it.
enum CircleClip {
    Inside,
    Outside,
    Crossing(usize),
}

fn circle_clip(x: f64, y: f64, r: f64, rect: &ClipRect) -> CircleClip {
    if x - r > rect.xmin && x + r < rect.xmax && y - r > rect.ymin && y + r < rect.ymax {
        return CircleClip::Inside;
    }
    let far = |cx: f64, cy: f64| (x - cx).powi(2) + (y - cy).powi(2) > r * r;
    if x - r > rect.xmax
        || x + r < rect.xmin
        || y - r > rect.ymax
        || y + r < rect.ymin
        || (x < rect.xmin && y < rect.ymin && far(rect.xmin, rect.ymin))
        || (x > rect.xmax && y < rect.ymin && far(rect.xmax, rect.ymin))
        || (x < rect.xmin && y > rect.ymax && far(rect.xmin, rect.ymax))
        || (x > rect.xmax && y > rect.ymax && far(rect.xmax, rect.ymax))
    {
        return CircleClip::Outside;
    }
    // Vertices close enough that each chord strays about one device unit from the arc.
    let vertices = if r <= 6.0 {
        10
    } else {
        (2.0 * std::f64::consts::PI / (1.0 - 1.0 / r).acos()) as usize
    };
    CircleClip::Crossing(vertices)
}

/// `GECircle`.
pub unsafe fn circle(x: f64, y: f64, radius: f64, gc: &mut GEContext, dd: *mut GEDevDesc) -> R<()> {
    if radius <= 0.0 {
        return Ok(());
    }
    check_lwd(gc)?;
    if invisible_line(gc) {
        gc.col = TRANSPARENT_WHITE;
    }
    let dev = (*dd).dev;
    let Some(draw) = (*dev).circle else {
        return Ok(());
    };
    let draw_circle = |gc: &GEContext| guarded(|| draw(x, y, radius, gc, dev));
    if device_clips(dd) {
        return draw_circle(gc);
    }
    match circle_clip(x, y, radius, &region(can_clip(dd), dd)) {
        CircleClip::Inside => draw_circle(gc),
        CircleClip::Outside => Ok(()),
        CircleClip::Crossing(_) if can_clip(dd) => draw_circle(gc),
        CircleClip::Crossing(vertices) => {
            let theta = 2.0 * std::f64::consts::PI / vertices as f64;
            let xs: Vec<f64> = (0..vertices)
                .map(|index| x + radius * (theta * index as f64).sin())
                .collect();
            let ys: Vec<f64> = (0..vertices)
                .map(|index| y + radius * (theta * index as f64).cos())
                .collect();
            polygon(&xs, &ys, gc, dd)
        }
    }
}

/// How a rectangle meets a region: 0 outside, 1 inside, 2 crossing.
fn rect_clip(x0: f64, y0: f64, x1: f64, y1: f64, rect: &ClipRect) -> u8 {
    if (x0 < rect.xmin && x1 < rect.xmin)
        || (x0 > rect.xmax && x1 > rect.xmax)
        || (y0 < rect.ymin && y1 < rect.ymin)
        || (y0 > rect.ymax && y1 > rect.ymax)
    {
        0
    } else if x0 > rect.xmin
        && x0 < rect.xmax
        && x1 > rect.xmin
        && x1 < rect.xmax
        && y0 > rect.ymin
        && y0 < rect.ymax
        && y1 > rect.ymin
        && y1 < rect.ymax
    {
        1
    } else {
        2
    }
}

/// `GERect`.
pub unsafe fn rect(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    gc: &mut GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    check_lwd(gc)?;
    if invisible_line(gc) {
        gc.col = TRANSPARENT_WHITE;
    }
    let dev = (*dd).dev;
    let Some(draw) = (*dev).rect else {
        return Ok(());
    };
    let draw_rect = |gc: &GEContext| guarded(|| draw(x0, y0, x1, y1, gc, dev));
    if device_clips(dd) {
        return draw_rect(gc);
    }
    match rect_clip(x0, y0, x1, y1, &region(can_clip(dd), dd)) {
        0 => Ok(()),
        1 => draw_rect(gc),
        _ if can_clip(dd) => draw_rect(gc),
        _ => polygon(&[x0, x0, x1, x1], &[y0, y1, y1, y0], gc, dd),
    }
}

/// `GEPath`: polygons that share one fill, `counts` giving each one's vertex count.
pub unsafe fn path(
    x: &[f64],
    y: &[f64],
    counts: &[c_int],
    winding: bool,
    gc: &mut GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    let Some(draw) = (*dev).path else {
        return crate::conditions::warning("path rendering is not implemented for this device");
    };
    check_lwd(gc)?;
    if invisible_line(gc) {
        gc.col = TRANSPARENT_WHITE;
    }
    if counts.is_empty() {
        return Ok(());
    }
    if counts.iter().any(|count| *count < 2) {
        return Err(error("Invalid graphics path"));
    }
    let (mut x, mut y, mut counts) = (x.to_vec(), y.to_vec(), counts.to_vec());
    let polygons = counts.len() as c_int;
    guarded(|| {
        draw(
            x.as_mut_ptr(),
            y.as_mut_ptr(),
            polygons,
            counts.as_mut_ptr(),
            c_int::from(winding),
            gc,
            dev,
        )
    })
}

/// `GERaster`: an image of `width` by `height` pixels, row by row from the top, placed with
/// its bottom-left corner at `(x, y)`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn raster(
    pixels: &mut [c_uint],
    columns: c_int,
    rows: c_int,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    angle: f64,
    interpolate: bool,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    let Some(draw) = (*dev).raster else {
        return crate::conditions::warning("raster rendering is not implemented for this device");
    };
    if width != 0.0 && height != 0.0 {
        guarded(|| {
            draw(
                pixels.as_mut_ptr(),
                columns,
                rows,
                x,
                y,
                width,
                height,
                angle,
                c_int::from(interpolate),
                gc,
                dev,
            )
        })?;
    }
    Ok(())
}

/// `GECap`: the device's current image, or `NULL` with a warning when it cannot capture.
pub unsafe fn capture(dd: *mut GEDevDesc) -> R<Sexp> {
    let dev = (*dd).dev;
    let Some(cap) = (*dev).cap else {
        crate::conditions::warning("raster capture is not available for this device")?;
        return Ok(nil());
    };
    guarded_sexp(|| cap(dev))
}

/// `GEMode`: tells the device that drawing starts (1) or stops (0).
pub unsafe fn mode(mode: c_int, dd: *mut GEDevDesc) -> R<()> {
    if no_devices() {
        return Err(error("no graphics device is active"));
    }
    let dev = (*dd).dev;
    if let Some(set_mode) = (*dev).mode {
        guarded(|| set_mode(mode, dev))?;
    }
    Ok(())
}

/// `GENewPage`.
pub unsafe fn new_page(gc: &GEContext, dd: *mut GEDevDesc) -> R<()> {
    (*dd).appending = 0;
    let dev = (*dd).dev;
    if let Some(new_page) = (*dev).new_page {
        guarded(|| new_page(gc, dev))?;
    }
    Ok(())
}

pub unsafe fn dirty_device(dd: *mut GEDevDesc) {
    (*dd).dirty = 1;
}

pub unsafe fn device_dirty(dd: *mut GEDevDesc) -> bool {
    (*dd).dirty != 0
}

pub unsafe fn clean_device(dd: *mut GEDevDesc) {
    (*dd).dirty = 0;
}

/// `GEStroke`, `GEFill` and `GEFillStroke`: draw a path a function builds, which needs a
/// device of version 15 or later that is not already building one.
pub unsafe fn stroke(path: Sexp, gc: &GEContext, dd: *mut GEDevDesc) -> R<()> {
    let dev = (*dd).dev;
    if (*dev).device_version < VERSION_GROUP {
        return Ok(());
    }
    if (*dd).appending != 0 {
        return crate::conditions::warning("Stroke ignored (device is appending path)");
    }
    if let Some(draw) = (*dev).stroke {
        (*dd).appending = 1;
        let outcome = guarded(|| draw(path, gc, dev));
        (*dd).appending = 0;
        outcome?;
    }
    Ok(())
}

pub unsafe fn fill(path: Sexp, rule: c_int, gc: &GEContext, dd: *mut GEDevDesc) -> R<()> {
    fill_with(path, rule, gc, dd, false)
}

pub unsafe fn fill_stroke(path: Sexp, rule: c_int, gc: &GEContext, dd: *mut GEDevDesc) -> R<()> {
    fill_with(path, rule, gc, dd, true)
}

unsafe fn fill_with(
    path: Sexp,
    rule: c_int,
    gc: &GEContext,
    dd: *mut GEDevDesc,
    stroke: bool,
) -> R<()> {
    let dev = (*dd).dev;
    if (*dev).device_version < VERSION_GROUP {
        return Ok(());
    }
    if (*dd).appending != 0 {
        let what = if stroke { "Fill and stroke" } else { "Fill" };
        return crate::conditions::warning(format!("{what} ignored (device is appending path)"));
    }
    let draw = if stroke {
        (*dev).fill_stroke
    } else {
        (*dev).fill
    };
    if let Some(draw) = draw {
        (*dd).appending = 1;
        let outcome = guarded(|| draw(path, rule, gc, dev));
        (*dd).appending = 0;
        outcome?;
    }
    Ok(())
}

/// `GEGlyph`: draws glyphs by their index in a font file, on devices of version 16 or later.
#[allow(clippy::too_many_arguments)]
pub unsafe fn glyph(
    glyphs: &mut [c_int],
    x: &mut [f64],
    y: &mut [f64],
    font: Sexp,
    size: f64,
    colour: c_int,
    rotation: f64,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    if (*dev).device_version < VERSION_GLYPHS {
        return Ok(());
    }
    if let Some(draw) = (*dev).glyph {
        let count = glyphs.len() as c_int;
        guarded(|| {
            draw(
                count,
                glyphs.as_mut_ptr(),
                x.as_mut_ptr(),
                y.as_mut_ptr(),
                font,
                size,
                colour,
                rotation,
                dev,
            )
        })?;
    }
    Ok(())
}

/// Calls one of the device's definition callbacks (patterns, clip paths, masks, groups),
/// which only devices of version 13 or later have.
pub unsafe fn definition_version(dd: *mut GEDevDesc) -> bool {
    (*(*dd).dev).device_version >= VERSION_DEFINITIONS
}

// Text.

/// The encoding a string reaches the device in: UTF-8, or the symbol font's own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextEncoding {
    Utf8,
    Native,
    Symbol,
}

unsafe fn text_encoding(gc: &GEContext, dd: *mut GEDevDesc) -> TextEncoding {
    let dev = &*(*dd).dev;
    if gc.fontface == 5 {
        if dev.want_symbol_utf8 == 1 || dev.want_symbol_utf8 == NA_LOGICAL {
            TextEncoding::Utf8
        } else {
            TextEncoding::Symbol
        }
    } else if dev.has_text_utf8 == 1 {
        TextEncoding::Utf8
    } else {
        TextEncoding::Native
    }
}

/// The character codes `metricInfo` expects for `text`: bytes for ASCII and the symbol font,
/// negated code points for UTF-8, and code points in the native (UTF-8) encoding.
fn metric_codes(text: &str, encoding: TextEncoding) -> Vec<c_int> {
    if encoding == TextEncoding::Symbol || text.is_ascii() {
        return text.bytes().map(c_int::from).collect();
    }
    text.chars()
        .map(|character| match encoding {
            TextEncoding::Utf8 => -(character as c_int),
            _ => character as c_int,
        })
        .collect()
}

unsafe fn device_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// `GEMetricInfo`: the ascent, descent and width of character `code`, in device units.
pub unsafe fn metric_info(code: c_int, gc: &GEContext, dd: *mut GEDevDesc) -> R<[f64; 3]> {
    if is_hershey(gc) {
        return Ok([0.0; 3]);
    }
    let dev = (*dd).dev;
    let mut metrics = [0.0; 3];
    if let Some(info) = (*dev).metric_info {
        let [ascent, descent, width] = &mut metrics;
        guarded(|| info(code, gc, ascent, descent, width, dev))?;
    }
    Ok(metrics)
}

fn is_hershey(gc: &GEContext) -> bool {
    gc.family().starts_with("Hershey")
}

/// `GEStrWidth`: the width of the widest line of `text`, in device units.
pub unsafe fn str_width(text: &str, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    let dev = (*dd).dev;
    let utf8 = (*dev).has_text_utf8 == 1 && text_encoding(gc, dd) == TextEncoding::Utf8;
    let measure = if utf8 {
        (*dev).str_width_utf8
    } else {
        (*dev).str_width
    };
    let Some(measure) = measure else {
        return Ok(0.0);
    };
    let mut widest: f64 = 0.0;
    if text.is_empty() {
        return Ok(0.0);
    }
    for line in text.split('\n') {
        let line = device_string(line);
        widest = widest.max(guarded(|| measure(line.as_ptr(), gc, dev))?);
    }
    Ok(widest)
}

unsafe fn line_height(gc: &GEContext, dd: *mut GEDevDesc) -> f64 {
    let dev = &*(*dd).dev;
    gc.lineheight * gc.cex * dev.cra[1] * gc.ps / dev.startps
}

/// `GEStrHeight`: the height of `text` from the baseline of its last line to the top of its
/// first, in device units.
pub unsafe fn str_height(text: &str, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    let breaks = text.matches('\n').count() as f64;
    let mut ascent = metric_info(c_int::from(b'M'), gc, dd)?;
    if ascent == [0.0; 3] {
        ascent[0] = line_height(gc, dd);
    }
    Ok(breaks * line_height(gc, dd) + ascent[0])
}

/// `GEStrMetric`: the ascent of the first line plus the lines below it, the descent of the
/// last line, and the width.
pub unsafe fn str_metric(text: &str, gc: &GEContext, dd: *mut GEDevDesc) -> R<[f64; 3]> {
    if is_hershey(gc) {
        return Ok([0.0; 3]);
    }
    let no_metrics = metric_info(c_int::from(b'M'), gc, dd)? == [0.0; 3];
    let encoding = text_encoding(gc, dd);
    let first = text.split('\n').next().unwrap_or("");
    let last = text.rsplit('\n').next().unwrap_or("");
    let mut ascent: f64 = 0.0;
    let mut descent: f64 = 0.0;
    if no_metrics {
        ascent = str_height(first, gc, dd)?;
    } else {
        for code in metric_codes(first, encoding) {
            ascent = ascent.max(metric_info(code, gc, dd)?[0]);
        }
        for code in metric_codes(last, encoding) {
            descent = descent.max(metric_info(code, gc, dd)?[1]);
        }
    }
    ascent += text.matches('\n').count() as f64 * line_height(gc, dd);
    Ok([ascent, descent, str_width(text, gc, dd)?])
}

/// `GEText`: draws `text` with `(x, y)` at `(xc, yc)` of its box, where 0 is the left or the
/// baseline and 1 the right or the top. An `NA` `yc` centres each line on its ink.
#[allow(clippy::too_many_arguments)]
pub unsafe fn text(
    x: f64,
    y: f64,
    text: &str,
    mut xc: f64,
    mut yc: f64,
    rotation: f64,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    if text.is_empty() {
        return Ok(());
    }
    let encoding = text_encoding(gc, dd);
    let dev = (*dd).dev;
    let x = from_device_x(x, Unit::Inches, dd);
    let y = from_device_y(y, Unit::Inches, dd);
    let lines: Vec<&str> = text.split('\n').collect();
    let count = lines.len();
    let (sin, cos) = rotation.to_radians().sin_cos();
    let mut no_metrics = None;
    for (index, line) in lines.iter().enumerate() {
        let mut width_device = f64::NAN;
        let mut height_device = f64::NAN;
        let (xoff, yoff) = if count > 1 {
            if !xc.is_finite() {
                xc = 0.5;
            }
            if !yc.is_finite() {
                yc = 0.5;
            }
            let offset = ((1.0 - yc) * (count - 1) as f64 - index as f64) * line_height(gc, dd);
            let offset = from_device_height(offset, Unit::Inches, dd);
            (x - offset * sin, y + offset * cos)
        } else {
            (x, y)
        };
        let (xleft, ybottom, hadj) = if xc != 0.0 || yc != 0.0 {
            width_device = str_width(line, gc, dd)?;
            let width = from_device_width(width_device, Unit::Inches, dd);
            if !xc.is_finite() {
                xc = 0.5;
            }
            let height;
            if !yc.is_finite() {
                let none = match no_metrics {
                    Some(none) => none,
                    None => {
                        let none = metric_info(c_int::from(b'M'), gc, dd)? == [0.0; 3];
                        no_metrics = Some(none);
                        none
                    }
                };
                if count > 1 || none {
                    height = from_device_height(str_height(line, gc, dd)?, Unit::Inches, dd);
                    yc = (*dev).y_char_offset;
                } else {
                    let mut extremes: Option<(f64, f64)> = None;
                    for code in metric_codes(line, encoding) {
                        let [ascent, descent, _] = metric_info(code, gc, dd)?;
                        let ascent = from_device_height(ascent, Unit::Inches, dd);
                        let descent = from_device_height(descent, Unit::Inches, dd);
                        extremes = Some(match extremes {
                            None => (ascent, descent),
                            Some((high, deep)) => (high.max(ascent), deep.max(descent)),
                        });
                    }
                    let (high, deep) = extremes.unwrap_or((0.0, 0.0));
                    height = high - deep;
                    yc = 0.5;
                }
            } else {
                height_device = str_height(line, gc, dd)?;
                height = from_device_height(height_device, Unit::Inches, dd);
            }
            let hadj = match (*dev).can_hadj {
                2 => xc,
                1 => (0.5 * (2.0 * xc + 0.5).floor()).clamp(0.0, 1.0),
                _ => 0.0,
            };
            (
                xoff - (xc - hadj) * width * cos + yc * height * sin,
                yoff - (xc - hadj) * width * sin - yc * height * cos,
                hadj,
            )
        } else {
            (xoff, yoff, 0.0)
        };
        let xleft = to_device_x(xleft, Unit::Inches, dd);
        let ybottom = to_device_y(ybottom, Unit::Inches, dd);
        clip_text(
            xleft,
            ybottom,
            line,
            encoding,
            width_device,
            height_device,
            rotation,
            hadj,
            gc,
            dd,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn clip_text(
    x: f64,
    y: f64,
    line: &str,
    encoding: TextEncoding,
    width: f64,
    height: f64,
    rotation: f64,
    hadj: f64,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    let dev = (*dd).dev;
    let draw = if (*dev).has_text_utf8 == 1 && encoding == TextEncoding::Utf8 {
        (*dev).text_utf8
    } else {
        (*dev).text
    };
    let Some(draw) = draw else {
        return Ok(());
    };
    let string = device_string(line);
    let draw_text = || guarded(|| draw(x, y, string.as_ptr(), rotation, hadj, gc, dev));
    if device_clips(dd) {
        return draw_text();
    }
    let width = if width.is_finite() {
        width
    } else {
        str_width(line, gc, dd)?
    };
    let height = if height.is_finite() {
        height
    } else {
        str_height(line, gc, dd)?
    };
    let width = from_device_width(width, Unit::Inches, dd);
    let height = from_device_height(height, Unit::Inches, dd);
    let angle = rotation.to_radians();
    let theta1 = std::f64::consts::FRAC_PI_2 - angle;
    let diagonal = width.hypot(height);
    let theta2 = angle + height.atan2(width);
    let x0 = from_device_x(x, Unit::Inches, dd) - hadj * width * angle.cos();
    let y0 = from_device_y(y, Unit::Inches, dd) - hadj * width * angle.sin();
    let xs = [
        x0 + height * theta1.cos(),
        x0,
        x0 + diagonal * theta2.cos(),
        x0 + width * angle.cos(),
    ];
    let ys = [
        y0 + height * theta1.sin(),
        y0,
        y0 + diagonal * theta2.sin(),
        y0 + width * angle.sin(),
    ];
    let fold = |values: &[f64; 4], pick: fn(f64, f64) -> f64| {
        values[1..].iter().fold(values[0], |a, b| pick(a, *b))
    };
    let code = rect_clip(
        to_device_x(fold(&xs, f64::min), Unit::Inches, dd),
        to_device_y(fold(&ys, f64::min), Unit::Inches, dd),
        to_device_x(fold(&xs, f64::max), Unit::Inches, dd),
        to_device_y(fold(&ys, f64::max), Unit::Inches, dd),
        &region(can_clip(dd), dd),
    );
    match code {
        1 => draw_text(),
        2 if can_clip(dd) => draw_text(),
        // Text that crosses the clip region of a device that cannot clip is dropped.
        _ => Ok(()),
    }
}

// Plotting symbols.

const SYMBOL_RADIUS: f64 = 0.375;
const SYMBOL_SMALL: f64 = 0.25;
/// `sqrt(pi / 4)`: a square of side `2 * SQRC * r` has the area of a circle of radius `r`.
const SQRC: f64 = 0.886_226_925_452_758;
/// `sqrt(pi / 4) * sqrt(2)`, the half-diagonal of that square.
const DMDC: f64 = 1.253_314_137_315_500_3;
/// `sqrt(4 * pi / (3 * sqrt(3)))`, the circumradius of an equilateral triangle with that area,
/// then its half side and its inradius.
const TRC0: f64 = 1.555_120_301_556_214_7;
const TRC1: f64 = 1.346_773_687_088_598_4;
const TRC2: f64 = 0.777_560_150_778_107_1;

/// `GESymbol`: plotting symbol `pch` centred at `(x, y)`. `size` is the symbol's height in
/// device units, except for `"."`, where it is the character expansion.
pub unsafe fn symbol(
    x: f64,
    y: f64,
    pch: c_int,
    size: f64,
    gc: &mut GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    let largest = if gc.fontface != 5 { 127 } else { 255 };
    if pch == NA_INTEGER {
        return Ok(());
    }
    if pch < 0 {
        if gc.fontface == 5 {
            return Err(error("use of negative pch with symbol font is invalid"));
        }
        let character =
            char::from_u32(pch.unsigned_abs()).ok_or_else(|| error("invalid Unicode point"))?;
        return text(
            x,
            y,
            &character.to_string(),
            f64::NAN,
            f64::NAN,
            0.0,
            gc,
            dd,
        );
    }
    if (32..=largest).contains(&pch) {
        if pch == c_int::from(b'.') {
            gc.fill = gc.col;
            gc.col = TRANSPARENT_WHITE;
            let mut half_x = size * to_device_width(0.005, Unit::Inches, dd).abs();
            let mut half_y = size * to_device_height(0.005, Unit::Inches, dd).abs();
            if size > 0.0 {
                half_x = half_x.max(0.5);
                half_y = half_y.max(0.5);
            }
            return rect(x - half_x, y - half_y, x + half_x, y + half_y, gc, dd);
        }
        let character = char::from_u32(pch as u32).unwrap_or(' ');
        return text(
            x,
            y,
            &character.to_string(),
            f64::NAN,
            f64::NAN,
            0.0,
            gc,
            dd,
        );
    }
    if pch > largest {
        return crate::conditions::warning(format!("pch value '{pch}' is invalid in this locale"));
    }
    let unit = from_device_width(size, Unit::Inches, dd);
    let width = |inches: f64| to_device_width(inches, Unit::Inches, dd);
    let height = |inches: f64| to_device_height(inches, Unit::Inches, dd);
    let radius = SYMBOL_RADIUS * unit;
    // The triangle's apex distance, base half-width and base distance.
    let triangle = || {
        (
            height(TRC0 * radius),
            width(TRC1 * radius),
            height(TRC2 * radius),
        )
    };
    let half = |scale: f64| (width(scale * unit), height(scale * unit));
    let transparent = TRANSPARENT_WHITE;
    match pch {
        0 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            gc.fill = transparent;
            rect(x - xc, y - yc, x + xc, y + yc, gc, dd)
        }
        1 => {
            gc.fill = transparent;
            circle(x, y, SYMBOL_RADIUS * size, gc, dd)
        }
        2 | 6 | 17 | 24 | 25 => {
            let (r, xc, yc) = triangle();
            let (apex, base) = if pch == 6 || pch == 25 {
                (-r, yc)
            } else {
                (r, -yc)
            };
            match pch {
                2 | 6 => gc.fill = transparent,
                17 => {
                    gc.fill = gc.col;
                    gc.col = transparent;
                }
                _ => {}
            }
            polygon(
                &[x, x + xc, x - xc],
                &[y + apex, y + base, y + base],
                gc,
                dd,
            )
        }
        3 => {
            let (xc, yc) = half(std::f64::consts::SQRT_2 * SYMBOL_RADIUS);
            line(x - xc, y, x + xc, y, gc, dd)?;
            line(x, y - yc, x, y + yc, gc, dd)
        }
        4 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            line(x - xc, y - yc, x + xc, y + yc, gc, dd)?;
            line(x - xc, y + yc, x + xc, y - yc, gc, dd)
        }
        5 | 18 | 23 => {
            let scale = match pch {
                5 => std::f64::consts::SQRT_2 * SYMBOL_RADIUS,
                18 => SYMBOL_RADIUS,
                _ => SYMBOL_RADIUS * DMDC,
            };
            let (xc, yc) = half(scale);
            match pch {
                5 => gc.fill = transparent,
                18 => {
                    gc.fill = gc.col;
                    gc.col = transparent;
                }
                _ => {}
            }
            polygon(&[x - xc, x, x + xc, x], &[y, y + yc, y, y - yc], gc, dd)
        }
        7 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            gc.fill = transparent;
            rect(x - xc, y - yc, x + xc, y + yc, gc, dd)?;
            line(x - xc, y - yc, x + xc, y + yc, gc, dd)?;
            line(x - xc, y + yc, x + xc, y - yc, gc, dd)
        }
        8 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            line(x - xc, y - yc, x + xc, y + yc, gc, dd)?;
            line(x - xc, y + yc, x + xc, y - yc, gc, dd)?;
            let (xc, yc) = half(std::f64::consts::SQRT_2 * SYMBOL_RADIUS);
            line(x - xc, y, x + xc, y, gc, dd)?;
            line(x, y - yc, x, y + yc, gc, dd)
        }
        9 => {
            let (xc, yc) = half(std::f64::consts::SQRT_2 * SYMBOL_RADIUS);
            line(x - xc, y, x + xc, y, gc, dd)?;
            line(x, y - yc, x, y + yc, gc, dd)?;
            gc.fill = transparent;
            polygon(&[x - xc, x, x + xc, x], &[y, y + yc, y, y - yc], gc, dd)
        }
        10 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            gc.fill = transparent;
            circle(x, y, xc, gc, dd)?;
            line(x - xc, y, x + xc, y, gc, dd)?;
            line(x, y - yc, x, y + yc, gc, dd)
        }
        11 => {
            let (r, xc, yc) = triangle();
            let yc = 0.5 * (yc + r);
            gc.fill = transparent;
            polygon(&[x, x + xc, x - xc], &[y - r, y + yc, y + yc], gc, dd)?;
            polygon(&[x, x + xc, x - xc], &[y + r, y - yc, y - yc], gc, dd)
        }
        12 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            line(x - xc, y, x + xc, y, gc, dd)?;
            line(x, y - yc, x, y + yc, gc, dd)?;
            gc.fill = transparent;
            rect(x - xc, y - yc, x + xc, y + yc, gc, dd)
        }
        13 => {
            gc.fill = transparent;
            circle(x, y, SYMBOL_RADIUS * size, gc, dd)?;
            let (xc, yc) = half(SYMBOL_RADIUS);
            line(x - xc, y - yc, x + xc, y + yc, gc, dd)?;
            line(x - xc, y + yc, x + xc, y - yc, gc, dd)
        }
        14 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            gc.fill = transparent;
            polygon(&[x, x + xc, x - xc], &[y + yc, y - yc, y - yc], gc, dd)?;
            rect(x - xc, y - yc, x + xc, y + yc, gc, dd)
        }
        15 => {
            let (xc, yc) = half(SYMBOL_RADIUS);
            gc.fill = gc.col;
            gc.col = transparent;
            polygon(
                &[x - xc, x + xc, x + xc, x - xc],
                &[y - yc, y - yc, y + yc, y + yc],
                gc,
                dd,
            )
        }
        16 => {
            gc.fill = gc.col;
            gc.col = transparent;
            circle(x, y, SYMBOL_RADIUS * size, gc, dd)
        }
        19 => {
            gc.fill = gc.col;
            circle(x, y, SYMBOL_RADIUS * size, gc, dd)
        }
        20 => {
            gc.fill = gc.col;
            circle(x, y, SYMBOL_SMALL * size, gc, dd)
        }
        21 => circle(x, y, SYMBOL_RADIUS * size, gc, dd),
        22 => {
            let (xc, yc) = half(SYMBOL_RADIUS * SQRC);
            rect(x - xc, y - yc, x + xc, y + yc, gc, dd)
        }
        _ => crate::conditions::warning(format!("unimplemented pch value '{pch}'")),
    }
}

/// `GEPretty`: widens `[low, high]` to about `divisions` intervals of 1, 2 or 5 times a power
/// of ten, never past the original bounds by more than rounding.
pub fn pretty_axis(low: &mut f64, high: &mut f64, divisions: &mut c_int) -> R<()> {
    if *divisions <= 0 {
        return Err(error(format!(
            "invalid axis extents [GEPretty(.,.,n={divisions})"
        )));
    }
    if !low.is_finite() || !high.is_finite() {
        return Err(error(format!(
            "non-finite axis extents [GEPretty({},{}, n={divisions})]",
            crate::format::c_general(*low, 6, false),
            crate::format::c_general(*high, 6, false)
        )));
    }
    let rounded = crate::arith::pretty(*low, *high, *divisions, 1, 0.25, [0.8, 1.7], 2, false);
    let (mut start, mut end, unit) = (rounded.low, rounded.high, rounded.unit);
    *divisions = rounded.count;
    const ROUNDING: f64 = 1e-10;
    if end >= start + 1.0 {
        let mut moved = false;
        if start * unit < *low - ROUNDING * unit {
            start += 1.0;
            moved = true;
        }
        if end > start + 1.0 && end * unit > *high + ROUNDING * unit {
            end -= 1.0;
            moved = true;
        }
        if moved {
            *divisions = (end - start) as c_int;
        }
    }
    *low = start * unit;
    *high = end * unit;
    Ok(())
}

// Mathematical annotation. ry does not lay out plotmath yet: an expression is measured and
// drawn as the plain text of its deparse.

unsafe fn expression_text(expression: Sexp) -> String {
    let expression = if expression.kind() == SexpType::Expression && length(expression) > 0 {
        expression.element(0)
    } else {
        expression
    };
    match expression.kind() {
        SexpType::String => {
            values::first_string(expression).map_or_else(String::new, |text| text.into_owned())
        }
        SexpType::Symbol => values::symbol_name(expression).into_owned(),
        _ => crate::deparse::deparse_one_line(expression),
    }
}

pub unsafe fn expression_width(expression: Sexp, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    str_width(&expression_text(expression), gc, dd)
}

pub unsafe fn expression_height(expression: Sexp, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    str_height(&expression_text(expression), gc, dd)
}

pub unsafe fn expression_metric(
    expression: Sexp,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<[f64; 3]> {
    str_metric(&expression_text(expression), gc, dd)
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn math_text(
    x: f64,
    y: f64,
    expression: Sexp,
    xc: f64,
    yc: f64,
    rotation: f64,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    text(x, y, &expression_text(expression), xc, yc, rotation, gc, dd)
}

// The display list.

fn engine_device(dd: *mut GEDevDesc) -> *mut EngineDevice {
    dd.cast()
}

/// `GEinitDisplayList`: lets every system save its state, then empties the list.
pub unsafe fn init_display_list(dd: *mut GEDevDesc) -> R<()> {
    notify_systems(dd, Event::SaveState, nil())?;
    let device = engine_device(dd);
    (*device).ops.clear();
    (*device).args.clear();
    Ok(())
}

/// `GErecordGraphicOperation`.
pub unsafe fn record_operation(op: Sexp, args: Sexp, dd: *mut GEDevDesc) {
    let device = engine_device(dd);
    (*device).ops.push(op);
    (*device).args.push(args);
}

pub unsafe fn display_list_is_empty(dd: *mut GEDevDesc) -> bool {
    (*engine_device(dd)).ops.is_empty()
}

/// The display list as GNU R stores it: a pairlist of `list(op, args)` pairlists.
unsafe fn display_list_object(dd: *mut GEDevDesc) -> R<Sexp> {
    let device = engine_device(dd);
    let mut list = nil();
    for (&op, &args) in (*device).ops.iter().zip(&(*device).args).rev() {
        let entry = values::pairlist(&[(nil(), op), (nil(), args)], SexpType::Pairlist)?;
        crate::heap().root(entry);
        list = values::cons(entry, list)?;
        crate::heap().root(list);
    }
    Ok(list)
}

unsafe fn set_display_list(dd: *mut GEDevDesc, list: Sexp) {
    let device = engine_device(dd);
    (*device).ops.clear();
    (*device).args.clear();
    let mut cell = list;
    while cell.kind() == SexpType::Pairlist {
        let entry = cell.car();
        if matches!(entry.kind(), SexpType::Pairlist | SexpType::Language) {
            (*device).ops.push(entry.car());
            (*device).args.push(if entry.cdr() == nil() {
                nil()
            } else {
                entry.cdr().car()
            });
        } else if entry.kind() == SexpType::List && length(entry) >= 2 {
            (*device).ops.push(entry.element(0));
            (*device).args.push(entry.element(1));
        }
        cell = cell.cdr();
    }
}

/// `GEcheckState`: whether every system accepts the state the last operation left.
pub unsafe fn check_state(dd: *mut GEDevDesc) -> R<bool> {
    let mut valid = true;
    for result in notify_systems(dd, Event::CheckPlot, nil())? {
        if !(result.kind() == SexpType::Logical
            && length(result) > 0
            && values::ints(result)[0] == 1)
        {
            valid = false;
        }
    }
    Ok(valid)
}

/// `GEplayDisplayList`: redraws the recorded operations on `dd`.
pub unsafe fn play_display_list(dd: *mut GEDevDesc) -> R<()> {
    let number = device_number(dd);
    if number == 0 || display_list_is_empty(dd) {
        return Ok(());
    }
    let list = display_list_object(dd)?;
    notify_systems(dd, Event::RestoreState, list)?;
    let device = engine_device(dd);
    let operations: Vec<(Sexp, Sexp)> = (*device)
        .ops
        .iter()
        .copied()
        .zip((*device).args.iter().copied())
        .collect();
    let saved = current_number();
    select_device(number)?;
    let replay = (|| -> R<()> {
        for (op, args) in operations {
            if !matches!(op.kind(), SexpType::Builtin | SexpType::Special) {
                return crate::conditions::warning("invalid display list");
            }
            let definition = eval::builtin_def(op)?;
            let arguments = eval::unevaluated_args(args);
            eval::call_builtin(definition, nil(), op, &arguments, values::base_env())?;
            if !check_state(dd)? {
                return crate::conditions::warning("display list redraw incomplete");
            }
        }
        Ok(())
    })();
    select_device(saved)?;
    replay
}

/// `GEcopyDisplayList`: replays device `from`'s display list on the current device.
pub unsafe fn copy_display_list(from: usize) -> R<()> {
    let dd = current_device()?;
    let source = device(from);
    if source.is_null() {
        return Err(error("invalid device"));
    }
    let (ops, args) = (
        (*engine_device(source)).ops.clone(),
        (*engine_device(source)).args.clone(),
    );
    let device = engine_device(dd);
    (*device).ops = ops;
    (*device).args = args;
    notify_systems(source, Event::CopyState, nil())?;
    play_display_list(dd)?;
    if (*dd).display_list_on == 0 {
        init_display_list(dd)?;
    }
    Ok(())
}

/// `GEcreateSnapshot`: the display list, then each system's state in index order, with the
/// engine version attached.
pub unsafe fn create_snapshot(dd: *mut GEDevDesc) -> R<Sexp> {
    let list = display_list_object(dd)?;
    let states = notify_systems(dd, Event::SaveSnapshotState, nil())?;
    let mut items = vec![list];
    items.extend(states);
    let snapshot = values::list_vector(&items)?;
    crate::heap().root(snapshot);
    values::set_attr(
        snapshot,
        values::install("engineVersion")?,
        scalar_integer(ENGINE_VERSION)?,
    )?;
    Ok(snapshot)
}

/// `GEplaySnapshot`.
pub unsafe fn play_snapshot(snapshot: Sexp, dd: *mut GEDevDesc) -> R<()> {
    let version = values::get_attr(snapshot, values::install("engineVersion")?);
    if version == nil() {
        crate::conditions::warning(format!(
            "snapshot recorded with different graphics engine version (pre 11 - this is version {ENGINE_VERSION})"
        ))?;
    } else {
        let recorded = values::as_integer_scalar(version);
        if recorded != ENGINE_VERSION {
            crate::conditions::warning(format!(
                "snapshot recorded with different graphics engine version ({recorded} - this is version {ENGINE_VERSION})"
            ))?;
        }
    }
    clean_device(dd);
    notify_systems(dd, Event::RestoreSnapshotState, snapshot)?;
    (*dd).record_graphics = 1;
    let list = if snapshot.kind() == SexpType::List && length(snapshot) > 0 {
        snapshot.element(0)
    } else {
        nil()
    };
    set_display_list(dd, list);
    play_display_list(dd)?;
    if (*dd).display_list_on == 0 {
        init_display_list(dd)?;
    }
    Ok(())
}

/// `GEonExit`: after an error at top level, turns recording back on and tells each device.
pub unsafe fn on_exit() -> R<()> {
    for dd in open_devices() {
        (*dd).record_graphics = 1;
        let dev = (*dd).dev;
        if let Some(on_exit) = (*dev).on_exit {
            guarded(|| on_exit(dev))?;
        }
    }
    Ok(())
}

/// Runs a graphics primitive the way `.Call.graphics` and `.External.graphics` do: with
/// recording off while it draws, then recorded on the display list unless it is itself being
/// replayed (a `NULL` call).
pub unsafe fn graphics_call(call: &Call<'_>, body: impl FnOnce() -> R<Sexp>) -> R<Sexp> {
    let dd = current_device()?;
    let record = (*dd).record_graphics;
    (*dd).record_graphics = 0;
    let result = body();
    let still_open = device_number(dd) != 0;
    if still_open {
        (*dd).record_graphics = record;
    }
    let value = result?;
    crate::heap().root(value);
    if still_open && record != 0 && call.call != nil() {
        if !check_state(dd)? {
            return Err(crate::unwind::error_in(call.call, "invalid graphics state"));
        }
        record_call(call, dd)?;
    }
    Ok(value)
}

/// Records `call` on `dd`'s display list, as a routine does that records itself rather than
/// through `.External.graphics`, unless recording is off or the call is being replayed.
pub unsafe fn record_call(call: &Call<'_>, dd: *mut GEDevDesc) -> R<()> {
    if call.call == nil() || (*dd).record_graphics == 0 {
        return Ok(());
    }
    let items: Vec<(Sexp, Sexp)> = call.args.iter().map(|arg| (arg.tag, arg.value)).collect();
    let args = values::pairlist(&items, SexpType::Pairlist)?;
    crate::heap().root(args);
    record_operation(call.op, args, dd);
    Ok(())
}

/// `.Internal(recordGraphics(expr, list, env))`: evaluates `expr` with the elements of `list`
/// bound in a child of `env`, and records the call so that a redraw evaluates it again.
pub unsafe fn record_graphics(call: &Call<'_>) -> R<Sexp> {
    let dd = current_device()?;
    let [code, list, parent] =
        [0, 1, 2].map(|index| call.args.get(index).map_or_else(nil, |arg| arg.value));
    if !matches!(code.kind(), SexpType::Language) {
        return Err(error("'expr' argument must be an expression"));
    }
    if list.kind() != SexpType::List {
        return Err(error("'list' argument must be a list"));
    }
    if parent == nil() {
        return Err(error("use of NULL environment is defunct"));
    }
    if parent.kind() != SexpType::Environment {
        return Err(error("'env' argument must be an environment"));
    }
    let frame = values::list_to_pairlist(list, SexpType::Pairlist)?;
    crate::heap().root(frame);
    let env = values::new_env(parent)?;
    crate::heap().root(env);
    for (tag, value) in values::pairlist_items(frame) {
        if tag != nil() {
            eval::define(tag, value, env)?;
        }
    }
    let record = (*dd).record_graphics;
    (*dd).record_graphics = 0;
    let result = eval::eval(code, env);
    let still_open = device_number(dd) != 0;
    if still_open {
        (*dd).record_graphics = record;
    }
    let value = result?;
    crate::heap().root(value);
    if still_open && record != 0 && call.call != nil() {
        if !check_state(dd)? {
            return Err(error("invalid graphics state"));
        }
        record_call(call, dd)?;
    }
    Ok(value)
}
