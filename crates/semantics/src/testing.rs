//! Shared test support, including the fuzz invariant battery: no panics, deterministic output
//! across fresh databases, in-bounds ranges, and incremental equivalence with a fresh build.

use {
    crate::{
        Db, DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, SpliceCache,
        diagnostics::{TypeRenderer, file_diagnostics, strict_diagnostics},
        item_check, item_tree,
        lints::{LintConfig, LintLevel, NameStyle, lint_file},
        stubs::install_shipped_stubs,
    },
    salsa::{Setter as _, plumbing::AsId as _},
    std::{
        cell::RefCell,
        mem,
        sync::{Arc, Mutex},
    },
};

/// Renders schemes, diagnostics, and every lint (all enabled) for a file, asserting each range
/// stays within the file.
pub fn render_semantics(db: &RootDatabase, file: SourceFile) -> String {
    let mut output = String::new();
    for &item in item_tree(db, file) {
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        let Some(name) = item.name(db) else {
            continue;
        };
        let Some(check) = item_check(db, item) else {
            continue;
        };
        let Some(scheme) = check.scheme else {
            continue;
        };
        let mut renderer = TypeRenderer::default();
        output.push_str(&format!(
            "{name}: {}\n",
            renderer.render_scheme(db, &scheme)
        ));
    }
    let lint_config = LintConfig {
        naming_style: Some(NameStyle::Snake),
        unused_parameter: LintLevel::Warn,
        shadows_builtin: LintLevel::Warn,
        shadows_namespace: LintLevel::Warn,
        ..LintConfig::default()
    };
    let length = file.text(db).len();
    for diagnostic in file_diagnostics(db, file)
        .into_iter()
        .chain(strict_diagnostics(db, file))
        .chain(lint_file(db, file, &lint_config))
    {
        let start = u32::from(diagnostic.range.start()) as usize;
        let end = u32::from(diagnostic.range.end()) as usize;
        assert!(
            start <= end && end <= length,
            "diagnostic range {start}..{end} escapes the file (length {length})"
        );
        output.push_str(&format!(
            "{start}..{end} [{}] {}\n",
            diagnostic.code, diagnostic.message
        ));
    }
    output
}

fn single_file_project(source: &str, kind: DocumentKind) -> (RootDatabase, SourceFile) {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), kind);
    ProjectFiles::new(&db, vec![file]);
    (db, file)
}

fn fresh_output(source: &str, kind: DocumentKind) -> String {
    let (db, file) = single_file_project(source, kind);
    render_semantics(&db, file)
}

pub fn check_semantics_invariants(source: &str, edited_source: &str, kind: DocumentKind) {
    let first = fresh_output(source, kind);
    let (mut db, file) = single_file_project(source, kind);
    let before = render_semantics(&db, file);
    // `first` comes from an independent database, so this comparison is the determinism check.
    assert_eq!(
        before, first,
        "non-deterministic pipeline output across fresh databases for:\n{source}"
    );
    file.set_text(&mut db).to(edited_source.to_owned());
    let incremental = render_semantics(&db, file);
    let fresh = fresh_output(edited_source, kind);
    assert_eq!(
        incremental, fresh,
        "incremental output diverges from a fresh build after editing\n---- from:\n{source}\n---- to:\n{edited_source}"
    );
    file.set_text(&mut db).to(source.to_owned());
    let back = render_semantics(&db, file);
    assert_eq!(
        back, before,
        "output not restored after editing back\n---- source:\n{source}\n---- via:\n{edited_source}"
    );
}

/// Derives the document kind and the edit from the input bytes, so one byte stream drives the
/// whole battery.
pub fn check_semantics_input(input: &str) {
    let hash = input.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    });
    let kind = if hash % 4 == 0 {
        DocumentKind::Script
    } else {
        DocumentKind::Package
    };
    let cut = input.floor_char_boundary((hash as usize >> 8) % (input.len() + 1));
    let edited = format!("{}\nprobe <- 1L\n", &input[..cut]);
    check_semantics_invariants(input, &edited, kind);
}

