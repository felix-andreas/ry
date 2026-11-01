//! What one file's diagnostics cost on a cold database, and which queries they execute.
//! Settled time after opening a file at startup is bounded below by this, so a query kind
//! that executes once per project item here is a whole-project dependency of one file.
//! Usage: `cargo run --release -p ry-lang --example cold_open_probe <package-dir> <file-suffix>`

use {
    semantics::{
        Db, DocumentKind, ProjectFiles, SourceFile, SpliceCache, diagnostics::file_diagnostics,
        item_tree, stubs::install_shipped_stubs, test_document_kind,
    },
    std::{
        cmp::Reverse,
        collections::HashMap,
        env, fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::Instant,
    },
};

static EXECUTES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

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
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    let key = format!("{database_key:?}");
                    let kind = key.split('(').next().unwrap_or_default().to_owned();
                    if let Ok(mut executes) = EXECUTES.lock() {
                        *executes.get_or_insert_default().entry(kind).or_default() += 1;
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
    let arguments: Vec<String> = env::args().skip(1).collect();
    let [root, suffix] = arguments.as_slice() else {
        panic!("pass a package directory and a file suffix");
    };
    let root = PathBuf::from(root);
    let sources = collect_sources(&root);
    let db = ProbeDatabase::default();
    install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|(path, source)| SourceFile::new(&db, source.clone(), kind_of(path, &root)))
        .collect();
    ProjectFiles::new(&db, files.clone());
    let index = sources
        .iter()
        .position(|(path, _)| path.to_string_lossy().ends_with(suffix.as_str()))
        .expect("no file matches the suffix");

    // Parsing and item trees are the floor every cold check pays; timing them apart shows
    // what the check itself costs on top.
    let start = Instant::now();
    for &file in &files {
        item_tree(&db, file);
    }
    println!("item trees of every file: {:?}", start.elapsed());
    if let Some(executes) = EXECUTES.lock().expect("probe lock").as_mut() {
        executes.clear();
    }

    let start = Instant::now();
    let findings = file_diagnostics(&db, files[index]).len();
    println!(
        "{} files; cold diagnostics of {}: {:?} ({findings} findings)",
        files.len(),
        sources[index].0.display(),
        start.elapsed()
    );
    let executes = EXECUTES
        .lock()
        .expect("probe lock")
        .take()
        .unwrap_or_default();
    let mut kinds: Vec<(String, usize)> = executes.into_iter().collect();
    kinds.sort_by_key(|(_, count)| Reverse(*count));
    for (kind, count) in kinds.iter().take(30) {
        println!("{count:>8} {kind}");
    }
}

fn collect_sources(root: &Path) -> Vec<(PathBuf, String)> {
    let mut sources = Vec::new();
    let mut stack = vec![root.to_path_buf()];
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

/// Mirrors the CLI's classification, so test files are analysed as the server analyses them.
fn kind_of(path: &Path, root: &Path) -> DocumentKind {
    if path.starts_with(root.join("R")) {
        return DocumentKind::Package;
    }
    if path.parent() != Some(root.join("tests").join("testthat").as_path()) {
        return DocumentKind::Script;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    test_document_kind(file_name)
}
