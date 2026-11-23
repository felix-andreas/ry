//! Shared test support, including the fuzz invariant battery: no panics, deterministic output
//! across fresh databases, in-bounds ranges, and incremental equivalence with a fresh build.

use {
    crate::{
        AnalysisSettings, DataFiles, Db, DocumentKind, ItemKind, ProjectFiles, RootDatabase,
        SourceFile, TreeStore, binds_only_own_name,
        diagnostics::{TypeRenderer, file_diagnostics, strict_diagnostics},
        item_check, item_naming, item_syntax, item_tree,
        lints::{LintConfig, LintLevel, NameStyle, lint_file},
        naming::BindingKind,
        stubs::install_shipped_stubs,
        test_document_kind,
    },
    salsa::{Setter as _, plumbing::AsId as _},
    std::{
        cell::RefCell,
        collections::BTreeMap,
        mem,
        panic::{self, AssertUnwindSafe},
        sync::{Arc, Mutex},
    },
    syntax::testing::{
        Entropy, FAILURES_PER_RUN, SplitMix64, fuzz_seeds, generate_program, generate_project,
        replay_command, small_edit, split_files,
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
        let Some(scheme) = &check.scheme else {
            continue;
        };
        let mut renderer = TypeRenderer::default();
        output.push_str(&format!("{name}: {}\n", renderer.render_scheme(db, scheme)));
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

/// Panics unless a finding's message or related label follows the wording convention: it ends
/// without a full stop, and it starts lowercase unless its first word is `I`, `R`, code, an
/// acronym or a proper noun (`.agents/memory/decisions/syntax-semantics.md`, "Diagnostic
/// messages are lowercase and end without a full stop").
pub fn assert_message_style(message: &str) {
    assert!(
        !message.ends_with('.') || message.ends_with("..."),
        "a diagnostic message ends without a full stop: {message:?}"
    );
    let first_word: String = message
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    let lowercase_start = first_word
        .chars()
        .next()
        .is_none_or(|character| !character.is_uppercase());
    let exempt = first_word == "I"
        || first_word == "R"
        || first_word
            .chars()
            .skip(1)
            .any(|character| character.is_uppercase() || character.is_numeric())
        || PROPER_NOUN_STARTS
            .iter()
            .any(|noun| message.starts_with(noun));
    assert!(
        lowercase_start || exempt,
        "a diagnostic message starts lowercase unless it opens with `I`, `R`, code or a proper noun: {message:?}"
    );
}

const PROPER_NOUN_STARTS: [&str; 1] = ["Reference Classes"];

/// One state of a project in a fuzz session.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectState {
    /// `(key, text, kind)` in project order, which decides last-writer-wins. A key names one
    /// file across the states of a session, so a state edits, adds, drops, reorders or
    /// reclassifies files by what it lists.
    pub files: Vec<(usize, String, DocumentKind)>,
    pub strict: bool,
    pub prelude: bool,
}

/// Two databases that outlive a fuzz case, so a case pays for its analysis and not for
/// re-parsing the stub corpus, which costs thirty times as much.
///
/// The incremental database is an editor's: it keeps one input per file key and moves from
/// state to state through the setters, across cases too, so every state is reached by an edit
/// from an unrelated history. The reference database is a command's (`ry check`): bounded tree
/// store, no editor facts, and new inputs for every state, so nothing it memoized about a file
/// survives into the next. The two must render every file alike.
pub struct Workbench {
    incremental: RootDatabase,
    inputs: Vec<(usize, SourceFile)>,
    reference: RootDatabase,
    sessions: usize,
}

impl Default for Workbench {
    fn default() -> Workbench {
        let incremental = RootDatabase::default();
        let reference = RootDatabase::for_command(false);
        for db in [&incremental, &reference] {
            install_shipped_stubs(db);
            AnalysisSettings::new(db, true, false, true);
            ProjectFiles::new(db, Vec::new());
        }
        Workbench {
            incremental,
            inputs: Vec::new(),
            reference,
            sessions: 0,
        }
    }
}

/// Runs one session per seed of [`fuzz_seeds`] on a warm [`Workbench`]. A failure is retried on
/// fresh databases to tell a failure of the session alone, which its seed replays, from one that
/// needs the sessions before it, which the window from the workbench's first seed replays. The
/// workbench is rebuilt every [`WORKBENCH_SESSIONS`] seeds so that window stays short, and after
/// a failure, since the run goes on to report up to [`FAILURES_PER_RUN`] failing seeds.
pub fn for_each_session(default: usize, mut session: impl FnMut(u64) -> Vec<ProjectState>) {
    let mut bench: Option<(u64, Workbench)> = None;
    let mut replays: Vec<String> = Vec::new();
    for seed in fuzz_seeds(default) {
        if seed % WORKBENCH_SESSIONS == 0 {
            bench = None;
        }
        let (first_seed, workbench) = bench.get_or_insert_with(|| (seed, Workbench::default()));
        let states = session(seed);
        if panic::catch_unwind(AssertUnwindSafe(|| check_session(workbench, &states))).is_ok() {
            continue;
        }
        let alone = panic::catch_unwind(AssertUnwindSafe(|| {
            check_session(&mut Workbench::default(), &states)
        }));
        let history = match alone {
            Err(_) => "",
            Ok(()) => ", which fails only after the sessions before it",
        };
        eprintln!(
            "---- the failing session (seed {seed}{history}) ----\n{}",
            describe_session(&states)
        );
        replays.push(match alone {
            Err(_) => format!("    {}", replay_command(seed)),
            Ok(()) => format!(
                "    FUZZ_ITERS={} {}",
                seed - *first_seed + 1,
                replay_command(*first_seed)
            ),
        });
        bench = None;
        if replays.len() == FAILURES_PER_RUN {
            break;
        }
    }
    assert!(
        replays.is_empty(),
        "{} session(s) failed; replay with\n{}",
        replays.len(),
        replays.join("\n")
    );
}

const WORKBENCH_SESSIONS: u64 = 256;

/// Walks the states and then returns to the first. At every step the incremental database must
/// render each file as the reference does, and the return must restore the first rendering.
/// Every sixty-fourth session also compares its first state with a database that has no history
/// at all, which is the determinism check across interning orders.
pub fn check_session(workbench: &mut Workbench, states: &[ProjectState]) {
    let mut first: Option<Vec<String>> = None;
    // A session of one state has nothing to edit back from.
    let back = states.first().filter(|_| states.len() > 1);
    for (step, state) in states.iter().chain(back).enumerate() {
        let incremental = render_incrementally(workbench, state);
        let reference = render_afresh(&mut workbench.reference, state);
        assert!(
            incremental == reference,
            "step {step}: the incremental output diverges from a fresh build\n{}\n---- incremental:\n{}\n---- fresh:\n{}",
            describe_session(states),
            incremental.join("----\n"),
            reference.join("----\n"),
        );
        match &first {
            None => {
                if workbench.sessions % 64 == 0 {
                    let mut pristine = RootDatabase::default();
                    install_shipped_stubs(&pristine);
                    AnalysisSettings::new(&pristine, true, false, true);
                    ProjectFiles::new(&pristine, Vec::new());
                    assert!(
                        render_afresh(&mut pristine, state) == reference,
                        "the output depends on what the database analyzed before\n{}",
                        describe_session(states)
                    );
                }
                first = Some(incremental);
            }
            Some(first) if step == states.len() => assert!(
                incremental == *first,
                "the output is not restored after editing back\n{}",
                describe_session(states)
            ),
            Some(_) => {}
        }
    }
    workbench.sessions += 1;
}

/// Moves the incremental database to `state` as a host would, setting only what differs.
fn render_incrementally(workbench: &mut Workbench, state: &ProjectState) -> Vec<String> {
    let db = &mut workbench.incremental;
    apply_settings(db, state);
    let mut files = Vec::with_capacity(state.files.len());
    for (key, text, kind) in &state.files {
        let known = workbench
            .inputs
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, file)| *file);
        let file = match known {
            Some(file) => {
                if file.text(db) != text {
                    file.set_text(db).to(text.clone());
                }
                if *file.kind(db) != *kind {
                    file.set_kind(db).to(*kind);
                }
                file
            }
            None => {
                let file = SourceFile::new(db, text.clone(), *kind);
                workbench.inputs.push((*key, file));
                file
            }
        };
        files.push(file);
    }
    let project = ProjectFiles::get(db);
    if *project.files(db) != files {
        project.set_files(db).to(files.clone());
    }
    files
        .iter()
        .map(|file| render_semantics(db, *file))
        .collect()
}

