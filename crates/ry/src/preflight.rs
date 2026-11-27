//! The check that runs before ry executes R code: `ry run` checks the script, and the console
//! checks each top-level command. It prints the inline-annotation warnings and enforces strict
//! mode; everything else stays `ry check`'s job, so plain R runs exactly as it always did.

use {
    crate::{
        check_cache::CacheUse,
        cli::{
            CommandError, MinSeverity, OutputFormat, PackageCheck, ProjectAnalysis, Purpose,
            RelatedNote, analyze_project, check_package_code, render_findings, report, warn_config,
        },
        config::Config,
        diagnostics::{apply_suppressions, document_diagnostics},
    },
    loader::SourceEncoding,
    repl::{Filtered, InputFilter},
    rustc_hash::{FxHashMap, FxHashSet},
    semantics::{
        Elaboration, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{Diagnostic, RelatedLocation, Severity},
        has_ry_syntax, is_ry_syntax, session,
        stubs::{install_shipped_stubs, is_native_function},
    },
    std::{
        borrow::Cow,
        fs, io,
        path::{Path, PathBuf},
        sync::{Arc, LazyLock, Mutex, PoisonError},
        thread,
    },
    syntax::{
        NodeOrToken, SyntaxKind, SyntaxNode, erase_inline_annotations, erased_lines_end,
        input_is_unfinished, is_inline_annotation,
    },
    xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed},
};

/// Analysis state for one run or one console session. Each accepted command is a file of its
/// own in the database's console session (see [`session`]), checked against the interface of
/// the commands before it, so a command costs time in proportion to its own length.
pub struct Preflight {
    db: RootDatabase,
    config: Config,
    /// Names the source of the next command in rendered findings.
    path: Arc<Path>,
    /// The path each session command was checked under, in session order, and whether it was
    /// checked at all: the plain input buffered before analysis and the part of an interrupted
    /// paste that ran join the session unchecked, so they may hold strict errors already.
    command_paths: Vec<(Arc<Path>, bool)>,
    /// Accepted plain R typed before the session held anything analysis decides,
    /// newline-terminated. It joins the session as one command once analysis starts, so a long
    /// plain paste never pays for analysis.
    plain: String,
    plain_path: Option<Arc<Path>>,
    /// The session holds something analysis decides: strict mode from `ry.toml`, ry syntax, or a
    /// `typing:` directive.
    analyzed: bool,
    /// Where the last accepted input went, so that [`Preflight::retract`] can take back the part
    /// of it that never ran.
    last: Option<Last>,
}

#[derive(Debug, Clone, Copy)]
enum Last {
    /// Appended to [`Preflight::plain`] at this offset.
    Plain { start: usize },
    /// The session's last command.
    Command,
}

/// What the preflight decided about a piece of input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Run,
    /// Strict mode found errors, which were printed.
    Refuse,
}

impl Preflight {
    /// Reads the configuration that governs `anchor` (a script, or the console's working
    /// directory) and prints its warnings. A malformed `ry.toml` is reported and refused,
    /// as `ry check` refuses it.
    pub fn discover(anchor: &Path, path: PathBuf) -> Option<Preflight> {
        let config = match Config::discover(anchor) {
            Ok(config) => config,
            Err(error) => {
                report(&error);
                return None;
            }
        };
        warn_config(&config);
        Some(Preflight::new(config, path))
    }

