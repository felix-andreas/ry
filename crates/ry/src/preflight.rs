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
    semantics::{
        Elaboration, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{Diagnostic, RelatedLocation, Severity},
        is_ry_syntax, session,
        stubs::{install_shipped_stubs, is_native_function},
    },
    std::{
        fs,
        path::{Path, PathBuf},
        sync::{Arc, LazyLock, Mutex, PoisonError},
        thread,
    },
    syntax::{
        NodeOrToken, SyntaxKind, SyntaxNode, erase_inline_annotations, erased_lines_end,
        input_is_unfinished, is_inline_annotation,
    },
};

/// Analysis state for one run or one console session. Each accepted command is a file of its
/// own in the database's console session (see [`session`]), checked against the interface of
/// the commands before it, so a command costs time in proportion to its own length.
pub struct Preflight {
    db: RootDatabase,
    config: Config,
    /// Names the source of the next command in rendered findings.
    path: Arc<Path>,
    /// The path each session command was checked under, in session order.
    command_paths: Vec<Arc<Path>>,
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
        // Only ASCII is inserted, and only between tokens, so the text stays UTF-8.
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
            self.command_paths.push(self.path.clone());
            // A command that rebinds a name changes the functions earlier commands defined that
            // read it, which can break them in strict mode.
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
                    .cloned()
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
        self.command_paths.push(path);
    }

    fn truncate(&mut self, len: usize) {
        session::truncate(&mut self.db, len);
        self.command_paths.truncate(len);
    }

    fn command_path(&self, file: SourceFile) -> Option<&Path> {
        let index = (*file.command(&self.db))?;
        self.command_paths.get(index as usize).map(|path| &**path)
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

/// `input` rewritten for ry's runtime with the types the checker gave it. `elaborations` are
/// sorted by start, outermost first, and either nest or are disjoint.
///
/// - A literal the checker typed `I64` is spelled as that `I64`: `5` and `5L` become `5i64`, and a
///   whole `1e3` or `2.0` becomes `1000i64` or `2i64`, because the `i64` suffix takes only an
///   integer. The conversion is exact, digit by digit, because an `f64` rounds past 2^53.
/// - A list the runtime may build empty is passed through `.ry_items` with its item type, so
///   `List.map(xs, f)` runs as `.ry_items(List.map(xs, f), "I64")`.
///
/// Text after a rewrite on its line moves right.
pub fn elaborate(input: &[u8], elaborations: &[Elaboration]) -> Vec<u8> {
    let mut program = Vec::with_capacity(input.len() + 16 * elaborations.len());
    let mut copied = 0;
    // The ends of the open `.ry_items` calls, innermost last, with the text that closes each.
    let mut open: Vec<(usize, &'static str)> = Vec::new();
    let close_through = |program: &mut Vec<u8>,
                         copied: &mut usize,
                         open: &mut Vec<(usize, &'static str)>,
                         offset: usize| {
        while let Some(&(end, items)) = open.last().filter(|(end, _)| *end <= offset) {
            program.extend_from_slice(&input[*copied..end]);
            program.extend_from_slice(b", \"");
            program.extend_from_slice(items.as_bytes());
            program.extend_from_slice(b"\")");
            *copied = end;
            open.pop();
        }
    };
    for elaboration in elaborations {
        let range = elaboration.range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if start < copied || end > input.len() {
            continue;
        }
        close_through(&mut program, &mut copied, &mut open, start);
        match *elaboration {
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
            Elaboration::TypedList(_, items) => {
                program.extend_from_slice(&input[copied..start]);
                program.extend_from_slice(b".ry_items(");
                copied = start;
                open.push((end, items.name()));
            }
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

/// Whether `source` holds anything analysis decides: an inline or comment annotation, a
/// `# typing:` directive, a record, tuple, list, tag or `match`, a suffixed number, or a native
/// function call such as `List.range(1, 10)`. Missing one of these leaves an `I64` literal an R double at run time, so
/// the test errs toward analysis.
fn has_ry_syntax(root: &SyntaxNode, source: &str) -> bool {
    source.contains("#:")
        || has_inline_annotation(root)
        || root.descendants_with_tokens().any(|element| match element {
            NodeOrToken::Node(node) => is_ry_syntax(node.kind()),
            NodeOrToken::Token(token) => match token.kind() {
                SyntaxKind::SUFFIXED_NUMBER => true,
                SyntaxKind::COMMENT => token
                    .text()
                    .trim_start_matches('#')
                    .trim_start()
                    .starts_with("typing:"),
                SyntaxKind::IDENT => is_native_function(token.text()),
                _ => false,
            },
        })
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
        if input_is_unfinished(&self.held) {
            self.held.push('\n');
            return Filtered::Hold;
        }
        let command = std::mem::take(&mut self.held);
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
