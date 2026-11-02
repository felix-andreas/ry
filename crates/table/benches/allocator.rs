//! The allocator `ry run` gives the engine, so a benchmark run from Rust measures the same memory
//! behavior. `libR` (`crates/libr/src/libr.rs`) declares mimalloc with these two defaults; glibc's
//! malloc would instead return every large freed block to the kernel and fault it in again, which
//! made the Rust driver slower than the same queries run from R.

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `mi_option_arena_eager_commit` and `mi_option_purge_delay` in mimalloc's `mi_option_e`.
#[cfg(target_os = "linux")]
const MI_OPTION_ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;
#[cfg(target_os = "linux")]
const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;

/// Set from a constructor, before the first allocation reserves an arena, as `libR` does.
#[cfg(target_os = "linux")]
#[used]
#[unsafe(link_section = ".init_array")]
static ALLOCATOR_DEFAULTS: extern "C" fn() = {
    extern "C" fn allocator_defaults() {
        // SAFETY: setting an option's default has no preconditions.
        unsafe {
            libmimalloc_sys::mi_option_set_default(MI_OPTION_ARENA_EAGER_COMMIT, 0);
            libmimalloc_sys::mi_option_set_default(MI_OPTION_PURGE_DELAY, -1);
        }
    }
    allocator_defaults
};
