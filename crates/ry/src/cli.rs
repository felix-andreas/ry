use {
    crate::{
        config::{self, ExperimentalFeatures},
        diagnostics::{apply_suppressions, document_diagnostics},
        namespace,
        position::LineIndex,
    },
    console::style,
    ignore::gitignore::{Gitignore, GitignoreBuilder},
    miette::{
        GraphicalReportHandler, GraphicalTheme, LabeledSpan, MietteError, MietteSpanContents,
        Severity as ReportSeverity, SourceCode, SourceSpan, SpanContents,
    },
    rustc_hash::FxHashSet,
    salsa::Durability,
    semantics::{
        AnalysisSettings, DataFiles, DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{Diagnostic, RelatedLocation, Severity, derive_from_syntax},
        metadata::{
            PackageMetadata, attached_union, normalized_imports, parse_description_collate,
            parse_description_dependencies, parse_description_package, parse_namespace,
        },
        stubs::{
            StubSources, namespace_exports, namespace_known, shipped_export_manifests,
            shipped_stub_sources, stub_source_problems,
        },
        test_document_kind,
    },
    std::{
        cmp::Reverse,
        collections::{BTreeMap, BTreeSet, HashMap},
        env, fmt, fs,
        io::Read,
        iter, mem,
        num::NonZero,
        path::{Path, PathBuf},
        sync::{
            LazyLock, Mutex, PoisonError,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    },
    syntax::{
        Parse, TextRange,
        literate::{is_literate_extension, r_source_of_literate},
    },
};

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
        warn_config(&config);
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
        let analysis = analyze_project(&root, config, &requested)?;
        n_failures += analysis.failures;
        for (path, source, problems) in &analysis.stub_problems {
            n_diagnostics +=
                render_findings(output, min_severity, path, source, problems, &|_| None);
        }
        let resolve_note = |related: &RelatedLocation| analysis.related_note(related);
        for file in analysis.files.iter().filter(|file| file.requested) {
            n_files += 1;
            n_diagnostics += render_findings(
                output,
                min_severity,
                &file.path,
                analysis.source(file),
                &file.diagnostics,
                &resolve_note,
            );
        }
        if let Some((path, source, problems)) = &analysis.namespace {
            n_diagnostics +=
                render_findings(output, min_severity, path, source, problems, &|_| None);
        }
        // Freeing every memo one by one costs close to a second on a large project, and the
        // process is about to exit.
        std::mem::forget(analysis);
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

/// One project's analysis, as `ry check` reports it and as `ry load`'s strict-mode check reads
/// it. Analysis always covers the whole project root; `requested` only marks which files the
/// caller asked about.
pub(crate) struct ProjectAnalysis {
    pub db: RootDatabase,
    pub files: Vec<AnalyzedFile>,
    /// A broken override stub's dropped declarations: its path, text and findings.
    pub stub_problems: Vec<(PathBuf, String, Vec<Diagnostic>)>,
    /// `NAMESPACE`'s findings, when the project has one.
    pub namespace: Option<(PathBuf, String, Vec<Diagnostic>)>,
    /// Files that could not be read, each already reported.
    pub failures: usize,
}

pub(crate) struct AnalyzedFile {
    pub path: PathBuf,
    pub file: SourceFile,
    pub requested: bool,
    pub diagnostics: Vec<Diagnostic>,
}

impl ProjectAnalysis {
    /// The text lives only in the database, so a large project holds one copy of it.
    pub fn source(&self, file: &AnalyzedFile) -> &str {
        file.file.text(&self.db)
    }

    /// Resolves a related range against the document it lives in, which is often another file.
    pub fn related_note(&self, related: &RelatedLocation) -> Option<RelatedNote<'_>> {
        let analyzed = self
            .files
            .iter()
            .find(|analyzed| analyzed.file == related.file)?;
        Some(RelatedNote {
            path: &analyzed.path,
            source: self.source(analyzed),
            range: related.range,
            message: related.message,
        })
    }
}

