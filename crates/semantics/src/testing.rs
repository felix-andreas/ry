//! Shared test support, including the fuzz invariant battery: no panics, deterministic output
//! across fresh databases, in-bounds ranges, and incremental equivalence with a fresh build.

use {
    crate::{
        ANALYSIS_STACK_SIZE, AnalysisSettings, DataFiles, Db, DocumentKind, ItemKind, ProjectFiles,
        RootDatabase, SourceFile, TreeStore, binds_only_own_name,
        diagnostics::{TypeRenderer, file_diagnostics, strict_diagnostics},
        hir::{ExprId, PatId, Symbol},
        item_check, item_hir, item_naming, item_node, item_syntax, item_tree,
        lints::{LintConfig, LintLevel, NameStyle, lint_file},
        naming::BindingKind,
        stubs::{
            StubSources, install_shipped_stubs, shipped_stub_sources, stub_source_problems, stubs,
        },
        test_document_kind,
    },
    rustc_hash::FxHashSet,
    salsa::{Setter as _, plumbing::AsId as _},
    std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        env,
        io::{self, Write as _},
        mem,
        ops::Bound,
        panic,
        process::Command,
        slice,
        sync::{Arc, LazyLock, Mutex},
        thread,
    },
    syntax::{
        SyntaxKind, SyntaxToken,
        testing::{
            Entropy, FAILURES_PER_RUN, SplitMix64, fuzz_seeds, generate_program, generate_project,
            on_seed_thread, replay_command, running_test_name, small_edit, split_files,
        },
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

/// Three databases that outlive a fuzz case, so a case pays for its analysis and not for
/// re-parsing the stub corpus, which costs thirty times as much.
///
/// The incremental database is an editor's: it keeps one input per file key and moves from
/// state to state through the setters, across cases too, so every state is reached by an edit
/// from an unrelated history. The reference database is a command's (`ry check`): bounded tree
/// store, no editor facts, and new inputs for every state, so nothing it memoized about a file
/// survives into the next. The two must render every file alike. The editor reference records
/// editor facts like the incremental database but gets new inputs for every state it checks,
/// so the two must record the same editor facts.
pub struct Workbench {
    incremental: RootDatabase,
    inputs: Vec<(usize, SourceFile)>,
    reference: RootDatabase,
    editor_reference: RootDatabase,
    sessions: usize,
}

impl Default for Workbench {
    fn default() -> Workbench {
        let incremental = RootDatabase::default();
        let reference = RootDatabase::for_command(false);
        let editor_reference = RootDatabase::default();
        for db in [&incremental, &reference, &editor_reference] {
            install_shipped_stubs(db);
            AnalysisSettings::new(db, true, false, true);
            ProjectFiles::new(db, Vec::new());
        }
        Workbench {
            incremental,
            inputs: Vec::new(),
            reference,
            editor_reference,
            sessions: 0,
        }
    }
}

/// Runs one session per seed of [`fuzz_seeds`] on a warm [`Workbench`], each on a thread named
/// after the test and the seed, so a stack overflow names its seed. A failure is retried on
/// fresh databases to tell a failure of the session alone, which its seed replays, from one that
/// needs the sessions before it, which the window from the workbench's first seed replays. The
/// workbench is rebuilt every [`WORKBENCH_SESSIONS`] seeds so that window stays short, and after
/// a failure, since the run goes on to report up to [`FAILURES_PER_RUN`] failing seeds.
///
/// Every [`CHILD_SAMPLE`]th session of the run is then rendered again by one child process
/// ([`compare_with_child_process`]), whose interners fill in another order.
pub fn for_each_session(default: usize, mut session: impl FnMut(u64) -> Vec<ProjectState> + Send) {
    if let Some(seeds) = interning_child_seeds() {
        render_in_another_interning_order(&seeds, &mut session);
        return;
    }
    let mut bench: Option<(u64, Workbench)> = None;
    let mut replays: Vec<String> = Vec::new();
    let mut samples: Vec<(u64, Vec<Vec<String>>)> = Vec::new();
    let seeds = fuzz_seeds(default);
    let first_of_run = seeds.start;
    for seed in seeds {
        if seed % WORKBENCH_SESSIONS == 0 {
            bench = None;
        }
        let (first_seed, workbench) = bench.get_or_insert_with(|| (seed, Workbench::default()));
        let states = session(seed);
        if let Ok(renderings) = on_seed_thread(seed, || check_session(workbench, &states)) {
            if (seed - first_of_run) % CHILD_SAMPLE == 0 {
                samples.push((seed, renderings));
            }
            continue;
        }
        let alone = on_seed_thread(seed, || {
            check_session(&mut Workbench::default(), &states);
        });
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
    compare_with_child_process(&samples);
}

const WORKBENCH_SESSIONS: u64 = 256;

/// One session in this many, counted from the run's first seed, is rendered again in the child
/// process, so a replay of one seed (`FUZZ_SEED`) always samples it.
pub const CHILD_SAMPLE: u64 = 1;

/// Walks the states and then returns to the first. At every step the incremental database must
/// render each file as the reference does, and the return must restore the first rendering.
/// The final state's editor facts must equal those of a database that checks it from new
/// inputs. Every fourth session also renders each state on several threads at once, which must
/// equal the sequential rendering, and every sixty-fourth compares its first state with a
/// database that has no history at all. Returns the reference rendering of each state.
pub fn check_session(workbench: &mut Workbench, states: &[ProjectState]) -> Vec<Vec<String>> {
    let mut first: Option<Vec<String>> = None;
    let mut renderings = Vec::with_capacity(states.len());
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
        if step + 1 == states.len() {
            let incremental = editor_facts_of_inputs(workbench);
            let fresh = render_editor_facts_afresh(&mut workbench.editor_reference, state);
            if let Err(difference) = compare_renderings(&fresh, &incremental) {
                panic!(
                    "step {step}: the incremental editor facts diverge from a fresh check\n{}\n{difference}",
                    describe_session(states),
                );
            }
        }
        let parallel = match workbench.sessions % 4 {
            0 => render_in_parallel(&mut workbench.reference, state),
            _ => Vec::new(),
        };
        for (worker, parallel) in parallel.into_iter().enumerate() {
            if let Err(difference) = compare_renderings(&reference, &parallel) {
                panic!(
                    "step {step}: a parallel check diverges from a sequential one (worker {worker} of {PARALLEL_WORKERS})\n{}\n{difference}",
                    describe_session(states),
                );
            }
        }
        match &first {
            None => {
                if workbench.sessions % 64 == 0 {
                    let mut pristine = RootDatabase::for_command(false);
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
        if step < states.len() {
            renderings.push(reference);
        }
    }
    workbench.sessions += 1;
    renderings
}

/// `Ok` when two renderings of the same files are equal, else the first file that differs
/// with both its renderings. The comparison of the parallel check, the editor facts and the
/// child process.
pub fn compare_renderings(expected: &[String], actual: &[String]) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!(
            "{} files rendered against {} expected",
            actual.len(),
            expected.len()
        ));
    }
    match expected
        .iter()
        .zip(actual)
        .position(|(expected, actual)| expected != actual)
    {
        None => Ok(()),
        Some(file) => Err(format!(
            "file {file} differs\n---- expected:\n{}---- actual:\n{}",
            expected[file], actual[file]
        )),
    }
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

/// The editor facts of the incremental database's current project, file by file.
fn editor_facts_of_inputs(workbench: &Workbench) -> Vec<String> {
    let db = &workbench.incremental;
    ProjectFiles::get(db)
        .files(db)
        .iter()
        .map(|file| render_editor_facts(db, *file))
        .collect()
}

/// [`render_editor_facts`] of `state` from inputs `db` has never seen.
fn render_editor_facts_afresh(db: &mut RootDatabase, state: &ProjectState) -> Vec<String> {
    let files = set_new_inputs(db, state);
    files
        .iter()
        .map(|file| render_editor_facts(db, *file))
        .collect()
}

/// Every fact an item's check records for the editor, one line each: the type of every
/// expression and pattern rendered as hover renders it, the expected type of each literal and
/// loop variable, the selected overloads and the callee targets, with the absolute range of the
/// expression they belong to.
pub fn render_editor_facts(db: &RootDatabase, file: SourceFile) -> String {
    let mut output = String::new();
    for &item in item_tree(db, file) {
        let (Some(check), Some(hir), Some(node)) = (
            item_check(db, item),
            item_hir(db, item),
            item_node(db, item),
        ) else {
            continue;
        };
        let offset = node.text_range().start();
        let facts = check.editor();
        let range = |id: ExprId| {
            let range = hir.range(id) + offset;
            format!("{}..{}", u32::from(range.start()), u32::from(range.end()))
        };
        let render = |ty| TypeRenderer::default().render(db, ty);
        for index in 0..hir.ranges().len() {
            let id = ExprId::new(index);
            if let Some(&ty) = facts.expression_types.get(&id) {
                output.push_str(&format!("{} {}\n", range(id), render(ty)));
            }
        }
        for index in 0..hir.pattern_count() {
            if let Some(ty) = facts.pattern_types.get(PatId::new(index)) {
                output.push_str(&format!("pattern {index} {}\n", render(ty)));
            }
        }
        for (&id, &ty) in facts.literal_expectations.iter() {
            output.push_str(&format!("{} expects {}\n", range(id), render(ty)));
        }
        for (&id, &ty) in facts.loop_variables.iter() {
            output.push_str(&format!("{} binds {}\n", range(id), render(ty)));
        }
        for (&id, overload) in facts.selected_overloads.iter() {
            output.push_str(&format!("{} selects overload {overload}\n", range(id)));
        }
        for (&id, target) in facts.callee_targets.iter() {
            output.push_str(&format!("{} calls {target:?}\n", range(id)));
        }
    }
    output
}

/// Sets `state` on `db` as new inputs, which share no memo with any file `db` saw before.
fn set_new_inputs(db: &mut RootDatabase, state: &ProjectState) -> Vec<SourceFile> {
    apply_settings(db, state);
    let files: Vec<SourceFile> = state
        .files
        .iter()
        .map(|(_, text, kind)| SourceFile::new(db, text.clone(), *kind))
        .collect();
    ProjectFiles::get(db).set_files(db).to(files.clone());
    files
}

/// Renders `state` from inputs `db` has never seen.
pub fn render_afresh(db: &mut RootDatabase, state: &ProjectState) -> Vec<String> {
    let files = set_new_inputs(db, state);
    files
        .iter()
        .zip(&state.files)
        .map(|(file, (_, text, _))| {
            check_write_screen(db, *file, text);
            render_semantics(db, *file)
        })
        .collect()
}

/// How many threads [`render_in_parallel`] races: enough that two of them meet on most items.
const PARALLEL_WORKERS: usize = 3;

/// Renders `state` from new inputs on [`PARALLEL_WORKERS`] threads at once, one rendering of
/// every file per thread, as `ry check` and the server fan a check out over database handles.
/// The threads are handles of the warm reference database, so they share its memos of the stub
/// library but none of the new inputs. Each first checks every item in its own rotated order, so
/// the threads race to claim items and settle fixpoints, and each then renders the files
/// starting from a different one. Every rendering must equal the sequential one: what a check
/// finds may not depend on which thread got to an item first. The interners are process-global,
/// so this cannot vary the order in which ids were handed out before; the child process of
/// [`compare_with_child_process`] does.
fn render_in_parallel(db: &mut RootDatabase, state: &ProjectState) -> Vec<Vec<String>> {
    let files = set_new_inputs(db, state);
    let files = &files;
    thread::scope(|scope| {
        let handles: Vec<_> = (0..PARALLEL_WORKERS)
            .map(|worker| {
                let db = db.clone();
                thread::Builder::new()
                    .stack_size(ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        let count = files.len();
                        for offset in 0..count {
                            let file = files[(worker + offset) % count];
                            let items = item_tree(&db, file);
                            for index in 0..items.len() {
                                let at = (worker * 7 + index) % items.len();
                                item_check(&db, items[at]);
                            }
                        }
                        let mut rendered = vec![String::new(); count];
                        for offset in 0..count {
                            let index = (count - offset + worker) % count;
                            rendered[index] = render_semantics(&db, files[index]);
                        }
                        rendered
                    })
                    .expect("a checking thread must spawn")
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| panic::resume_unwind(payload))
            })
            .collect()
    })
}

