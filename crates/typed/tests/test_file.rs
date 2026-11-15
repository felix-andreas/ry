//! The typed code file's format: random metadata reads back as written, and corrupt bytes are
//! refused with an error rather than a panic.

use {
    syntax::testing::{SplitMix64, for_each_seed},
    typed::file::testing::{check_parse_input, check_round_trip},
};

#[test]
fn fuzz_typed_code_parse() {
    for_each_seed(600, |seed| check_round_trip(&mut SplitMix64(seed)));
}

#[test]
fn arbitrary_bytes_never_panic_the_parser() {
    for_each_seed(300, |seed| {
        let mut rng = SplitMix64(seed);
        let length = (rng.next_u64() % 400) as usize;
        let mut bytes: Vec<u8> = (0..length).map(|_| rng.next_u64() as u8).collect();
        if rng.next_u64() % 2 == 0 && bytes.len() >= 8 {
            bytes[..8].copy_from_slice(b"RYTYPED\0");
        }
        check_parse_input(&bytes);
    });
}
