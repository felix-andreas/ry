//! What one edit costs after a cold prime, and which whole-project queries
//! re-execute for it. Execution counts, not wall times, are the trustworthy signal.
//! Usage: `cargo run --release -p ry-lang --example keystroke_probe <package-dir>...`

use {
    salsa::{Durability, Setter as _},
    semantics::{
        AnalysisSettings, DataFiles, Db, DocumentKind, ProjectFiles, SourceFile, SpliceCache,
        diagnostics::file_diagnostics,
        metadata::PackageMetadata,
        stubs::{StubSources, shipped_export_manifests, shipped_stub_sources},
    },
    std::{
        cmp::Reverse,
        collections::HashMap,
        env, fs, mem,
        path::PathBuf,
        sync::{
            Arc, LazyLock, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    },
};

static EXECUTES: AtomicUsize = AtomicUsize::new(0);
static WALK_EXECUTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static KIND_EXECUTES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
static KIND_VALIDATES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
/// Logging every validation costs more than the validation itself, so it is opt-in.
static COUNT_VALIDATES: LazyLock<bool> = LazyLock::new(|| env::var_os("PROBE_VALIDATES").is_some());
const WALKS: &[&str] = &[
    "conditional_slot_items",
    "package_definitions",
    "project_type_definitions",
];

#[salsa::db]
#[derive(Clone)]
struct ProbeDatabase {
    storage: salsa::Storage<Self>,
    splice_cache: Arc<SpliceCache>,
}

impl Default for ProbeDatabase {
    fn default() -> Self {
        Self {
            storage: salsa::Storage::new(Some(Box::new(|event| {
                if let salsa::EventKind::DidValidateMemoizedValue { database_key } = event.kind
                    && *COUNT_VALIDATES
                {
                    let key = format!("{database_key:?}");
                    let kind = key.split('(').next().unwrap_or_default().to_owned();
                    *KIND_VALIDATES
                        .lock()
                        .expect("probe lock")
                        .get_or_insert_default()
                        .entry(kind)
                        .or_default() += 1;
                }
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    EXECUTES.fetch_add(1, Ordering::Relaxed);
                    let key = format!("{database_key:?}");
                    let kind = key.split('(').next().unwrap_or_default().to_owned();
                    *KIND_EXECUTES
                        .lock()
                        .expect("probe lock")
                        .get_or_insert_default()
                        .entry(kind)
                        .or_default() += 1;
                    if let Some(walk) = WALKS.iter().find(|walk| key.contains(*walk)) {
                        WALK_EXECUTES
                            .lock()
                            .expect("probe lock")
                            .push((*walk).to_owned());
                    }
                }
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
    let lines: usize = sources
        .iter()
        .map(|(_, source)| source.lines().count())
        .sum();
    println!("{} files, {} lines", sources.len(), lines);

    let mut db = ProbeDatabase::default();
    // Every input but the file list starts at MEDIUM, as the server enters them: one LOW input
    // that nearly every memo reads would make each keystroke deep-verify the whole project.
    let _stubs = StubSources::builder(shipped_stub_sources(), shipped_export_manifests())
        .durability(Durability::MEDIUM)
        .new(&db);
    let _settings = AnalysisSettings::builder(true)
        .durability(Durability::MEDIUM)
        .new(&db);
    let _metadata = PackageMetadata::builder(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .durability(Durability::MEDIUM)
    .new(&db);
    let _data_files = DataFiles::builder(Default::default())
        .durability(Durability::MEDIUM)
        .new(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|(_, source)| {
            // As the server enters workspace files.
            SourceFile::builder(source.clone(), DocumentKind::Package)
                .durability(Durability::MEDIUM)
                .new(&db)
        })
        .collect();
    ProjectFiles::new(&db, files.clone());

    let start = Instant::now();
    let findings: usize = files
        .iter()
        .map(|&file| file_diagnostics(&db, file).len())
        .sum();
    println!("cold prime: {:?} ({findings} findings)", start.elapsed());

    let mut by_size: Vec<usize> = (0..sources.len()).collect();
    by_size.sort_by_key(|&index| Reverse(sources[index].1.len()));
    let editable: Vec<usize> = by_size
        .into_iter()
        .filter(|&index| body_literal_offset(&sources[index].1).is_some())
        .collect();
    let picks: Vec<usize> = [0, editable.len() / 2]
        .iter()
        .filter_map(|&rank| editable.get(rank).copied())
        .collect();

    // More edits make the edit phase long enough to sample with a profiler.
    let edits_per_kind: usize = env::var("PROBE_EDITS")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(8);
    for &index in &picks {
        let (path, original) = &sources[index];
        let file = files[index];
        let offset = body_literal_offset(original).expect("picked for a literal");
        // As the server lowers a file on its first edit.
        file.set_text(&mut db)
            .with_durability(Durability::LOW)
            .to(original.clone());
        file_diagnostics(&db, file);
        let edits = [
            (
                "body edit",
                format!("{}11{}", &original[..offset], &original[offset + 1..]),
            ),
            ("new item ", format!("{original}\nprobe_binding <- 0L\n")),
        ];
        for (label, edited) in edits {
            let mut times = Vec::new();
            let mut executes = Vec::new();
            let mut walks: Vec<String> = Vec::new();
            KIND_EXECUTES.lock().expect("probe lock").take();
            KIND_VALIDATES.lock().expect("probe lock").take();
            for edit in 0..edits_per_kind {
                let text = if edit % 2 == 0 { &edited } else { original };
                file.set_text(&mut db).to(text.clone());
                EXECUTES.store(0, Ordering::Relaxed);
                drain_walks();
                let start = Instant::now();
                file_diagnostics(&db, file);
                times.push(start.elapsed());
                executes.push(EXECUTES.load(Ordering::Relaxed));
                walks.extend(drain_walks());
            }
            let kinds = KIND_EXECUTES
                .lock()
                .expect("probe lock")
                .take()
                .unwrap_or_default();
            let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
            kinds.sort_by_key(|(_, count)| Reverse(*count));
            let top: Vec<String> = kinds
                .iter()
                .take(60)
                .map(|(kind, count)| format!("{kind} {}", count / edits_per_kind))
                .collect();
            println!("  per edit: {}", top.join(", "));
            let validates = KIND_VALIDATES
                .lock()
                .expect("probe lock")
                .take()
                .unwrap_or_default();
            let total: usize = validates.values().sum();
            let mut validates: Vec<(String, usize)> = validates.into_iter().collect();
            validates.sort_by_key(|(_, count)| Reverse(*count));
            let top: Vec<String> = validates
                .iter()
                .take(12)
                .map(|(kind, count)| format!("{kind} {}", count / edits_per_kind))
                .collect();
            if *COUNT_VALIDATES {
                println!(
                    "  validated per edit: {} total, {}",
                    total / edits_per_kind,
                    top.join(", ")
                );
            }
            times.sort();
            executes.sort();
            println!(
                "{label} {:>50}: median {:?} / worst {:?}, {}..{} queries, walks: {:?}",
                path.display(),
                times[times.len() / 2],
                times[times.len() - 1],
                executes[0],
                executes[executes.len() - 1],
                walks
            );
        }
        file.set_text(&mut db).to(original.clone());
        file_diagnostics(&db, file);
    }
}

fn collect_sources(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
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
    sources
}

/// A numeric literal to lengthen so ranges shift inside one item without any name changing:
/// a `1` that does not continue an identifier or another number.
fn body_literal_offset(source: &str) -> Option<usize> {
    let function = source.find("function")?;
    source[function..]
        .match_indices('1')
        .map(|(relative, _)| function + relative)
        .find(|&offset| {
            source[..offset]
                .chars()
                .next_back()
                .is_some_and(|previous| {
                    !(previous.is_alphanumeric() || matches!(previous, '_' | '.' | '"' | '\''))
                })
        })
}

fn drain_walks() -> Vec<String> {
    mem::take(&mut *WALK_EXECUTES.lock().expect("probe lock"))
}