    pub fn new(config: Config, path: PathBuf) -> Preflight {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        semantics::AnalysisSettings::new(
            &db,
            config.check.inline_annotations,
            config.check.strict,
            config.check.prelude,
        );
        // The commands are not project files: a project-wide fold over them would make every
        // command cost time in proportion to the session.
        ProjectFiles::new(&db, Vec::new());
        Preflight {
            analyzed: config.check.strict,
            db,
            config,
            path: Arc::from(path),
            command_paths: Vec::new(),
            plain: String::new(),
            plain_path: None,
            last: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Names the source in the findings of later checks, as when a console continues the
    /// session a script began.
    pub fn rename(&mut self, path: PathBuf) {
        self.path = Arc::from(path);
    }

    /// Checks `input` as the next part of the session and returns the program ry's runtime runs
    /// for it, or `None` when strict mode refused it. That program is `input` elaborated with the
    /// checker's types (see [`elaborate`]). Accepted input joins the session; refused input does
    /// not, so the next command is checked as if it was never typed.
    pub fn check(&mut self, input: &str) -> Option<String> {
        let elaborations = self.check_elaborations(input)?;
        let program = elaborate(input.as_bytes(), &elaborations);
        // Only ASCII is inserted, between tokens or in place of whole ones, so the text stays UTF-8.
        Some(String::from_utf8_lossy(&program).into_owned())
    }

    /// Like [`Preflight::check`], but returns the rewrites in `input`, for a caller that
    /// rewrites bytes `input` only stands for, such as a Latin-1 script whose invalid bytes
    /// `input` spells as `?`.
    pub fn check_elaborations(&mut self, input: &str) -> Option<Vec<Elaboration>> {
        self.last = None;
        if !self.analyzed {
            if !has_ry_syntax(&syntax::parse(input).syntax_node(), input) {
                self.last = Some(Last::Plain {
                    start: self.plain.len(),
                });
                self.plain.push_str(input);
                self.plain.push('\n');
                self.plain_path.get_or_insert_with(|| self.path.clone());
                return Some(Vec::new());
            }
            self.analyzed = true;
        }
        self.flush_plain();
        let file = session::command_file(&self.db, format!("{input}\n"));
        let db = self.db.clone();
        let config = &self.config;
        let (contributions, (verdict, reported, elaborations)) = on_analysis_stack(move || {
            let contributions = session::prepare(&db, file);
            (contributions, findings(&db, file, config))
        });
        let mut reports = vec![(file, reported)];
        let mut refused = verdict == Verdict::Refuse;
        if !refused {
            let len = session::len(&self.db);
            let readers = session::commit(&mut self.db, file, contributions);
            self.command_paths.push((self.path.clone(), true));
            // A command that rebinds a name changes the functions earlier commands defined that
            // read it, which can break them in strict mode.
            let readers: Vec<SourceFile> = readers
                .into_iter()
                .filter(|&reader| {
                    (*reader.command(&self.db))
                        .and_then(|index| self.command_paths.get(index as usize))
                        .is_some_and(|&(_, checked)| checked)
                })
                .collect();
            if !readers.is_empty() {
                let db = self.db.clone();
                let config = &self.config;
                let broken: Vec<(SourceFile, Vec<Diagnostic>)> = on_analysis_stack(move || {
                    readers
                        .into_iter()
                        .filter(|&reader| semantics::file_is_strict(&db, reader))
                        .filter_map(|reader| {
                            let errors: Vec<Diagnostic> = apply_suppressions(
                                document_diagnostics(&db, reader, config),
                                reader.text(&db),
                            )
                            .into_iter()
                            .filter(is_strict_error)
                            .collect();
                            (!errors.is_empty()).then_some((reader, errors))
                        })
                        .collect()
                });
                if !broken.is_empty() {
                    refused = true;
                    reports.extend(broken);
                    self.truncate(len);
                }
            }
        }
        let mut errors = 0;
        let pending_path = self.path.clone();
        for (shown, diagnostics) in &reports {
            let shown_path = self.command_path(*shown).unwrap_or(&pending_path);
            let resolve_note = |related: &RelatedLocation| {
                let path = if related.file == file {
                    &pending_path
                } else {
                    self.command_path(related.file)?
                };
                Some(RelatedNote {
                    path,
                    source: related.file.text(&self.db),
                    range: related.range,
                    message: related.message,
                })
            };
            render_findings(
                OutputFormat::Human,
                MinSeverity::Warning,
                shown_path,
                shown.text(&self.db),
                diagnostics,
                &resolve_note,
            );
            errors += diagnostics
                .iter()
                .filter(|diagnostic| is_strict_error(diagnostic))
                .count();
        }
        if refused {
            refuse(errors);
            return None;
        }
        self.last = Some(Last::Command);
        Some(elaborations)
    }

    /// Takes back all of the last accepted input but its first `ran` bytes, which is all that
    /// ran of it: an interrupt dropped the rest of a paste.
    pub fn retract(&mut self, ran: usize) {
        match self.last.take() {
            None => {}
            Some(Last::Plain { start }) => self.plain.truncate(start + ran),
            Some(Last::Command) => {
                let len = session::len(&self.db);
                let Some(file) = session::command_files(&self.db).last().copied() else {
                    return;
                };
                let text = file.text(&self.db);
                let kept = text.get(..ran).unwrap_or(text).to_owned();
                let path = self
                    .command_paths
                    .last()
                    .map(|(path, _)| path.clone())
                    .unwrap_or_else(|| self.path.clone());
                self.truncate(len - 1);
                // What ran of the command defines what it defines, whether or not it checks.
                if !kept.trim().is_empty() {
                    self.push_unchecked(format!("{}\n", kept.trim_end_matches('\n')), path);
                }
            }
        }
    }

    /// Joins the plain input buffered before analysis started to the session, unchecked: it
    /// already ran.
    fn flush_plain(&mut self) {
        if self.plain.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.plain);
        let path = self.plain_path.take().unwrap_or_else(|| self.path.clone());
        self.push_unchecked(text, path);
    }

    fn push_unchecked(&mut self, text: String, path: Arc<Path>) {
        let file = session::command_file(&self.db, text);
        let db = self.db.clone();
        let contributions = on_analysis_stack(move || session::prepare(&db, file));
        // The readers a command that already ran breaks are not this command's to refuse.
        session::commit(&mut self.db, file, contributions);
        self.command_paths.push((path, false));
    }

    fn truncate(&mut self, len: usize) {
        session::truncate(&mut self.db, len);
        self.command_paths.truncate(len);
    }

    fn command_path(&self, file: SourceFile) -> Option<&Path> {
        let index = (*file.command(&self.db))?;
        self.command_paths
            .get(index as usize)
            .map(|(path, _)| &**path)
    }
}

/// Runs `work` on a thread with the stack analysis needs: inference recurses per nested
/// expression, which can outgrow the main thread's stack.
fn on_analysis_stack<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    thread::scope(|scope| {
        let worker = thread::Builder::new()
            .stack_size(crate::ANALYSIS_STACK_SIZE)
            .spawn_scoped(scope, work);
        match worker {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(error) => panic!("the preflight thread should spawn: {error}"),
        }
    })
}

