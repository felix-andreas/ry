use crate::config::{self, ExperimentalFeatures};
use crate::diagnostics::{apply_suppressions, document_diagnostics};
use crate::namespace;
use crate::position::LineIndex;
use console::style;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use miette::{
    GraphicalReportHandler, GraphicalTheme, LabeledSpan, MietteError, MietteSpanContents,
    Severity as ReportSeverity, SourceCode, SourceSpan, SpanContents,
};
use semantics::diagnostics::{Diagnostic, RelatedLocation, Severity};
use semantics::metadata::{
    PackageMetadata, attached_union, normalized_imports, parse_description_collate,
    parse_description_dependencies, parse_description_package, parse_namespace_exports,
    parse_namespace_imports,
};
use semantics::stubs::{
    StubSources, namespace_exports, namespace_known, shipped_export_manifests,
    shipped_stub_sources, stub_source_problems, stubs,
};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use std::{env, fmt, fs, iter, thread};
use syntax::TextRange;
use syntax::literate::{is_literate_extension, r_source_of_literate};

/// A run summary: not a finding, so a plain line rather than a report.
pub fn info(message: &str) {
    eprintln!("{}", style(message).bold());
}

pub fn warn(message: &str) {
    report(&Report {
        severity: ReportSeverity::Warning,
        message,
        ..Report::default()
    });
}

pub fn error(message: &str) {
    report(&Report {
        message,
        ..Report::default()
    });
}

