#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // SAFETY: a fuzz target runs its inputs one at a time on one thread, so it has the runtime
    // to itself.
    unsafe { R::serialize::check_unserialize_input(data) }
});
