//! Cold-pass parallelism probe: how much wall time serializes inside single
//! project-wide salsa queries. On throttled vCPUs check raw thread scaling (H) first.
//! Usage: `WORKERS=4 cargo run --release -p ry-lang --example parallel_probe <package-dir>...`

use {
    semantics::{
        Db, DocumentKind, Item, ProjectFiles, RootDatabase, SourceFile, SpliceCache,
        diagnostics::file_diagnostics,
        global_scheme, interface_sccs, item_naming, item_tree, package_definitions,
        stubs::install_shipped_stubs,
        types::{Name, Ty, TyKind, unknown},
    },
    std::{
        collections::HashMap,
        env, fs,
        num::NonZero,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Instant,
    },
};

static EXECUTES: AtomicUsize = AtomicUsize::new(0);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);

/// Counts salsa events, to see how often parallel workers block on each other's
/// in-flight queries.
#[salsa::db]
#[derive(Clone)]
struct ProbeDatabase {
    storage: salsa::Storage<Self>,
    splice_cache: Arc<SpliceCache>,
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
            splice_cache: Arc::default(),
        }
    }
}

#[salsa::db]
impl salsa::Database for ProbeDatabase {}

#[salsa::db]
impl Db for ProbeDatabase {
    fn splice_cache(&self) -> &SpliceCache {
        &self.splice_cache
    }
}

