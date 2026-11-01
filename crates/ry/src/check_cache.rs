//! `ry check`'s on-disk record of the last run's findings, kept in `.ry_cache/` at the project
//! root. A run reuses a file's findings when nothing they can depend on changed, so a repeat
//! check of an unchanged project only reads and hashes its files.
//!
//! A file's findings depend on its own text, on the project's top-level declarations (every
//! name, type, class and attached package that resolution sees), and on the files whose
//! exports its check reads. The record keeps each file's text fingerprint, its *surface*
//! fingerprint (its text with the bodies of its top-level function definitions blanked, which
//! is everything the project's declarations read from it), and the files its check reached.
//! When every changed file keeps its surface, only the changed files and the files that
//! reached them are checked again; any other change checks everything.

use {
    rustc_hash::{FxHashMap, FxHasher},
    semantics::{
        SourceFile,
        diagnostics::{Applicability, Diagnostic, Fix, RelatedLocation, Severity},
    },
    serde::{Deserialize, Serialize},
    std::{
        collections::BTreeSet,
        fs,
        hash::{DefaultHasher, Hasher},
        io,
        path::{Path, PathBuf},
        sync::{LazyLock, Mutex, PoisonError},
    },
    syntax::{SyntaxKind, SyntaxNode, TextRange},
};

pub const DIRECTORY: &str = ".ry_cache";
const RECORD: &str = "check.json";
/// Bumped whenever the record's layout or its reuse rule changes.
const FORMAT: u32 = 2;

/// Whether a run may read and write the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheUse {
    Off,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint(u64, u64);

/// Two independent 64-bit hashes, so an accidental collision needs both to collide. The record
/// is keyed by the ry binary itself, so neither hash has to be stable across ry releases.
pub struct Fingerprinter {
    fast: FxHasher,
    sip: DefaultHasher,
}