pub(crate) fn analyze_project(
    root: &Path,
    config: config::Config,
    requested: &[PathBuf],
) -> Result<ProjectAnalysis, CommandError> {
    let mut failures = 0;
    let exclude = exclude_matcher(&config, root)?;
    let mut paths = collect_r_files(root, exclude.as_ref())?;
    // `exclude` scopes the project walk; it does not veto a file named directly.
    for target in requested {
        if !target.is_dir() && !paths.contains(target) {
            paths.push(target.clone());
        }
    }

    // Override stubs fold on top of the shipped corpus (later sources win); an
    // unreadable one is an I/O failure.
    let project_stubs = match read_project_stubs(root) {
        Ok(project_stubs) => project_stubs,
        Err((path, err)) => {
            failures += 1;
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

    // A one-shot check changes one input after analysis starts: the attached namespaces,
    // known only once every file is parsed. Every other input is high durability, so a memo
    // that never read the attached set skips verification in the revision that sets it.
    let mut db = RootDatabase::one_shot();
    let _: StubSources = StubSources::builder(stub_sources, shipped_export_manifests())
        .durability(Durability::HIGH)
        .new(&db);
    let _: AnalysisSettings =
        AnalysisSettings::builder(config.check.inline_annotations, config.check.strict)
            .durability(Durability::HIGH)
            .new(&db);

    let namespace_path = root.join("NAMESPACE");
    let namespace_source = fs::read_to_string(&namespace_path).ok();
    let namespace_file = namespace_source
        .as_deref()
        .map(parse_namespace)
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
    let metadata = PackageMetadata::builder(
        normalized_imports(&namespace_file.imports),
        dependencies,
        BTreeSet::new(),
        description_source
            .as_deref()
            .and_then(parse_description_package),
        namespace_file.native_symbols,
    )
    .durability(Durability::HIGH)
    .attached_durability(Durability::LOW)
    .new(&db);

    // A broken override stub silently changes what every file checks against, so
    // its dropped declarations are reported as findings. This reads the stub library, so it
    // must follow the metadata input: a high-durability memo computed before that input
    // existed is never re-verified, and would keep every conditional namespace inactive.
    let mut stub_problems = Vec::new();
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
        stub_problems.push((stub.path.clone(), stub_text.clone(), problems));
    }

    let mut checked: Vec<(PathBuf, String)> = Vec::with_capacity(paths.len());
    let mut reported: Vec<bool> = Vec::with_capacity(paths.len());
    for path in paths {
        let source = match fs::read_to_string(&path) {
            Ok(source) => analysable_source(&path, source),
            Err(err) => {
                failures += 1;
                error_because(&format!("failed to read: {}", path.display()), &err);
                continue;
            }
        };
        let asked_about = requested
            .iter()
            .any(|target| path == *target || path.starts_with(target));
        reported.push(asked_about);
        checked.push((path, source));
    }

    // Analysis follows the project order; diagnostics still print in discovery order.
    let order = project_order(root, &collate);
    let mut ordered: Vec<usize> = (0..checked.len()).collect();
    ordered.sort_by_cached_key(|&index| order(&checked[index].0));
    let mut files: Vec<Option<SourceFile>> = vec![None; checked.len()];
    let mut project = Vec::with_capacity(checked.len());
    for index in ordered {
        let (path, source) = &mut checked[index];
        let kind = document_kind(path, root);
        // `ordered` is a permutation, so each source is moved out exactly once.
        let file = SourceFile::builder(mem::take(source), kind)
            .durability(Durability::HIGH)
            .new(&db);
        files[index] = Some(file);
        project.push(file);
    }
    let files: Vec<SourceFile> = files.into_iter().flatten().collect();
    let _: ProjectFiles = ProjectFiles::builder(project.clone())
        .durability(Durability::HIGH)
        .new(&db);
    let _: DataFiles = DataFiles::builder(scan_data_files(&root))
        .durability(Durability::HIGH)
        .new(&db);
    let paths: Vec<PathBuf> = checked.into_iter().map(|(path, _)| path).collect();
    // Salsa storage-handle clones share memos, so threads compute disjoint files concurrently.
    // Workers need the analysis stack: interface resolution recurses per dependency edge.
    let workers = thread::available_parallelism()
        .map(NonZero::get)
        .unwrap_or(1)
        .min(files.len().max(1));
    // Largest files first, so the last file a worker picks up is a small one.
    let mut by_size: Vec<usize> = (0..files.len()).collect();
    by_size.sort_by_cached_key(|&index| Reverse(files[index].text(&db).len()));

    // Everything later passes read from a tree is derived here, across all cores, so a check
    // that reaches another file never walks that file's tree on its own thread.
    let imported = namespace::tracked_names(&namespace_file.imports, &namespace_file.exports);
    let want_tokens = namespace_source.is_some() && !imported.is_empty();
    let mut used_tokens = FxHashSet::default();
    for (_, tokens) in in_parallel(&db, workers, &by_size, |db, index| {
        let file = files[index];
        derive_from_syntax(db, file, &config.lint);
        let mut tokens = FxHashSet::default();
        if want_tokens {
            namespace::collect_used_tokens(
                &semantics::parse(db, file).syntax_node(),
                &imported,
                &mut tokens,
            );
        }
        tokens
    }) {
        used_tokens.extend(tokens);
    }
    let attached = attached_union(&db, project);
    // Freeing a million tree nodes takes longer than naming the first files, so each naming
    // worker frees the tree of the file it is about to name, across all cores.
    let trees: Vec<Mutex<Option<Parse>>> = semantics::release_trees(&mut db, &files)
        .into_iter()
        .map(|tree| Mutex::new(Some(tree)))
        .collect();
    if !attached.is_empty() {
        use salsa::Setter;
        metadata.set_attached(&mut db).to(attached);
    }

    // Every item is named across cores first: a file's check names the items of other files its
    // interface reaches, and two threads reaching the same item would block on one claim.
    in_parallel(&db, workers, &by_size, |db, index| {
        if let Some(tree) = trees.get(index) {
            drop(tree.lock().unwrap_or_else(PoisonError::into_inner).take());
        }
        let file = files[index];
        for &item in semantics::item_tree(db, file) {
            semantics::item_naming(db, item);
        }
    });
    let mut per_file: Vec<Vec<Diagnostic>> = (0..files.len()).map(|_| Vec::new()).collect();
    for (index, diagnostics) in in_parallel(&db, workers, &by_size, |db, index| {
        let rendered = document_diagnostics(db, files[index], &config);
        apply_suppressions(rendered, files[index].text(db))
    }) {
        per_file[index] = diagnostics;
    }
    // Computed last so the `unused-import` lint sees the whole package.
    let mut namespace = None;
    if let Some(namespace_source) = &namespace_source {
        let knows = |package: &str| namespace_known(&db, package).unwrap_or(false);
        let exports = |package: &str, name: &str| namespace_exports(&db, package, name);
        let mut problems =
            namespace::namespace_import_problems(&namespace_file.imports, &knows, &exports);
        // R resolves an export with `exists()` from the namespace, which inherits through base
        // and the default search path, so `export(diff)` beside `diff.git_repository` loads.
        let defines = |name: &str| {
            ProjectFiles::try_get(&db)
                .is_some_and(|files| semantics::package_binds(&db, files, name))
                || semantics::check::DEFAULT_SEARCH_PATH
                    .iter()
                    .any(|package| namespace_exports(&db, package, name))
        };
        problems.extend(namespace::namespace_export_problems(
            &namespace_file.exports,
            &namespace_file.imports,
            &defines,
            &knows,
            &exports,
            &used_tokens,
        ));
        problems.extend(namespace::unused_import_diagnostics(
            &namespace_file.imports,
            &used_tokens,
            config.lint.unused_import,
        ));
        namespace = Some((namespace_path, namespace_source.clone(), problems));
    }
    let files = paths
        .into_iter()
        .zip(files)
        .zip(reported)
        .zip(per_file)
        .map(|(((path, file), requested), diagnostics)| AnalyzedFile {
            path,
            file,
            requested,
            diagnostics,
        })
        .collect();
    Ok(ProjectAnalysis {
        db,
        files,
        stub_problems,
        namespace,
        failures,
    })
}

/// Runs `work` for every index in `order` across `workers` threads.
fn in_parallel<T: Send>(
    db: &RootDatabase,
    workers: usize,
    order: &[usize],
    work: impl Fn(&RootDatabase, usize) -> T + Sync,
) -> Vec<(usize, T)> {
    let mut results = Vec::with_capacity(order.len());
    // Shared rather than dealt out: check times vary by orders of magnitude per file.
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(order.len()))
            .map(|_| {
                let db = db.clone();
                let (next, work) = (&next, &work);
                thread::Builder::new()
                    .stack_size(crate::ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        let mut done = Vec::new();
                        while let Some(&index) = order.get(next.fetch_add(1, Ordering::Relaxed)) {
                            done.push((index, work(&db, index)));
                        }
                        done
                    })
                    .expect("check worker thread should spawn")
            })
            .collect();
        for handle in handles {
            results.extend(handle.join().expect("check worker should not panic"));
        }
    });
    results
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