/// Enforces strict mode on the package `ry load` is about to load, on either runtime, because
/// strict mode is a property of the code. The check is `ry check`'s analysis of the project
/// asked about `R/` alone, so its verdict is the one `ry check` reaches for those files, and
/// the two share the check cache: a package that `ry check` or the editor last checked loads
/// without a second analysis. A reload in the same process keeps the analysis it builds in
/// [`PACKAGE_CHECKS`] and re-derives only what the edit invalidated, and so does the first
/// check when the runtime `elaborates` the package, because [`elaborate_loaded_package`] then
/// reads the same analysis. A package that turns strict mode on nowhere, neither in `ry.toml`
/// nor in a file's `# typing: strict`, skips analysis entirely.
pub fn check_package(
    root: &Path,
    config: &Config,
    elaborates: bool,
) -> Result<Verdict, CommandError> {
    let sources = root.join("R");
    // The directive is ASCII, so the bytes answer for a file in any encoding.
    let mentions_strict = |path: &Path| {
        fs::read(path).is_ok_and(|source| {
            source
                .windows(b"typing: strict".len())
                .any(|window| window == b"typing: strict")
        })
    };
    if !config.check.strict
        && !fs::read_dir(&sources).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| mentions_strict(&entry.path()))
        })
    {
        return Ok(Verdict::Run);
    }
    let key = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut checks = PACKAGE_CHECKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // The first check in a process reads the shared record, which makes a cold load after
    // `ry check` or the editor cheap, but its database replays findings it never derived. A
    // reload builds a database kept for edits instead, so every reload after it is incremental.
    let first = !elaborates && !checks.contains_key(&key);
    let previous = checks.remove(&key).flatten();
    let check = on_analysis_stack(|| {
        if first {
            let analysis = analyze_project(
                root,
                root,
                config.clone(),
                std::slice::from_ref(&sources),
                CacheUse::ReadWrite,
                Purpose::Diagnostics,
            )?;
            let errors = strict_errors(&analysis);
            Ok::<_, CommandError>(Checked::Recorded(analysis, errors))
        } else {
            let check = check_package_code(previous, root, config)?;
            let errors = strict_errors(&check.analysis);
            Ok(Checked::Kept(check, errors))
        }
    })?;
    let (analysis, errors) = match &check {
        Checked::Recorded(analysis, errors) => (analysis, errors),
        Checked::Kept(check, errors) => (&check.analysis, errors),
    };
    // A file the analysis could not read went unchecked, and it has said which.
    if analysis.failures > 0 {
        return Err(CommandError);
    }
    let problems = render_strict_errors(analysis, errors);
    let kept = match check {
        Checked::Recorded(analysis, _) => {
            // Freeing every memo of a large package takes longer than any phase of its check,
            // and the loader is waiting for this verdict, so the memos are freed beside it.
            if let Err(error) = thread::Builder::new()
                .name("ry-strict-check-free".to_owned())
                .spawn(move || drop(analysis))
            {
                panic!("the thread that frees the strict check should spawn: {error}");
            }
            None
        }
        Checked::Kept(check, _) => Some(check),
    };
    checks.insert(key, kept);
    drop(checks);
    if problems == 0 {
        return Ok(Verdict::Run);
    }
    refuse(problems);
    Ok(Verdict::Refuse)
}

