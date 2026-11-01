#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        format::check_format_invariants(input);
        format::check_range_format_invariants(input);
    }
});
