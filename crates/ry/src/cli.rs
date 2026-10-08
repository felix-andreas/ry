//! The CLI commands. Exit codes are a documented contract: 0 clean, 1
//! findings (diagnostics, or files a `fmt --check`/`--diff` run would
//! change), 2 usage/configuration/IO errors.

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
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use syntax::TextRange;

/// A run summary, saying what was checked and what was formatted. It is not a
/// finding, so it is a plain line rather than a report.
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

/// An error over an underlying failure, such as an IO error or a parse error.
/// The failure renders as the report's cause rather than as a bare line after
/// it.
pub fn error_because(message: &str, cause: &(dyn std::error::Error + 'static)) {
    report(&Report {
        message,
        cause: Some(cause),
        ..Report::default()
    });
}

/// Result of a CLI command, mapped by `main` onto the documented exit codes:
/// `Clean` exits 0 and `Findings` exits 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Findings,
}

/// A usage, configuration, or I/O failure, already reported on stderr;
/// `main` exits 2.
#[derive(Debug)]
pub struct CommandError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Rendered diagnostics with source snippets, on stderr
    Human,
    /// One JSON object per diagnostic (JSON Lines), on stdout
    Json,
}

/// The severity floor for `check` output: diagnostics below it are neither
/// rendered nor counted toward the exit code, so `--min-severity error`
/// makes warnings-only trees exit clean.
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

    // One project per root, however the user spelled the paths. A name
    // declared in a sibling file must resolve the same way for `check .`,
    // `check R/` and `check R/a.R R/b.R`, so analysis always covers the whole
    // root, and only *reporting* is scoped to the paths that were named.
    // Otherwise the verdict depends on the command line, and
    // `check $(git diff --name-only)` cannot be trusted as a gate.
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
        let target = std::fs::canonicalize(file).map_err(|err| {
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
        // Naming a single file always checks it: `exclude` scopes the project
        // walk, it does not veto a file the command pointed at directly. A
        // named *directory* is a walk like any other, so `exclude` still
        // applies inside it.
        for target in &requested {
            if !target.is_dir() && !paths.contains(target) {
                paths.push(target.clone());
            }
        }

        // Project override stubs fold on top of the shipped corpus (later
        // sources win); an unreadable override is an I/O failure because it
        // silently changes what every file below checks against.
        let mut stub_sources = semantics::stubs::shipped_stub_sources();
        let mut project_stub_files: Vec<(PathBuf, String)> = Vec::new();
        match discover_project_stubs(&root) {
            Ok(project_stubs) => {
                for stub in project_stubs {
                    stub_sources.push((stub.stem, stub.text.clone()));
                    project_stub_files.push((stub.path, stub.text));
                }
            }
            Err((path, err)) => {
                n_failures += 1;
                error_because(
                    &format!("failed to read override stub: {}", path.display()),
                    &err,
                );
            }
        }

        let mut db = RootDatabase::default();
        semantics::stubs::StubSources::new(
            &db,
            stub_sources,
            semantics::stubs::shipped_export_manifests(),
        );

        // A broken override stub silently changes what every file below
        // checks against, so what the loader drops is reported as findings
        // before the per-file diagnostics: one whole-line error per dropped
        // declaration.
        for (stub_path, stub_text) in &project_stub_files {
            let stub_index = LineIndex::new(stub_text);
            let stub_lines = LineStarts::new(stub_text);
            for problem in semantics::stubs::stub_source_problems(&db, stub_text) {
                n_diagnostics += 1;
                let start = stub_index.line_start(problem.line as u32);
                let end = start + stub_index.line_length(problem.line as u32, stub_text);
                let diagnostic = Diagnostic {
                    range: TextRange::new(start.into(), end.into()),
                    severity: Severity::Error,
                    code: "stub",
                    message: problem.message,
                    related: Vec::new(),
                };
                match output {
                    OutputFormat::Human => {
                        render_human_diagnostic(stub_path, stub_text, &stub_lines, &diagnostic, &[])
                    }
                    OutputFormat::Json => {
                        render_json_diagnostic(stub_path, stub_text, &stub_index, &diagnostic, &[])
                    }
                }
            }
        }

        let namespace_path = root.join("NAMESPACE");
        let namespace_source = std::fs::read_to_string(&namespace_path).ok();
        let namespace_imports = namespace_source
            .as_deref()
            .map(namespace::parse_namespace_imports)
            .unwrap_or_default();
        let description_source = std::fs::read_to_string(root.join("DESCRIPTION")).ok();
        let dependencies = description_source
            .as_deref()
            .map(semantics::metadata::parse_description_dependencies)
            .unwrap_or_default();
        let collate = description_source
            .as_deref()
            .map(semantics::metadata::parse_description_collate)
            .unwrap_or_default();
        let collate_rank: HashMap<&str, usize> = collate
            .iter()
            .enumerate()
            .map(|(rank, name)| (name.as_str(), rank))
            .collect();
        let metadata = semantics::metadata::PackageMetadata::new(
            &db,
            semantics::metadata::normalized_imports(&namespace_imports),
            dependencies,
            BTreeSet::new(),
            description_source
                .as_deref()
                .and_then(semantics::metadata::parse_description_package),
        );

        let r_path = root.join("R");
        let mut used_tokens = BTreeSet::new();
        let mut checked: Vec<(PathBuf, String)> = Vec::with_capacity(paths.len());
        // Which of the project's files the command actually asked about.
        let mut reported: Vec<bool> = Vec::with_capacity(paths.len());
        for path in paths {
            let source = match std::fs::read_to_string(&path) {
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

        // Feed the project in the server's order, so the last-writer-wins
        // symbol index selects the same winners. Package files come first, in
        // the DESCRIPTION `Collate` order when it is declared, with unlisted
        // files after the listed ones. The rest follow, ascending by
        // root-relative path.
        // Diagnostics still print in discovery order below.
        let mut ordered: Vec<usize> = (0..checked.len()).collect();
        ordered.sort_by_key(|index| {
            let path = &checked[*index].0;
            let rank = path
                .file_name()
                .and_then(|name| collate_rank.get(name.to_string_lossy().as_ref()))
                .copied()
                .unwrap_or(usize::MAX);
            (
                !path.starts_with(&r_path),
                rank,
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        });
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
        ProjectFiles::new(&db, project.clone());
        let attached = semantics::metadata::attached_union(&db, project);
        if !attached.is_empty() {
            use salsa::Setter;
            metadata.set_attached(&mut db).to(attached);
        }

        let path_by_file: std::collections::HashMap<SourceFile, &PathBuf> = checked
            .iter()
            .enumerate()
            .filter_map(|(index, (path, _))| files[index].map(|file| (file, path)))
            .collect();
        // The cold pass fans out across cores: salsa storage-handle clones
        // share memos, so threads compute disjoint files concurrently (the
        // parallel stress suite gates this). Rendering stays sequential in
        // discovery order below.
        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1)
            .min(checked.len().max(1));
        // Warm per-item naming (and the stub corpus) across cores first: the
        // project-wide interface walk demands naming for every item, and
        // computed inside that one salsa query it serializes the whole
        // front half of the cold pass on a single thread. With the memos
        // warm the walk is a cheap graph assembly. Files are dealt to
        // workers largest-first so one big file cannot skew a chunk.
        // Worker threads need the analysis stack: cross-file interface
        // resolution recurses per dependency edge, so a long definition
        // chain overflows a default-sized thread long before the dedicated
        // command thread would.
        std::thread::scope(|scope| {
            {
                let db = db.clone();
                std::thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        let _ = semantics::stubs::stubs(&db);
                    })
                    .expect("check worker thread should spawn");
            }
            let mut by_size: Vec<usize> = (0..checked.len()).collect();
            by_size.sort_by_key(|&index| std::cmp::Reverse(checked[index].1.len()));
            for worker in 0..workers {
                let db = db.clone();
                let files = &files;
                let deal: Vec<usize> = by_size
                    .iter()
                    .copied()
                    .skip(worker)
                    .step_by(workers)
                    .collect();
                std::thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for index in deal {
                            let Some(file) = files[index] else {
                                continue;
                            };
                            for &item in semantics::item_tree(&db, file) {
                                let _ = semantics::item_naming(&db, item);
                            }
                        }
                    })
                    .expect("check worker thread should spawn");
            }
        });
        let mut per_file: Vec<Vec<Diagnostic>> = (0..checked.len()).map(|_| Vec::new()).collect();
        std::thread::scope(|scope| {
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
                let worker = std::thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for (index, slot) in chunk {
                            let (_, source) = &checked[index];
                            let file =
                                files[index].expect("every checked file was fed to the project");
                            let rendered = document_diagnostics(&db, file, config);
                            *slot = apply_suppressions(rendered, source);
                        }
                    });
                worker.expect("check worker thread should spawn");
            }
        });
        // A related range routinely lives in another of the project's files,
        // so a note is resolved against the document it points into rather
        // than the one being reported.
        let resolve_note = |related: &RelatedLocation| {
            Some(RelatedNote {
                path: path_by_file.get(&related.file)?.as_path(),
                source: related.file.text(&db),
                range: related.range,
                message: related.message,
            })
        };
        for (index, (path, source)) in checked.iter().enumerate() {
            if !reported.get(index).copied().unwrap_or(false) {
                continue;
            }
            let line_index = LineIndex::new(source);
            let snippet_lines = LineStarts::new(source);
            for diagnostic in &per_file[index] {
                if min_severity == MinSeverity::Error && diagnostic.severity != Severity::Error {
                    continue;
                }
                n_diagnostics += 1;
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
        }

        // Package-level NAMESPACE diagnostics: the import-typo check (a
        // warning per `importFrom` naming something a known namespace does
        // not export) and the opt-in `unused-import` lint (an imported name
        // appearing in no checked source's token set). Emitted last so the
        // lint sees the whole package.
        if let Some(namespace_source) = &namespace_source {
            let knows =
                |package: &str| semantics::stubs::namespace_known(&db, package).unwrap_or(false);
            let exports =
                |package: &str, name: &str| semantics::stubs::namespace_exports(&db, package, name);
            let mut problems =
                namespace::namespace_import_problems(&namespace_imports, &knows, &exports);
            // `export(name)` naming nothing the package defines is `R CMD
            // check`'s "undefined exports", caught here instead of at install.
            let defines = |name: &str| {
                semantics::ProjectFiles::try_get(&db).is_some_and(|files| {
                    semantics::package_definitions(&db, files).contains_key(name)
                })
            };
            problems.extend(namespace::namespace_export_problems(
                &semantics::metadata::parse_namespace_exports(namespace_source),
                &defines,
            ));
            problems.extend(namespace::unused_import_diagnostics(
                &namespace_imports,
                &used_tokens,
                config.lint.unused_import,
            ));
            let index = LineIndex::new(namespace_source);
            for diagnostic in problems {
                if min_severity == MinSeverity::Error && diagnostic.severity != Severity::Error {
                    continue;
                }
                n_diagnostics += 1;
                match output {
                    OutputFormat::Human => render_human_diagnostic(
                        &namespace_path,
                        namespace_source,
                        &LineStarts::new(namespace_source),
                        &diagnostic,
                        &[],
                    ),
                    OutputFormat::Json => render_json_diagnostic(
                        &namespace_path,
                        namespace_source,
                        &index,
                        &diagnostic,
                        &[],
                    ),
                }
            }
        }
    }

    if n_failures > 0 {
        return Err(CommandError);
    }
    // A summary always, so a clean run and a run that matched nothing never
    // look alike: silence would otherwise read as success on a tree of `.Rmd`
    // files, of which none is analysed.
    if output == OutputFormat::Human {
        report_check_summary(n_files, n_diagnostics);
    }
    Ok(if n_diagnostics == 0 {
        Outcome::Clean
    } else {
        Outcome::Findings
    })
}