/// The package's R files that `ry install` and `ry build` put in the tarball rewritten, each
/// with its path under `root` and the bytes ry's runtime should run: the [`elaborate`]d source
/// of every code file R installs (the `.R`, `.r`, `.S` and `.q` files directly in `R/`) whose
/// checked types change how it runs, in the package's declared encoding. A package with no
/// typed code costs one read and one parse of each such file. Strict mode's errors are printed
/// and refuse the build, as they refuse `ry load`.
pub fn elaborate_package(
    root: &Path,
    config: &Config,
) -> Result<Vec<(PathBuf, Vec<u8>)>, CommandError> {
    let failed = |path: &Path, error: io::Error| {
        crate::cli::error(&format!("cannot read {}: {error}", path.display()));
        CommandError
    };
    let description_path = root.join("DESCRIPTION");
    let description =
        fs::read_to_string(&description_path).map_err(|error| failed(&description_path, error))?;
    let encoding = SourceEncoding::of(&description);
    let code_files =
        installed_code_files(&root.join("R")).map_err(|(path, error)| failed(&path, error))?;
    let mut typed = config.check.strict;
    for path in &code_files {
        if typed {
            break;
        }
        let bytes = fs::read(path).map_err(|error| failed(path, error))?;
        // A file that is not text in the package's encoding is the analysis's to report.
        typed = encoding.decode(bytes).is_none_or(|source| {
            source.contains("typing: strict")
                || has_ry_syntax(&syntax::parse(&source).syntax_node(), &source)
        });
    }
    if !typed {
        return Ok(Vec::new());
    }
    on_analysis_stack(|| {
        // The files are named one by one, because R installs exactly these, whatever the
        // project's ignore files say, and nothing in a directory below `R/`.
        let analysis = analyze_project(
            root,
            root,
            config.clone(),
            &code_files,
            CacheUse::Off,
            Purpose::Diagnostics,
        )?;
        if analysis.failures > 0 {
            return Err(CommandError);
        }
        let problems = render_strict_errors(&analysis, &strict_errors(&analysis));
        if problems > 0 {
            refuse(problems);
            return Err(CommandError);
        }
        let mut rewritten = Vec::new();
        for file in analysis
            .files
            .iter()
            .filter(|file| file.requested && file.document.is_none())
        {
            let elaborations = semantics::file_elaborations(&analysis.db, file.file);
            if elaborations.is_empty() {
                continue;
            }
            let program = elaborated_program(&file.path, analysis.source(file), &elaborations)?;
            let Some(program) = encoding.encode(&program) else {
                crate::cli::error(&format!(
                    "ry's rewrite of {} is not text in the package's encoding; this is a bug in ry",
                    file.path.display()
                ));
                return Err(CommandError);
            };
            rewritten.push((file.path.clone(), program));
        }
        Ok(rewritten)
    })
}

/// The text ry's runtime runs for each file of the package at `root` whose checked types change
/// how it runs, as `ry load` takes it: the [`elaborate`]d source, by path relative to `root`
/// with `/` between components, beside the [`xxh3_64`] of the source the checker read, so the
/// loader takes it only for that text. The analysis is the strict check's, kept in
/// [`PACKAGE_CHECKS`] for edits, so a reload re-derives only what the edit invalidated.
pub fn elaborate_loaded_package(
    root: &Path,
    config: &Config,
) -> Result<Vec<(String, u64, String)>, CommandError> {
    let key = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut checks = PACKAGE_CHECKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let previous = checks.remove(&key).flatten();
    let elaborated = on_analysis_stack(|| {
        let check = check_package_code(previous, root, config)?;
        let analysis = &check.analysis;
        if analysis.failures > 0 {
            return Err(CommandError);
        }
        let mut files = Vec::new();
        for file in analysis
            .files
            .iter()
            .filter(|file| file.requested && file.document.is_none())
        {
            let elaborations = semantics::file_elaborations(&analysis.db, file.file);
            if elaborations.is_empty() {
                continue;
            }
            let source = analysis.source(file);
            let program = elaborated_program(&file.path, source, &elaborations)?;
            let Some(path) = package_path(root, &key, &file.path) else {
                continue;
            };
            // The loader reads a file with its line endings made `\n`, so the hash and the
            // text it takes are of that form.
            let lines = |text: &str| text.replace("\r\n", "\n").replace('\r', "\n");
            files.push((path, xxh3_64(lines(source).as_bytes()), lines(&program)));
        }
        Ok((check, files))
    });
    match elaborated {
        Ok((check, files)) => {
            checks.insert(key, Some(check));
            Ok(files)
        }
        Err(error) => {
            checks.insert(key, None);
            Err(error)
        }
    }
}

