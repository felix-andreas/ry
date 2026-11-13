#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        loader::testing::check_load_index_input(input);
    }
});

// The loader parses ahead on a detached thread when a package's load index comes from the cache.
// A leak check that runs while that thread exits reports the thread's own bookkeeping as leaked,
// so this target runs without leak detection.
#[unsafe(no_mangle)]
pub extern "C" fn __lsan_default_options() -> *const std::ffi::c_char {
    c"detect_leaks=0".as_ptr()
}