/// The variable that makes a test binary the child of [`compare_with_child_process`], holding
/// the seeds to render, comma-separated.
const INTERNING_CHILD: &str = "FUZZ_INTERNING_CHILD";

/// What starts each rendering a child prints: `<marker> <seed> <state> <file> <bytes>`, a line
/// break, and then that many bytes of rendering.
const CHILD_FRAME: &str = "@@ry-child-rendering";

fn interning_child_seeds() -> Option<Vec<u64>> {
    let seeds = env::var(INTERNING_CHILD).ok()?;
    Some(
        seeds
            .split(',')
            .filter_map(|seed| seed.trim().parse().ok())
            .collect(),
    )
}

/// Renders the sampled sessions again in one child process and asserts it renders every state
/// as this process did. The `Ty` and `Symbol` interners are process-global, so any database of
/// this process gets the same id for a type or a name as the reference did, and output that
/// depends on an id's order passes every comparison made here. The child starts with empty
/// interners and fills them in another order ([`render_in_another_interning_order`]).
fn compare_with_child_process(samples: &[(u64, Vec<Vec<String>>)]) {
    let test = running_test_name();
    if samples.is_empty() || test.is_empty() || test == "main" {
        return;
    }
    let seeds: Vec<String> = samples.iter().map(|(seed, _)| seed.to_string()).collect();
    let child = render_in_child_process(&test, &seeds.join(","));
    let replay = |seed: u64| {
        format!(
            "    {}\n  and the child alone with\n    {INTERNING_CHILD}={seed} {} --nocapture",
            replay_command(seed),
            replay_command(seed).trim_start_matches(&format!("FUZZ_SEED={seed} ")),
        )
    };
    let mut failures = Vec::new();
    for (seed, states) in samples {
        for (state, expected) in states.iter().enumerate() {
            let actual: Vec<String> = (0..expected.len())
                .map(|file| {
                    child
                        .get(&(*seed, state, file))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect();
            if let Err(difference) = compare_renderings(expected, &actual) {
                failures.push(format!(
                    "seed {seed}, state {state}: the rendering depends on the interning order\n{difference}\nreplay with\n{}",
                    replay(*seed)
                ));
            }
        }
        if failures.len() == FAILURES_PER_RUN {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Runs `test` of this test binary as the child, and returns each rendering it printed by seed,
/// state and file.
fn render_in_child_process(test: &str, seeds: &str) -> BTreeMap<(u64, usize, usize), String> {
    let executable = env::current_exe().expect("a test knows its own executable");
    let output = Command::new(executable)
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(INTERNING_CHILD, seeds)
        .env_remove("FUZZ_SEED")
        .env_remove("FUZZ_ITERS")
        .env_remove("FUZZ_DEPTH")
        .output()
        .expect("the child test process must start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the child process for seeds {seeds} failed\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut renderings = BTreeMap::new();
    let mut rest: &str = &stdout;
    while let Some(at) = rest.find(CHILD_FRAME) {
        let header_end = rest[at..].find('\n').map_or(rest.len(), |end| at + end);
        let fields: Vec<&str> = rest[at + CHILD_FRAME.len()..header_end]
            .split_whitespace()
            .collect();
        let parsed = match fields[..] {
            [seed, state, file, length] => seed
                .parse()
                .ok()
                .zip(state.parse().ok())
                .zip(file.parse().ok())
                .zip(length.parse::<usize>().ok()),
            _ => None,
        };
        let Some((((seed, state), file), length)) = parsed else {
            panic!(
                "the child printed a malformed frame header: {:?}",
                &rest[at..header_end]
            );
        };
        let body_start = (header_end + 1).min(rest.len());
        let body = rest
            .get(body_start..body_start + length)
            .unwrap_or_else(|| panic!("the child's rendering of seed {seed} is cut short"));
        renderings.insert((seed, state, file), body.to_owned());
        rest = &rest[body_start + length..];
    }
    renderings
}

/// The child's side of [`compare_with_child_process`]. The parent interns the stubs' words
/// first and then the sessions' words in the order they appear, so the child interns the
/// sessions' words first, each list in reverse, which flips the order of almost every pair. It
/// then checks
/// every state on a database without stubs, sessions and items in reverse, so the project's
/// types take ids before the stub library's; and then renders every state from new inputs on a
/// command database, again checking the sessions, states, files and items in reverse before it
/// renders. Each rendering goes to stdout in a [`CHILD_FRAME`].
fn render_in_another_interning_order(
    seeds: &[u64],
    session: &mut impl FnMut(u64) -> Vec<ProjectState>,
) {
    let sessions: Vec<(u64, Vec<ProjectState>)> = seeds
        .iter()
        .rev()
        .map(|&seed| (seed, session(seed)))
        .collect();
    // The parent meets the stubs' words first and the sessions' words in the order they
    // appear; interning both in reverse, the sessions' first, flips the order of every pair.
    let stub_sources = shipped_stub_sources();
    let mut seen = FxHashSet::default();
    let project_words = first_appearances(
        sessions
            .iter()
            .rev()
            .flat_map(|(_, states)| states)
            .flat_map(|state| &state.files)
            .map(|(_, text, _)| text.as_str()),
        &mut seen,
    );
    let stub_words = first_appearances(
        stub_sources.iter().map(|(_, text)| text.as_str()),
        &mut seen,
    );
    for word in project_words.iter().rev().chain(stub_words.iter().rev()) {
        Symbol::new(word);
    }
    let mut bare = RootDatabase::for_command(false);
    AnalysisSettings::new(&bare, true, false, true);
    ProjectFiles::new(&bare, Vec::new());
    for (_, states) in &sessions {
        for state in states.iter().rev() {
            let files = set_new_inputs(&mut bare, state);
            check_in_reverse(&bare, &files);
        }
    }
    let mut db = RootDatabase::for_command(false);
    install_shipped_stubs(&db);
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    let mut out = io::stdout().lock();
    for (seed, states) in &sessions {
        for (index, state) in states.iter().enumerate().rev() {
            let files = set_new_inputs(&mut db, state);
            check_in_reverse(&db, &files);
            for (file_index, file) in files.iter().enumerate().rev() {
                let rendering = render_semantics(&db, *file);
                write!(
                    out,
                    "\n{CHILD_FRAME} {seed} {index} {file_index} {}\n{rendering}",
                    rendering.len()
                )
                .expect("the child writes its renderings to stdout");
            }
        }
    }
    out.flush().expect("the child flushes its renderings");
}

/// The words of `texts` in the order they first appear, skipping those in `seen`.
fn first_appearances<'text>(
    texts: impl Iterator<Item = &'text str>,
    seen: &mut FxHashSet<&'text str>,
) -> Vec<&'text str> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '.' || character == '_';
    let mut words = Vec::new();
    for text in texts {
        for word in text.split(|character: char| !is_word(character)) {
            if !word.is_empty() && seen.insert(word) {
                words.push(word);
            }
        }
    }
    words
}

fn check_in_reverse(db: &RootDatabase, files: &[SourceFile]) {
    for &file in files.iter().rev() {
        for &item in item_tree(db, file).iter().rev() {
            item_check(db, item);
        }
    }
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

/// Which metamorphic edits [`check_metamorphic_relations`] found a place for, so a battery can
/// tell an oracle that checks from one whose preconditions never hold.
#[derive(Clone, Copy, Debug, Default)]
pub struct MetamorphicCoverage {
    pub leading_trivia: bool,
    pub rename: bool,
}

/// Checks two edits that change no meaning against the rendering of `state` on the
/// workbench's command database, each where `rng` places it.
///
/// - A blank line or a comment line inserted before a top-level expression moves the findings
///   after it by its length and changes nothing else.
/// - Renaming every occurrence of an identifier the project alone spells, to a fresh name of
///   the same length and character classes, changes nothing but that name in the findings.
pub fn check_metamorphic_relations(
    workbench: &mut Workbench,
    state: &ProjectState,
    rng: &mut dyn Entropy,
) -> MetamorphicCoverage {
    let db = &mut workbench.reference;
    let original = render_afresh(db, state);
    MetamorphicCoverage {
        leading_trivia: check_leading_trivia(db, state, &original, rng),
        rename: check_consistent_rename(db, state, &original, rng),
    }
}

fn check_leading_trivia(
    db: &mut RootDatabase,
    state: &ProjectState,
    original: &[String],
    rng: &mut dyn Entropy,
) -> bool {
    if state.files.is_empty() {
        return false;
    }
    let target = rng.below(state.files.len());
    let text = &state.files[target].1;
    // A line under a comment is left alone: `#:` and `#'` comments belong to the expression
    // below them, and a line between would detach them.
    let starts: Vec<usize> = syntax::parse(text)
        .syntax_node()
        .children()
        .map(|child| u32::from(child.text_range().start()) as usize)
        .filter(|&start| {
            let before = &text[..start];
            let previous = before.strip_suffix('\n').unwrap_or(before);
            let previous = previous.rsplit('\n').next().unwrap_or(previous);
            (start == 0 || before.ends_with('\n')) && !previous.trim_start().starts_with('#')
        })
        .collect();
    if starts.is_empty() {
        return false;
    }
    let offset = starts[rng.below(starts.len())];
    let inserted = ["\n", "# note\n"][rng.below(2)];
    let mut edited = state.clone();
    edited.files[target].1.insert_str(offset, inserted);
    let expected: Vec<String> = original
        .iter()
        .enumerate()
        .map(|(index, rendering)| match index == target {
            true => shift_ranges(rendering, offset, inserted.len()),
            false => rendering.clone(),
        })
        .collect();
    let actual = render_afresh(db, &edited);
    assert!(
        actual == expected,
        "inserting {inserted:?} at byte {offset} of file {target} changes more than the positions after it\n{}\n---- expected:\n{}\n---- after the insertion:\n{}",
        describe_session(slice::from_ref(state)),
        expected.join("----\n"),
        actual.join("----\n"),
    );
    true
}

/// Moves every finding of a [`render_semantics`] rendering that lies at or after `offset` by
/// `length` bytes; a finding that spans `offset` grows by it.
fn shift_ranges(rendering: &str, offset: usize, length: usize) -> String {
    let mut shifted = String::with_capacity(rendering.len());
    for line in rendering.split_inclusive('\n') {
        let finding = line.split_once(' ').and_then(|(range, rest)| {
            let (start, end) = range.split_once("..")?;
            let start: usize = start.parse().ok()?;
            let end: usize = end.parse().ok()?;
            rest.starts_with('[').then_some((start, end, rest))
        });
        let Some((start, end, rest)) = finding else {
            shifted.push_str(line);
            continue;
        };
        let (start, end) = match start >= offset {
            true => (start + length, end + length),
            false if end > offset => (start, end + length),
            false => (start, end),
        };
        shifted.push_str(&format!("{start}..{end} {rest}"));
    }
    shifted
}

/// Whether an identifier names something outside the project, which a rename of the
/// project's own names must leave alone: a package's export, a word of the annotation language
/// (`fn`, `trust`, a type), a name R6 binds by its spelling, or a member or argument name a
/// stub declares (`add` in `on.exit(add = TRUE)`, a field of a stub's record). The parameters
/// and fields of a type the project writes are its own.
fn names_foreign(token: &SyntaxToken) -> bool {
    const SPELLING_BOUND: [&str; 6] =
        ["self", "private", "super", "public", "active", "initialize"];
    let parent = token
        .parent_ancestors()
        .map(|ancestor| ancestor.kind())
        .find(|kind| *kind != SyntaxKind::NAME);
    let foreign_syntax = parent.is_some_and(|parent| {
        parent == SyntaxKind::NAMESPACE_EXPR
            || ((SyntaxKind::ANNOTATION..=SyntaxKind::PURE_MARKER).contains(&parent)
                && !matches!(parent, SyntaxKind::TYPE_PARAMETER | SyntaxKind::TYPE_FIELD))
    });
    let trivia = |kind: SyntaxKind| {
        matches!(
            kind,
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        )
    };
    let mut previous = token.prev_token();
    while let Some(candidate) = previous
        .as_ref()
        .filter(|candidate| trivia(candidate.kind()))
    {
        previous = candidate.prev_token();
    }
    let mut next = token.next_token();
    while let Some(candidate) = next.as_ref().filter(|candidate| trivia(candidate.kind())) {
        next = candidate.next_token();
    }
    let member = previous
        .is_some_and(|previous| matches!(previous.kind(), SyntaxKind::DOLLAR | SyntaxKind::AT))
        || next.is_some_and(|next| next.kind() == SyntaxKind::EQ);
    foreign_syntax
        || SPELLING_BOUND.contains(&token.text())
        || (member && stub_words().contains(token.text()))
}

/// Every word the shipped stubs spell, which includes each parameter and field name they
/// declare.
fn stub_words() -> &'static FxHashSet<String> {
    static WORDS: LazyLock<FxHashSet<String>> = LazyLock::new(|| {
        let is_word = |character: char| {
            character.is_ascii_alphanumeric() || character == '.' || character == '_'
        };
        shipped_stub_sources()
            .iter()
            .flat_map(|(_, text)| text.split(|character: char| !is_word(character)))
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect()
    });
    &WORDS
}

fn check_consistent_rename(
    db: &mut RootDatabase,
    state: &ProjectState,
    original: &[String],
    rng: &mut dyn Entropy,
) -> bool {
    let files: Vec<Vec<(SyntaxKind, String, bool)>> = state
        .files
        .iter()
        .map(|(_, text, _)| {
            syntax::parse(text)
                .syntax_node()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .map(|token| {
                    let foreign = token.kind() == SyntaxKind::IDENT && names_foreign(&token);
                    (token.kind(), token.text().to_owned(), foreign)
                })
                .collect()
        })
        .collect();
    let tokens: Vec<&(SyntaxKind, String, bool)> = files.iter().flatten().collect();
    let identifiers: BTreeSet<&str> = tokens
        .iter()
        .filter(|(kind, _, _)| *kind == SyntaxKind::IDENT)
        .map(|(_, text, _)| text.as_str())
        .collect();
    let candidates: Vec<&str> = identifiers
        .iter()
        .copied()
        .filter(|word| renameable(db, word, &tokens))
        .collect();
    if candidates.is_empty() {
        return false;
    }
    let old = candidates[rng.below(candidates.len())];
    let Some(new) = fresh_name(db, old, &tokens, original, rng) else {
        return false;
    };
    let mut edited = state.clone();
    for ((_, text, _), tokens) in edited.files.iter_mut().zip(&files) {
        *text = tokens
            .iter()
            .map(
                |(kind, text, _)| match *kind == SyntaxKind::IDENT && text == old {
                    true => new.as_str(),
                    false => text.as_str(),
                },
            )
            .collect();
    }
    let renamed = render_afresh(db, &edited);
    // A near-miss suggestion picks by spelling, so a rename may change which name it offers.
    let suggests = |renderings: &[String], word: &str| {
        renderings
            .iter()
            .flat_map(|rendering| rendering.lines())
            .any(|line| line.contains("Did you mean") && line.contains(word))
    };
    if suggests(original, old) || suggests(&renamed, &new) {
        return false;
    }
    let sorted_lines = |rendering: &str| {
        let mut lines: Vec<&str> = rendering.lines().collect();
        lines.sort_unstable();
        lines.join("\n")
    };
    let expected: Vec<String> = original.iter().map(|text| sorted_lines(text)).collect();
    let actual: Vec<String> = renamed
        .iter()
        .map(|text| sorted_lines(&text.replace(new.as_str(), old)))
        .collect();
    assert!(
        actual == expected,
        "renaming `{old}` to `{new}` throughout changes more than the name\n{}\n---- before, sorted:\n{}\n---- after, renamed back and sorted:\n{}",
        describe_session(slice::from_ref(state)),
        expected.join("\n----\n"),
        actual.join("\n----\n"),
    );
    true
}

/// Whether every occurrence of `word` is an identifier token of the project's own: no string,
/// comment or other token contains it, no identifier is a prefix R's partial matching could
/// complete to it, and the shipped stubs do not declare it.
fn renameable(db: &RootDatabase, word: &str, tokens: &[&(SyntaxKind, String, bool)]) -> bool {
    let mut characters = word.chars();
    let snake = characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });
    snake
        && !declared_outside(db, word)
        && tokens.iter().all(|(kind, text, foreign)| {
            match *kind == SyntaxKind::IDENT && text == word {
                true => !foreign,
                false => {
                    !text.contains(word)
                        && !(*kind == SyntaxKind::IDENT && word.starts_with(text.as_str()))
                }
            }
        })
}

/// A name of `old`'s length and character classes that no token, stub or rendering contains,
/// that no identifier of the project is a prefix of, and that sorts against every word of the
/// project and its rendering as `old` does, or `None` when a few draws find none. Records
/// list their fields by name and symbols order by spelling, so a name that moved past another
/// would reorder what a rendering prints.
fn fresh_name(
    db: &RootDatabase,
    old: &str,
    tokens: &[&(SyntaxKind, String, bool)],
    original: &[String],
    rng: &mut dyn Entropy,
) -> Option<String> {
    let is_word =
        |character: char| character.is_ascii_alphanumeric() || character == '.' || character == '_';
    let words: BTreeSet<&str> = tokens
        .iter()
        .map(|(_, text, _)| text.as_str())
        .chain(original.iter().map(String::as_str))
        .flat_map(|text| text.split(|character: char| !is_word(character)))
        .filter(|word| !word.is_empty())
        .collect();
    let below = words.range::<&str, _>(..old).next_back().copied();
    let above = words
        .range::<&str, _>((Bound::Excluded(old), Bound::Unbounded))
        .next()
        .copied();
    for _ in 0..32 {
        let kept = rng.below(old.len());
        let new: String = old
            .char_indices()
            .map(|(index, character)| match (index < kept, character) {
                (true, _) => character,
                (false, 'a'..='z') => (b'a' + rng.below(26) as u8) as char,
                (false, '0'..='9') => (b'0' + rng.below(10) as u8) as char,
                (false, other) => other,
            })
            .collect();
        let in_place = below.is_none_or(|below| below < new.as_str())
            && above.is_none_or(|above| new.as_str() < above);
        let lexes_as_name = syntax::parse(&new)
            .syntax_node()
            .first_token()
            .is_some_and(|token| token.kind() == SyntaxKind::IDENT && token.text() == new);
        let unused = new != old
            && in_place
            && lexes_as_name
            && !declared_outside(db, &new)
            && !stub_words().contains(new.as_str())
            && original
                .iter()
                .all(|rendering| !rendering.contains(new.as_str()))
            && tokens.iter().all(|(kind, text, _)| {
                !text.contains(new.as_str())
                    && !(*kind == SyntaxKind::IDENT && new.starts_with(text.as_str()))
            });
        if unused {
            return Some(new);
        }
    }
    None
}

/// Whether the shipped stubs know `word` as a name or a namespace, in the library the
/// project's attached packages select: such a name means something outside the project.
fn declared_outside(db: &RootDatabase, word: &str) -> bool {
    stubs(db).is_some_and(|library| {
        library.declarations.contains_key(word)
            || library.type_declarations.contains_key(word)
            || library.known_exports.contains(word)
            || library.knows_namespace(word)
    })
}

/// Loads `text` as a project's only stub source on `db`, a warm database whose stub input this
/// call owns, and checks the loader: it never panics, every problem names a line of the text
/// and every declaration a range inside it, the library is the same after loading another
/// source and this one again, and a database that never saw another source loads it alike.
/// A few declared names are then called from a project file, which must check without a panic.
pub fn check_stub_source(db: &mut RootDatabase, text: &str) {
    let sources = vec![(String::from("pkg"), text.to_owned())];
    let line_count = text.lines().count();
    for problem in stub_source_problems(db, text) {
        assert!(
            problem.line < line_count,
            "a problem on line {} of a stub of {line_count} lines: {}\n{text}",
            problem.line,
            problem.message
        );
    }
    let stub_input = match StubSources::try_get(db) {
        Some(input) => {
            input.set_sources(db).to(sources.clone());
            input
        }
        None => StubSources::new(db, sources.clone(), Vec::new()),
    };
    let first = render_stub_library(db, text);
    let other = "other : fn(x: Any) -> Any\n";
    stub_input
        .set_sources(db)
        .to(vec![(String::from("pkg"), other.to_owned())]);
    render_stub_library(db, other);
    stub_input.set_sources(db).to(sources.clone());
    assert!(
        render_stub_library(db, text) == first,
        "the stub loads differently after another one\n{text}"
    );
    let pristine = RootDatabase::default();
    AnalysisSettings::new(&pristine, true, false, true);
    ProjectFiles::new(&pristine, Vec::new());
    StubSources::new(&pristine, sources, Vec::new());
    assert!(
        render_stub_library(&pristine, text) == first,
        "the stub loads differently on a database with no history\n{text}"
    );
    let called: Vec<String> = stubs(db)
        .map(|library| {
            let mut names: Vec<String> = library
                .schemes
                .keys()
                .map(|name| name.to_string())
                .collect();
            names.sort_unstable();
            names.truncate(3);
            names
        })
        .unwrap_or_default();
    let probe: String = called
        .iter()
        .enumerate()
        .map(|(index, name)| format!("probe{index} <- `{name}`(1L, x = \"a\")\n"))
        .collect();
    let file = SourceFile::new(db, probe, DocumentKind::Package);
    ProjectFiles::get(db).set_files(db).to(vec![file]);
    render_semantics(db, file);
}

/// Every fact a stub library holds, one sorted line each, asserting each declaration's range
/// lies inside `text`.
fn render_stub_library(db: &RootDatabase, text: &str) -> String {
    let Some(library) = stubs(db) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for (name, schemes) in library.schemes.iter() {
        let mut renderer = TypeRenderer::default();
        let rendered: Vec<String> = schemes
            .iter()
            .map(|scheme| renderer.render_scheme(db, scheme))
            .collect();
        lines.push(format!("{name} : {}", rendered.join(" / ")));
    }
    for (kind, declarations) in [
        ("value", &library.declarations),
        ("type", &library.type_declarations),
    ] {
        for (name, declaration) in declarations.iter() {
            let range = declaration.range;
            assert!(
                usize::from(range.end()) <= text.len(),
                "the {kind} `{name}` is declared at {range:?}, past the end of the stub\n{text}"
            );
            lines.push(format!("{kind} {name} at {range:?}"));
        }
    }
    lines.extend(
        library
            .nominals
            .iter()
            .map(|name| format!("nominal {name}")),
    );
    lines.extend(
        library
            .known_exports
            .iter()
            .map(|name| format!("known {name}")),
    );
    for (name, names) in library.masked.iter() {
        lines.push(format!("masked {name} {names:?}"));
    }
    for (name, names) in library.opaque.iter() {
        lines.push(format!("opaque {name} {names:?}"));
    }
    for (namespace, pure) in library.pure_by_namespace.iter() {
        for (name, names) in pure.iter() {
            lines.push(format!("pure {namespace}::{name} {names:?}"));
        }
    }
    for (namespace, exports) in library.exports_by_namespace.iter() {
        lines.extend(
            exports
                .iter()
                .map(|name| format!("export {namespace}::{name}")),
        );
    }
    for (name, documentation) in library.documentation.iter() {
        lines.push(format!("documentation {name} {documentation:?}"));
    }
    lines.sort_unstable();
    lines.join("\n")
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
