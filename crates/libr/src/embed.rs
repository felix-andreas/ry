//! ry's own entry points for a host that embeds the runtime, such as the test harness and later
//! the `ry` binary. They are not GNU R API, so they carry the `ry_` prefix and are declared in
//! `include/ry_embed.h`.

use crate::{Global, runtime};
use heap::{Collection, GcStress};
use std::ffi::{c_char, c_int, c_void};

type ConsoleCallback = unsafe extern "C" fn(*const c_char, usize, c_int, *mut c_void);

/// Collection statistics, laid out for C.
#[repr(C)]
pub struct RyGcStats {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub objects_freed: u64,
    pub live_objects: u64,
    pub live_bytes: u64,
}

/// Starts the runtime. Calling it again does nothing.
#[unsafe(no_mangle)]
pub extern "C" fn ry_initialize() -> c_int {
    runtime();
    0
}

/// With `enabled`, every allocation collects first (and every eighth collection is a major
/// one), which turns a missing root into a crash at the allocation that exposes it.
#[unsafe(no_mangle)]
pub extern "C" fn ry_set_gc_stress(enabled: c_int) {
    runtime().heap.policy_mut().stress = if enabled != 0 {
        GcStress::EveryAllocation
    } else {
        GcStress::Off
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_collect(major: c_int) {
    runtime().heap.collect(if major != 0 {
        Collection::Major
    } else {
        Collection::Minor
    });
    crate::boundary::run_pending_finalizers();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_gc_stats(stats: *mut RyGcStats) {
    let current = runtime().heap.stats();
    *stats = RyGcStats {
        minor_collections: current.minor_collections,
        major_collections: current.major_collections,
        objects_freed: current.objects_freed,
        live_objects: current.live_objects,
        live_bytes: current.live_bytes,
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_protect_depth() -> c_int {
    runtime().heap.roots.protect.depth() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_handle_depth() -> usize {
    runtime().heap.handle_mark()
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_arena_bytes() -> usize {
    runtime().arena.allocated()
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_warning_count() -> c_int {
    runtime().warnings.len() as c_int
}

static WARNING_TEXT: Global<Vec<u8>> = Global::new(Vec::new());

/// The text of warning `index`, valid until the next call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_warning_message(index: c_int) -> *const c_char {
    let text = &mut *WARNING_TEXT.as_ptr();
    text.clear();
    if let Some(message) = runtime().warnings.get(index.max(0) as usize) {
        text.extend_from_slice(message.as_bytes());
    }
    text.push(0);
    text.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_clear_warnings() {
    runtime().warnings.clear();
}

/// Sends console output to `callback` instead of stdout and stderr; a null callback restores
/// them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_set_console(callback: Option<ConsoleCallback>, data: *mut c_void) {
    let runtime = runtime();
    runtime.console = match callback {
        Some(callback) => Box::new(move |text: &[u8], is_error: bool| {
            // SAFETY: the host promised a callback that accepts this text.
            unsafe {
                callback(
                    text.as_ptr() as *const c_char,
                    text.len(),
                    is_error as c_int,
                    data,
                )
            }
        }),
        None => Box::new(crate::boundary::default_console),
    };
}

unsafe extern "C" {
    fn ry_frontend_start(home: *const c_char, temp_dir: *const c_char);
}

/// Sets the front-end data native code reads (`R_Home`, `R_TempDir`, the console streams).
/// `R_Home` comes from `R_HOME`; the session's temporary directory is created here, as GNU R
/// creates its `Rtmp` directory at startup.
pub(crate) fn start_frontend() {
    let home = std::env::var("R_HOME").ok();
    let temp_root = ["TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let temp_dir = temp_root.join(format!("Rtmp{}", std::process::id()));
    // Without a temporary directory the variable stays null, which is what code testing it
    // expects of a session that has none.
    let temp_dir = std::fs::create_dir_all(&temp_dir)
        .ok()
        .map(|()| temp_dir.to_string_lossy().into_owned());
    let to_c = |text: Option<String>| text.and_then(|text| std::ffi::CString::new(text).ok());
    let home = to_c(home);
    let temp_dir = to_c(temp_dir);
    // SAFETY: both strings are NUL-terminated and copied by the C side.
    unsafe {
        ry_frontend_start(
            home.as_ref().map_or(std::ptr::null(), |text| text.as_ptr()),
            temp_dir
                .as_ref()
                .map_or(std::ptr::null(), |text| text.as_ptr()),
        );
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_run_exit_finalizers() {
    crate::extptr::R_RunExitFinalizers();
}

/// `Rf_initEmbeddedR(argc, argv)`: starts the runtime. The arguments are ry's to interpret
/// once the interpreter reads them; the runtime itself needs none.
#[unsafe(no_mangle)]
pub extern "C" fn Rf_initEmbeddedR(_argc: c_int, _argv: *mut *mut c_char) -> c_int {
    runtime();
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_initialize_R(_argc: c_int, _argv: *mut *mut c_char) -> c_int {
    runtime();
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_endEmbeddedR(_fatal: c_int) {
    crate::extptr::R_RunExitFinalizers();
}

#[unsafe(no_mangle)]
pub extern "C" fn setup_Rmainloop() {
    runtime();
}

/// The read-eval-print loop belongs to the interpreter's front end, not to libR.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn run_Rmainloop() {
    crate::boundary::write_console(
        b"run_Rmainloop: ry's console runs through the `ry` binary, not through libR\n",
        true,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn R_ReplDLLinit() {
    runtime();
}

/// One step of the embedded read-eval-print loop: -1 means the loop has ended.
#[unsafe(no_mangle)]
pub extern "C" fn R_ReplDLLdo1() -> c_int {
    -1
}

/// Reads an RDS file (`saveRDS` output, compressed or not), for hosts and tests that need R
/// objects GNU R recorded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_read_rds(path: *const c_char) -> crate::SEXP {
    crate::boundary::enter(|| {
        let path = String::from_utf8_lossy(crate::strings::c_bytes(path)).into_owned();
        crate::serialize::read_rds(std::path::Path::new(&path))
    })
}