/// `check`'s closing line. An empty tree is not a usage error, because a
/// monorepo stage with no R in it yet must not fail a pipeline. It reports
/// that it found nothing and exits clean.
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

/// The R program a file contributes to analysis. For an ordinary file that is
/// its own text. For a literate document it is the R its chunks contain, with
/// the prose blanked, so every reported range still points at the original
/// file.
pub(crate) fn analysable_source(path: &Path, text: String) -> String {
    let literate = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(syntax::literate::is_literate_extension);
    if literate {
        syntax::literate::r_source_of_literate(&text)
    } else {
        text
    }
}

/// Unknown config keys warn but never block: the rest of the file is
/// honored (forward compatibility for configs written against newer
/// versions). Wrong types on known keys remain hard errors.
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

/// The `[check] exclude` matcher: gitignore-style patterns anchored at the
/// config file's directory (the walk target's directory when no config file
/// exists). The anchor is canonicalized so matching agrees with the
/// canonicalized walk paths. A canonical path on Windows carries the `\\?\`
/// prefix, and a non-canonical anchor would silently never match.
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
    let root = std::fs::canonicalize(&root).unwrap_or(root);
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

/// Walks `target` for R sources. Excluded directories are pruned without
/// descending. A `target` that is itself a file bypasses exclusion, because a
/// file named explicitly is always checked.
/// Directories that hold vendored dependency infrastructure rather than
/// project code. `renv/activate.R` alone is a thousand generated lines every
/// `renv`-using project would otherwise see reported. A path named explicitly
/// on the command line still wins, because this only scopes the directory
/// walk.
const VENDORED_DIRECTORIES: [&str; 5] = ["renv", "packrat", "revdep", ".Rproj.user", ".Rcheck"];

