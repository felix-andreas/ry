//! The check that runs before ry executes R code: `ry run` checks the script, and the console
//! checks each top-level command. It prints the inline-annotation warnings and enforces strict
//! mode; everything else stays `ry check`'s job, so plain R runs exactly as it always did.

use crate::cli::{MinSeverity, OutputFormat, RelatedNote, render_findings, report, warn_config};
use crate::config::Config;
use crate::diagnostics::{apply_suppressions, document_diagnostics};
use repl::{Filtered, InputFilter};
use salsa::Setter;
use semantics::diagnostics::{Diagnostic, RelatedLocation, Severity};
use semantics::stubs::install_shipped_stubs;
use semantics::{
    DocumentKind, ProjectFiles, RootDatabase, SourceFile, TypingMode, file_typing_mode,
};
use std::path::{Path, PathBuf};
use std::thread;
use syntax::{SyntaxNode, erase_inline_annotations, input_is_unfinished, is_inline_annotation};

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
            Verdict::Refuse => {
                let problems = reported
                    .iter()
                    .filter(|diagnostic| is_strict_error(diagnostic))
                    .count();
                let noun = if problems == 1 { "error" } else { "errors" };
                crate::cli::error(&format!(
                    "strict mode found {problems} {noun}, so this code does not run"
                ));
            }
        }
        verdict
    }
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
    let strict = match file_typing_mode(db, file) {
        Some(TypingMode::Strict) => true,
        Some(TypingMode::On | TypingMode::Off) => false,
        None => config.check.strict,
    };
    // Plain R without strict mode has nothing to report, so it skips analysis entirely.
    if !strict && !has_inline_annotation(&semantics::parse(db, file).syntax_node()) {
        return (Verdict::Run, Vec::new());
    }
    let diagnostics = apply_suppressions(document_diagnostics(db, file, config), source);
    let refuse = strict
        && diagnostics
            .iter()
            .any(|diagnostic| is_strict_error(diagnostic));
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