/// The program ry's runtime runs for the file at `path` that `source()` reads, beside the
/// [`xxh3_64`] of the text the checker read, as R's parser reads it (line endings made `\n`, one
/// final `\n` dropped), or `None` when the checker rewrites nothing. The file is checked on its
/// own under the configuration that governs it, as `ry run` checks a script, and prints the
/// same annotation warnings, but strict mode refuses nothing here: a sourced file reads names
/// the code that sources it binds, which a check of the file alone reports as unresolved.
pub fn elaborate_sourced(path: &Path) -> Result<Option<(u64, String)>, CommandError> {
    // A sourced file that ry cannot check runs as R reads it: R reports a file it cannot read,
    // and a broken `ry.toml` beside it must not stop plain R code.
    let Ok(bytes) = fs::read(path) else {
        return Ok(None);
    };
    // A script that sources a helper in a loop would repeat the same findings each time, so
    // they print for the first `source()` of a file with this text.
    static REPORTED: LazyLock<Mutex<FxHashSet<u64>>> = LazyLock::new(Mutex::default);
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let first = REPORTED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(xxh3_64_with_seed(
            &bytes,
            xxh3_64(canonical.as_os_str().as_encoded_bytes()),
        ));
    let mut config = match Config::discover(path) {
        Ok(config) => config,
        Err(error) => {
            if first {
                report(&error);
            }
            return Ok(None);
        }
    };
    // R reads the file in the session's encoding, UTF-8, and drops a byte order mark; text
    // that is not UTF-8 is no file the parse reads as the checker would.
    let Ok(source) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    let source = source.strip_prefix('\u{FEFF}').unwrap_or(&source);
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let source = source.strip_suffix('\n').unwrap_or(&source);
    // The project's strict mode is the sourcing script's: a plain R file it sources keeps R's
    // types, and one that turns strict mode on itself (`# typing: strict`) is checked strictly.
    config.check.strict = false;
    let preflight = Preflight::new(config, path.to_path_buf());
    let file = session::command_file(&preflight.db, format!("{source}\n"));
    let db = preflight.db.clone();
    let config = &preflight.config;
    let (_, reported, elaborations) = on_analysis_stack(move || findings(&db, file, config));
    // Checked on its own, the file cannot see the names the sourcing script binds.
    let warnings: Vec<Diagnostic> = reported
        .into_iter()
        .filter(|diagnostic| diagnostic.code != "unresolved")
        .collect();
    let text = file.text(&preflight.db);
    let resolve_note = |related: &RelatedLocation| {
        (related.file == file).then_some(RelatedNote {
            path,
            source: text,
            range: related.range,
            message: related.message,
        })
    };
    if first {
        render_findings(
            OutputFormat::Human,
            MinSeverity::Warning,
            path,
            text,
            &warnings,
            &resolve_note,
        );
    }
    if elaborations.is_empty() {
        return Ok(None);
    }
    let program = elaborated_program(path, source, &elaborations)?;
    Ok(Some((xxh3_64(source.as_bytes()), program)))
}

/// [`elaborate`]s `source`, the text of the file at `path`. The rewrites are ASCII, between
/// tokens or in place of whole ones, so the program is UTF-8, keeps every line where it was,
/// and parses into as many functions with as many errors as the source. A rewrite that breaks
/// one of those is a bug in ry, and running its output would run code the checker never saw,
/// so it is reported and refused.
fn elaborated_program(
    path: &Path,
    source: &str,
    elaborations: &[Elaboration],
) -> Result<String, CommandError> {
    let program = elaborate(source.as_bytes(), elaborations);
    let shape = |text: &str| {
        let parsed = syntax::parse(text);
        let functions = parsed
            .syntax_node()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FUNCTION_DEF)
            .count();
        (text.lines().count(), parsed.errors().len(), functions)
    };
    String::from_utf8(program)
        .ok()
        .filter(|program| shape(program) == shape(source))
        .ok_or_else(|| {
            crate::cli::error(&format!(
                "ry's rewrite of {} for its runtime does not read as the file does; this is a \
                 bug in ry",
                path.display()
            ));
            CommandError
        })
}

/// `path` relative to the package root, which the analysis may name through `root` as given or
/// through its canonical form, with `/` between components as the loader names a file.
fn package_path(root: &Path, canonical: &Path, path: &Path) -> Option<String> {
    let relative = path
        .strip_prefix(root)
        .or_else(|_| path.strip_prefix(canonical))
        .ok()?;
    let parts: Option<Vec<&str>> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}