/// Whether a file shares one namespace with its siblings rather than being
/// analysed alone. Two directories do: `R/`, whose files are all sourced into
/// the package environment, and `tests/testthat/`, whose files testthat sources
/// into one environment as well. testthat sources `helper-*.R` first, then the
/// test files, which is the documented way to share a fixture. Analysing those separately reported
/// every helper as unused *and* every use of one as unresolved, two findings per
/// helper on a package that is perfectly correct.
///
/// **Every surface that assigns a [`DocumentKind`] must call this**, or it
/// analyses a different program than the others. Three copies of the rule
/// existed and one of them counted `R/` alone, which is why `analysis-stats`
/// reported three findings on a testthat package where `check` reported none.
/// An instrument must not disagree with the product it exists to measure. This
/// is not the *ordering* key. A package file sorts ahead of a script by
/// `R/`-membership alone, so a testthat file shares the namespace while still
/// sorting after every `R/` file.
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
    // Honour `.gitignore` whether or not the tree is a git checkout: a
    // generated or vendored file is not project code either way.
    builder.require_git(false);
    // ONE predicate for both rules: `filter_entry` replaces the previous
    // closure rather than composing with it, so registering the configured
    // excludes separately would silently drop the vendored-directory skip and
    // walk a project's whole `renv` library.
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
                        extension == "R"
                            || extension == "r"
                            || syntax::literate::is_literate_extension(extension)
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

