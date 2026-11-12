//! The allocator `ry run` gives the engine, so a benchmark run from Rust measures the same memory
//! behavior. `libR` (`crates/libr/src/libr.rs`) declares this allocator with these two defaults:
//! a block of a megabyte or more gets a mapping of its own, which goes back to the kernel when
//! the block is freed, and mimalloc serves the smaller blocks and keeps their freed pages for
//! reuse. With every block from mimalloc, a freed column would stay resident beside the next one,
//! and the driver's peak memory would not be the one `ry run` has.

#[global_allocator]
static ALLOCATOR: heap::large::Mapped<mimalloc::MiMalloc> = heap::large::Mapped(mimalloc::MiMalloc);

/// `mi_option_arena_eager_commit` and `mi_option_purge_delay` in mimalloc's `mi_option_e`.
#[cfg(target_os = "linux")]
const MI_OPTION_ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;
#[cfg(target_os = "linux")]
const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;

/// `libR`'s purge delay in milliseconds, long enough that no purge comes due while a benchmark
/// runs.
#[cfg(target_os = "linux")]
const PURGE_DELAY_MS: std::ffi::c_long = 1_000_000_000;

/// Set from a constructor, before the first allocation reserves an arena, as `libR` does.
#[cfg(target_os = "linux")]
#[used]
#[unsafe(link_section = ".init_array")]
static ALLOCATOR_DEFAULTS: extern "C" fn() = {
    extern "C" fn allocator_defaults() {
        // SAFETY: setting an option's default has no preconditions.
        unsafe {
            libmimalloc_sys::mi_option_set_default(MI_OPTION_ARENA_EAGER_COMMIT, 0);
            libmimalloc_sys::mi_option_set_default(MI_OPTION_PURGE_DELAY, PURGE_DELAY_MS);
        }
    }
    allocator_defaults
};