pub(crate) fn warn_config(config: &config::Config) {
    for warning in config.warnings() {
        warn(&warning);
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

/// `R/` files form the package namespace. Of the files directly in `tests/testthat/`, the
/// helper and setup files fill the environment testthat runs every test file in, and the rest
/// are test files, each in its own child of it. Every surface assigning a [`DocumentKind`]
/// must call this, or it analyses a different program. Not the ordering key: that is `R/`
/// membership alone.
pub(crate) fn document_kind(path: &Path, root: &Path) -> DocumentKind {
    if path.starts_with(root.join("R")) {
        return DocumentKind::Package;
    }
    if path
        .parent()
        .is_none_or(|parent| parent != root.join("tests").join("testthat"))
    {
        return DocumentKind::Script;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    test_document_kind(file_name)
}

/// How much of a data file the checker sniffs: enough rows to type the columns, and little
/// enough that a project with large data files still starts fast.
const DATA_FILE_HEAD_BYTES: u64 = 64 * 1024;

/// The layouts of the project's `.csv` and `.tsv` files, keyed by their path relative to
/// `root`, so `fread("data/prices.csv")` types with the file's columns. Data directories are
/// commonly gitignored, so this walk ignores `.gitignore` and skips only hidden and vendored
/// directories.
pub(crate) fn scan_data_files(root: &Path) -> BTreeMap<String, sniff::Layout> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder.standard_filters(false).hidden(true);
    builder.filter_entry(|entry| {
        let is_dir = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir());
        !(is_dir
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| VENDORED_DIRECTORIES.contains(&name)))
    });
    builder
        .build()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let key = data_file_key(root, path)?;
            Some((key, sniff_data_file(path)?))
        })
        .collect()
}