/// One project override stub under `<root>/stubs/`: its path, file stem
/// (the namespace label), and text.
pub(crate) struct ProjectStub {
    pub(crate) path: PathBuf,
    pub(crate) stem: String,
    pub(crate) text: String,
}

/// The project override stubs under `<root>/stubs/*.Rtypes`, in path order.
pub(crate) fn discover_project_stubs(
    root: &Path,
) -> Result<Vec<ProjectStub>, (PathBuf, std::io::Error)> {
    let stubs_dir = root.join("stubs");
    let Ok(entries) = std::fs::read_dir(&stubs_dir) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "Rtypes")
        })
        .collect();
    paths.sort();
    let mut sources = Vec::new();
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let stem = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                sources.push(ProjectStub { path, stem, text });
            }
            Err(err) => return Err((path, err)),
        }
    }
    Ok(sources)
}

/// One companion location of a diagnostic, resolved to the file it lives in.
/// A related range routinely sits in another document, such as the sibling
/// binding an overwrite warning points at, so each note carries its own text to
/// render from.
struct RelatedNote<'a> {
    path: &'a Path,
    source: &'a str,
    range: TextRange,
    message: &'static str,
}

/// Renders one diagnostic on stderr with miette's graphical reporter: the
/// code, the message, the source snippet with the range underlined, and one
/// nested report per related location, drawn from that location's own file.
fn render_human_diagnostic(
    path: &Path,
    source: &str,
    lines: &LineStarts,
    diagnostic: &Diagnostic,
    related: &[RelatedNote],
) {
    // A companion location lives in another file, whose line table this run has
    // not built. There are only ever a handful per finding, so building one here
    // costs a walk each. That is set against one walk per finding, which is
    // the point.
    let related_lines: Vec<LineStarts> = related
        .iter()
        .map(|note| LineStarts::new(note.source))
        .collect();
    report(&Report {
        // The code heads the report, because it is what a suppression comment
        // must spell, as `# ry: allow(unused)` does. The human output
        // therefore teaches it.
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
                // A companion location is context for the finding above it,
                // never a finding of its own: advice severity is what keeps a
                // reader from counting it as a second problem.
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
    // Findings arrive in runs; a blank line between them keeps two adjacent
    // snippets from reading as one.
    eprintln!();
}

/// The label under the finding: its own range, clamped to the first line when
/// the range covers more lines than a snippet should show. Drawing every line
/// of a range that spans a whole function buries the finding in the item that
/// contains it.
fn primary_label(source: &str, range: TextRange) -> LabeledSpan {
    const MAX_UNDERLINED_LINES: usize = 3;

    let start = usize::from(range.start());
    let mut end = usize::from(range.end()).min(source.len());
    // An empty range has nothing to underline, so it takes the character it
    // points at. A parser error at a position rather than over a token
    // produces one. At end of file there is no such character, and the header
    // position alone has to carry it.
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

/// Renders one diagnostic as a JSON Lines record on stdout for CI use.
/// Positions are 1-based like the human renderer; the field names are a
/// documented contract.
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

/// Everything the CLI reports, in one shape: a finding with its snippet, a
/// companion note under one, or a bare failure with no source at all. A
/// configuration error and a type error are drawn by the same reporter, so
/// they can never look like output from two different tools.
#[derive(Debug)]
struct Report<'a> {
    severity: ReportSeverity,
    /// The diagnostic code, for the findings that carry one.
    code: Option<&'a str>,
    message: &'a str,
    /// The underlying failure, drawn as the report's cause.
    cause: Option<&'a (dyn std::error::Error + 'static)>,
    snippet: Option<Snippet<'a>>,
    related: Vec<Report<'a>>,
}

/// The source excerpt a report points at, and the range it underlines.
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

impl std::fmt::Display for Report<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for Report<'_> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
    }
}

