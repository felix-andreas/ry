#![no_main]

use libfuzzer_sys::fuzz_target;
use ry::server::simulation::run_session;
use std::env::temp_dir;
use syntax::testing::ByteStream;

// The input is the simulator's stream of choices: client options, then each editor action.
fuzz_target!(|data: &[u8]| {
    let root = temp_dir().join(format!("ry-lsp-fuzz-{}", std::process::id()));
    if let Err(failure) = run_session(&mut ByteStream::new(data), &root, 30) {
        panic!("{failure}");
    }
});