/// `ry build --plain-r`: rewrites every R file in `copy`, a package's build copy, into plain R
/// with each statement's inline annotations in `#:` comments, warning about each annotation no
/// comment can carry. ry code with no R spelling, such as a record, a tag or `List.map`,
/// refuses the build, naming the first one per file. Paths print relative to `root`.
pub fn write_plain_r(copy: &Path, root: &Path) -> Result<(), String> {
    let description_path = copy.join("DESCRIPTION");
    let description = fs::read_to_string(&description_path)
        .map_err(|error| format!("cannot read {}: {error}", description_path.display()))?;
    let encoding = SourceEncoding::of(&description);
    let mut pending = vec![copy.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "R" || extension == "r")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut refused = 0;
    for path in files {
        let shown = root.join(path.strip_prefix(copy).unwrap_or(&path));
        let bytes =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        // A file that is not text in the package's encoding is no R code to rewrite.
        let Some(source) = encoding.decode(bytes) else {
            continue;
        };
        let parsed = syntax::parse(&source);
        let commented = syntax::inline_annotations_to_comments(&parsed);
        let position = |text: &str, offset: syntax::TextSize| {
            let before = &text[..usize::from(offset)];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
            format!("{}:{line}:{column}", shown.display())
        };
        for range in &commented.dropped {
            crate::cli::warn(&format!(
                "{}: `{}` has no `#:` spelling there, so the plain R leaves it out",
                position(&source, range.start()),
                source[*range].trim()
            ));
        }
        if let Some(range) = first_unspellable(&syntax::parse(&commented.text).syntax_node()) {
            crate::cli::error(&format!(
                "{}: `{}` is ry code that plain R cannot spell; only annotations move into \
                 comments",
                position(&commented.text, range.start()),
                commented.text[range]
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
            ));
            refused += 1;
            continue;
        }
        if commented.text == source {
            continue;
        }
        let program = encoding.encode(&commented.text).ok_or_else(|| {
            format!(
                "{} cannot be written in the package's encoding",
                shown.display()
            )
        })?;
        fs::write(&path, program)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }
    match refused {
        0 => Ok(()),
        count => Err(format!(
            "plain R cannot spell the ry code in {count} file{} of {}",
            if count == 1 { "" } else { "s" },
            root.display()
        )),
    }
}

/// The first construct GNU R cannot run in a program with its annotations erased: a record,
/// tuple, list, tag or `match`, a suffixed number, or a native function.
fn first_unspellable(root: &SyntaxNode) -> Option<syntax::TextRange> {
    root.descendants_with_tokens()
        .find_map(|element| match element {
            NodeOrToken::Node(node) => is_ry_syntax(node.kind()).then(|| node.text_range()),
            NodeOrToken::Token(token) => (token.kind() == SyntaxKind::SUFFIXED_NUMBER
                || (token.kind() == SyntaxKind::IDENT && is_native_function(token.text())))
            .then(|| token.text_range()),
        })
}