impl miette::Diagnostic for Report<'_> {
    fn code(&self) -> Option<Box<dyn std::fmt::Display + '_>> {
        self.code
            .map(|code| Box::new(code) as Box<dyn std::fmt::Display>)
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
        Some(Box::new(std::iter::once(snippet.label.clone())))
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

/// Draws one report on stderr. The look follows the destination: colour and
/// unicode rules on an attended terminal, monochrome unicode when colour is
/// refused, and plain ASCII into a pipe or a file. A CI log and a captured
/// expectation both want the ASCII.
pub fn report(diagnostic: &dyn miette::Diagnostic) {
    let reporter = &*REPORTER;
    let mut rendered = String::new();
    if reporter
        .handler
        .render_report(&mut rendered, diagnostic)
        .is_err()
    {
        // Formatting into a `String` cannot fail; the fallback is here so a
        // reporter bug degrades the finding rather than swallowing it.
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

/// The graphical reporter, built once, and the closing rule its theme draws.
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
    // A snippet is a window on the source, not a box: the gutter runs
    // unbroken down every row of it, and the header opens with a plain rule
    // rather than a corner that promises a frame nothing closes.
    theme.characters.vbar_break = theme.characters.vbar;
    theme.characters.ltop = theme.characters.hbar;
    // Carets go under the reported range rather than a rule. The underline is
    // the one part of the drawing a reader is meant to look at.
    theme.characters.underline = '^';

    let closing_rule = format!(
        "{}{}",
        theme.characters.lbot,
        theme.characters.hbar.to_string().repeat(CLOSING_RULE_WIDTH)
    );
    let handler = GraphicalReportHandler::new_themed(theme)
        // A companion location reads as part of the finding above it, so it
        // is drawn nested under that finding rather than as a report of its
        // own with its own severity banner.
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

/// Whether `line` is the rule miette closes every snippet with. It is drawn
/// from the same two characters as the cause-chain arrows, so it cannot be
/// themed away, and a run of findings reads better without one under each.
/// Only a line that is nothing *but* the rule counts. A nested report indents
/// its own rule, in the parent's colour, and R source may spell anything at
/// all.
fn closes_a_snippet(line: &str, closing_rule: &str) -> bool {
    line.trim_end()
        .strip_suffix(closing_rule)
        .is_some_and(|before| console::strip_ansi_codes(before).trim().is_empty())
}

/// How many horizontal bars miette's snippet-closing rule carries. Read off
/// miette 7.6's `GraphicalReportHandler`, which hardcodes the width rather than
/// exposing it on the theme, so an upgrade can move it. Nothing breaks
/// silently if it does. The rule then stops matching and reappears under every
/// snippet, which `check_draws_the_snippet_as_a_window` fails on.
const CLOSING_RULE_WIDTH: usize = 4;

/// Where each line of one file starts, by miette's own rule for what ends a
/// line: `\n`, `\r\n`, or a lone `\r`.
///
/// It exists to bound snippet drawing. miette finds a span's lines by walking
/// the source from byte zero, so a run reporting many findings against one file
/// re-walks that file once per finding. On a file with 8,000 findings that was
/// 6.7 s of a 7.3 s run, which is 98% of the reporter. Built once per file,
/// this turns the walk into a binary search plus a bounded window.
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

    /// The zero-based line a byte offset falls on, clamped to the last line.
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

/// A borrowed file behind a snippet, named for the snippet header.
/// [`miette::NamedSource`] owns its text, which would copy the whole file into
/// every finding reported against it.
#[derive(Debug)]
struct NamedText<'a> {
    name: String,
    text: &'a str,
    lines: &'a LineStarts,
}

impl<'a> NamedText<'a> {
    /// Names the file the way the reader named it: relative to the working
    /// directory when it lies below it, as given otherwise.
    fn new(path: &Path, text: &'a str, lines: &'a LineStarts) -> NamedText<'a> {
        static WORKING_DIRECTORY: LazyLock<Option<PathBuf>> =
            LazyLock::new(|| std::env::current_dir().ok());
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

    /// The zero-based column of a byte offset, counted in characters. A
    /// column is what a person counts and an editor shows, so non-ASCII text
    /// earlier on the line must not shift it. It also has to agree with the
    /// column the JSON records carry for the very same finding.
    fn character_column(&self, offset: usize) -> usize {
        let Some(before) = self.text.get(..offset) else {
            return 0;
        };
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        before.get(line_start..).unwrap_or_default().chars().count()
    }
}

impl SourceCode for NamedText<'_> {
    /// miette's own answer, computed over a bounded window instead of the whole
    /// file. The window is the span's lines plus a margin wider than the context
    /// asked for, so the walk inside sees everything it would have seen and
    /// stops where it would have stopped; the result is then translated back
    /// into whole-file coordinates. Delegating keeps one implementation of
    /// what a snippet contains. Reimplementing it is how a drawing drifts while
    /// still looking plausible.
    fn read_span<'a>(
        &'a self,
        span: &SourceSpan,
        context_lines_before: usize,
        context_lines_after: usize,
    ) -> Result<Box<dyn SpanContents<'a> + 'a>, MietteError> {
        // A line of slack past the requested context on each side, so the walk
        // inside breaks on a line boundary rather than on running out of input.
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

/// The project root a target is analysed in: the NEAREST ancestor carrying a
/// config file or a `DESCRIPTION`, else the target's own directory. This is
/// what makes the answer independent of how the command names its paths.
///
/// Nearest wins whichever marker it is, so a `ry.toml` at a repository
/// root does not swallow a package in a subdirectory. That package's own
/// `DESCRIPTION` is closer, and its `R/` must still be package source. The
/// ancestor config still *configures* it, and only the root is decided
/// here.
fn project_root_for_target(target: &Path) -> PathBuf {
    let mut current = if target.is_dir() {
        Some(target)
    } else {
        target.parent()
    };
    while let Some(directory) = current {
        if crate::config::CONFIG_FILE_NAMES
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
    // A file directly under an `R/` directory is a package source file; its
    // package root is the directory containing `R/`.
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
            // `[check] exclude` scopes analysis, not formatting: fmt walks
            // everything, matching the key's name and the formatter's speed.
            // A literate document is analysed but never formatted. The
            // formatter rewrites whole-file layout, and most of an `.Rmd` is
            // prose it has no business touching.
            let paths: Vec<PathBuf> = collect_r_files(file, None)?
                .into_iter()
                .filter(|path| {
                    !path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(syntax::literate::is_literate_extension)
                })
                .collect();
            Ok((paths, config))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let start_global = std::time::Instant::now();
    let mut elapsed_total = std::time::Duration::ZERO;
    let mut bytes_total = 0usize;

    let mut n_files = 0;
    let mut n_unformatted = 0;
    let mut n_errors = 0;
    for (paths, config) in paths_with_config {
        for path in paths {
            n_files += 1;
            let initial = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    n_errors += 1;
                    error_because(&format!("failed to format: {}", path.display()), &err);
                    continue;
                }
            };

            let start = std::time::Instant::now();
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
                } else if let Err(err) = std::fs::write(&path, &new) {
                    n_errors += 1;
                    error_because(
                        &format!("failed to write to file: {}", path.display()),
                        &err,
                    );
                }
            }
        }
    }

    // Nothing to format is not a usage error, exactly as for `check`: a
    // pre-commit hook passing one changed file at a time hands the formatter a
    // literate document it deliberately skips, and failing there fails the
    // commit over a file the tool was never going to touch.
    if n_files == 0 {
        info("0 files formatted");
        return Ok(Outcome::Clean);
    }

    let (action_format, action_skip) = if check || diff {
        ("would be reformatted", "already formatted")
    } else {
        ("reformatted", "left unchanged")
    };
    // A file that could not be read or parsed is neither reformatted nor
    // "already formatted". Counting it as the latter told the reader their
    // whole tree was clean when part of it was never looked at.
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

pub fn server(experimental_features: ExperimentalFeatures, debug: bool) {
    crate::server::run(experimental_features, debug);
}

pub fn ast(path: &Path) -> Result<(), CommandError> {
    let text = match std::fs::read_to_string(path) {
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
    impl std::fmt::Display for Line {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
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

    /// The windowed `read_span` must answer exactly what miette's own
    /// whole-file walk answers, with the same bytes, span, line, and line
    /// count, or the drawing drifts while still looking plausible. The sweep
    /// covers every line ending an R file carries, a span that starts mid-line,
    /// a span that crosses a break, a span at the very start and at the very
    /// end, and a file with no trailing newline.
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