#[derive(Serialize, Deserialize)]
pub struct Record {
    format: u32,
    key: Fingerprint,
    files: Vec<RecordedFile>,
    namespace: Option<Vec<RecordedDiagnostic>>,
    /// The packages the project's files attach. Attach calls are part of a file's surface, so a
    /// partial run reuses the set instead of scanning every file for them.
    attached: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RecordedFile {
    path: PathBuf,
    content: Fingerprint,
    surface: Fingerprint,
    /// Positions, in the run's file order, of the files this file's findings depend on.
    dependencies: Vec<u32>,
    /// The NAMESPACE imports the file's code uses, which the NAMESPACE findings depend on.
    tokens: Vec<String>,
    diagnostics: Vec<RecordedDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedDiagnostic {
    start: u32,
    end: u32,
    error: bool,
    code: String,
    message: String,
    related: Vec<RecordedRelated>,
    fix: Option<RecordedFix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecordedRelated {
    file: u32,
    start: u32,
    end: u32,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecordedFix {
    title: String,
    start: u32,
    end: u32,
    replacement: String,
    safe: bool,
}

/// What a run must compute, decided from the previous record before any analysis.
pub enum Plan {
    /// No usable record: check every file.
    Fresh,
    /// Nothing changed: every file's findings come from the record.
    Replay(Record),
    /// Only the files marked stale are checked; the others' findings come from the record.
    Partial { previous: Record, stale: Vec<bool> },
}

/// One file's part of a new record.
pub struct FileOutcome<'a> {
    pub path: &'a Path,
    pub content: Fingerprint,
    pub surface: Fingerprint,
    pub dependencies: Vec<u32>,
    pub tokens: Vec<String>,
    pub diagnostics: &'a [Diagnostic],
}

impl Fingerprinter {
    pub fn new() -> Self {
        Fingerprinter {
            fast: FxHasher::default(),
            sip: DefaultHasher::new(),
        }
    }

    pub fn text(&mut self, text: &str) -> &mut Self {
        self.bytes(text.as_bytes())
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        // The length first, so consecutive parts cannot shift into one another.
        self.fast.write_usize(bytes.len());
        self.sip.write_usize(bytes.len());
        self.fast.write(bytes);
        self.sip.write(bytes);
        self
    }

    pub fn finish(&self) -> Fingerprint {
        Fingerprint(self.fast.finish(), self.sip.finish())
    }
}

pub fn fingerprint(text: &str) -> Fingerprint {
    Fingerprinter::new().text(text).finish()
}

/// The fingerprint of what the project's declarations read from a file: its text with the body
/// of each top-level function definition left out. A body that can write or declare something
/// outside its own frame (`<<-`, `assign`, `library`, `setClass` and the like) stays in.
pub fn surface(text: &str, root: &SyntaxNode) -> Fingerprint {
    let mut fingerprinter = Fingerprinter::new();
    let mut cursor = 0;
    for body in root
        .children()
        .filter_map(|statement| function_body(&statement))
    {
        let range = body.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let (Some(kept), Some(body)) = (text.get(cursor..start), text.get(start..end)) else {
            continue;
        };
        if reaches_past_its_frame(body) {
            continue;
        }
        fingerprinter.text(kept);
        // A marker, so moving text across a body boundary changes the fingerprint.
        fingerprinter.bytes(&[0]);
        cursor = end;
    }
    if let Some(rest) = text.get(cursor..) {
        fingerprinter.text(rest);
    }
    fingerprinter.finish()
}

/// Reads the record a previous run left at `root`. A missing, unreadable or outdated record is
/// no record: the run checks every file and writes a new one.
pub fn load(root: &Path) -> Option<Record> {
    let bytes = fs::read(root.join(DIRECTORY).join(RECORD)).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.format == FORMAT).then_some(record)
}

pub fn plan(
    previous: Option<Record>,
    key: Fingerprint,
    paths: &[PathBuf],
    contents: &[Fingerprint],
    surface_of: impl Fn(usize) -> Fingerprint,
) -> Plan {
    let Some(previous) = previous else {
        return Plan::Fresh;
    };
    let same_files = previous.files.len() == paths.len()
        && previous
            .files
            .iter()
            .zip(paths)
            .all(|(recorded, path)| recorded.path == *path);
    if previous.key != key || !same_files {
        return Plan::Fresh;
    }
    let changed: Vec<bool> = previous
        .files
        .iter()
        .zip(contents)
        .map(|(recorded, content)| recorded.content != *content)
        .collect();
    if !changed.contains(&true) {
        return Plan::Replay(previous);
    }
    for (index, recorded) in previous.files.iter().enumerate() {
        if changed[index] && surface_of(index) != recorded.surface {
            return Plan::Fresh;
        }
    }
    let stale = previous
        .files
        .iter()
        .enumerate()
        .map(|(index, recorded)| {
            changed[index]
                || recorded
                    .dependencies
                    .iter()
                    .any(|&dependency| changed.get(dependency as usize).copied().unwrap_or(true))
        })
        .collect();
    Plan::Partial { previous, stale }
}

impl Record {
    /// The recorded findings of the file at `index`, against this run's files.
    pub fn diagnostics(&self, index: usize, files: &[SourceFile]) -> Option<Vec<Diagnostic>> {
        let recorded = self.files.get(index)?;
        recorded
            .diagnostics
            .iter()
            .map(|diagnostic| restore(diagnostic, files))
            .collect()
    }

    pub fn dependencies(&self, index: usize) -> Option<&[u32]> {
        self.files
            .get(index)
            .map(|recorded| recorded.dependencies.as_slice())
    }

    pub fn surface(&self, index: usize) -> Option<Fingerprint> {
        self.files.get(index).map(|recorded| recorded.surface)
    }

    pub fn tokens(&self, index: usize) -> Option<&[String]> {
        self.files
            .get(index)
            .map(|recorded| recorded.tokens.as_slice())
    }

    pub fn attached(&self) -> BTreeSet<String> {
        self.attached.iter().cloned().collect()
    }

    pub fn namespace(&self, files: &[SourceFile]) -> Option<Option<Vec<Diagnostic>>> {
        match &self.namespace {
            None => Some(None),
            Some(diagnostics) => diagnostics
                .iter()
                .map(|diagnostic| restore(diagnostic, files))
                .collect::<Option<Vec<_>>>()
                .map(Some),
        }
    }
}

/// Builds the record for this run. `None` when a finding cannot be recorded, such as one whose
/// related location lies outside the project's files; the next run then checks every file.
pub fn record(
    key: Fingerprint,
    files: &[SourceFile],
    outcomes: Vec<FileOutcome<'_>>,
    namespace: Option<&[Diagnostic]>,
    attached: &BTreeSet<String>,
) -> Option<Record> {
    let positions: FxHashMap<SourceFile, u32> = files
        .iter()
        .enumerate()
        .map(|(index, &file)| (file, index as u32))
        .collect();
    let recorded_files = outcomes
        .into_iter()
        .map(|outcome| {
            let diagnostics = outcome
                .diagnostics
                .iter()
                .map(|diagnostic| recorded(diagnostic, &positions))
                .collect::<Option<Vec<_>>>()?;
            // A finding that points into another file shows that file's text.
            let mut dependencies = outcome.dependencies;
            dependencies.extend(
                diagnostics
                    .iter()
                    .flat_map(|diagnostic| diagnostic.related.iter())
                    .map(|related| related.file),
            );
            dependencies.sort_unstable();
            dependencies.dedup();
            let mut tokens = outcome.tokens;
            tokens.sort_unstable();
            Some(RecordedFile {
                path: outcome.path.to_path_buf(),
                content: outcome.content,
                surface: outcome.surface,
                dependencies,
                tokens,
                diagnostics,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let namespace = match namespace {
        None => None,
        Some(diagnostics) => Some(
            diagnostics
                .iter()
                .map(|diagnostic| recorded(diagnostic, &positions))
                .collect::<Option<Vec<_>>>()?,
        ),
    };
    Some(Record {
        format: FORMAT,
        key,
        files: recorded_files,
        namespace,
        attached: attached.iter().cloned().collect(),
    })
}

/// Writes the record beside a `.gitignore` that keeps the directory out of version control.
/// The record is written to a temporary file and renamed, so a concurrent run reads either the
/// old record or the new one, never half of one.
pub fn store(root: &Path, record: &Record) -> io::Result<()> {
    let directory = root.join(DIRECTORY);
    fs::create_dir_all(&directory)?;
    let ignore = directory.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "# Created by ry; safe to delete.\n*\n")?;
    }
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    let partial = directory.join(format!("{RECORD}.{}", std::process::id()));
    fs::write(&partial, bytes)?;
    fs::rename(&partial, directory.join(RECORD))
}

/// A finding as the record stores it, so two runs' findings compare without sharing a database.
pub fn recorded_form(diagnostic: &Diagnostic, files: &[SourceFile]) -> Option<RecordedDiagnostic> {
    let positions: FxHashMap<SourceFile, u32> = files
        .iter()
        .enumerate()
        .map(|(index, &file)| (file, index as u32))
        .collect();
    recorded(diagnostic, &positions)
}

fn recorded(
    diagnostic: &Diagnostic,
    positions: &FxHashMap<SourceFile, u32>,
) -> Option<RecordedDiagnostic> {
    let related = diagnostic
        .related
        .iter()
        .map(|related| {
            Some(RecordedRelated {
                file: *positions.get(&related.file)?,
                start: related.range.start().into(),
                end: related.range.end().into(),
                message: related.message.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(RecordedDiagnostic {
        start: diagnostic.range.start().into(),
        end: diagnostic.range.end().into(),
        error: diagnostic.severity == Severity::Error,
        code: diagnostic.code.to_owned(),
        message: diagnostic.message.clone(),
        related,
        fix: diagnostic.fix.as_ref().map(|fix| RecordedFix {
            title: fix.title.clone(),
            start: fix.range.start().into(),
            end: fix.range.end().into(),
            replacement: fix.replacement.clone(),
            safe: fix.applicability == Applicability::Safe,
        }),
    })
}

fn restore(diagnostic: &RecordedDiagnostic, files: &[SourceFile]) -> Option<Diagnostic> {
    let range =
        |start: u32, end: u32| (start <= end).then(|| TextRange::new(start.into(), end.into()));
    let related = diagnostic
        .related
        .iter()
        .map(|related| {
            Some(RelatedLocation {
                file: *files.get(related.file as usize)?,
                range: range(related.start, related.end)?,
                message: static_text(&related.message),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let fix = match &diagnostic.fix {
        None => None,
        Some(fix) => Some(Fix {
            title: fix.title.clone(),
            range: range(fix.start, fix.end)?,
            replacement: fix.replacement.clone(),
            applicability: match fix.safe {
                true => Applicability::Safe,
                false => Applicability::Unsafe,
            },
        }),
    };
    Some(Diagnostic {
        range: range(diagnostic.start, diagnostic.end)?,
        severity: match diagnostic.error {
            true => Severity::Error,
            false => Severity::Warning,
        },
        code: static_text(&diagnostic.code),
        message: diagnostic.message.clone(),
        related,
        fix,
    })
}

/// Diagnostic codes and related-location labels are static text in a finding, and a run reads
/// only a few dozen distinct ones back, so each is leaked once and shared.
fn static_text(text: &str) -> &'static str {
    static TEXTS: LazyLock<Mutex<FxHashMap<String, &'static str>>> =
        LazyLock::new(|| Mutex::new(FxHashMap::default()));
    let mut texts = TEXTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&shared) = texts.get(text) {
        return shared;
    }
    let shared: &'static str = Box::leak(text.to_owned().into_boxed_str());
    texts.insert(text.to_owned(), shared);
    shared
}

/// The body of `name <- function(...) body` (or `=`, or `->`) at the top level.
fn function_body(statement: &SyntaxNode) -> Option<SyntaxNode> {
    if statement.kind() != SyntaxKind::BINARY_EXPR {
        return None;
    }
    let assigns = statement.children_with_tokens().any(|child| {
        matches!(
            child.kind(),
            SyntaxKind::LESS_MINUS | SyntaxKind::EQ | SyntaxKind::MINUS_GREATER
        )
    });
    if !assigns {
        return None;
    }
    let function = statement
        .children()
        .find(|child| child.kind() == SyntaxKind::FUNCTION_DEF)?;
    function
        .children()
        .last()
        .filter(|body| body.kind() != SyntaxKind::PARAMETER_LIST)
}

const DECLARING_CALLS: &[&str] = &[
    "assign",
    "attach",
    "attachNamespace",
    "delayedAssign",
    "library",
    "loadNamespace",
    "makeActiveBinding",
    "R6Class",
    "require",
    "requireNamespace",
    "setClass",
    "setClassUnion",
    "setGeneric",
    "setMethod",
    "setOldClass",
    "setRefClass",
    "setValidity",
    "source",
    "sys.source",
];

/// Read off the text rather than the tokens, so a word inside a string or a comment also counts:
/// a false alarm only keeps the body in the surface.
fn reaches_past_its_frame(body: &str) -> bool {
    if body.contains("<<-") || body.contains("->>") {
        return true;
    }
    body.split(|character: char| {
        !(character.is_alphanumeric() || character == '.' || character == '_')
    })
    .any(|word| DECLARING_CALLS.contains(&word))
}