pub fn error_because(message: &str, cause: &(dyn std::error::Error + 'static)) {
    report(&Report {
        message,
        cause: Some(cause),
        ..Report::default()
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Findings,
}

/// Already reported on stderr; `main` exits 2.
#[derive(Debug)]
pub struct CommandError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Rendered diagnostics with source snippets, on stderr
    Human,
    /// One JSON object per diagnostic (JSON Lines), on stdout
    Json,
}

/// Diagnostics below the floor are neither rendered nor counted toward the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum MinSeverity {
    Warning,
    Error,
}

pub fn check(
    maybe_files: Option<&[PathBuf]>,
    output: OutputFormat,
    min_severity: MinSeverity,
) -> Result<Outcome, CommandError> {
    let root: Vec<PathBuf> = vec![".".into()];
    let files = maybe_files.unwrap_or(&root);

    // Analysis always covers the whole project root and only reporting is scoped to
    // the named paths, so the verdict cannot depend on how the paths were spelled.
    let mut groups: Vec<(PathBuf, config::Config, Vec<PathBuf>)> = Vec::new();
    for file in files {
        let config = match config::Config::discover(file) {
            Ok(config) => config,
            Err(err) => {
                report(&err);
                return Err(CommandError);
            }
        };
        warn_unknown_config_keys(&config);
        let target = fs::canonicalize(file).map_err(|err| {
            error_because(&format!("failed to resolve: {}", file.display()), &err);
            CommandError
        })?;
        let root = project_root_for_target(&target);
        match groups.iter_mut().find(|(existing, _, _)| *existing == root) {
            Some((_, _, requested)) => requested.push(target),
            None => groups.push((root, config, vec![target])),
        }
    }

    let mut n_files = 0;
    let mut n_diagnostics = 0;
    let mut n_failures = 0;
    for (root, config, requested) in groups {
        let exclude = exclude_matcher(&config, &root)?;
        let mut paths = collect_r_files(&root, exclude.as_ref())?;
        // `exclude` scopes the project walk; it does not veto a file named directly.
        for target in &requested {
            if !target.is_dir() && !paths.contains(target) {
                paths.push(target.clone());
            }
        }

        // Override stubs fold on top of the shipped corpus (later sources win); an
        // unreadable one is an I/O failure.
        let project_stubs = match read_project_stubs(&root) {
            Ok(project_stubs) => project_stubs,
            Err((path, err)) => {
                n_failures += 1;
                error_because(
                    &format!("failed to read override stub: {}", path.display()),
                    &err,
                );
                Vec::new()
            }
        };
        let mut stub_sources = shipped_stub_sources();
        stub_sources.extend(
            project_stubs
                .iter()
                .map(|stub| (stub.stem.clone(), stub.text.clone())),
        );

        let mut db = RootDatabase::default();
        StubSources::new(&db, stub_sources, shipped_export_manifests());

        // A broken override stub silently changes what every file checks against, so
        // its dropped declarations are reported as findings.
        for stub in &project_stubs {
            let stub_text = &stub.text;
            let stub_index = LineIndex::new(stub_text);
            let problems: Vec<Diagnostic> = stub_source_problems(&db, stub_text)
                .into_iter()
                .map(|problem| {
                    let start = stub_index.line_start(problem.line as u32);
                    let end = start + stub_index.line_length(problem.line as u32, stub_text);
                    Diagnostic::new(
                        TextRange::new(start.into(), end.into()),
                        Severity::Error,
                        "stub",
                        problem.message,
                    )
                })
                .collect();
            n_diagnostics += render_findings(
                output,
                min_severity,
                &stub.path,
                stub_text,
                &problems,
                &|_| None,
            );
        }

        let namespace_path = root.join("NAMESPACE");
        let namespace_source = fs::read_to_string(&namespace_path).ok();
        let namespace_imports = namespace_source
            .as_deref()
            .map(parse_namespace_imports)
            .unwrap_or_default();
        let description_source = fs::read_to_string(root.join("DESCRIPTION")).ok();
        let dependencies = description_source
            .as_deref()
            .map(parse_description_dependencies)
            .unwrap_or_default();
        let collate = description_source
            .as_deref()
            .map(parse_description_collate)
            .unwrap_or_default();
        let metadata = PackageMetadata::new(
            &db,
            normalized_imports(&namespace_imports),
            dependencies,
            BTreeSet::new(),
            description_source
                .as_deref()
                .and_then(parse_description_package),
        );

        let mut used_tokens = BTreeSet::new();
        let mut checked: Vec<(PathBuf, String)> = Vec::with_capacity(paths.len());
        let mut reported: Vec<bool> = Vec::with_capacity(paths.len());
        for path in paths {
            let source = match fs::read_to_string(&path) {
                Ok(source) => analysable_source(&path, source),
                Err(err) => {
                    n_failures += 1;
                    error_because(&format!("failed to read: {}", path.display()), &err);
                    continue;
                }
            };
            let asked_about = requested
                .iter()
                .any(|target| path == *target || path.starts_with(target));
            if asked_about {
                n_files += 1;
            }
            reported.push(asked_about);
            namespace::collect_used_tokens(&source, &mut used_tokens);
            checked.push((path, source));
        }

        // Analysis follows the project order; diagnostics still print in discovery order.
        let order = project_order(&root, &collate);
        let mut ordered: Vec<usize> = (0..checked.len()).collect();
        ordered.sort_by_cached_key(|&index| order(&checked[index].0));
        let mut files: Vec<Option<SourceFile>> = vec![None; checked.len()];
        let mut project = Vec::with_capacity(checked.len());
        for index in ordered {
            let (path, source) = &checked[index];
            let kind = if shares_a_namespace(path, &root) {
                DocumentKind::Package
            } else {
                DocumentKind::Script
            };
            let file = SourceFile::new(&db, source.clone(), kind);
            files[index] = Some(file);
            project.push(file);
        }
        let files: Vec<SourceFile> = files.into_iter().flatten().collect();
        ProjectFiles::new(&db, project.clone());
        let attached = attached_union(&db, project);
        if !attached.is_empty() {
            use salsa::Setter;
            metadata.set_attached(&mut db).to(attached);
        }

        let path_by_file: HashMap<SourceFile, &PathBuf> = files
            .iter()
            .zip(&checked)
            .map(|(&file, (path, _))| (file, path))
            .collect();
        // Salsa storage-handle clones share memos, so threads compute disjoint files concurrently.
        let workers = thread::available_parallelism()
            .map(NonZero::get)
            .unwrap_or(1)
            .min(checked.len().max(1));
        // Warm per-item naming across cores first: inside the project-wide interface walk
        // it would serialize on one thread. Largest files first so one cannot skew a chunk.
        // Workers need the analysis stack: interface resolution recurses per dependency edge.
        thread::scope(|scope| {
            {
                let db = db.clone();
                thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        let _ = stubs(&db);
                    })
                    .expect("check worker thread should spawn");
            }
            let mut by_size: Vec<usize> = (0..checked.len()).collect();
            by_size.sort_by_key(|&index| Reverse(checked[index].1.len()));
            for worker in 0..workers {
                let db = db.clone();
                let files = &files;
                let deal: Vec<usize> = by_size
                    .iter()
                    .copied()
                    .skip(worker)
                    .step_by(workers)
                    .collect();
                thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for index in deal {
                            for &item in semantics::item_tree(&db, files[index]) {
                                let _ = semantics::item_naming(&db, item);
                            }
                        }
                    })
                    .expect("check worker thread should spawn");
            }
        });
        let mut per_file: Vec<Vec<Diagnostic>> = (0..checked.len()).map(|_| Vec::new()).collect();
        thread::scope(|scope| {
            let mut pending: Vec<(usize, &mut Vec<Diagnostic>)> =
                per_file.iter_mut().enumerate().collect();
            let chunk_size = pending.len().div_ceil(workers.max(1)).max(1);
            let mut chunks = Vec::new();
            while !pending.is_empty() {
                let take = chunk_size.min(pending.len());
                chunks.push(pending.split_off(pending.len() - take));
            }
            for chunk in chunks {
                let db = db.clone();
                let checked = &checked;
                let files = &files;
                let config = &config;
                let worker = thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for (index, slot) in chunk {
                            let rendered = document_diagnostics(&db, files[index], config);
                            *slot = apply_suppressions(rendered, &checked[index].1);
                        }
                    });
                worker.expect("check worker thread should spawn");
            }
        });
        // A related range often lives in another file, so resolve it against that document.
        let resolve_note = |related: &RelatedLocation| {
            Some(RelatedNote {
                path: path_by_file.get(&related.file)?.as_path(),
                source: related.file.text(&db),
                range: related.range,
                message: related.message,
            })
        };
        for (index, (path, source)) in checked.iter().enumerate() {
            if reported[index] {
                n_diagnostics += render_findings(
                    output,
                    min_severity,
                    path,
                    source,
                    &per_file[index],
                    &resolve_note,
                );
            }
        }

        // Emitted last so the `unused-import` lint sees the whole package.
        if let Some(namespace_source) = &namespace_source {
            let knows = |package: &str| namespace_known(&db, package).unwrap_or(false);
            let exports = |package: &str, name: &str| namespace_exports(&db, package, name);
            let mut problems =
                namespace::namespace_import_problems(&namespace_imports, &knows, &exports);
            let defines = |name: &str| {
                ProjectFiles::try_get(&db).is_some_and(|files| {
                    semantics::package_definitions(&db, files).contains_key(name)
                })
            };
            problems.extend(namespace::namespace_export_problems(
                &parse_namespace_exports(namespace_source),
                &defines,
            ));
            problems.extend(namespace::unused_import_diagnostics(
                &namespace_imports,
                &used_tokens,
                config.lint.unused_import,
            ));
            n_diagnostics += render_findings(
                output,
                min_severity,
                &namespace_path,
                namespace_source,
                &problems,
                &|_| None,
            );
        }
    }

    if n_failures > 0 {
        return Err(CommandError);
    }
    // Always summarize: silence would read as success on a run that matched nothing.
    if output == OutputFormat::Human {
        report_check_summary(n_files, n_diagnostics);
    }
    Ok(if n_diagnostics == 0 {
        Outcome::Clean
    } else {
        Outcome::Findings
    })
}

