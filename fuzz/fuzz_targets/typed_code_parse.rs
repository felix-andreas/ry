#![no_main]

use libfuzzer_sys::fuzz_target;

use syntax::testing::ByteStream;

// Half the inputs are raw bytes for the parser; the other half drive the generator of valid
// metadata, which corrupts what it wrote, so a byte mutation reaches deep into the format.
fuzz_target!(|data: &[u8]| {
    match data.split_first() {
        Some((selector, rest)) if selector % 2 == 0 => typed::file::testing::check_parse_input(rest),
        _ => typed::file::testing::check_round_trip(&mut ByteStream::new(data)),
    }
});
