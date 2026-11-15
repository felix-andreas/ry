#![no_main]

use libfuzzer_sys::fuzz_target;

use syntax::testing::ByteStream;

// The input is the generator's stream of choices: the package, then its edits.
fuzz_target!(|data: &[u8]| {
    loader::testing::check_random_reload(
        &mut loader::testing::ReloadLoaders::default(),
        &mut ByteStream::new(data),
    );
});

// The loader parses ahead on a detached thread when a package's load index comes from the cache.
// A leak check that runs while that thread exits reports the thread's own bookkeeping as leaked,
// so this target runs without leak detection.
#[unsafe(no_mangle)]
pub extern "C" fn __lsan_default_options() -> *const std::ffi::c_char {
    c"detect_leaks=0".as_ptr()
}