/// Renders `state` from inputs `db` has never seen.
fn render_afresh(db: &mut RootDatabase, state: &ProjectState) -> Vec<String> {
    apply_settings(db, state);
    let files: Vec<SourceFile> = state
        .files
        .iter()
        .map(|(_, text, kind)| SourceFile::new(db, text.clone(), *kind))
        .collect();
    ProjectFiles::get(db).set_files(db).to(files.clone());
    files
        .iter()
        .zip(&state.files)
        .map(|(file, (_, text, _))| {
            check_write_screen(db, *file, text);
            render_semantics(db, *file)
        })
        .collect()
}

fn apply_settings(db: &mut RootDatabase, state: &ProjectState) {
    let settings = AnalysisSettings::get(db);
    if *settings.strict(db) != state.strict {
        settings.set_strict(db).to(state.strict);
    }
    if *settings.prelude(db) != state.prelude {
        settings.set_prelude(db).to(state.prelude);
    }
}

fn describe_session(states: &[ProjectState]) -> String {
    let mut text = String::new();
    for (step, state) in states.iter().enumerate() {
        text.push_str(&format!(
            "==== state {step} (strict {}, prelude {})\n",
            state.strict, state.prelude
        ));
        for (key, source, kind) in &state.files {
            text.push_str(&format!("---- file {key} ({kind:?})\n{source}\n"));
        }
    }
    text
}

