#![no_main]

use libfuzzer_sys::fuzz_target;

use syntax::testing::ByteStream;

// The input is the generator's stream of choices, so a byte mutation is a program mutation.
fuzz_target!(|data: &[u8]| {
    interp::testing::check_bytecode_agreement(&mut ByteStream::new(data));
});