/// The code files R installs from `sources`, a package's `R/` directory: the files directly in
/// it named `.R`, `.r`, `.S` or `.q`, in path order.
fn installed_code_files(sources: &Path) -> Result<Vec<PathBuf>, (PathBuf, io::Error)> {
    let entries = match fs::read_dir(sources) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err((sources.to_path_buf(), error)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| (sources.to_path_buf(), error))?
            .path();
        let code = path
            .extension()
            .is_some_and(|extension| ["R", "r", "S", "q"].iter().any(|code| extension == *code));
        if code && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Each package's analysis from its last strict check in this process, by canonical root:
/// `None` after a first check that read the shared record, and the database kept for edits
/// after any later one.
static PACKAGE_CHECKS: LazyLock<Mutex<FxHashMap<PathBuf, Option<PackageCheck>>>> =
    LazyLock::new(Mutex::default);

/// A strict check's analysis and each file's strict-mode errors.
enum Checked {
    Recorded(ProjectAnalysis, Vec<Vec<Diagnostic>>),
    Kept(PackageCheck, Vec<Vec<Diagnostic>>),
}

/// Each file's strict-mode errors, empty for a file outside `R/` or one that strict mode does
/// not cover.
/// Prints each file's strict errors and returns how many it printed.
fn render_strict_errors(analysis: &ProjectAnalysis, errors: &[Vec<Diagnostic>]) -> usize {
    let resolve_note = |related: &RelatedLocation| analysis.related_note(related);
    analysis
        .files
        .iter()
        .zip(errors)
        .map(|(file, errors)| {
            render_findings(
                OutputFormat::Human,
                MinSeverity::Warning,
                &file.path,
                analysis.shown_source(file),
                errors,
                &resolve_note,
            )
        })
        .sum()
}

fn strict_errors(analysis: &ProjectAnalysis) -> Vec<Vec<Diagnostic>> {
    analysis
        .files
        .iter()
        .map(|file| {
            // Findings replayed from the record leave the file unparsed, so whether it is
            // strict is asked only of a file that has an error.
            if !file.requested
                || !file.diagnostics.iter().any(is_strict_error)
                || !semantics::file_is_strict(&analysis.db, file.file)
            {
                return Vec::new();
            }
            file.diagnostics
                .iter()
                .filter(|diagnostic| is_strict_error(diagnostic))
                .cloned()
                .collect()
        })
        .collect()
}

fn refuse(problems: usize) {
    let noun = if problems == 1 { "error" } else { "errors" };
    crate::cli::error(&format!(
        "strict mode found {problems} {noun}, so this code does not run"
    ));
}

/// The findings worth printing before code runs, and whether it may run. Without strict mode
/// only the annotation warnings print: a type error is `ry check`'s to report, and R runs
/// the code regardless.
fn findings(
    db: &RootDatabase,
    file: SourceFile,
    config: &Config,
) -> (Verdict, Vec<Diagnostic>, Vec<Elaboration>) {
    let strict = semantics::file_is_strict(db, file);
    let source = file.text(db);
    let elaborations = semantics::file_elaborations(db, file);
    let diagnostics = apply_suppressions(document_diagnostics(db, file, config), source);
    let refuse = strict && diagnostics.iter().any(is_strict_error);
    let kept = diagnostics
        .into_iter()
        .filter(|diagnostic| {
            matches!(diagnostic.code, "inline-annotation" | "annotation-mismatch")
                || (refuse && is_strict_error(diagnostic))
        })
        .collect();
    (
        if refuse {
            Verdict::Refuse
        } else {
            Verdict::Run
        },
        kept,
        elaborations,
    )
}

/// `input` rewritten for ry's runtime with the types the checker gave it. `elaborations` are sorted
/// by start, outermost first, and either nest or are disjoint. Text after a rewrite on its line
/// moves with it.
///
/// - A literal typed `I64` is spelled as an `I64`: `5` and `5L` become `5i64`, and a whole `1e3` or
///   `2.0` becomes `1000i64` or `2i64`, since the suffix takes only an integer. The conversion goes
///   digit by digit, because an `f64` rounds past 2^53.
/// - A list the runtime may build empty passes through `.ry_items` with its item type, as in
///   `.ry_items(List.map(xs, f), "I64")`, and an empty dictionary with its key and value types.
/// - A signature type is spelled canonically: `x: Region` with `alias Region = :North | :South`
///   runs as `x: :North | :South`.
pub fn elaborate(input: &[u8], elaborations: &[Elaboration]) -> Vec<u8> {
    let mut program = Vec::with_capacity(input.len() + 16 * elaborations.len());
    let mut copied = 0;
    // The ends of the open `.ry_items` calls, innermost last, with the text that closes each.
    let mut open: Vec<(usize, Cow<'static, str>)> = Vec::new();
    let close_through = |program: &mut Vec<u8>,
                         copied: &mut usize,
                         open: &mut Vec<(usize, Cow<'static, str>)>,
                         offset: usize| {
        while let Some((end, items)) = open.pop_if(|(end, _)| *end <= offset) {
            program.extend_from_slice(&input[*copied..end]);
            program.extend_from_slice(b", \"");
            program.extend_from_slice(items.as_bytes());
            program.extend_from_slice(b"\")");
            *copied = end;
        }
    };
    for elaboration in elaborations {
        let range = elaboration.range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if start < copied || end > input.len() {
            continue;
        }
        close_through(&mut program, &mut copied, &mut open, start);
        match elaboration {
            Elaboration::I64Literal(_) => {
                let Some(text) = std::str::from_utf8(&input[start..end]).ok() else {
                    continue;
                };
                program.extend_from_slice(&input[copied..start]);
                let digits = text.strip_suffix('L').unwrap_or(text);
                match whole_decimal(digits) {
                    Some(whole) => program.extend_from_slice(whole.as_bytes()),
                    None => program.extend_from_slice(digits.as_bytes()),
                }
                program.extend_from_slice(b"i64");
                copied = end;
            }
            Elaboration::CheckedType(_, ty) => {
                program.extend_from_slice(&input[copied..start]);
                program.extend_from_slice(b".ry_items(");
                copied = start;
                open.push((end, ty.name()));
            }
            // A type printed with a non-ASCII field name would not stand for the bytes of a
            // script in another encoding, so it stays as written.
            Elaboration::Annotation(_, text) if text.is_ascii() => {
                program.extend_from_slice(&input[copied..start]);
                program.extend_from_slice(text.as_bytes());
                copied = end;
            }
            Elaboration::Annotation(..) => {}
        }
    }
    close_through(&mut program, &mut copied, &mut open, usize::MAX);
    program.extend_from_slice(&input[copied..]);
    program
}

/// The plain decimal digits of a whole number written with a fraction or an exponent (`2.0`,
/// `1e3`, `.5e1`), or `None` for a spelling R reads the same way with an `i64` suffix (plain
/// digits, hexadecimal) or one that is not whole.
fn whole_decimal(text: &str) -> Option<String> {
    if text.starts_with("0x") || text.starts_with("0X") {
        return None;
    }
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if fraction.is_empty() && exponent == 0 {
        return None;
    }
    let mut digits: String = whole.chars().chain(fraction.chars()).collect();
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let shift = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    if shift >= 0 {
        // Past 19 digits the literal cannot fit an `I64` anyway; the runtime reports it.
        let zeros = usize::try_from(shift).ok()?.min(40);
        digits.extend(std::iter::repeat_n('0', zeros));
    } else {
        let dropped = usize::try_from(-shift).ok()?;
        let kept = digits.len().checked_sub(dropped)?;
        if !digits[kept..].bytes().all(|byte| byte == b'0') {
            return None;
        }
        digits.truncate(kept);
    }
    let trimmed = digits.trim_start_matches('0');
    Some(if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    })
}

/// A syntax error is left to the runtime, which reports it in R's own words; in the console
/// it is usually a command the user has not finished typing.
fn is_strict_error(diagnostic: &Diagnostic) -> bool {
    diagnostic.severity == Severity::Error && diagnostic.code != "syntax-error"
}

fn has_inline_annotation(root: &SyntaxNode) -> bool {
    root.descendants().any(|node| is_inline_annotation(&node))
}

/// GNU R reads the plain program an annotated one means.
pub struct Erase;

impl InputFilter for Erase {
    fn filter(&mut self, input: &str) -> Filtered {
        Filtered::Run(erase(input))
    }
}

/// The console's filter. The console hands it one line at a time, so it holds lines until they
/// form a whole command, checks the command, then releases it either as typed (ry's runtime
/// reads annotations) or erased (GNU R cannot).
pub struct ConsoleFilter {
    preflight: Preflight,
    erase: bool,
    /// The lines of an unfinished command, each newline-terminated.
    held: String,
    /// The last command released to R, as checked, and the number of lines R was handed for it.
    released: Option<(String, usize)>,
}

impl ConsoleFilter {
    pub fn new(preflight: Preflight, erase: bool) -> ConsoleFilter {
        ConsoleFilter {
            preflight,
            erase,
            held: String::new(),
            released: None,
        }
    }
}

impl InputFilter for ConsoleFilter {
    fn filter(&mut self, input: &str) -> Filtered {
        self.held.push_str(input);
        self.held.push('\n');
        if input_is_unfinished(&self.held) {
            return Filtered::Hold;
        }
        let mut command = std::mem::take(&mut self.held);
        command.pop();
        self.released = None;
        let program = match self.preflight.check(&command) {
            None => return Filtered::Refuse,
            Some(_) if self.erase => erase(&command),
            Some(program) => program,
        };
        self.released = Some((command, program.matches('\n').count() + 1));
        Filtered::Run(program)
    }

    fn interrupt(&mut self) {
        self.held.clear();
    }

    fn discard(&mut self, lines: usize) {
        let Some((command, total)) = self.released.take() else {
            return;
        };
        let ran = total.saturating_sub(lines);
        // Elaboration only inserts text within a line, so the program's lines are the
        // command's; erasing can drop or merge lines, which the erased program maps back.
        let end = if self.erase {
            usize::from(erased_lines_end(&syntax::parse(&command), ran))
        } else if ran == 0 {
            0
        } else {
            command
                .match_indices('\n')
                .nth(ran - 1)
                .map_or(command.len(), |(at, _)| at + 1)
        };
        self.preflight.retract(end);
    }
}

pub fn erase(input: &str) -> String {
    let parse = syntax::parse(input);
    let root = parse.syntax_node();
    if has_inline_annotation(&root) {
        erase_inline_annotations(&parse)
    } else {
        input.to_owned()
    }
}
