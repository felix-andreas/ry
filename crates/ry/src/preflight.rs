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
    repl::{Filtered, InputFilter},
    rustc_hash::FxHashMap,
    salsa::Setter,
    semantics::{
        DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{Diagnostic, RelatedLocation, Severity},
        stubs::install_shipped_stubs,
    },
    std::{
        fs,
        path::{Path, PathBuf},
        sync::{LazyLock, Mutex, PoisonError},
        thread,
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
        semantics::AnalysisSettings::new(&db, config.check.inline_annotations, config.check.strict);
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
/// strict mode is a property of the code. The check is `ry check`'s analysis of the project
/// asked about `R/` alone, so its verdict is the one `ry check` reaches for those files, and
/// the two share the check cache: a package that `ry check` or the editor last checked loads
/// without a second analysis. A reload in the same process keeps the analysis it builds in
/// [`PACKAGE_CHECKS`] and re-derives only what the edit invalidated. A package that turns
/// strict mode on nowhere, neither in `ry.toml` nor in a file's `# typing: strict`, skips
/// analysis entirely.
pub fn check_package(root: &Path, config: &Config) -> Result<Verdict, CommandError> {
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
    let first = !checks.contains_key(&key);
    let previous = checks.remove(&key).flatten();
    let check = thread::scope(|scope| {
        let worker = thread::Builder::new()
            .stack_size(crate::ANALYSIS_STACK_SIZE)
            .spawn_scoped(scope, || {
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
            });
        match worker {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(error) => panic!("the preflight thread should spawn: {error}"),
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
    let resolve_note = |related: &RelatedLocation| analysis.related_note(related);
    let mut problems = 0;
    for (file, errors) in analysis.files.iter().zip(errors) {
        problems += render_findings(
            OutputFormat::Human,
            MinSeverity::Warning,
            &file.path,
            analysis.shown_source(file),
            errors,
            &resolve_note,
        );
    }
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
    source: &str,
) -> (Verdict, Vec<Diagnostic>) {
    let strict = semantics::file_is_strict(db, file);
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