/// An empty tree exits clean: a monorepo stage with no R in it yet must not fail a pipeline.
fn report_check_summary(n_files: usize, n_diagnostics: usize) {
    let files = if n_files == 1 { "file" } else { "files" };
    if n_diagnostics == 0 {
        println!("{n_files} {files} checked, no problems");
        return;
    }
    let problems = if n_diagnostics == 1 {
        "problem"
    } else {
        "problems"
    };
    println!("{n_diagnostics} {problems} in {n_files} {files}");
}

/// For a literate document, the R its chunks contain with prose blanked, so
/// reported ranges still point into the original file.
pub(crate) fn analysable_source(path: &Path, text: String) -> String {
    let literate = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(is_literate_extension);
    if literate {
        r_source_of_literate(&text)
    } else {
        text
    }
}

pub(crate) fn warn_unknown_config_keys(config: &config::Config) {
    for key in &config.unknown_keys {
        match config::suggested_table(key) {
            Some(table) => warn(&format!(
                "ignoring config key `{key}`. It belongs under `[{table}]`, and nothing outside a \
                 table sets it"
            )),
            None => warn(&format!(
                "ignoring unknown config key `{key}`. Check the spelling, or update ry"
            )),
        }
    }
}

/// The anchor is canonicalized to agree with the canonicalized walk paths: on
/// Windows those carry `\\?\`, and a non-canonical anchor would never match.
pub(crate) fn exclude_matcher(
    config: &config::Config,
    target: &Path,
) -> Result<Option<Gitignore>, CommandError> {
    if config.check.exclude.is_empty() {
        return Ok(None);
    }
    let root = config.source_directory.clone().unwrap_or_else(|| {
        if target.is_dir() {
            target.to_path_buf()
        } else {
            target.parent().unwrap_or(target).to_path_buf()
        }
    });
    let root = fs::canonicalize(&root).unwrap_or(root);
    let mut builder = GitignoreBuilder::new(&root);
    for pattern in &config.check.exclude {
        if let Err(err) = builder.add_line(None, pattern) {
            error(&format!(
                "invalid `[check] exclude` pattern '{pattern}': {err}"
            ));
            return Err(CommandError);
        }
    }
    match builder.build() {
        Ok(gitignore) => Ok(Some(gitignore)),
        Err(err) => {
            error(&format!("invalid `[check] exclude` configuration: {err}"));
            Err(CommandError)
        }
    }
}

/// Vendored dependency infrastructure rather than project code; scopes only the walk.
const VENDORED_DIRECTORIES: [&str; 5] = ["renv", "packrat", "revdep", ".Rproj.user", ".Rcheck"];

/// `R/` and `tests/testthat/` files each share one environment (testthat sources
/// helpers first). Every surface assigning a [`DocumentKind`] must call this, or it
/// analyses a different program. Not the ordering key: that is `R/` membership alone.
pub(crate) fn shares_a_namespace(path: &Path, root: &Path) -> bool {
    path.starts_with(root.join("R"))
        || path
            .parent()
            .is_some_and(|parent| parent == root.join("tests").join("testthat"))
}

