//! The cold-check memory budget over the three synthetic benchmark code bases. Ignored by
//! default, because it generates about 900,000 lines and needs a release build:
//!
//! ```sh
//! cargo test -p ry-lang --release --test test_perf -- --ignored --nocapture
//! ```

#![cfg(unix)]

use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

/// Peak resident set of one cold `ry check --no-cache` of a code base. Measured at 222-228 MiB
/// on the generated benches, so a feature that grows memory by a few percent shows up here first.
const PEAK_RESIDENT_KIB: i64 = 256 * 1024;

const BENCHES: [&str; 3] = ["finbench", "dsbench", "biobench"];

#[test]
#[ignore = "generates the benchmark code bases; run in release with --ignored"]
fn cold_check_memory_budget() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = tempfile::tempdir().expect("temporary directory");
    let status = Command::new("python3")
        .arg("-I")
        .arg(workspace.join("tools/bench/generate.py"))
        .args(["--lines", "300000", "--files", "500", "--out"])
        .arg(out.path())
        .stdout(Stdio::null())
        .status()
        .expect("python3 runs the generator");
    assert!(status.success(), "tools/bench/generate.py failed");

    let mut over = Vec::new();
    for bench in BENCHES {
        let root = out.path().join(bench);
        // Type errors are opt-in, and the budget covers the type checker's memory too.
        fs::write(root.join("ry.toml"), "[check]\ntyping = true\n").expect("write ry.toml");
        let started = Instant::now();
        let child = Command::new(env!("CARGO_BIN_EXE_ry"))
            .args(["check", "--no-cache", "."])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ry check");
        let (exit, peak) = wait_with_peak(child);
        let elapsed = started.elapsed();
        assert!(
            matches!(exit, 0 | 1),
            "ry check exited with status {exit} on {bench}"
        );
        eprintln!(
            "{bench}: peak resident {} MiB, wall {:.2} s",
            peak / 1024,
            elapsed.as_secs_f64()
        );
        if peak > PEAK_RESIDENT_KIB {
            over.push(format!("{bench} peaked at {} MiB", peak / 1024));
        }
    }
    assert!(
        over.is_empty(),
        "cold check over its {} MiB budget: {}",
        PEAK_RESIDENT_KIB / 1024,
        over.join(", ")
    );
}

/// Reaps the child with `wait4` and reads its own peak resident set in KiB. `Child::wait` cannot
/// return resource usage, and `RUSAGE_CHILDREN` would report the largest child the test process
/// ever waited for, not this one.
fn wait_with_peak(child: Child) -> (i32, i64) {
    let pid = libc::pid_t::try_from(child.id()).expect("pid fits pid_t");
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value, and `wait4` writes only through the two
    // pointers, which live for the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let reaped = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    assert_eq!(
        reaped,
        pid,
        "wait4 failed: {}",
        std::io::Error::last_os_error()
    );
    let exit = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    (exit, usage.ru_maxrss)
}
