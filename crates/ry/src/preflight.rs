//! The check that runs before ry executes R code: `ry run` checks the script, and the console
//! checks each top-level command. It prints the inline-annotation warnings and enforces strict
//! mode; everything else stays `ry check`'s job, so plain R runs exactly as it always did.

use {
    crate::{
        cli::{
            self, CommandError, MinSeverity, OutputFormat, ProjectAnalysis, RelatedNote,
            analyze_project, collect_r_files, exclude_matcher, project_stub_paths, render_findings,
            report, scan_data_files, warn_config,
        },
        config::Config,
        diagnostics::{apply_suppressions, document_diagnostics, file_is_strict},
    },
    repl::{Filtered, InputFilter},
    salsa::Setter,
    semantics::{
        DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{Diagnostic, RelatedLocation, Severity},
        stubs::install_shipped_stubs,
    },
    std::{
        env, fs,
        hash::{DefaultHasher, Hash, Hasher},
        io,
        path::{Path, PathBuf},
        thread,
        time::UNIX_EPOCH,
    },
    syntax::{SyntaxNode, erase_inline_annotations, input_is_unfinished, is_inline_annotation},
};

/// Analysis state for one run or one console session. A console session is checked as one
/// growing script, so a command sees the definitions earlier commands made.
pub struct Preflight {
    db: RootDatabase,
    file: SourceFile,
    config: Config,
    /// Names the source in rendered findings.
    path: PathBuf,
    /// Accepted input so far, newline-terminated.
    session: String,
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
        semantics::AnalysisSettings::new(&db, config.check.inline_annotations);
        let file = SourceFile::new(&db, String::new(), DocumentKind::Script);
        ProjectFiles::new(&db, vec![file]);
        Preflight {
            db,
            file,
            config,
            path,
            session: String::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Names the source in the findings of later checks, as when a console continues the
    /// session a script began.
    pub fn rename(&mut self, path: PathBuf) {
        self.path = path;
    }

    /// Checks `input` as the next part of the session. Accepted input joins the session;
    /// refused input does not, so the next command is checked as if it was never typed.
    pub fn check(&mut self, input: &str) -> Verdict {
        let start = self.session.len();
        let mut source = String::with_capacity(start + input.len() + 1);
        source.push_str(&self.session);
        source.push_str(input);
        source.push('\n');
        self.file.set_text(&mut self.db).to(source.clone());
        // Inference recurses per nested expression, which can outgrow the main thread's stack.
        let db = self.db.clone();
        let (file, config, text) = (self.file, &self.config, source.as_str());
        let (verdict, findings) = thread::scope(|scope| {
            let worker = thread::Builder::new()
                .stack_size(crate::ANALYSIS_STACK_SIZE)
                .spawn_scoped(scope, move || findings(&db, file, config, text));
            match worker {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                Err(error) => panic!("the preflight thread should spawn: {error}"),
            }
        });
        // Earlier commands' warnings were printed when they ran, but an error that blocks this
        // one is printed wherever it is: redefining a name can break a function defined earlier.
        let reported: Vec<Diagnostic> = findings
            .into_iter()
            .filter(|diagnostic| {
                usize::from(diagnostic.range.start()) >= start
                    || (verdict == Verdict::Refuse && is_strict_error(diagnostic))
            })
            .collect();
        let resolve_note = |related: &RelatedLocation| {
            (related.file == file).then(|| RelatedNote {
                path: &self.path,
                source: &source,
                range: related.range,
                message: related.message,
            })
        };
        render_findings(
            OutputFormat::Human,
            MinSeverity::Warning,
            &self.path,
            &source,
            &reported,
            &resolve_note,
        );
        match verdict {
            Verdict::Run => self.session = source,
            Verdict::Refuse => refuse(
                reported
                    .iter()
                    .filter(|diagnostic| is_strict_error(diagnostic))
                    .count(),
            ),
        }
        verdict
    }
}

/// Enforces strict mode on the package `ry load` is about to load, on either runtime, because
/// strict mode is a property of the code. Its `R/` files are checked as `ry check` checks the
/// project, so a name another file defines resolves. A package that turns strict mode on
/// nowhere, neither in `ry.toml` nor in a file's `# typing: strict`, skips analysis entirely.
pub fn check_package(root: &Path, config: &Config) -> Result<Verdict, CommandError> {
    let sources = root.join("R");
    let mentions_strict = |path: &Path| {
        fs::read_to_string(path).is_ok_and(|source| source.contains("typing: strict"))
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
    // A package that passed unchanged loads again without a second analysis, because the
    // check is a function of these inputs and the ry binary alone.
    let record = strict_fingerprint(root, config)?.zip(verdict_record(root));
    if let Some((fingerprint, record)) = &record
        && fs::read_to_string(record).is_ok_and(|stored| stored == *fingerprint)
    {
        return Ok(Verdict::Run);
    }
    let analysis = thread::scope(|scope| {
        let worker = thread::Builder::new()
            .stack_size(crate::ANALYSIS_STACK_SIZE)
            .spawn_scoped(scope, || {
                let analysis =
                    analyze_project(root, config.clone(), std::slice::from_ref(&sources))?;
                let errors = strict_errors(&analysis, config);
                Ok::<_, CommandError>((analysis, errors))
            });
        match worker {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(error) => panic!("the preflight thread should spawn: {error}"),
        }
    });
    let (analysis, errors) = analysis?;
    let resolve_note = |related: &RelatedLocation| analysis.related_note(related);
    let mut problems = 0;
    for (file, errors) in analysis.files.iter().zip(&errors) {
        problems += render_findings(
            OutputFormat::Human,
            MinSeverity::Warning,
            &file.path,
            analysis.source(file),
            errors,
            &resolve_note,
        );
    }
    if problems == 0 {
        // A file saved while the analysis ran would leave the earlier contents marked clean
        // although the analysis judged the later ones, so the verdict is kept only when the
        // inputs still hash the same afterwards.
        if let Some((fingerprint, record)) = record
            && strict_fingerprint(root, config)?.as_ref() == Some(&fingerprint)
        {
            remember_clean(&record, &fingerprint);
        }
        return Ok(Verdict::Run);
    }
    refuse(problems);
    Ok(Verdict::Refuse)
}

/// Hashes everything the strict check reads: the ry binary, the configuration, every R file
/// the project analysis walks, the override stubs, the package metadata and the data files.
/// Two independently seeded hashes give 128 bits, so a stale verdict needs a real collision.
fn strict_fingerprint(root: &Path, config: &Config) -> Result<Option<String>, CommandError> {
    let mut low = DefaultHasher::new();
    let mut high = DefaultHasher::new();
    "ry strict verdict, low".hash(&mut low);
    "ry strict verdict, high".hash(&mut high);
    let mut feed = |bytes: &[u8]| {
        bytes.len().hash(&mut low);
        low.write(bytes);
        bytes.len().hash(&mut high);
        high.write(bytes);
    };
    // Without a binary identity a verdict could outlive the ry that reached it, so none is kept.
    let Ok(binary) = env::current_exe() else {
        return Ok(None);
    };
    let Ok(metadata) = fs::metadata(&binary) else {
        return Ok(None);
    };
    feed(binary.as_os_str().as_encoded_bytes());
    feed(&metadata.len().to_le_bytes());
    let Some(modified) = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
    else {
        return Ok(None);
    };
    feed(&modified.as_nanos().to_le_bytes());
    feed(env!("CARGO_PKG_VERSION").as_bytes());
    feed(format!("{config:?}").as_bytes());
    let exclude = exclude_matcher(config, root)?;
    // An input that cannot be read cannot be fingerprinted, so that load keeps no verdict; the
    // analysis itself reports the unreadable file.
    for path in collect_r_files(root, exclude.as_ref())? {
        let Ok(contents) = fs::read(&path) else {
            return Ok(None);
        };
        feed(path.as_os_str().as_encoded_bytes());
        feed(&contents);
    }
    for (path, stem) in project_stub_paths(root) {
        let Ok(contents) = fs::read(&path) else {
            return Ok(None);
        };
        feed(stem.as_bytes());
        feed(&contents);
    }
    for name in ["DESCRIPTION", "NAMESPACE"] {
        match fs::read(root.join(name)) {
            Ok(contents) => feed(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => feed(b"absent"),
            Err(_) => return Ok(None),
        }
    }
    feed(format!("{:?}", scan_data_files(root)).as_bytes());
    Ok(Some(format!("{:016x}{:016x}", low.finish(), high.finish())))
}

/// Where the last clean verdict for the package at `root` is kept: one small file per package
/// in the user's cache directory, so the cache never grows with the number of edits.
fn verdict_record(root: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let directory = dirs::cache_dir()?.join("ry").join("strict");
    Some(directory.join(format!("{:016x}", hasher.finish())))
}

/// A verdict that cannot be stored only costs the next load a fresh check, so failing to store
/// it is reported and otherwise ignored.
fn remember_clean(record: &Path, fingerprint: &str) {
    let stored = record
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(record, fingerprint));
    if let Err(error) = stored {
        cli::warn(&format!(
            "cannot remember the strict check of this package in {}: {error}",
            record.display()
        ));
    }
}

/// Each file's strict-mode errors, empty for a file outside `R/` or one that strict mode does
/// not cover.
fn strict_errors(analysis: &ProjectAnalysis, config: &Config) -> Vec<Vec<Diagnostic>> {
    analysis
        .files
        .iter()
        .map(|file| {
            if !file.requested || !file_is_strict(&analysis.db, file.file, config) {
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
    source: &str,
) -> (Verdict, Vec<Diagnostic>) {
    let strict = file_is_strict(db, file, config);
    // Plain R without strict mode has nothing to report, so it skips analysis entirely.
    if !strict && !has_inline_annotation(&semantics::parse(db, file).syntax_node()) {
        return (Verdict::Run, Vec::new());
    }
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
    )
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
}

impl ConsoleFilter {
    pub fn new(preflight: Preflight, erase: bool) -> ConsoleFilter {
        ConsoleFilter {
            preflight,
            erase,
            held: String::new(),
        }
    }
}

impl InputFilter for ConsoleFilter {
    fn filter(&mut self, input: &str) -> Filtered {
        self.held.push_str(input);
        if input_is_unfinished(&self.held) {
            self.held.push('\n');
            return Filtered::Hold;
        }
        let command = std::mem::take(&mut self.held);
        match self.preflight.check(&command) {
            Verdict::Refuse => Filtered::Refuse,
            Verdict::Run if self.erase => Filtered::Run(erase(&command)),
            Verdict::Run => Filtered::Run(command),
        }
    }

    fn interrupt(&mut self) {
        self.held.clear();
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
