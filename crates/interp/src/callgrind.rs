//! Callgrind's client requests, so a benchmark region marked by `.ry_bench_region` gets its own
//! instruction count when the process runs under `valgrind --tool=callgrind`.
//!
//! A client request is a fixed sequence of instructions that does nothing on a real CPU and
//! that valgrind's translator recognizes: four rotates of one register by a total of 128 bits,
//! then a marker instruction, with the address of the request's six words in a fixed register.
//! The sequences below are those of `valgrind.h`; outside valgrind they run as no-ops, and on
//! other architectures the requests do nothing.

use std::ffi::CStr;

/// `VG_USERREQ_TOOL_BASE('C', 'T')`, the first of callgrind's requests in `callgrind.h`.
const BASE: usize = (b'C' as usize) << 24 | (b'T' as usize) << 16;
const ZERO_STATS: usize = BASE + 1;
const DUMP_STATS_AT: usize = BASE + 3;
const START_INSTRUMENTATION: usize = BASE + 4;
const STOP_INSTRUMENTATION: usize = BASE + 5;

/// Starts a region: discards the counts so far and turns instrumentation on.
pub fn start_region() {
    request(ZERO_STATS, 0);
    request(START_INSTRUMENTATION, 0);
}

/// Ends a region: turns instrumentation off and writes the counts as a dump named `name`.
pub fn end_region(name: &CStr) {
    request(STOP_INSTRUMENTATION, 0);
    request(DUMP_STATS_AT, name.as_ptr() as usize);
}

#[cfg(target_arch = "x86_64")]
fn request(code: usize, argument: usize) {
    let words: [usize; 6] = [code, argument, 0, 0, 0, 0];
    // SAFETY: the rotates leave `rdi` as it was and `xchg rbx, rbx` changes nothing; under
    // valgrind the request only reads the six words.
    unsafe {
        std::arch::asm!(
            "rol rdi, 3",
            "rol rdi, 13",
            "rol rdi, 61",
            "rol rdi, 51",
            "xchg rbx, rbx",
            in("rax") words.as_ptr(),
            inout("rdx") 0usize => _,
            inout("rdi") 0usize => _,
            options(nostack),
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn request(code: usize, argument: usize) {
    let words: [usize; 6] = [code, argument, 0, 0, 0, 0];
    // SAFETY: the rotates leave `x12` as it was and `orr x10, x10, x10` changes nothing; under
    // valgrind the request only reads the six words.
    unsafe {
        std::arch::asm!(
            "ror x12, x12, #3",
            "ror x12, x12, #13",
            "ror x12, x12, #51",
            "ror x12, x12, #61",
            "orr x10, x10, x10",
            in("x4") words.as_ptr(),
            inout("x3") 0usize => _,
            inout("x12") 0usize => _,
            options(nostack),
        );
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn request(_code: usize, _argument: usize) {}