pub(crate) fn collect_r_files(
    target: &Path,
    exclude: Option<&Gitignore>,
) -> Result<Vec<PathBuf>, CommandError> {
    let mut builder = ignore::WalkBuilder::new(target);
    // Honour `.gitignore` even outside a git checkout.
    builder.require_git(false);
    // One predicate for both rules: `filter_entry` replaces the previous closure
    // rather than composing with it.
    let gitignore = exclude.filter(|_| target.is_dir()).cloned();
    builder.filter_entry(move |entry| {
        let is_dir = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir());
        if is_dir
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| VENDORED_DIRECTORIES.contains(&name))
        {
            return false;
        }
        let Some(gitignore) = &gitignore else {
            return true;
        };
        let relative = entry
            .path()
            .strip_prefix(gitignore.path())
            .unwrap_or_else(|_| entry.path());
        !gitignore
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    });
    builder
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let path = entry.into_path();
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension == "R" || extension == "r" || is_literate_extension(extension)
                    })
                    .then_some(Ok(path))
            }
            Err(err) => {
                error(&err.to_string());
                Some(Err(CommandError))
            }
        })
        .collect()
}

/// `<root>/stubs/*.Rtypes` in path order, each with its file stem (the namespace label).
pub(crate) fn project_stub_paths(root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(root.join("stubs")) else {
        return Vec::new();
    };
    let mut stubs: Vec<(PathBuf, String)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "Rtypes")
        })
        .map(|path| {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            (path, stem)
        })
        .collect();
    stubs.sort();
    stubs
}

pub(crate) struct ProjectStub {
    pub(crate) path: PathBuf,
    /// The namespace label.
    pub(crate) stem: String,
    pub(crate) text: String,
}

/// Every project stub, or the first one that cannot be read.
pub(crate) fn read_project_stubs(
    root: &Path,
) -> Result<Vec<ProjectStub>, (PathBuf, std::io::Error)> {
    project_stub_paths(root)
        .into_iter()
        .map(|(path, stem)| match fs::read_to_string(&path) {
            Ok(text) => Ok(ProjectStub { path, stem, text }),
            Err(err) => Err((path, err)),
        })
        .collect()
}

/// Package documents first (in `Collate` order when declared, then by path), then
/// scripts. The server, `check`, and the stats agree on it, so the last-writer-wins
/// symbol index selects the same winners everywhere.
pub(crate) fn project_order<'a>(
    root: &'a Path,
    collate: &'a [String],
) -> impl Fn(&Path) -> (bool, usize, String) + 'a {
    let r_path = root.join("R");
    let collate_rank: HashMap<&str, usize> = collate
        .iter()
        .enumerate()
        .map(|(rank, name)| (name.as_str(), rank))
        .collect();
    move |path| {
        let rank = path
            .file_name()
            .and_then(|name| collate_rank.get(name.to_string_lossy().as_ref()))
            .copied()
            .unwrap_or(usize::MAX);
        (
            !path.starts_with(&r_path),
            rank,
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }
}

/// Renders the findings at or above the floor and returns how many it rendered.
fn render_findings<'a>(
    output: OutputFormat,
    min_severity: MinSeverity,
    path: &Path,
    source: &str,
    diagnostics: &[Diagnostic],
    resolve_note: &dyn Fn(&RelatedLocation) -> Option<RelatedNote<'a>>,
) -> usize {
    let line_index = LineIndex::new(source);
    let snippet_lines = LineStarts::new(source);
    let mut rendered = 0;
    for diagnostic in diagnostics {
        if min_severity == MinSeverity::Error && diagnostic.severity != Severity::Error {
            continue;
        }
        rendered += 1;
        let related: Vec<RelatedNote> =
            diagnostic.related.iter().filter_map(resolve_note).collect();
        match output {
            OutputFormat::Human => {
                render_human_diagnostic(path, source, &snippet_lines, diagnostic, &related)
            }
            OutputFormat::Json => {
                render_json_diagnostic(path, source, &line_index, diagnostic, &related)
            }
        }
    }
    rendered
}

/// A related range often sits in another document, so each note carries its own text.
struct RelatedNote<'a> {
    path: &'a Path,
    source: &'a str,
    range: TextRange,
    message: &'static str,
}

fn render_human_diagnostic(
    path: &Path,
    source: &str,
    lines: &LineStarts,
    diagnostic: &Diagnostic,
    related: &[RelatedNote],
) {
    let related_lines: Vec<LineStarts> = related
        .iter()
        .map(|note| LineStarts::new(note.source))
        .collect();
    report(&Report {
        // A suppression comment must spell the code, so the human output leads with it.
        code: Some(diagnostic.code),
        severity: match diagnostic.severity {
            Severity::Warning => ReportSeverity::Warning,
            Severity::Error => ReportSeverity::Error,
        },
        message: &diagnostic.message,
        snippet: Some(Snippet {
            source: NamedText::new(path, source, lines),
            label: primary_label(source, diagnostic.range),
        }),
        related: related
            .iter()
            .zip(&related_lines)
            .map(|(note, note_lines)| Report {
                // Advice keeps a reader from counting a companion location as a second problem.
                severity: ReportSeverity::Advice,
                message: note.message,
                snippet: Some(Snippet {
                    source: NamedText::new(note.path, note.source, note_lines),
                    label: primary_label(note.source, note.range),
                }),
                ..Report::default()
            })
            .collect(),
        ..Report::default()
    });
    eprintln!();
}