/// A session a seed draws: a project of one to four files, each a generated program or one of
/// `pool`, then up to three further states. Most steps are one [`small_edit`] of one file, as
/// an editor makes them; the rest replace a file's text, add or drop a file, reorder the
/// project, reclassify a file, or flip a setting.
pub fn generate_session(rng: &mut dyn Entropy, pool: &[String]) -> Vec<ProjectState> {
    const KINDS: [DocumentKind; 8] = [
        DocumentKind::Package,
        DocumentKind::Package,
        DocumentKind::Package,
        DocumentKind::Package,
        DocumentKind::Package,
        DocumentKind::Script,
        DocumentKind::Test,
        DocumentKind::TestHelper,
    ];
    let file_count = match rng.below(8) {
        0..=4 => 1,
        5 | 6 => 2,
        _ => 3 + rng.below(2),
    };
    let mut texts = match rng.below(2) {
        0 => generate_project(rng, file_count),
        _ => Vec::new(),
    };
    while texts.len() < file_count && !pool.is_empty() {
        texts.push(pool[rng.below(pool.len())].clone());
    }
    let mut next_key = texts.len();
    let mut state = ProjectState {
        files: texts
            .into_iter()
            .enumerate()
            .map(|(key, text)| (key, text, KINDS[rng.below(KINDS.len())]))
            .collect(),
        strict: rng.below(8) == 7,
        prelude: rng.below(12) != 11,
    };
    let mut states = vec![state.clone()];
    for _ in 0..1 + rng.below(3) {
        let target = rng.below(state.files.len());
        match rng.below(16) {
            0..=10 if !state.files.is_empty() => {
                let donor = match pool.is_empty() {
                    true => "probe <- 1L",
                    false => pool[rng.below(pool.len())].as_str(),
                };
                state.files[target].1 = small_edit(rng, &state.files[target].1, donor);
            }
            11 if !state.files.is_empty() => state.files[target].1 = generate_program(rng),
            12 if state.files.len() > 1 => {
                state.files.remove(target);
            }
            13 if state.files.len() > 1 => {
                let other = rng.below(state.files.len());
                state.files.swap(target, other);
            }
            14 if !state.files.is_empty() => state.files[target].2 = KINDS[rng.below(KINDS.len())],
            15 => match rng.below(2) {
                0 => state.strict = !state.strict,
                _ => state.prelude = !state.prelude,
            },
            _ => {
                state.files.push((
                    next_key,
                    generate_program(rng),
                    KINDS[rng.below(KINDS.len())],
                ));
                next_key += 1;
            }
        }
        states.push(state.clone());
    }
    states
}

/// A fixture case as a project state: a `#~~~~ <path>` case as its files, with the kinds their
/// paths give them.
pub fn fixture_state(source: &str, default_kind: DocumentKind) -> ProjectState {
    let files = match split_files(source) {
        Some(files) => files
            .into_iter()
            .enumerate()
            .map(|(key, (path, text))| {
                let kind = match path.strip_prefix("tests/testthat/") {
                    Some(name) => test_document_kind(name),
                    None => default_kind,
                };
                (key, text, kind)
            })
            .collect(),
        None => vec![(0, source.to_owned(), default_kind)],
    };
    ProjectState {
        files,
        strict: false,
        prelude: !source.starts_with(PRELUDE_OFF_DIRECTIVE),
    }
}

/// A single file edited to `edited_source` and back, on databases built for this call. The warm
/// [`Workbench`] is the cheaper way to ask the same question; this one has no history, so a
/// coverage-guided target's input is its whole reproducer.
pub fn check_semantics_invariants(source: &str, edited_source: &str, kind: DocumentKind) {
    let state = |text: &str| ProjectState {
        files: vec![(0, text.to_owned(), kind)],
        strict: false,
        prelude: true,
    };
    check_session(
        &mut Workbench::default(),
        &[state(source), state(edited_source)],
    );
}

