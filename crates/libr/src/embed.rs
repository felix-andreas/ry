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