fn main() {
    let roots: Vec<PathBuf> = env::args().skip(1).map(PathBuf::from).collect();
    assert!(!roots.is_empty(), "pass package directories");
    let sources = collect_sources(&roots);
    let lines: usize = sources.iter().map(|source| source.lines().count()).sum();
    println!("{} files, {} lines", sources.len(), lines);
    let workers = env::var("WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            thread::available_parallelism()
                .map(NonZero::get)
                .unwrap_or(1)
        });

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let findings = full_check(&db, &files);
    let sequential = start.elapsed();
    println!("A sequential full check: {sequential:?} ({findings} findings)");

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    let sccs = interface_sccs(&db, ProjectFiles::get(&db));
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
    let findings = parallel_check(&db, &files, workers);
    println!(
        "D parallel x{workers} (CLI shape): {:?} ({findings} findings)",
        start.elapsed()
    );

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    warm_naming(&db, &files, workers);
    let findings = parallel_check(&db, &files, workers);
    println!(
        "E parallel x{workers} + naming pre-warm: {:?} ({findings} findings)",
        start.elapsed()
    );

    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    warm_naming(&db, &files, workers);
    println!("E1 parallel naming warm: {:?}", start.elapsed());
    let start = Instant::now();
    interface_sccs(&db, ProjectFiles::get(&db));
    println!("E2 interface_sccs after warm: {:?}", start.elapsed());
    let start = Instant::now();
    let cpu_before = cpu_ticks();
    let findings = parallel_check(&db, &files, workers);
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

    let (db, files) = setup::<ProbeDatabase>(&sources);
    warm_naming(&db, &files, workers);
    interface_sccs(&db, ProjectFiles::get(&db));
    let executes_before = EXECUTES.load(Ordering::Relaxed);
    let blocks_before = BLOCKS.load(Ordering::Relaxed);
    let start = Instant::now();
    let findings = parallel_check(&db, &files, workers);
    println!(
        "F parallel check events: {:?} ({findings} findings, {} executes, {} blocks)",
        start.elapsed(),
        EXECUTES.load(Ordering::Relaxed) - executes_before,
        BLOCKS.load(Ordering::Relaxed) - blocks_before,
    );

    // If a levelized schedule is no faster than the naive fan-out, the critical
    // path, not scheduling, is the ceiling.
    let (db, files) = setup::<RootDatabase>(&sources);
    let start = Instant::now();
    warm_naming(&db, &files, workers);
    let project = ProjectFiles::get(&db);
    let sccs = interface_sccs(&db, project);
    let winners = package_definitions(&db, project);
    let mut nodes: Vec<Item> = Vec::new();
    let mut position = HashMap::new();
    for &file in &files {
        for &item in item_tree(&db, file) {
            if item.name(&db).is_some() {
                position.insert(item, nodes.len());
                nodes.push(item);
            }
        }
    }
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, &item) in nodes.iter().enumerate() {
        if let Some(naming) = item_naming(&db, item) {
            for name in naming.non_locals.values() {
                if let Some(target) = winners.get(name.as_str())
                    && let Some(&target_index) = position.get(target)
                    && target_index != index
                {
                    edges[index].push(target_index);
                }
            }
        }
    }
    // Cycle members all take their group's first member's level.
    let same_group = |a: usize, b: usize| {
        sccs.membership.contains_key(&nodes[a])
            && sccs.membership.get(&nodes[a]) == sccs.membership.get(&nodes[b])
    };
    let mut level: Vec<usize> = vec![0; nodes.len()];
    let mut changed = true;
    let mut rounds = 0usize;
    while changed && rounds < nodes.len() {
        changed = false;
        rounds += 1;
        for index in 0..nodes.len() {
            for &target in &edges[index] {
                if !same_group(index, target) && level[index] <= level[target] {
                    level[index] = level[target] + 1;
                    changed = true;
                }
            }
        }
    }
    let depth = level.iter().copied().max().unwrap_or(0) + 1;
    let mut by_level: Vec<Vec<Item>> = vec![Vec::new(); depth];
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
        fan_out(&db, level_items, workers, |db, &item| {
            global_scheme(db, item);
        });
    }
    let schemes_done = start.elapsed();
    let findings = parallel_check(&db, &files, workers);
    println!(
        "G levelized schemes then fan-out: schemes {:?}, total {:?} ({findings} findings)",
        schemes_done,
        start.elapsed()
    );

    // H1 hammers one hot value (every `unknown()` in the checker takes this path);
    // H2 interns values distinct per thread.
    let (db, _files) = setup::<RootDatabase>(&sources);
    const OPS: usize = 2_000_000;
    for (label, distinct) in [
        ("H1 hot-value interns", false),
        ("H2 distinct interns", true),
    ] {
        for threads in [1usize, workers] {
            let start = Instant::now();
            thread::scope(|scope| {
                for thread in 0..threads {
                    let db = db.clone();
                    scope.spawn(move || {
                        for i in 0..OPS / threads {
                            if distinct {
                                let name = Name::new(&db, format!("t{thread}_{}", i % 4096));
                                Ty::new(&db, TyKind::Named(name, Vec::new()));
                            } else {
                                unknown(&db);
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

fn collect_sources(roots: &[PathBuf]) -> Vec<String> {
    let mut sources = Vec::new();
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("r"))
                && let Ok(bytes) = fs::read(&path)
            {
                sources.push((path, String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    sources.sort();
    sources.into_iter().map(|(_, source)| source).collect()
}

fn setup<D: Db + Default>(sources: &[String]) -> (D, Vec<SourceFile>) {
    let db = D::default();
    install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(&db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&db, files.clone());
    (db, files)
}

fn full_check<D: Db>(db: &D, files: &[SourceFile]) -> usize {
    files
        .iter()
        .map(|&file| file_diagnostics(db, file).len())
        .sum()
}

/// Runs `work` over `items` split into one contiguous chunk per worker, each
/// worker on its own database handle.
fn fan_out<D, T, F>(db: &D, items: &[T], workers: usize, work: F)
where
    D: Db + Clone + Send,
    T: Sync,
    F: Fn(&D, &T) + Sync,
{
    let chunk = items.len().div_ceil(workers).max(1);
    thread::scope(|scope| {
        for chunk_items in items.chunks(chunk) {
            let db = db.clone();
            let work = &work;
            scope.spawn(move || {
                for item in chunk_items {
                    work(&db, item);
                }
            });
        }
    });
}

fn warm_naming<D: Db + Clone + Send>(db: &D, files: &[SourceFile], workers: usize) {
    fan_out(db, files, workers, |db, &file| {
        for &item in item_tree(db, file) {
            item_naming(db, item);
        }
    });
}

fn parallel_check<D: Db + Clone + Send>(db: &D, files: &[SourceFile], workers: usize) -> usize {
    let total = AtomicUsize::new(0);
    fan_out(db, files, workers, |db, &file| {
        total.fetch_add(file_diagnostics(db, file).len(), Ordering::Relaxed);
    });
    total.into_inner()
}

/// (utime, stime) in clock ticks, counting /proc/self/stat fields after the
/// parenthesized comm.
fn cpu_ticks() -> (u64, u64) {
    let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after_comm = stat.rsplit(')').next().unwrap_or("");
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let field = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse().ok())
            .unwrap_or(0)
    };
    (field(11), field(12))
}