/// The syntactic screen that spares most items a naming pass must never hide a top-level write
/// naming records, or the project-wide maps would lose that binding.
fn check_write_screen(db: &RootDatabase, file: SourceFile, source: &str) {
    for &item in item_tree(db, file) {
        let Some(syntax) = item_syntax(db, item) else {
            continue;
        };
        if !binds_only_own_name(db, item, &syntax.syntax_node()) {
            continue;
        }
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        let own_name = item.name(db);
        for binding in naming.bindings.values() {
            assert!(
                binding.kind != BindingKind::TopLevel
                    || own_name.as_deref() == Some(binding.name.as_str()),
                "the write screen passed an item that binds `{}` at top level in:\n{source}",
                binding.name.as_str()
            );
        }
    }
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
    let edited = small_edit(&mut SplitMix64(hash), input, "probe <- 1L\n");
    check_semantics_invariants(input, &edited, kind);
}

thread_local! {
    static FIXTURE_DATABASE: RefCell<Option<RootDatabase>> = const { RefCell::new(None) };
}

/// The first line that turns the prelude off for a fixture case, as `[check] prelude = false` does
/// for a project.
pub const PRELUDE_OFF_DIRECTIVE: &str = "# [check] prelude = false";

/// Reuses one database per thread across cases, because parsing the shipped stubs costs far more
/// than checking a case. A case whose first file starts with [`PRELUDE_OFF_DIRECTIVE`] runs with
/// the prelude off; every other case gets the default settings.
pub fn with_fixture_project<R>(
    files: Vec<(String, DocumentKind)>,
    render: impl FnOnce(&RootDatabase, &[SourceFile]) -> R,
) -> R {
    with_fixture_settings(files, Vec::new(), false, render)
}

/// [`with_fixture_project`] in a project with `[check] strict = true`.
pub fn with_strict_fixture_project<R>(
    files: Vec<(String, DocumentKind)>,
    render: impl FnOnce(&RootDatabase, &[SourceFile]) -> R,
) -> R {
    with_fixture_settings(files, Vec::new(), true, render)
}

/// [`with_fixture_project`] with the project's data files, each a path and its text, sniffed as
/// a host sniffs a project's `.csv` files.
pub fn with_fixture_data_project<R>(
    files: Vec<(String, DocumentKind)>,
    data_files: Vec<(String, String)>,
    render: impl FnOnce(&RootDatabase, &[SourceFile]) -> R,
) -> R {
    with_fixture_settings(files, data_files, false, render)
}

fn with_fixture_settings<R>(
    files: Vec<(String, DocumentKind)>,
    data_files: Vec<(String, String)>,
    strict: bool,
    render: impl FnOnce(&RootDatabase, &[SourceFile]) -> R,
) -> R {
    let layouts: BTreeMap<String, sniff::Layout> = data_files
        .into_iter()
        .map(|(path, text)| (path, sniff::sniff_head(text.as_bytes())))
        .collect();
    FIXTURE_DATABASE.with_borrow_mut(|slot| {
        let db = slot.get_or_insert_with(|| {
            let db = RootDatabase::default();
            install_shipped_stubs(&db);
            AnalysisSettings::new(&db, true, strict, true);
            db
        });
        let prelude = files
            .first()
            .is_none_or(|(text, _)| !text.starts_with(PRELUDE_OFF_DIRECTIVE));
        let settings = AnalysisSettings::get(db);
        if *settings.prelude(db) != prelude {
            settings.set_prelude(db).to(prelude);
        }
        if *settings.strict(db) != strict {
            settings.set_strict(db).to(strict);
        }
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
        match DataFiles::try_get(db) {
            Some(data) if *data.layouts(db) != layouts => {
                data.set_layouts(db).to(layouts);
            }
            Some(_) => {}
            None if layouts.is_empty() => {}
            None => {
                DataFiles::new(db, layouts);
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
    trees: Arc<TreeStore>,
    executed: Arc<Mutex<Vec<String>>>,
    diagnostics_only: bool,
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
            trees: Arc::default(),
            executed,
            diagnostics_only: false,
        }
    }
}

#[salsa::db]
impl salsa::Database for ProbeDatabase {}

#[salsa::db]
impl Db for ProbeDatabase {
    fn trees(&self) -> &TreeStore {
        &self.trees
    }

    fn editor_facts(&self) -> bool {
        !self.diagnostics_only
    }
}

impl ProbeDatabase {
    /// A probe for a host that only reports diagnostics, like
    /// `RootDatabase::for_command(false)`, with a store so small that any project of three
    /// files drops trees.
    pub fn diagnostics_only() -> ProbeDatabase {
        ProbeDatabase {
            trees: Arc::new(TreeStore::bounded(2)),
            diagnostics_only: true,
            ..ProbeDatabase::default()
        }
    }

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
            .filter_map(|item| item.name(self).map(|name| name.as_str().to_owned()))
            .collect();
        names.sort();
        names
    }
}
