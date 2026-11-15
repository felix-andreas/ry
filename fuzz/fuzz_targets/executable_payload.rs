#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ry::executable::testing::check_parse_input(data);
    ry::executable::testing::check_round_trip(data);
});
