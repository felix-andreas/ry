//! Cold-pass parallelism probe: where does the wall time go, and how much of
//! it is serialized inside single project-wide salsa queries?
//!
//! Usage: `WORKERS=4 cargo run --release -p ry-lang --example parallel_probe
//! <package-dir>...`
//!
//! Phases, each on a fresh database:
//!   A. sequential full check (one thread, every file)
//!   B. demand `interface_sccs` alone. It pulls `item_naming` for every
//!      item, so computed cold inside that one query it serializes the whole
//!      parse+naming front half of the pass on one thread
//!   C. after B, sequential full check (the remainder)
//!   D. parallel full check (the CLI's fan-out shape, without the warm pass)
//!   E. the same with a parallel per-item naming warm first, then E1-E4
//!      decomposing it (warm / interface walk / fan-out / memoized re-check)
//!   F. the E pipeline on an event-counting database: query executions vs
//!      cross-thread blocks (blocks ≪ executes means salsa's same-query
//!      blocking is not the constraint)
//!   G. interface-graph shape (node count, depth, level widths) plus a
//!      levelized dependency-first schedule, to separate DAG shape from
//!      scheduling effects
//!   H. interner scaling in isolation (hot-value hits vs distinct values)
//!
//! Caveat when reading numbers: on shared/throttled cloud vCPUs, verify raw
//! thread scaling first (phase H, or any lock-free compute loop), because a host
//! that delivers well under N cores of throughput makes every parallel
//! phase look serialized regardless of the code under test.

use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static EXECUTES: AtomicUsize = AtomicUsize::new(0);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);

/// A RootDatabase twin whose storage counts salsa events, to see how often
/// parallel workers block on each other's in-flight queries.
#[salsa::db]
#[derive(Clone)]
struct ProbeDatabase {
    storage: salsa::Storage<Self>,
    splice_cache: std::sync::Arc<semantics::SpliceCache>,
}

