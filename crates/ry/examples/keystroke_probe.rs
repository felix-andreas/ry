//! Keystroke-incrementality probe: after a cold prime, what does one edit
//! cost, and which whole-project queries re-execute for it?
//!
//! Usage: `cargo run --release -p ry-lang --example keystroke_probe
//! <package-dir>...`
//!
//! Two edit shapes per probed file, mirroring the language server's settled
//! wave (re-demand `file_diagnostics` for the edited file):
//!
//! - body edit: the first numeric literal after the first `function`
//!   keyword alternates `1` <-> `11`. Ranges inside that one item shift, so
//!   its naming value changes, but no read name or top-level binding does.
//!   This is the representative typing case. Whole-project walks should not
//!   re-execute for it.
//! - new item: a `probe_binding <- 0L` statement appended at the end. The
//!   item set and name projections genuinely change, so the project walks
//!   must re-execute. This is the bounded worst case, for comparison.
//!
//! Each edit reports wall time, total queries executed, and whether the
//! whole-project graph walks (`interface_sccs`, `conditional_slot_items`,
//! `package_definitions`) re-executed, via a salsa event listener. Wall
//! times on shared or throttled vCPUs are noisy, so the execution counts are
//! the trustworthy signal.

use salsa::Setter as _;
use semantics::{DocumentKind, ProjectFiles, SourceFile};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static EXECUTES: AtomicUsize = AtomicUsize::new(0);
static WALK_EXECUTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
const WALKS: &[&str] = &[
    "interface_sccs",
    "conditional_slot_items",
    "package_definitions",
    "project_type_definitions",
];

#[salsa::db]
#[derive(Clone)]
struct ProbeDatabase {
    storage: salsa::Storage<Self>,
    splice_cache: std::sync::Arc<semantics::SpliceCache>,
}

impl Default for ProbeDatabase {
    fn default() -> Self {
        Self {
            storage: salsa::Storage::new(Some(Box::new(|event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    EXECUTES.fetch_add(1, Ordering::Relaxed);
                    let key = format!("{database_key:?}");
                    if let Some(walk) = WALKS.iter().find(|walk| key.contains(*walk)) {
                        WALK_EXECUTES
                            .lock()
                            .expect("probe lock")
                            .push((*walk).to_owned());
                    }
                }
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

/// The byte offset of the first `1` after the first `function` keyword: a
/// literal to lengthen so ranges shift inside one item without any name
/// changing.
fn body_literal_offset(source: &str) -> Option<usize> {
    let function = source.find("function")?;
    let relative = source[function..].find('1')?;
    Some(function + relative)
}

fn drain_walks() -> Vec<String> {
    std::mem::take(&mut *WALK_EXECUTES.lock().expect("probe lock"))
}

fn timed_demand(db: &ProbeDatabase, file: SourceFile) -> (std::time::Duration, usize, Vec<String>) {
    EXECUTES.store(0, Ordering::Relaxed);
    drain_walks();
    let start = Instant::now();
    let _ = semantics::diagnostics::file_diagnostics(db, file);
    (
        start.elapsed(),
        EXECUTES.load(Ordering::Relaxed),
        drain_walks(),
    )
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

    let mut db = ProbeDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|(_, source)| SourceFile::new(&db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&db, files.clone());

    let start = Instant::now();
    let mut findings = 0usize;
    for &file in &files {
        findings += semantics::diagnostics::file_diagnostics(&db, file).len();
    }
    println!("cold prime: {:?} ({findings} findings)", start.elapsed());

    // The largest file with an editable body literal, plus a mid-size one.
    let mut by_size: Vec<usize> = (0..sources.len()).collect();
    by_size.sort_by_key(|&index| std::cmp::Reverse(sources[index].1.len()));
    let editable: Vec<usize> = by_size
        .iter()
        .copied()
        .filter(|&index| body_literal_offset(&sources[index].1).is_some())
        .collect();
    let picks: Vec<usize> = [0, editable.len() / 2]
        .iter()
        .filter_map(|&rank| editable.get(rank).copied())
        .collect();

    const EDITS: usize = 8;
    for &index in &picks {
        let (path, original) = &sources[index];
        let file = files[index];
        let offset = body_literal_offset(original).expect("picked for a literal");

        let mut times = Vec::new();
        let mut executes = Vec::new();
        let mut walks: Vec<String> = Vec::new();
        for edit in 0..EDITS {
            let text = if edit % 2 == 0 {
                format!("{}11{}", &original[..offset], &original[offset + 1..])
            } else {
                original.clone()
            };
            file.set_text(&mut db).to(text);
            let (elapsed, count, walk) = timed_demand(&db, file);
            times.push(elapsed);
            executes.push(count);
            walks.extend(walk);
        }
        times.sort();
        executes.sort();
        println!(
            "body edit {:>50}: median {:?} / worst {:?}, {}..{} queries, walks: {:?}",
            path.display(),
            times[times.len() / 2],
            times[times.len() - 1],
            executes[0],
            executes[executes.len() - 1],
            walks
        );

        let mut times = Vec::new();
        let mut executes = Vec::new();
        let mut walks: Vec<String> = Vec::new();
        for edit in 0..EDITS {
            let text = if edit % 2 == 0 {
                format!("{original}\nprobe_binding <- 0L\n")
            } else {
                original.clone()
            };
            file.set_text(&mut db).to(text);
            let (elapsed, count, walk) = timed_demand(&db, file);
            times.push(elapsed);
            executes.push(count);
            walks.extend(walk);
        }
        times.sort();
        executes.sort();
        println!(
            "new item  {:>50}: median {:?} / worst {:?}, {}..{} queries, walks: {:?}",
            path.display(),
            times[times.len() / 2],
            times[times.len() - 1],
            executes[0],
            executes[executes.len() - 1],
            walks
        );
        file.set_text(&mut db).to(original.clone());
        let _ = semantics::diagnostics::file_diagnostics(&db, file);
    }
}
