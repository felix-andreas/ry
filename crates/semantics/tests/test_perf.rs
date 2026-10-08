//! Performance budgets over the real-world corpus, which
//! `scripts/fetch-corpus.rs` populates into the gitignored `corpus/`. Ignored
//! by default because it needs the corpus and a release build:
//!
//! ```text
//! cargo test -p semantics --release --test test_perf -- --ignored --nocapture
//! ```
//!
//! One test, so the resident-set reading is not polluted by a sibling test
//! thread. `ry debug analysis-stats` is the per-phase diagnosis for one
//! workspace; this is the regression tripwire across all of them.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use salsa::Setter;
use semantics::diagnostics::{file_diagnostics, strict_diagnostics};
use semantics::infer::RESOLVE_CALLS;
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

/// Budgets are the measured numbers with headroom: wall ~25 µs and resident
/// ~1 KiB per line, ~8 resolve steps per line (the memoization tripwire), and
/// keystroke-to-diagnostics p50 ≤ 30 ms / p95 ≤ 100 ms on the largest package.
#[test]
#[ignore = "needs the fetched corpus and a release build"]
fn corpus_budgets() {
    let packages = corpus_packages();
    assert!(!packages.is_empty(), "run scripts/fetch-corpus.rs first");

    // Every package fully analyzed and retained: the warm worst case.
    let lines: usize = packages
        .iter()
        .flatten()
        .map(|source| source.lines().count())
        .sum();
    let start = Instant::now();
    let mut retained = Vec::new();
    for sources in &packages {
        let db = RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&db);
        let files = package_files(&db, sources);
        for &file in &files {
            file_diagnostics(&db, file);
            strict_diagnostics(&db, file);
        }
        retained.push(db);
    }
    let elapsed = start.elapsed();
    let per_line = |value: f64| value / lines.max(1) as f64;
    let microseconds = per_line(elapsed.as_secs_f64() * 1e6);
    let steps = per_line(RESOLVE_CALLS.load(Ordering::Relaxed) as f64);
    let resident = proc_status_kb("VmRSS").map(|kb| per_line(kb as f64 * 1024.0));
    println!(
        "cold: {} packages, {lines} lines, {:.2}s ({microseconds:.1} µs/line), \
         {steps:.1} resolve steps/line, {} bytes/line resident",
        packages.len(),
        elapsed.as_secs_f64(),
        resident.map_or("?".to_owned(), |bytes| format!("{bytes:.0}")),
    );
    assert!(microseconds <= 40.0, "cold-pass wall budget exceeded");
    assert!(
        steps <= 20.0,
        "resolve-step budget exceeded (memoization regression)"
    );
    assert!(
        resident.is_none_or(|bytes| bytes <= 2048.0),
        "resident-set budget exceeded"
    );
    drop(retained);

    // Typing at the end of the largest file of the largest package: each
    // keystroke is one setter write plus that file's diagnostics, the server's
    // per-edit work. A trailing comment shifts no item, so identities hold.
    let sources = packages
        .iter()
        .max_by_key(|sources| sources.iter().map(String::len).sum::<usize>())
        .expect("the corpus is not empty");
    let mut db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files = package_files(&db, sources);
    for &file in &files {
        file_diagnostics(&db, file);
    }
    let (largest, mut text) = files
        .iter()
        .zip(sources)
        .max_by_key(|(_, source)| source.len())
        .map(|(file, source)| (*file, source.clone()))
        .expect("the package is not empty");
    let mut latencies: Vec<Duration> = (0..50)
        .map(|_| {
            text.push('#');
            let start = Instant::now();
            largest.set_text(&mut db).to(text.clone());
            file_diagnostics(&db, largest);
            start.elapsed()
        })
        .collect();
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[latencies.len() * 95 / 100];
    // The from-scratch parse is the floor every keystroke pays; read the
    // latencies against it, since machine load swings both.
    let parse_start = Instant::now();
    syntax::parse(&text);
    println!(
        "keystrokes: {} lines in the edited file, p50 {:.2} ms, p95 {:.2} ms, raw parse {:.2} ms",
        text.lines().count(),
        p50.as_secs_f64() * 1e3,
        p95.as_secs_f64() * 1e3,
        parse_start.elapsed().as_secs_f64() * 1e3,
    );
    assert!(
        p50 <= Duration::from_millis(30),
        "keystroke p50 budget exceeded"
    );
    assert!(
        p95 <= Duration::from_millis(100),
        "keystroke p95 budget exceeded"
    );
}

fn package_files(db: &RootDatabase, sources: &[String]) -> Vec<SourceFile> {
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(db, files.clone());
    files
}

fn proc_status_kb(key: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// The corpus grouped per package: each directory under `r-base/` and
/// `cran/` is one package.
fn corpus_packages() -> Vec<Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut packages = Vec::new();
    for top in ["r-base", "cran"] {
        let Ok(entries) = std::fs::read_dir(root.join(top)) else {
            continue;
        };
        let mut directories: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        directories.sort();
        for directory in directories {
            let mut paths = Vec::new();
            collect_r_files(&directory, &mut paths);
            paths.sort();
            let sources: Vec<String> = paths
                .iter()
                .filter_map(|path| std::fs::read(path).ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .collect();
            if !sources.is_empty() {
                packages.push(sources);
            }
        }
    }
    packages
}

fn collect_r_files(directory: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_r_files(&path, paths);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "R" | "r" | "S" | "s" | "q"))
        {
            paths.push(path);
        }
    }
}
