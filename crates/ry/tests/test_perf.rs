//! Memory budgets that need a release build, so they are ignored by default: the cold check of
//! the three synthetic benchmark code bases, which generates about 900,000 lines, and the peak
//! memory of scripts on ry's runtime against GNU R's, which needs `libR` built beside the binary
//! and a GNU R installation, and skips without either:
//!
//! ```sh
//! cargo build --release -p ry-lang -p libr
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

/// The most a script's peak resident set on ry's runtime may exceed GNU R's.
const RUNTIME_PEAK_RATIO: f64 = 1.10;

/// Scripts that allocate and drop large vectors, from a report of CI machines killed for memory.
/// Each prints its own peak resident set in KiB as its last line.
const RUNTIME_CASES: [(&str, &str); 3] = [
    ("rep", "id <- rep(1:1000, 50000)\ninvisible(gc())\n"),
    (
        "subset",
        "v <- runif(5e7)\nid <- rep(1:1000, 50000)\ninvisible(gc())\n\
         for (i in 1:8) {\n  keep <- v > 0.5\n  s <- list(id[keep], v[keep])\n  rm(s, keep)\n}\n",
    ),
    (
        "datatable",
        "if (!requireNamespace(\"data.table\", quietly = TRUE)) quit(status = 3)\n\
         library(data.table)\n\
         dt <- data.table(id = rep(1:1000, 5000), date = rep(seq_len(5000), each = 1000), v = runif(5e6))\n\
         invisible(gc())\nfor (i in 1:20) r <- dt[v > 0.5]\n",
    ),
];

const PRINT_PEAK: &str = "line <- grep(\"^VmHWM\", readLines(\"/proc/self/status\"), value = TRUE)\n\
                          cat(gsub(\"\\\\D\", \"\", line), \"\\n\")\n";

#[test]
#[ignore = "needs a release build with libR beside it; run in release with --ignored"]
fn runtime_peak_memory_stays_near_gnu_r() {
    let binary = Path::new(env!("CARGO_BIN_EXE_ry"));
    if !binary.with_file_name("libR.so").exists() {
        eprintln!("skipped: ry's runtime library is not built beside the binary");
        return;
    }
    if !Command::new("Rscript")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("skipped: no GNU R installation on this machine");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut over = Vec::new();
    for (name, body) in RUNTIME_CASES {
        let script = directory.path().join(format!("{name}.R"));
        fs::write(&script, format!("{body}{PRINT_PEAK}")).expect("write the script");
        let Some(gnu_r) = script_peak(Command::new("Rscript").arg(&script)) else {
            eprintln!("{name}: skipped, it does not run on GNU R here");
            continue;
        };
        let ry = script_peak(Command::new(binary).arg("run").arg(&script))
            .unwrap_or_else(|| panic!("{name} runs on GNU R but fails on ry's runtime"));
        let ratio = ry as f64 / gnu_r as f64;
        eprintln!(
            "{name}: peak {} MiB on ry, {} MiB on GNU R ({ratio:.2}x)",
            ry / 1024,
            gnu_r / 1024
        );
        if ratio > RUNTIME_PEAK_RATIO {
            over.push(format!(
                "{name} peaked at {} MiB against GNU R's {} MiB",
                ry / 1024,
                gnu_r / 1024
            ));
        }
    }
    assert!(
        over.is_empty(),
        "ry's runtime over {RUNTIME_PEAK_RATIO} times GNU R's peak: {}",
        over.join(", ")
    );
}

/// The peak resident set in KiB a script prints as its last line, or `None` when it fails.
fn script_peak(command: &mut Command) -> Option<u64> {
    let output = command.stderr(Stdio::null()).output().expect("run the script");
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()?
        .trim()
        .parse()
        .ok()
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