/// Clamped to the first line when the range spans more: drawing a whole
/// function buries the finding.
fn primary_label(source: &str, range: TextRange) -> LabeledSpan {
    const MAX_UNDERLINED_LINES: usize = 3;

    let start = usize::from(range.start());
    let mut end = usize::from(range.end()).min(source.len());
    // An empty range takes the character it points at; at end of file there is none.
    if end == start {
        end = source
            .get(start..)
            .and_then(|rest| rest.chars().next())
            .map_or(start, |character| start + character.len_utf8());
    }
    let spanned = source.get(start..end).unwrap_or_default();
    let lines = spanned.lines().count();
    if lines <= MAX_UNDERLINED_LINES {
        return LabeledSpan::new_primary_with_span(None, start..end);
    }
    let first_line_end = spanned
        .find('\n')
        .map_or(end, |newline| start + newline)
        .min(end);
    LabeledSpan::new_primary_with_span(
        Some(format!(
            "the range continues for {} more lines",
            lines.saturating_sub(1)
        )),
        start..first_line_end,
    )
}

/// Positions are 1-based like the human renderer; field names are a documented contract.
fn render_json_diagnostic(
    path: &Path,
    source: &str,
    index: &LineIndex,
    diagnostic: &Diagnostic,
    related: &[RelatedNote],
) {
    let severity = match diagnostic.severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    };
    let start = index.line_column_chars(diagnostic.range.start(), source);
    let end = index.line_column_chars(diagnostic.range.end(), source);
    let related: Vec<serde_json::Value> = related
        .iter()
        .map(|note| {
            let index = LineIndex::new(note.source);
            let start = index.line_column_chars(note.range.start(), note.source);
            let end = index.line_column_chars(note.range.end(), note.source);
            serde_json::json!({
                "path": note.path.display().to_string(),
                "line": start.line + 1,
                "column": start.column + 1,
                "endLine": end.line + 1,
                "endColumn": end.column + 1,
                "message": note.message,
            })
        })
        .collect();
    let record = serde_json::json!({
        "path": path.display().to_string(),
        "line": start.line + 1,
        "column": start.column + 1,
        "endLine": end.line + 1,
        "endColumn": end.column + 1,
        "severity": severity,
        "code": diagnostic.code,
        "message": diagnostic.message,
        "related": related,
    });
    println!("{record}");
}

/// Config errors and type errors share one reporter so they never look like two tools.
#[derive(Debug)]
struct Report<'a> {
    severity: ReportSeverity,
    code: Option<&'a str>,
    message: &'a str,
    cause: Option<&'a (dyn std::error::Error + 'static)>,
    snippet: Option<Snippet<'a>>,
    related: Vec<Report<'a>>,
}

#[derive(Debug)]
struct Snippet<'a> {
    source: NamedText<'a>,
    label: LabeledSpan,
}

impl<'a> Default for Report<'a> {
    fn default() -> Report<'a> {
        Report {
            severity: ReportSeverity::Error,
            code: None,
            message: "",
            cause: None,
            snippet: None,
            related: Vec::new(),
        }
    }
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for Report<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
    }
}

impl miette::Diagnostic for Report<'_> {
    fn code(&self) -> Option<Box<dyn fmt::Display + '_>> {
        self.code
            .map(|code| Box::new(code) as Box<dyn fmt::Display>)
    }

    fn severity(&self) -> Option<ReportSeverity> {
        Some(self.severity)
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.snippet
            .as_ref()
            .map(|snippet| &snippet.source as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let snippet = self.snippet.as_ref()?;
        Some(Box::new(iter::once(snippet.label.clone())))
    }

    fn related(&self) -> Option<Box<dyn Iterator<Item = &dyn miette::Diagnostic> + '_>> {
        if self.related.is_empty() {
            return None;
        }
        Some(Box::new(
            self.related
                .iter()
                .map(|note| note as &dyn miette::Diagnostic),
        ))
    }
}

