//! Budgets that need a release build, so they are ignored by default: the cold check of the
//! three synthetic benchmark code bases, which generates about 900,000 lines, and the peak memory
//! and speed of scripts on ry's runtime against GNU R's (including the typed `array_dot`
//! benchmark's peak), which need `libR` built beside the
//! binary and a GNU R installation, and skip without either:
//!
//! ```sh
//! cargo build --release -p ry-lang -p libr
//! cargo test -p ry-lang --release --test test_perf -- --ignored --nocapture
//! ```
//!
//! One test runs by default: the benchmark runner's refusal to time a start that could not use
//! its R home's images, which skips without `libR` beside the binary or a built base R tree.

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
        .arg(workspace.join("tools/bench/tooling/generate.py"))
        .args(["--lines", "300000", "--files", "500", "--out"])
        .arg(out.path())
        .stdout(Stdio::null())
        .status()
        .expect("python3 runs the generator");
    assert!(status.success(), "tools/bench/tooling/generate.py failed");

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
    // ry's runtime has an R home of its own, so it sees packages installed for GNU R, such as
    // data.table in the site library, only through GNU R's library paths. Both engines then
    // load the same build of each package.
    let libraries = Command::new("Rscript")
        .args(["-e", "cat(.libPaths(), sep = ':')"])
        .output()
        .expect("ask GNU R for its library paths");
    let libraries = String::from_utf8_lossy(&libraries.stdout).into_owned();
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut over = Vec::new();
    for (name, body) in RUNTIME_CASES {
        let script = directory.path().join(format!("{name}.R"));
        fs::write(&script, format!("{body}{PRINT_PEAK}")).expect("write the script");
        let Some(gnu_r) = script_peak(Command::new("Rscript").arg(&script)) else {
            eprintln!("{name}: skipped, it does not run on GNU R here");
            continue;
        };
        let ry = script_peak(
            Command::new(binary)
                .arg("run")
                .arg(&script)
                .env("R_LIBS", &libraries),
        )
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

/// The most the typed `array_dot` benchmark's peak resident set may exceed the untyped program's
/// on GNU R. Both hold two million doubles. The typed program once peaked at more than twice
/// GNU R's: `List.reverse` boxed each of a million items, and its first call ran on tier 0
/// because the typed core refused a literal `I64` argument, so every `x[[i]]` boxed a double.
const TYPED_ARRAY_DOT_PEAK_RATIO: f64 = 1.25;

#[test]
#[ignore = "needs a release build with libR beside it; run in release with --ignored"]
fn typed_array_dot_peak_memory_stays_near_gnu_r() {
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
    let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench");
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = |typed: bool| {
        let (variant, harness, program) = if typed {
            ("typed", "harness.typed.R", "kernels/array_dot.typed.R")
        } else {
            ("untyped", "harness.R", "kernels/array_dot.R")
        };
        let folder = directory.path().join(variant);
        fs::create_dir_all(&folder).expect("create the program's directory");
        let harness = fs::read_to_string(bench.join(harness)).expect("read the harness");
        let program = fs::read_to_string(bench.join(program)).expect("read array_dot");
        // The typed program runs in strict mode, as tools/bench/bench.py runs it.
        if typed {
            fs::write(
                folder.join("ry.toml"),
                "[check]\ntyping = true\nstrict = true\n",
            )
            .expect("write ry.toml");
        }
        let script = folder.join("array_dot.R");
        fs::write(&script, format!("{harness}\n{program}")).expect("write the script");
        script
    };
    let peak = |command: &mut Command| {
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("run the program");
        let (exit, peak) = wait_with_peak(child);
        assert_eq!(exit, 0, "the program failed: {command:?}");
        peak
    };
    let gnu_r = peak(Command::new("Rscript").arg("--vanilla").arg(script(false)));
    let ry = peak(
        Command::new(binary)
            .args(["run", "--runtime", "ry"])
            .arg(script(true)),
    );
    let ratio = ry as f64 / gnu_r as f64;
    eprintln!(
        "array_dot: peak {} MiB typed on ry, {} MiB untyped on GNU R ({ratio:.2}x)",
        ry / 1024,
        gnu_r / 1024
    );
    assert!(
        ratio <= TYPED_ARRAY_DOT_PEAK_RATIO,
        "typed array_dot peaked at {} MiB, over {TYPED_ARRAY_DOT_PEAK_RATIO} times GNU R's {} MiB",
        ry / 1024,
        gnu_r / 1024
    );
}

/// The least speedup over GNU R each benchmark program from `tools/bench` keeps,
/// untyped and warm. Each sits a fifth or more below what it measures, since timings on a
/// shared machine vary that much, and each is a program the
/// runtime's allocation policy once slowed down without failing a memory budget: the vector
/// arithmetic and the `1:n` loop page-faulted in a fresh mapping for every large temporary, and
/// the data frame pipeline ran a major collection per call under too tight a vector limit.
const RUNTIME_SPEEDUPS: [(&str, f64); 3] =
    [("vector_arith", 0.85), ("loop_sum", 3.0), ("sales", 1.8)];

#[test]
#[ignore = "needs a release build with libR beside it; run in release with --ignored"]
fn runtime_speed_stays_ahead_of_gnu_r() {
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
    let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench");
    let harness = fs::read_to_string(bench.join("harness.R")).expect("read the harness");
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut slow = Vec::new();
    for (name, floor) in RUNTIME_SPEEDUPS {
        let program = ["kernels", "applications"]
            .iter()
            .find_map(|suite| fs::read_to_string(bench.join(suite).join(format!("{name}.R"))).ok())
            .unwrap_or_else(|| panic!("no benchmark program {name}.R"));
        let script = directory.path().join(format!("{name}.R"));
        fs::write(&script, format!("{harness}\n{program}")).expect("write the script");
        // Other load on the machine only slows a run down, so the fastest of three is the time.
        let fastest = |command: &dyn Fn() -> Command| {
            (0..3)
                .map(|_| warm_seconds(&mut command()))
                .fold(f64::INFINITY, f64::min)
        };
        let gnu_r = fastest(&|| {
            let mut command = Command::new("Rscript");
            command.arg("--vanilla").arg(&script);
            command
        });
        let ry = fastest(&|| {
            let mut command = Command::new(binary);
            command.arg("run").arg(&script);
            command
        });
        let speedup = gnu_r / ry;
        eprintln!("{name}: {ry:.3} s on ry, {gnu_r:.3} s on GNU R ({speedup:.2}x)");
        if speedup < floor {
            slow.push(format!("{name} at {speedup:.2}x, below {floor}x"));
        }
    }
    assert!(
        slow.is_empty(),
        "ry's runtime fell behind its speedup over GNU R: {}",
        slow.join(", ")
    );
}

/// A start-up or code image loads only into the build that wrote it, and a build that cannot load
/// one starts the slow way. `tools/bench/bench.py` must fail a timed process that reports such an
/// image, and refuse up front a build it cannot give a base R tree of its own.
#[test]
fn bench_refuses_a_start_from_another_builds_images() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary = Path::new(env!("CARGO_BIN_EXE_ry"));
    let runtime = binary.with_file_name("libR.so");
    let tree = workspace.join("target/base-r");
    let start_image = Path::new("library/base/R/ry-start.image");
    let code_image = Path::new("library/stats/R/stats.ryc");
    if !runtime.exists() || !tree.join(start_image).is_file() || !tree.join(code_image).is_file() {
        eprintln!("skipped: needs libR beside the binary and target/base-r (`just base-r`)");
        return;
    }
    // Hard links keep the copy of the tree cheap, so it lives in the target directory, on the
    // tree's file system; each image the test alters is written as a new file.
    let directory = tempfile::tempdir_in(workspace.join("target")).expect("temporary directory");
    let copy = directory.path().join("base-r");
    let status = Command::new("cp")
        .arg("-al")
        .arg(&tree)
        .arg(&copy)
        .status()
        .expect("cp runs");
    assert!(status.success(), "cp -al {} failed", tree.display());
    // Assembling an R home rewrites the tree's start-up image and base code images when another
    // build wrote them, so the home is assembled first, and the images are altered after it, as
    // when a second build's home rewrites a tree the first build's home already links.
    let cache = directory.path().join("cache");
    fs::write(
        directory.path().join("probe.R"),
        "invisible(stats::median(c(3, 1, 2)))\n",
    )
    .expect("write the probe");
    let assembled = Command::new(binary)
        .args(["run", "--runtime", "ry", "probe.R"])
        .current_dir(directory.path())
        .env("RY_BASE_R", &copy)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("ry runs");
    assert!(assembled.status.success(), "ry run failed: {assembled:?}");
    // Each image opens with an eight-byte magic number; the start-up image then gives its header's
    // length, and both then give the identity of the build that wrote them as a length and bytes.
    for (image, identity_byte) in [(start_image, 24), (code_image, 16)] {
        let mut bytes = fs::read(copy.join(image)).expect("read the image");
        bytes[identity_byte] ^= 0x55;
        fs::remove_file(copy.join(image)).expect("unlink the image's hard link");
        fs::write(copy.join(image), bytes).expect("write the altered image");
    }
    // A build without `ry-rcmd` beside it, which bench.py cannot build a base R tree for.
    let lone = directory.path().join("lone");
    fs::create_dir(&lone).expect("create a directory");
    fs::hard_link(binary, lone.join("ry")).expect("link the binary");
    let script = r#"
import pathlib, sys
sys.path.insert(0, "tools/bench")
import bench
work = pathlib.Path(sys.argv[3])
bench.BENCH_TREES = work / "trees"
env = dict(bench.os.environ, RY_IMAGE_LOG="1")
status, out, err, _, _ = bench.spawn([sys.argv[1], "run", "--runtime", "ry", "probe.R"], env, cwd=work)
print(f"timed run: status {status}: {err}")
bench.own_base_trees([sys.argv[2]])
"#;
    let output = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(binary)
        .arg(lone.join("ry"))
        .arg(directory.path())
        .current_dir(&workspace)
        .env("RY_BASE_R", &copy)
        .env("RY_LIBR", &runtime)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("python3 runs bench.py");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("{stdout}{stderr}");
    assert!(
        stdout.contains(
            "timed run: status None: started without an image it should load (ry: image unused: "
        ) && stdout.contains("was written by another build of the runtime"),
        "a timed process that could not use an image must fail"
    );
    assert!(
        !output.status.success()
            && stderr.contains("cannot use the images of the base R tree it finds")
            && stderr.contains("cannot build it a tree of its own without ry-rcmd and libR.so"),
        "a build bench.py cannot give a base R tree must be refused"
    );
}