/// The project-relative key of a data file, or `None` for any other path.
pub(crate) fn data_file_key(root: &Path, path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if extension != "csv" && extension != "tsv" {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<&str> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<_>>()?;
    Some(parts.join("/"))
}

/// An unreadable data file has no layout, and reads of it type with unknown columns.
pub(crate) fn sniff_data_file(path: &Path) -> Option<sniff::Layout> {
    let file = fs::File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(DATA_FILE_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    Some(sniff::sniff_head(&head))
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
pub(crate) fn render_findings<'a>(
    output: OutputFormat,
    min_severity: MinSeverity,
    path: &Path,
    source: &str,
    diagnostics: &[Diagnostic],
    resolve_note: &dyn Fn(&RelatedLocation) -> Option<RelatedNote<'a>>,
) -> usize {
    if diagnostics.is_empty() {
        return 0;
    }
    let mut line_index = None;
    let mut snippet_lines = None;
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
                let lines = snippet_lines.get_or_insert_with(|| LineStarts::new(source));
                render_human_diagnostic(path, source, lines, diagnostic, &related)
            }
            OutputFormat::Json => {
                let index = line_index.get_or_insert_with(|| LineIndex::new(source));
                render_json_diagnostic(path, source, index, diagnostic, &related)
            }
        }
    }
    rendered
}

/// A related range often sits in another document, so each note carries its own text.
pub(crate) struct RelatedNote<'a> {
    pub(crate) path: &'a Path,
    pub(crate) source: &'a str,
    pub(crate) range: TextRange,
    pub(crate) message: &'static str,
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
        // A pipe has no width, and a wrapped line breaks `grep` and every tool reading the output.
        false => handler.with_wrap_lines(false),
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
            warn_config(&config);
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
    use {
        console::Style,
        similar::{ChangeTag, TextDiff},
    };

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
