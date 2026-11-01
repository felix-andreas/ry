#![no_main]

use libfuzzer_sys::fuzz_target;

use syntax::testing::ByteStream;

// The input is the generator's stream of choices: the package, then its edits.
fuzz_target!(|data: &[u8]| {
    loader::testing::check_random_reload(&mut ByteStream::new(data));
});