/// Colour and unicode on an attended terminal, monochrome unicode when colour is
/// refused, plain ASCII into a pipe or file.
pub fn report(diagnostic: &dyn miette::Diagnostic) {
    let reporter = &*REPORTER;
    let mut rendered = String::new();
    if reporter
        .handler
        .render_report(&mut rendered, diagnostic)
        .is_err()
    {
        // Cannot fail; the fallback degrades a reporter bug rather than swallowing the finding.
        eprintln!("{diagnostic}");
        return;
    }
    let mut kept = String::with_capacity(rendered.len());
    for line in rendered.lines() {
        if closes_a_snippet(line, &reporter.closing_rule) {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    eprint!("{kept}");
}

struct Reporter {
    handler: GraphicalReportHandler,
    closing_rule: String,
}

static REPORTER: LazyLock<Reporter> = LazyLock::new(|| {
    let mut theme = if console::colors_enabled_stderr() {
        GraphicalTheme::unicode()
    } else if console::user_attended_stderr() {
        GraphicalTheme::unicode_nocolor()
    } else {
        GraphicalTheme::none()
    };
    // A snippet is a window on the source, not a box: an unbroken gutter, no corner.
    theme.characters.vbar_break = theme.characters.vbar;
    theme.characters.ltop = theme.characters.hbar;
    theme.characters.underline = '^';

    let closing_rule = format!(
        "{}{}",
        theme.characters.lbot,
        theme.characters.hbar.to_string().repeat(CLOSING_RULE_WIDTH)
    );
    let handler = GraphicalReportHandler::new_themed(theme)
        // A companion location reads as part of the finding above it, not as its own report.
        .with_show_related_as_nested(true);
    let handler = match console::user_attended_stderr() {
        true => handler.with_width(console::Term::stderr().size().1 as usize),
        false => handler,
    };
    Reporter {
        handler,
        closing_rule,
    }
});

/// The closing rule shares characters with the cause-chain arrows, so it cannot be
/// themed away. Only a line that is nothing but the rule counts: nested reports
/// indent theirs, and R source may spell anything.
fn closes_a_snippet(line: &str, closing_rule: &str) -> bool {
    line.trim_end()
        .strip_suffix(closing_rule)
        .is_some_and(|before| console::strip_ansi_codes(before).trim().is_empty())
}

/// Hardcoded in miette's `GraphicalReportHandler`, so an upgrade can move it;
/// `check_draws_the_snippet_as_a_window` then fails.
const CLOSING_RULE_WIDTH: usize = 4;

/// Line starts by miette's rule (`\n`, `\r\n`, or a lone `\r`). miette walks from
/// byte zero per span, so this bounds the walk to a binary search plus a window.
#[derive(Debug)]
struct LineStarts {
    starts: Vec<u32>,
}

impl LineStarts {
    fn new(text: &str) -> LineStarts {
        let mut starts = vec![0u32];
        let bytes = text.as_bytes();
        let mut offset = 0usize;
        while offset < bytes.len() {
            match bytes[offset] {
                b'\r' => {
                    // `\r\n` is one break, not two.
                    offset += usize::from(bytes.get(offset + 1) == Some(&b'\n'));
                    starts.push(offset as u32 + 1);
                }
                b'\n' => starts.push(offset as u32 + 1),
                _ => {}
            }
            offset += 1;
        }
        LineStarts { starts }
    }

    /// Clamped to the last line.
    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&start| (start as usize) <= offset)
            .saturating_sub(1)
    }

    fn line_start(&self, line: usize) -> usize {
        self.starts
            .get(line)
            .map_or(usize::MAX, |&start| start as usize)
    }
}

/// Borrowed: [`miette::NamedSource`] owns its text and would copy the whole file
/// into every finding.
#[derive(Debug)]
struct NamedText<'a> {
    name: String,
    text: &'a str,
    lines: &'a LineStarts,
}

impl<'a> NamedText<'a> {
    fn new(path: &Path, text: &'a str, lines: &'a LineStarts) -> NamedText<'a> {
        static WORKING_DIRECTORY: LazyLock<Option<PathBuf>> =
            LazyLock::new(|| env::current_dir().ok());
        let name = WORKING_DIRECTORY
            .as_deref()
            .and_then(|directory| path.strip_prefix(directory).ok())
            .unwrap_or(path);
        NamedText {
            name: name.display().to_string(),
            text,
            lines,
        }
    }

    /// In characters; must agree with the column the JSON records carry.
    fn character_column(&self, offset: usize) -> usize {
        let Some(before) = self.text.get(..offset) else {
            return 0;
        };
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        before.get(line_start..).unwrap_or_default().chars().count()
    }
}

impl SourceCode for NamedText<'_> {
    /// miette's own answer over a bounded window (the span's lines plus a margin),
    /// translated back to whole-file coordinates. Delegating keeps one implementation
    /// of what a snippet contains.
    fn read_span<'a>(
        &'a self,
        span: &SourceSpan,
        context_lines_before: usize,
        context_lines_after: usize,
    ) -> Result<Box<dyn SpanContents<'a> + 'a>, MietteError> {
        // Slack past the requested context so the inner walk breaks on a line boundary
        // rather than on running out of input.
        let first_line = self
            .lines
            .line_of(span.offset())
            .saturating_sub(context_lines_before + 1);
        let window_start = self.lines.line_start(first_line);
        let last_line = self
            .lines
            .line_of(span.offset() + span.len())
            .saturating_add(context_lines_after + 2);
        let window_end = self.lines.line_start(last_line).min(self.text.len());
        let Some(window) = self
            .text
            .get(window_start..window_end)
            .filter(|_| span.offset() >= window_start)
        else {
            return Err(MietteError::OutOfBounds);
        };

        let local = SourceSpan::from((span.offset() - window_start, span.len()));
        let contents = window.read_span(&local, context_lines_before, context_lines_after)?;
        let contents_span = SourceSpan::from((
            contents.span().offset() + window_start,
            contents.span().len(),
        ));
        Ok(Box::new(MietteSpanContents::new_named(
            self.name.clone(),
            contents.data(),
            contents_span,
            contents.line() + first_line,
            self.character_column(contents_span.offset()),
            contents.line_count() + first_line,
        )))
    }
}