impl Default for ProbeDatabase {
    fn default() -> Self {
        Self {
            storage: salsa::Storage::new(Some(Box::new(|event| match event.kind {
                salsa::EventKind::WillExecute { .. } => {
                    EXECUTES.fetch_add(1, Ordering::Relaxed);
                }
                salsa::EventKind::WillBlockOn { .. } => {
                    BLOCKS.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }))),
            splice_cache: std::sync::Arc::default(),
        }
    }
}

#[salsa::db]
impl salsa::Database for ProbeDatabase {}

#[salsa::db]
impl semantics::Db for ProbeDatabase {
    fn splice_cache(&self) -> &semantics::SpliceCache {
        &self.splice_cache
    }
}

fn collect_sources(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut sources = Vec::new();
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("r"))
                && let Ok(bytes) = std::fs::read(&path)
            {
                sources.push((path, String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    sources.sort();
    sources
}

fn setup<D: semantics::Db + Default>(sources: &[(PathBuf, String)]) -> (D, Vec<SourceFile>) {
    let db = D::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|(_, source)| SourceFile::new(&db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&db, files.clone());
    (db, files)
}

fn full_check<D: semantics::Db>(db: &D, files: &[SourceFile]) -> usize {
    let mut findings = 0usize;
    for &file in files {
        findings += semantics::diagnostics::file_diagnostics(db, file).len();
    }
    findings
}

fn parallel_check<D: semantics::Db + Clone + Send>(
    db: &D,
    files: &[SourceFile],
    workers: usize,
    warm: bool,
) -> usize {
    let chunk = files.len().div_ceil(workers).max(1);
    let total = std::sync::atomic::AtomicUsize::new(0);
    if warm {
        std::thread::scope(|scope| {
            for chunk_files in files.chunks(chunk) {
                let db = db.clone();
                scope.spawn(move || {
                    for &file in chunk_files {
                        for &item in semantics::item_tree(&db, file) {
                            let _ = semantics::item_naming(&db, item);
                        }
                    }
                });
            }
        });
    }
    std::thread::scope(|scope| {
        for chunk_files in files.chunks(chunk) {
            let db = db.clone();
            let total = &total;
            scope.spawn(move || {
                let mut findings = 0usize;
                for &file in chunk_files {
                    findings += semantics::diagnostics::file_diagnostics(&db, file).len();
                }
                total.fetch_add(findings, std::sync::atomic::Ordering::Relaxed);
            });
        }
    });
    total.into_inner()
}

/// (utime, stime) of the whole process in clock ticks, from /proc/self/stat
/// (fields after the parenthesized comm; utime/stime are 12th/13th there).
fn cpu_ticks() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after_comm = stat.rsplit(')').next().unwrap_or("");
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime = fields
        .get(11)
        .and_then(|field| field.parse().ok())
        .unwrap_or(0);
    let stime = fields
        .get(12)
        .and_then(|field| field.parse().ok())
        .unwrap_or(0);
    (utime, stime)
}

fn main() {
    let roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    assert!(!roots.is_empty(), "pass package directories");
    let sources = collect_sources(&roots);
    let lines: usize = sources
        .iter()
        .map(|(_, source)| source.lines().count())
        .sum();
    println!("{} files, {} lines", sources.len(), lines);
    let workers = std::env::var("WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
        });

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let findings = full_check(&db, &files);
    let sequential = start.elapsed();
    println!("A sequential full check: {sequential:?} ({findings} findings)");

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let project = semantics::ProjectFiles::try_get(&db).expect("project installed");
    let sccs = semantics::interface_sccs(&db, project);
    let interface = start.elapsed();
    let start = Instant::now();
    let findings = full_check(&db, &files);
    let rest = start.elapsed();
    println!(
        "B interface_sccs alone: {interface:?} ({} cyclic groups)",
        sccs.groups.len()
    );
    println!("C remaining check after B: {rest:?} ({findings} findings)");

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let findings = parallel_check(&db, &files, workers, false);
    println!(
        "D parallel x{workers} (CLI shape): {:?} ({findings} findings)",
        start.elapsed()
    );

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let findings = parallel_check(&db, &files, workers, true);
    println!(
        "E parallel x{workers} + naming pre-warm: {:?} ({findings} findings)",
        start.elapsed()
    );

    // E decomposed: warm, interface, fan-out timed separately.
    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let chunk = files.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        for chunk_files in files.chunks(chunk) {
            let db = db.clone();
            scope.spawn(move || {
                for &file in chunk_files {
                    for &item in semantics::item_tree(&db, file) {
                        let _ = semantics::item_naming(&db, item);
                    }
                }
            });
        }
    });
    println!("E1 parallel naming warm: {:?}", start.elapsed());
    let start = Instant::now();
    let project = semantics::ProjectFiles::try_get(&db).expect("project installed");
    let _ = semantics::interface_sccs(&db, project);
    println!("E2 interface_sccs after warm: {:?}", start.elapsed());
    let start = Instant::now();
    let cpu_before = cpu_ticks();
    let findings = parallel_check(&db, &files, workers, false);
    let cpu_after = cpu_ticks();
    println!(
        "E3 parallel check after warm+interface: {:?} ({findings} findings, \
         user {}t sys {}t)",
        start.elapsed(),
        cpu_after.0 - cpu_before.0,
        cpu_after.1 - cpu_before.1,
    );
    let start = Instant::now();
    let findings = full_check(&db, &files);
    println!(
        "E4 sequential re-check, all memoized: {:?} ({findings} findings)",
        start.elapsed()
    );

    // F: the E pipeline on the event-counting database. How many query
    // executions, against cross-thread blocks, does the parallel check incur?
    let (db, files) = setup::<ProbeDatabase>(&sources);
    let chunk = files.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        for chunk_files in files.chunks(chunk) {
            let db = db.clone();
            scope.spawn(move || {
                for &file in chunk_files {
                    for &item in semantics::item_tree(&db, file) {
                        let _ = semantics::item_naming(&db, item);
                    }
                }
            });
        }
    });
    let project = semantics::ProjectFiles::try_get(&db).expect("project installed");
    let _ = semantics::interface_sccs(&db, project);
    let executes_before = EXECUTES.load(Ordering::Relaxed);
    let blocks_before = BLOCKS.load(Ordering::Relaxed);
    let start = Instant::now();
    let findings = parallel_check(&db, &files, workers, false);
    println!(
        "F parallel check events: {:?} ({findings} findings, {} executes, {} blocks)",
        start.elapsed(),
        EXECUTES.load(Ordering::Relaxed) - executes_before,
        BLOCKS.load(Ordering::Relaxed) - blocks_before,
    );

    // G: is the interface DAG schedulable at all? Build the same item graph
    // interface_sccs uses, condense cycles, compute topological levels, then
    // warm `global_scheme` level by level with a parallel fan-out per level.
    // If this is no faster than the naive fan-out, the critical path (a deep
    // dependency chain) is the ceiling, not scheduling.
    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let chunk = files.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        for chunk_files in files.chunks(chunk) {
            let db = db.clone();
            scope.spawn(move || {
                for &file in chunk_files {
                    for &item in semantics::item_tree(&db, file) {
                        let _ = semantics::item_naming(&db, item);
                    }
                }
            });
        }
    });
    let project = semantics::ProjectFiles::try_get(&db).expect("project installed");
    let sccs = semantics::interface_sccs(&db, project);
    let winners = semantics::package_definitions(&db, project);
    let mut nodes: Vec<semantics::Item> = Vec::new();
    let mut position = std::collections::HashMap::new();
    for &file in &files {
        for &item in semantics::item_tree(&db, file) {
            if item.name(&db).is_some() {
                position.insert(item, nodes.len());
                nodes.push(item);
            }
        }
    }
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, &item) in nodes.iter().enumerate() {
        if let Some(naming) = semantics::item_naming(&db, item) {
            for name in naming.non_locals.values() {
                if let Some(target) = winners.get(name)
                    && let Some(&target_index) = position.get(target)
                    && target_index != index
                {
                    edges[index].push(target_index);
                }
            }
        }
    }
    // Collapse cyclic groups: treat every member of one group as one node by
    // mapping members to the group's first member for level purposes.
    let mut level: Vec<usize> = vec![0; nodes.len()];
    // Longest-path levels via repeated relaxation (bounded by graph depth).
    let mut changed = true;
    let mut rounds = 0usize;
    while changed && rounds < nodes.len() {
        changed = false;
        rounds += 1;
        for index in 0..nodes.len() {
            let same_group = |a: usize, b: usize| {
                sccs.membership.contains_key(&nodes[a])
                    && sccs.membership.get(&nodes[a]) == sccs.membership.get(&nodes[b])
            };
            for &target in &edges[index] {
                if same_group(index, target) {
                    continue;
                }
                if level[index] <= level[target] {
                    level[index] = level[target] + 1;
                    changed = true;
                }
            }
        }
    }
    let depth = level.iter().copied().max().unwrap_or(0) + 1;
    let mut by_level: Vec<Vec<semantics::Item>> = vec![Vec::new(); depth];
    for (index, &item) in nodes.iter().enumerate() {
        by_level[level[index]].push(item);
    }
    let widths: Vec<usize> = by_level.iter().map(Vec::len).collect();
    println!(
        "G graph: {} items, depth {depth}, level widths (first 12): {:?}",
        nodes.len(),
        &widths[..widths.len().min(12)]
    );
    for level_items in &by_level {
        let chunk = level_items.len().div_ceil(workers).max(1);
        std::thread::scope(|scope| {
            for chunk_items in level_items.chunks(chunk) {
                let db = db.clone();
                scope.spawn(move || {
                    for &item in chunk_items {
                        let _ = semantics::global_scheme(&db, item);
                    }
                });
            }
        });
    }
    let schemes_done = start.elapsed();
    let findings = parallel_check(&db, &files, workers, false);
    println!(
        "G levelized schemes then fan-out: schemes {:?}, total {:?} ({findings} findings)",
        schemes_done,
        start.elapsed()
    );

    // H: interner scaling in isolation. H1 hammers one hot value (every
    // `unknown()` in the checker takes this exact path); H2 interns values
    // distinct per thread. Ops/s per thread tells whether the interner's
    // shard locks serialize the check.
    let (db, _files) = setup::<RootDatabase>(&sources);
    const OPS: usize = 2_000_000;
    for (label, distinct) in [
        ("H1 hot-value interns", false),
        ("H2 distinct interns", true),
    ] {
        for threads in [1usize, workers] {
            let start = Instant::now();
            std::thread::scope(|scope| {
                for thread in 0..threads {
                    let db = db.clone();
                    scope.spawn(move || {
                        for i in 0..OPS / threads {
                            if distinct {
                                let name = semantics::types::Name::new(
                                    &db,
                                    format!("t{thread}_{}", i % 4096),
                                );
                                let _ = semantics::types::Ty::new(
                                    &db,
                                    semantics::types::TyKind::Named(name, Vec::new()),
                                );
                            } else {
                                let _ = semantics::types::unknown(&db);
                            }
                        }
                    });
                }
            });
            let elapsed = start.elapsed();
            println!(
                "{label} x{threads}: {elapsed:?} ({:.1}M ops/s)",
                OPS as f64 / elapsed.as_secs_f64() / 1e6
            );
        }
    }
}
