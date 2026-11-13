//! Each case runs this test binary again as a child that crashes on purpose, then checks the
//! report on its standard error and the signal that ended it.
#![cfg(unix)]

use {
    crash::{Emit, Readable},
    std::{os::unix::process::ExitStatusExt, process::Command},
};

const CASE_VARIABLE: &str = "CRASH_TEST_CASE";

#[test]
fn child() {
    let Ok(case) = std::env::var(CASE_VARIABLE) else {
        return;
    };
    crash::install("ry 9.9.9 (test)");
    match case.as_str() {
        "null_write" => unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u8>(), 1) },
        "kill_segv" => unsafe { assert_eq!(libc::kill(libc::getpid(), libc::SIGSEGV), 0) },
        "abort" => unsafe { libc::abort() },
        "stack_overflow" => {
            fn recurse(depth: u64) -> u64 {
                let padding = [depth as u8; 512];
                std::hint::black_box(&padding);
                recurse(depth + 1) + u64::from(padding[0])
            }
            std::hint::black_box(recurse(0));
        }
        "bus" => bus_error(),
        "traceback" => {
            crash::set_traceback(fake_traceback);
            unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u8>(), 1) }
        }
        "faulting_traceback" => {
            crash::set_traceback(faulting_traceback);
            unsafe { libc::abort() }
        }
        other => panic!("unknown case {other}"),
    }
}

unsafe extern "C" fn fake_traceback(emit: Emit, readable: Readable) {
    let text = b"\nTraceback:\n 1: inner()\n 2: outer()\n";
    unsafe { emit(text.as_ptr(), text.len()) };
    let bad = 8 as *const u8;
    let verdict: &[u8] = if unsafe { readable(bad, 8) } {
        b"bad address readable\n"
    } else {
        b"bad address refused\n"
    };
    unsafe { emit(verdict.as_ptr(), verdict.len()) };
}

unsafe extern "C" fn faulting_traceback(_: Emit, _: Readable) {
    unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u8>(), 1) };
}

fn bus_error() {
    use std::{fs::OpenOptions, os::fd::AsRawFd};
    let path = std::env::temp_dir().join(format!("crash-bus-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("temp file");
    file.set_len(4096).expect("size");
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(map, libc::MAP_FAILED);
    file.set_len(0).expect("shrink");
    std::fs::remove_file(&path).expect("remove");
    unsafe { std::ptr::write_volatile(map.cast::<u8>(), 1) };
}

struct Crashed {
    stderr: String,
    signal: Option<i32>,
}

fn crash(case: &str) -> Crashed {
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CASE_VARIABLE, case)
        .output()
        .expect("child runs");
    Crashed {
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        signal: output.status.signal(),
    }
}

#[test]
fn a_fault_reports_the_address_and_cause_and_dies_by_the_signal() {
    let crashed = crash("null_write");
    assert!(
        crashed
            .stderr
            .contains("*** ry caught SIGSEGV (segmentation fault) ***")
    );
    assert!(crashed.stderr.contains("ry 9.9.9 (test)"));
    assert!(
        crashed
            .stderr
            .contains("address 0x0, cause 'memory not mapped'")
    );
    assert!(crashed.stderr.contains("Native stack"));
    assert!(crashed.stderr.contains("crash"), "{}", crashed.stderr);
    assert_eq!(crashed.signal, Some(libc::SIGSEGV), "{}", crashed.stderr);
}

#[test]
fn a_signal_sent_by_a_process_is_reported_and_still_kills() {
    let crashed = crash("kill_segv");
    assert!(
        crashed
            .stderr
            .contains("raised by a call to kill, raise or abort")
    );
    assert_eq!(crashed.signal, Some(libc::SIGSEGV), "{}", crashed.stderr);
}

#[test]
fn an_abort_is_reported() {
    let crashed = crash("abort");
    assert!(crashed.stderr.contains("*** ry caught SIGABRT (abort) ***"));
    assert_eq!(crashed.signal, Some(libc::SIGABRT), "{}", crashed.stderr);
}

#[test]
fn a_bus_error_is_reported() {
    let crashed = crash("bus");
    assert!(
        crashed
            .stderr
            .contains("*** ry caught SIGBUS (bus error) ***")
    );
    assert_eq!(crashed.signal, Some(libc::SIGBUS), "{}", crashed.stderr);
}

#[test]
fn a_stack_overflow_is_reported_and_keeps_rusts_message() {
    let crashed = crash("stack_overflow");
    assert!(
        crashed.stderr.contains("*** ry caught SIGSEGV"),
        "{}",
        crashed.stderr
    );
    assert!(
        crashed.stderr.contains("has overflowed its stack"),
        "{}",
        crashed.stderr
    );
    assert_eq!(crashed.signal, Some(libc::SIGABRT), "{}", crashed.stderr);
}

#[test]
fn the_registered_r_traceback_comes_before_the_native_stack() {
    let crashed = crash("traceback");
    let traceback = crashed.stderr.find("1: inner()").expect(&crashed.stderr);
    let native = crashed.stderr.find("Native stack").expect(&crashed.stderr);
    assert!(traceback < native);
    assert!(crashed.stderr.contains("bad address refused"));
    assert_eq!(crashed.signal, Some(libc::SIGSEGV));
}

#[test]
fn a_fault_while_printing_the_traceback_still_prints_the_native_stack() {
    let crashed = crash("faulting_traceback");
    assert!(
        crashed.stderr.contains("could not be read to the end"),
        "{}",
        crashed.stderr
    );
    assert!(crashed.stderr.contains("Native stack"));
    assert_eq!(crashed.signal, Some(libc::SIGSEGV), "{}", crashed.stderr);
}