/// The nearest ancestor with a config file or `DESCRIPTION`, else the target's
/// directory. Nearest wins whichever marker it is, so a repository-root `ry.toml`
/// does not swallow a package in a subdirectory.
fn project_root_for_target(target: &Path) -> PathBuf {
    let mut current = if target.is_dir() {
        Some(target)
    } else {
        target.parent()
    };
    while let Some(directory) = current {
        if config::CONFIG_FILE_NAMES
            .iter()
            .any(|name| directory.join(name).is_file())
            || directory.join("DESCRIPTION").is_file()
        {
            return directory.to_path_buf();
        }
        current = directory.parent();
    }
    analysis_root_for_target(target)
}

pub(crate) fn analysis_root_for_target(target: &Path) -> PathBuf {
    if target.is_dir() {
        return target.to_path_buf();
    }
    match target.parent() {
        Some(parent) if parent.file_name().is_some_and(|name| name == "R") => parent
            .parent()
            .map(|root| root.to_path_buf())
            .unwrap_or_else(|| parent.to_path_buf()),
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::from("."),
    }
}

pub fn fmt(
    maybe_files: Option<&[PathBuf]>,
    check: bool,
    diff: bool,
    verbose: bool,
) -> Result<Outcome, CommandError> {
    let root: Vec<PathBuf> = vec![".".into()];
    let files = maybe_files.unwrap_or(&root);

    let paths_with_config = files
        .iter()
        .map(|file| {
            let config = config::Config::discover(file).map_err(|err| {
                report(&err);
                CommandError
            })?;
            warn_unknown_config_keys(&config);
            // `[check] exclude` scopes analysis, not formatting. Literate documents are never
            // formatted: most of an `.Rmd` is prose.
            let paths: Vec<PathBuf> = collect_r_files(file, None)?
                .into_iter()
                .filter(|path| {
                    !path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(is_literate_extension)
                })
                .collect();
            Ok((paths, config))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let start_global = Instant::now();
    let mut elapsed_total = Duration::ZERO;
    let mut bytes_total = 0usize;

    let mut n_files = 0;
    let mut n_unformatted = 0;
    let mut n_errors = 0;
    for (paths, config) in paths_with_config {
        for path in paths {
            n_files += 1;
            let initial = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    n_errors += 1;
                    error_because(&format!("failed to format: {}", path.display()), &err);
                    continue;
                }
            };

            let start = Instant::now();
            let new = match format::format(&initial, config.format) {
                Ok(new) => new,
                Err(err) => {
                    n_errors += 1;
                    error_because(&format!("failed to format: {}", path.display()), &err);
                    continue;
                }
            };
            elapsed_total += start.elapsed();
            bytes_total += initial.len();

            if initial != new {
                n_unformatted += 1;
                if diff {
                    eprintln!("Diff in {}:", path.display());
                    print_diff(&initial, &new);
                } else if check {
                    eprintln!("Would reformat: {}", style(path.display()).bold());
                } else if let Err(err) = fs::write(&path, &new) {
                    n_errors += 1;
                    error_because(
                        &format!("failed to write to file: {}", path.display()),
                        &err,
                    );
                }
            }
        }
    }

    // Not a usage error: a pre-commit hook may pass only a literate document, which
    // fmt deliberately skips.
    if n_files == 0 {
        info("0 files formatted");
        return Ok(Outcome::Clean);
    }

    let (action_format, action_skip) = if check || diff {
        ("would be reformatted", "already formatted")
    } else {
        ("reformatted", "left unchanged")
    };
    // An unreadable or unparsable file is not "already formatted".
    let n_unchanged = n_files - n_unformatted - n_errors;
    let failed = if n_errors == 0 {
        String::new()
    } else {
        format!(
            ", {} file{} could not be formatted",
            n_errors,
            if n_errors == 1 { "" } else { "s" }
        )
    };
    info(&format!(
        "{} file{} {}, {} file{} {}{}",
        n_unformatted,
        if n_unformatted == 1 { "" } else { "s" },
        action_format,
        n_unchanged,
        if n_unchanged == 1 { "" } else { "s" },
        action_skip,
        failed
    ));

    if verbose {
        let global_elapsed = start_global.elapsed();
        info(&format!(
            "Formatted {} bytes in {} ms ({}/s) - including I/O: {} ms ({}/s)",
            human_bytes(bytes_total as f64),
            elapsed_total.as_millis(),
            human_bytes(bytes_total as f64 / elapsed_total.as_secs_f64()),
            global_elapsed.as_millis(),
            human_bytes(bytes_total as f64 / global_elapsed.as_secs_f64())
        ));
    }

    if n_errors > 0 {
        return Err(CommandError);
    }
    Ok(if (check || diff) && n_unformatted > 0 {
        Outcome::Findings
    } else {
        Outcome::Clean
    })
}

