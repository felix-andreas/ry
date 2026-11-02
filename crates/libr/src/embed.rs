//! ry's own entry points for a host that embeds the runtime, such as the test harness and later
//! the `ry` binary. They are not GNU R API, so they carry the `ry_` prefix and are declared in
//! `include/ry_embed.h`.

use {
    crate::{Global, runtime},
    heap::{Collection, GcStress, object::Sexp},
    std::ffi::{c_char, c_int, c_void},
};

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

/// With `enabled`, every allocation collects first (with major collections spaced as
/// [`heap::GcStress::EveryAllocation`] describes), which turns a missing root into a crash at the
/// allocation that exposes it.
#[unsafe(no_mangle)]
pub extern "C" fn ry_set_gc_stress(enabled: c_int) {
    runtime().heap.update_policy(|policy| {
        policy.stress = if enabled != 0 {
            GcStress::EveryAllocation
        } else {
            GcStress::Off
        }
    });
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
    pending_warnings().len() as c_int
}

static WARNING_TEXT: Global<Vec<u8>> = Global::new(Vec::new());

/// The text of warning `index`, valid until the next call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_warning_message(index: c_int) -> *const c_char {
    let text = &mut *WARNING_TEXT.as_ptr();
    text.clear();
    if let Some((_, message)) = pending_warnings().get(index.max(0) as usize) {
        text.extend_from_slice(message.as_bytes());
    }
    text.push(0);
    text.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn ry_clear_warnings() {
    if interp::is_installed() {
        interp::interp().warnings.clear();
    }
}

/// The deferred warnings, which the interpreter keeps until the top level prints them.
fn pending_warnings() -> &'static [(Sexp, String)] {
    if interp::is_installed() {
        &interp::interp().warnings
    } else {
        &[]
    }
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
/// `R_Home` comes from `R_HOME`, and `R_TempDir` is the interpreter's session directory.
pub(crate) fn start_frontend() {
    let home = std::env::var("R_HOME").ok();
    // Without a temporary directory the variable stays null, which is what code testing it
    // expects of a session that has none.
    let temp_dir = interp::system::session_temp_dir().ok().map(str::to_owned);
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

#[unsafe(no_mangle)]
pub extern "C" fn ry_remove_session_temp_dir() {
    interp::system::remove_session_temp_dir();
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
    interp::system::remove_session_temp_dir();
}

/// The arguments an embedding front end hands `R_set_command_line_arguments`, which
/// `commandArgs()` returns once the session starts.
pub(crate) static EMBEDDING_ARGUMENTS: Global<Vec<String>> = Global::new(Vec::new());

/// `R_set_command_line_arguments(argc, argv)`: records what `commandArgs()` returns. A session
/// that is already running sees the new arguments at once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_set_command_line_arguments(argc: c_int, argv: *mut *mut c_char) {
    let mut arguments = Vec::new();
    for index in 0..argc.max(0) as usize {
        let argument = *argv.add(index);
        if !argument.is_null() {
            arguments.push(String::from_utf8_lossy(crate::strings::c_bytes(argument)).into_owned());
        }
    }
    if interp::is_installed() {
        interp::interp().command_args = arguments.clone();
    }
    *EMBEDDING_ARGUMENTS.as_ptr() = arguments;
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

/// Runs when the dynamic loader loads this library, after it has applied the relocations of the
/// executable and of every library loaded with it.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[used]
#[unsafe(link_section = ".init_array")]
static RELEASE_RELOCATION_TABLES: extern "C" fn() = release_relocation_tables;

/// Drops the pages of every loaded object's dynamic relocation table (`DT_RELA`). The loader
/// reads each table once, when it maps the object, and never again, yet the pages stay resident
/// for the life of the process: about 4 MB for `libR` and the `ry` executable, a third of their
/// resident start-up footprint. The tables live in a read-only file mapping, so dropping the
/// pages loses nothing, and a later read faults them back in from the file. The lazy-binding
/// table (`DT_JMPREL`) is left alone, because the loader reads it again on each first call
/// through the PLT. Packed relative relocations (`-z pack-relative-relocs`) would shrink the
/// tables instead, but they need glibc 2.36 and the release binaries support glibc 2.35.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
extern "C" fn release_relocation_tables() {
    unsafe extern "C" fn release(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        _data: *mut c_void,
    ) -> c_int {
        const DT_NULL: i64 = 0;
        const DT_RELA: i64 = 7;
        const DT_RELASZ: i64 = 8;
        // Small tables are not worth a system call.
        const MINIMUM: u64 = 64 * 1024;
        // SAFETY: the loader passes a valid description of one loaded object.
        let info = unsafe { &*info };
        let headers = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum.into()) };
        let Some(dynamic) = headers
            .iter()
            .find(|header| header.p_type == libc::PT_DYNAMIC)
        else {
            return 0;
        };
        let (mut table, mut size) = (0u64, 0u64);
        let mut entry = (info.dlpi_addr + dynamic.p_vaddr) as *const [u64; 2];
        loop {
            // SAFETY: the dynamic section is mapped and ends with a `DT_NULL` entry.
            let [tag, value] = unsafe { *entry };
            match tag as i64 {
                DT_NULL => break,
                DT_RELA => table = value,
                DT_RELASZ => size = value,
                _ => {}
            }
            entry = unsafe { entry.add(1) };
        }
        if size < MINIMUM {
            return 0;
        }
        // glibc rewrites the table's address to an absolute one on most targets; on others it
        // stays relative to the load address, which makes it smaller than that address.
        if table < info.dlpi_addr {
            table += info.dlpi_addr;
        }
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
        let start = table.next_multiple_of(page);
        let end = (table + size) / page * page;
        if start < end {
            // A failure leaves the pages resident, which is only a missed saving, so the result
            // is not checked.
            // SAFETY: the range lies inside the object's read-only, file-backed first segment.
            unsafe {
                libc::madvise(
                    start as *mut c_void,
                    (end - start) as usize,
                    libc::MADV_DONTNEED,
                )
            };
        }
        0
    }
    // SAFETY: the callback only reads the loader's descriptions and advises on their pages.
    unsafe { libc::dl_iterate_phdr(Some(release), std::ptr::null_mut()) };
}