thread_local! {
    static FIXTURE_DATABASE: RefCell<Option<RootDatabase>> = const { RefCell::new(None) };
}

/// Reuses one database per thread across cases, because parsing the shipped stubs costs far more
/// than checking a case.
pub fn with_fixture_project<R>(
    files: Vec<(String, DocumentKind)>,
    render: impl FnOnce(&RootDatabase, &[SourceFile]) -> R,
) -> R {
    FIXTURE_DATABASE.with_borrow_mut(|slot| {
        let db = slot.get_or_insert_with(|| {
            let db = RootDatabase::default();
            install_shipped_stubs(&db);
            db
        });
        let sources: Vec<SourceFile> = files
            .into_iter()
            .map(|(text, kind)| SourceFile::new(db, text, kind))
            .collect();
        match ProjectFiles::try_get(db) {
            Some(project) => {
                project.set_files(db).to(sources.clone());
            }
            None => {
                ProjectFiles::new(db, sources.clone());
            }
        }
        render(db, &sources)
    })
}

/// Marks a log entry as a revalidated memo rather than an execution.
const VALIDATED: &str = "validated ";

/// Logs every query execution and revalidation, for asserting what an edit re-runs.
#[salsa::db]
#[derive(Clone)]
pub struct ProbeDatabase {
    storage: salsa::Storage<Self>,
    splice_cache: Arc<SpliceCache>,
    executed: Arc<Mutex<Vec<String>>>,
}

impl Default for ProbeDatabase {
    fn default() -> Self {
        let executed: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = executed.clone();
        ProbeDatabase {
            storage: salsa::Storage::new(Some(Box::new(move |event| {
                let entry = match event.kind {
                    salsa::EventKind::WillExecute { database_key } => format!("{database_key:?}"),
                    salsa::EventKind::DidValidateMemoizedValue { database_key } => {
                        format!("{VALIDATED}{database_key:?}")
                    }
                    _ => return,
                };
                log.lock().expect("probe log").push(entry);
            }))),
            splice_cache: Arc::default(),
            executed,
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

impl ProbeDatabase {
    pub fn project(&self, sources: &[&str]) -> Vec<SourceFile> {
        let files: Vec<SourceFile> = sources
            .iter()
            .map(|source| SourceFile::new(self, (*source).to_owned(), DocumentKind::Package))
            .collect();
        ProjectFiles::new(self, files.clone());
        files
    }

    pub fn edit(&mut self, file: SourceFile, text: impl Into<String>) {
        file.set_text(self).to(text.into());
    }

    /// How many times each query in `queries` ran since the previous drain; drains the log.
    pub fn executions_since(&self, queries: &[&str]) -> Vec<usize> {
        let executed = mem::take(&mut *self.executed.lock().expect("probe log"));
        queries
            .iter()
            .map(|query| {
                let prefix = format!("{query}(");
                executed
                    .iter()
                    .filter(|entry| entry.starts_with(&prefix))
                    .count()
            })
            .collect()
    }

    /// How many memos of each query in `queries` were revalidated without running since the
    /// previous drain; drains the log.
    pub fn validations_since(&self, queries: &[&str]) -> Vec<usize> {
        let executed = mem::take(&mut *self.executed.lock().expect("probe log"));
        queries
            .iter()
            .map(|query| {
                let prefix = format!("{VALIDATED}{query}(");
                executed
                    .iter()
                    .filter(|entry| entry.starts_with(&prefix))
                    .count()
            })
            .collect()
    }

    /// Names of the items whose `item_check` ran since the previous call; drains the log.
    pub fn checked_since(&self) -> Vec<String> {
        let executed = mem::take(&mut *self.executed.lock().expect("probe log"));
        let mut names: Vec<String> = ProjectFiles::get(self)
            .files(self)
            .iter()
            .flat_map(|&file| item_tree(self, file).clone())
            .filter(|&item| executed.contains(&format!("item_check({:?})", item.as_id())))
            .filter_map(|item| item.name(self).clone())
            .collect();
        names.sort();
        names
    }
}