pub fn ast(path: &Path) -> Result<(), CommandError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            error(&err.to_string());
            return Err(CommandError);
        }
    };
    let parse = syntax::parse(&text);
    eprintln!("{:#?}", parse.syntax_node());
    for error in parse.errors() {
        eprintln!(
            "error {:?}..{:?}: {}",
            u32::from(error.range.start()),
            u32::from(error.range.end()),
            error.message
        );
    }
    Ok(())
}

pub fn parse_experimental_flags(flags: &[impl AsRef<str>]) -> ExperimentalFeatures {
    let mut features = ExperimentalFeatures::default();
    for flag in flags.iter().flat_map(|flag| flag.as_ref().split(' ')) {
        match flag {
            "" => {}
            "all" => {
                for feature in ExperimentalFeatures::KNOWN {
                    let enabled = features.enable(feature.name);
                    debug_assert!(enabled, "KNOWN feature must enable");
                }
            }
            name => {
                if !features.enable(name) {
                    let known = ExperimentalFeatures::KNOWN
                        .iter()
                        .map(|feature| feature.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    warn(&format!(
                        "unknown experimental feature: '{name}' (known: {known}, or \"all\")"
                    ));
                }
            }
        }
    }
    features
}

fn print_diff(old: &str, new: &str) {
    use console::Style;
    use similar::{ChangeTag, TextDiff};

    struct Line(Option<usize>);
    impl fmt::Display for Line {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            match self.0 {
                None => write!(f, "    "),
                Some(index) => write!(f, "{:<4}", index + 1),
            }
        }
    }

    let diff = TextDiff::from_lines(old, new);
    for (index, group) in diff.grouped_ops(3).iter().enumerate() {
        if index > 0 {
            eprintln!("{:-^1$}", "-", 80);
        }
        for op in group {
            for change in diff.iter_inline_changes(op) {
                let (sign, sign_style) = match change.tag() {
                    ChangeTag::Delete => ("-", Style::new().red()),
                    ChangeTag::Insert => ("+", Style::new().green()),
                    ChangeTag::Equal => (" ", Style::new().dim()),
                };
                eprint!(
                    "{}{} |{}",
                    Line(change.old_index()),
                    Line(change.new_index()),
                    sign_style.apply_to(sign).bold(),
                );
                for (emphasized, value) in change.iter_strings_lossy() {
                    if emphasized {
                        eprint!("{}", sign_style.apply_to(value).underlined().on_black());
                    } else {
                        eprint!("{}", sign_style.apply_to(value));
                    }
                }
                if change.missing_newline() {
                    eprintln!();
                }
            }
        }
    }
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The windowed `read_span` must match miette's whole-file walk exactly, across
    /// every line ending, mid-line and boundary spans, and no trailing newline.
    #[test]
    fn the_windowed_snippet_matches_miettes_own() {
        let bodies = [
            "",
            "x",
            "x <- 1\n",
            "a <- 1\nb <- 2\nc <- 3\nd <- 4\ne <- 5\n",
            "a <- 1\r\nb <- 2\r\nc <- 3\r\nd <- 4\r\n",
            "a <- 1\rb <- 2\rc <- 3\r",
            "mixed <- 1\r\nlone <- 2\rboth <- 3\nlast <- 4",
            "caf\u{e9} <- \"\u{1f600}\"\nnext_line <- 2\n",
            "no_trailing_newline <- 1\nsecond <- 2",
            "\n\n\n\n",
        ];
        let mut compared = 0usize;
        for body in bodies {
            let lines = LineStarts::new(body);
            let named = NamedText {
                name: "probe.R".to_owned(),
                text: body,
                lines: &lines,
            };
            for start in 0..=body.len() {
                if !body.is_char_boundary(start) {
                    continue;
                }
                for length in 0..=4usize {
                    let span = SourceSpan::from((start, length));
                    for context in 0..=2usize {
                        let ours = named.read_span(&span, context, context);
                        let theirs = body.read_span(&span, context, context);
                        match (ours, theirs) {
                            (Ok(ours), Ok(theirs)) => {
                                let case = format!(
                                    "body {body:?} span {start}..+{length} context {context}"
                                );
                                assert_eq!(ours.data(), theirs.data(), "data: {case}");
                                assert_eq!(ours.span(), theirs.span(), "span: {case}");
                                assert_eq!(ours.line(), theirs.line(), "line: {case}");
                                assert_eq!(
                                    ours.line_count(),
                                    theirs.line_count(),
                                    "line count: {case}"
                                );
                                compared += 1;
                            }
                            (Err(_), Err(_)) => compared += 1,
                            (ours, theirs) => panic!(
                                "one side refused {body:?} span {start}..+{length} context \
                                 {context}: ours ok = {}, miette ok = {}",
                                ours.is_ok(),
                                theirs.is_ok()
                            ),
                        }
                    }
                }
            }
        }
        assert!(
            compared > 1_000,
            "expected a real sweep, compared {compared}"
        );
    }
}