/// The best warm time a benchmark program prints, as `name checksum median best cold`.
fn warm_seconds(command: &mut Command) -> f64 {
    let output = command
        .stderr(Stdio::null())
        .output()
        .expect("run the program");
    assert!(output.status.success(), "the program failed: {command:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or_else(|| panic!("no timing line from {command:?}"))
}

/// The peak resident set in KiB a script prints as its last line, or `None` when it fails.
fn script_peak(command: &mut Command) -> Option<u64> {
    let output = command
        .stderr(Stdio::null())
        .output()
        .expect("run the script");
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

/// Loop bodies whose tier-0 instruction count per iteration must stay within a ratio of GNU R's,
/// each as `(name, start, body, ratio)`: `x` starts as `start`, and `body` grows it by one
/// element. A subscript that calls a function, such as `length(x) + 1L`, must stay in bytecode,
/// because the tree walker's complex assignment costs more than five times GNU R's.
const TIER0_INSTRUCTION_CASES: [(&str, &str, &str, f64); 2] = [
    ("append_length", "numeric(0)", "x[length(x) + 1L] <- k", 1.5),
    (
        "append_length_list",
        "list()",
        "x[[length(x) + 1L]] <- k",
        1.5,
    ),
];

#[test]
#[ignore = "needs a release build with libR beside it and valgrind; run in release with --ignored"]
fn tier0_computed_subscript_stores_stay_near_gnu_r() {
    let binary = Path::new(env!("CARGO_BIN_EXE_ry"));
    if !binary.with_file_name("libR.so").exists() {
        eprintln!("skipped: ry's runtime library is not built beside the binary");
        return;
    }
    let present = |program: &str| {
        Command::new(program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    };
    if !present("Rscript") || !present("valgrind") {
        eprintln!("skipped: this test needs GNU R and valgrind");
        return;
    }
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut over = Vec::new();
    for (name, start, body, ratio) in TIER0_INSTRUCTION_CASES {
        // The difference between two loop lengths cancels start-up and the loop's warm-up.
        let per_iteration = |command: &dyn Fn(&Path) -> Command| {
            let [short, long] = [20_000, 40_000].map(|count| {
                let script = directory.path().join(format!("{name}_{count}.R"));
                fs::write(
                    &script,
                    format!(
                        "f <- function(n) {{ x <- {start}; for (k in seq_len(n)) {body}; length(x) }}\n\
                         invisible(f(10L)); invisible(f(10L)); invisible(f({count}L))\n"
                    ),
                )
                .expect("write the script");
                callgrind_instructions(&mut command(&script))
            });
            long.saturating_sub(short) / 20_000
        };
        let gnu_r = per_iteration(&|script| {
            let mut command = Command::new("Rscript");
            command.arg("--vanilla").arg(script);
            command
        });
        let ry = per_iteration(&|script| {
            let mut command = Command::new(binary);
            command
                .args(["run", "--runtime", "ry"])
                .arg(script)
                .env("RY_JIT", "off");
            command
        });
        let measured = ry as f64 / gnu_r as f64;
        eprintln!(
            "{name}: {ry} instructions per iteration on tier 0, {gnu_r} on GNU R ({measured:.2}x)"
        );
        if measured > ratio {
            over.push(format!("{name} at {measured:.2}x, over {ratio}x"));
        }
    }
    assert!(
        over.is_empty(),
        "tier 0 fell behind GNU R's instruction count: {}",
        over.join(", ")
    );
}

/// The instructions callgrind counts for a command and every process it starts, since `Rscript`
/// runs R in a child.
fn callgrind_instructions(command: &mut Command) -> u64 {
    let program = command.get_program().to_owned();
    let arguments: Vec<_> = command.get_args().map(ToOwned::to_owned).collect();
    let mut valgrind = Command::new("valgrind");
    valgrind
        .args([
            "--tool=callgrind",
            "--trace-children=yes",
            "--callgrind-out-file=/dev/null",
        ])
        .arg(program)
        .args(arguments)
        .envs(
            command
                .get_envs()
                .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned()))),
        )
        .stdout(Stdio::null());
    let output = valgrind.output().expect("run valgrind");
    assert!(
        output.status.success(),
        "the script failed under valgrind: {command:?}"
    );
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| line.split("Collected : ").nth(1))
        .filter_map(|count| count.trim().parse::<u64>().ok())
        .sum()
}
