//! Shared test support, including the fuzz invariant battery: no panics, deterministic output
//! across fresh databases, in-bounds ranges, and incremental equivalence with a fresh build.

use {
    crate::{
        ANALYSIS_STACK_SIZE, AnalysisSettings, DataFiles, Db, DocumentKind, ItemKind, ProjectFiles,
        RootDatabase, SourceFile, TreeStore, binds_only_own_name,
        diagnostics::{TypeRenderer, file_diagnostics, nearest_name, strict_diagnostics},
        hir::{ExprId, PatId, Symbol},
        item_check, item_hir, item_naming, item_node, item_syntax, item_tree,
        lints::{LintConfig, LintLevel, NameStyle, lint_file},
        metadata::{
            NativeSymbols, PackageMetadata, call_boundaries, normalized_imports,
            parse_description_collate, parse_description_dependencies, parse_description_depends,
            parse_description_lazy_data, parse_description_package, parse_namespace_chunk,
            parse_namespace_in_chunks, parse_namespace_spec, qualified_names,
        },
        naming::BindingKind,
        stubs::{
            StubSources, install_shipped_stubs, shipped_export_manifests, shipped_stub_sources,
            stub_source_problems, stubs,
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
        iter, mem,
        ops::Bound,
        panic,
        process::Command,
        slice,
        sync::{Arc, LazyLock, Mutex},
        thread,
    },
    syntax::{
        SyntaxKind, SyntaxToken, TextSize,
        testing::{
            Entropy, FAILURES_PER_RUN, SplitMix64, fuzz_seeds, generate_program, generate_project,
            on_seed_thread, replay_command, run_seeds, running_test_name, small_edit, split_files,
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
        let editor_reference = RootDatabase::default();
        for db in [&incremental, &editor_reference] {
            install_shipped_stubs(db);
            AnalysisSettings::new(db, true, false, true);
            ProjectFiles::new(db, Vec::new());
        }
        Workbench {
            incremental,
            inputs: Vec::new(),
            reference: command_database(),
            editor_reference,
            sessions: 0,
        }
    }
}

/// A command's database (`ry check`) with the shipped stubs and an empty project.
pub fn command_database() -> RootDatabase {
    let db = RootDatabase::for_command(false);
    install_shipped_stubs(&db);
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    db
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
            if (seed - first_of_run).is_multiple_of(CHILD_SAMPLE) {
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
pub const CHILD_SAMPLE: u64 = 8;

/// Walks the states and then returns to the first. At every step the incremental database must
/// render each file as the reference does, and the return must restore the first rendering.
/// Every odd session's final state must record the editor facts a database that checks it from
/// new inputs records. Every fourth session also renders each state on several threads at once,
/// which must equal the sequential rendering, and every sixty-fourth compares its first state
/// with a database that has no history at all. Returns the reference rendering of each state.
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
        if step + 1 == states.len() && workbench.sessions % 2 == 1 {
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
                    let mut pristine = command_database();
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
pub fn render_in_parallel(db: &mut RootDatabase, state: &ProjectState) -> Vec<Vec<String>> {
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
                    "seed {seed}, state {state}: a child process renders it differently, so the rendering depends on the interning order or on state the process kept\n{difference}\nreplay with\n{}",
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
    /// The rename was free: the new name may differ in length, first letter and script, and
    /// some occurrences may be backquoted.
    pub free_rename: bool,
    /// The renamed identifier occurs in a finding of the unedited project.
    pub rename_in_finding: bool,
}

/// Checks two edits that change no meaning against the rendering of `state` on the
/// workbench's command database, each where `rng` places it.
///
/// - A blank line or a comment line inserted before a top-level expression moves the findings
///   after it by its length and changes nothing else.
/// - Renaming every occurrence of one to three identifiers the project alone spells changes
///   nothing but those names and the ranges after them ([`compare_renamed`]). Half the
///   renames keep each name's length and character classes; the other half pick free
///   snake_case names, which may be longer or shorter, start with another letter, hold
///   non-ASCII letters, and be backquoted at some occurrences. Each name is drawn, three times
///   in four, from those that occur in a finding. A name near another in spelling is never
///   renamed or picked, because a typo check reports or suggests by that distance.
pub fn check_metamorphic_relations(
    workbench: &mut Workbench,
    state: &ProjectState,
    rng: &mut dyn Entropy,
) -> MetamorphicCoverage {
    let db = &mut workbench.reference;
    let original = render_afresh(db, state);
    let trivia = insert_leading_trivia(state, &original, rng);
    if let Some(trivia) = &trivia {
        let actual = render_afresh(db, &trivia.edited);
        if let Err(difference) = compare_renderings(&trivia.expected, &actual) {
            panic!(
                "{} changes more than the positions after it\n{}\n---- {difference}",
                trivia.description,
                describe_session(slice::from_ref(state)),
            );
        }
    }
    let rename = plan_rename(db, state, &original, rng);
    if let Some(rename) = &rename {
        let renamed = render_afresh(db, &rename.edited);
        if let Err(difference) = compare_renamed(&rename.rename, &original, &renamed) {
            let names: Vec<String> = rename
                .rename
                .names
                .iter()
                .map(|(old, new)| format!("`{old}` to `{new}`"))
                .collect();
            panic!(
                "renaming {} throughout changes more than the names\n{}\n---- renamed project:\n{}\n---- {difference}",
                names.join(", "),
                describe_session(slice::from_ref(state)),
                rename
                    .edited
                    .files
                    .iter()
                    .map(|(_, text, _)| text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n----\n"),
            );
        }
    }
    MetamorphicCoverage {
        leading_trivia: trivia.is_some(),
        rename: rename.is_some(),
        free_rename: rename.as_ref().is_some_and(|rename| rename.free),
        rename_in_finding: rename.as_ref().is_some_and(|rename| rename.in_finding),
    }
}

/// A blank line or comment line inserted into a project, with the renderings it must have.
pub struct TriviaEdit {
    pub edited: ProjectState,
    /// The unedited renderings with every finding after the insertion moved by its length.
    pub expected: Vec<String>,
    pub description: String,
}

/// Inserts a blank line or a comment line before a top-level expression of `state` where `rng`
/// places it, or `None` when no file has such a place. `original` renders `state`.
pub fn insert_leading_trivia(
    state: &ProjectState,
    original: &[String],
    rng: &mut dyn Entropy,
) -> Option<TriviaEdit> {
    if state.files.is_empty() {
        return None;
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
        return None;
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
    Some(TriviaEdit {
        edited,
        expected,
        description: format!("inserting {inserted:?} at byte {offset} of file {target}"),
    })
}

/// Moves every finding of a [`render_semantics`] rendering that lies at or after `offset` by
/// `length` bytes; a finding that spans `offset` grows by it.
fn shift_ranges(rendering: &str, offset: usize, length: usize) -> String {
    map_ranges(
        rendering,
        |start| match start >= offset {
            true => start + length,
            false => start,
        },
        |end| match end > offset {
            true => end + length,
            false => end,
        },
    )
}

/// Rewrites the range of every finding of a [`render_semantics`] rendering, its start through
/// `start` and its end through `end`, and keeps every other line.
fn map_ranges(
    rendering: &str,
    start: impl Fn(usize) -> usize,
    end: impl Fn(usize) -> usize,
) -> String {
    let mut mapped = String::with_capacity(rendering.len());
    for line in rendering.split_inclusive('\n') {
        match finding_range(line) {
            Some((from, to, rest)) => {
                mapped.push_str(&format!("{}..{} {rest}", start(from), end(to)));
            }
            None => mapped.push_str(line),
        }
    }
    mapped
}

/// The range of a finding line of a [`render_semantics`] rendering and the rest of the line,
/// or `None` for a line that is not a finding.
fn finding_range(line: &str) -> Option<(usize, usize, &str)> {
    let (range, rest) = line.split_once(' ')?;
    let (start, end) = range.split_once("..")?;
    let start: usize = start.parse().ok()?;
    let end: usize = end.parse().ok()?;
    rest.starts_with('[').then_some((start, end, rest))
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

/// Every name a shipped stub declares or an export manifest lists.
fn package_names() -> &'static FxHashSet<String> {
    static NAMES: LazyLock<FxHashSet<String>> = LazyLock::new(|| {
        let declared = shipped_stub_sources().into_iter().flat_map(|(_, text)| {
            text.lines()
                .filter_map(|line| Some(line.split_once(" : ")?.0.trim().trim_matches('`')))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        let listed = shipped_export_manifests()
            .into_iter()
            .flat_map(|(_, text)| {
                text.lines()
                    .filter(|line| !line.starts_with('#'))
                    .map(|line| line.trim().to_owned())
                    .collect::<Vec<_>>()
            });
        declared.chain(listed).collect()
    });
    &NAMES
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

/// Identifiers renamed throughout a project, as [`compare_renamed`] reads it.
#[derive(Clone, Debug)]
pub struct Rename {
    /// Each renamed identifier and its new name as a finding spells it, without backquotes.
    /// No name contains another of the list.
    pub names: Vec<(String, String)>,
    /// Per file, the byte range of each renamed occurrence before the rename and its length
    /// after it, in file order.
    pub occurrences: Vec<Vec<(usize, usize, usize)>>,
}

/// Whether the renderings of a project after `rename` say what the renderings before it say,
/// file by file: the multiset of lines of each file must be equal once every finding of the
/// unedited rendering moves to where the rename puts its range and the new name is spelled
/// back as the old one. A finding's place in the rendering is left out of the comparison.
pub fn compare_renamed(
    rename: &Rename,
    original: &[String],
    renamed: &[String],
) -> Result<(), String> {
    if original.len() != renamed.len() || original.len() != rename.occurrences.len() {
        return Err(format!(
            "{} renderings before the rename, {} after it and {} files renamed",
            original.len(),
            renamed.len(),
            rename.occurrences.len()
        ));
    }
    let mut differences = String::new();
    for (file, ((before, after), occurrences)) in original
        .iter()
        .zip(renamed)
        .zip(&rename.occurrences)
        .enumerate()
    {
        let moved = |offset: usize| {
            let growth: i64 = occurrences
                .iter()
                .filter(|(_, end, _)| *end <= offset)
                .map(|(start, end, length)| *length as i64 - (end - start) as i64)
                .sum();
            (offset as i64 + growth) as usize
        };
        let mut expected: Vec<String> = map_ranges(before, moved, moved)
            .lines()
            .map(str::to_owned)
            .collect();
        let mut actual: Vec<String> = after
            .lines()
            .map(|line| {
                rename
                    .names
                    .iter()
                    .fold(line.to_owned(), |line, (old, new)| {
                        line.replace(new.as_str(), old)
                    })
            })
            .collect();
        expected.sort_unstable();
        actual.sort_unstable();
        if expected == actual {
            continue;
        }
        let only = |lines: &[String], others: &[String]| {
            let mut others = others.to_vec();
            let mut only = Vec::new();
            for line in lines {
                match others.iter().position(|other| other == line) {
                    Some(index) => {
                        others.swap_remove(index);
                    }
                    None => only.push(line.clone()),
                }
            }
            only.join("\n")
        };
        differences.push_str(&format!(
            "file {file}, only before the rename (ranges moved):\n{}\nonly after it (new names spelled back):\n{}\n",
            only(&expected, &actual),
            only(&actual, &expected),
        ));
    }
    match differences.is_empty() {
        true => Ok(()),
        false => Err(differences),
    }
}

/// A rename of one to three identifiers throughout a project.
pub struct RenameEdit {
    pub edited: ProjectState,
    pub rename: Rename,
    /// The new names may differ in length, first letter and script, and be backquoted.
    pub free: bool,
    /// A renamed name occurs in a finding of the unedited project.
    pub in_finding: bool,
}

/// Picks one to three identifiers of `state` and new names for them where `rng` decides, or
/// `None` when no identifier or no new name qualifies. `original` renders `state` on `db`,
/// whose library decides which names belong to a package.
pub fn plan_rename(
    db: &RootDatabase,
    state: &ProjectState,
    original: &[String],
    rng: &mut dyn Entropy,
) -> Option<RenameEdit> {
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
        return None;
    }
    let in_findings: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|word| occurs_in_finding(original, word))
        .collect();
    let free = rng.below(2) == 0;
    // A typo check reports a name, or suggests one, by its spelling's distance from the others,
    // so a rename into or out of a near miss changes findings for a reason. A string is also
    // one whole word, because a column check compares `Expr.col("amo$unt")` against `amount`.
    let project_words: BTreeSet<&str> = tokens
        .iter()
        .flat_map(|(kind, text, _)| {
            let whole = match kind {
                SyntaxKind::STRING => Some(text.trim_matches(['"', '\''])),
                SyntaxKind::RAW_STRING => Some(
                    text.trim_start_matches(['r', 'R'])
                        .trim_matches(['"', '\'', '-', '(', ')', '[', ']', '{', '}']),
                ),
                _ => None,
            };
            text.split(|character: char| !is_name_character(character))
                .chain(whole)
        })
        .filter(|word| !word.is_empty())
        .collect();
    let library = stubs(db);
    let outside_words = || {
        library.into_iter().flat_map(|library| {
            library
                .declarations
                .keys()
                .chain(library.schemes.keys())
                .chain(library.known_exports.iter())
                .chain(
                    library
                        .exports_by_namespace
                        .values()
                        .flat_map(|names| names.iter()),
                )
                .map(|name| name.as_str())
        })
    };
    // Each pair is asked alone: a short name gets a suggestion only when exactly one candidate
    // qualifies, so a larger candidate set than the checker's could hide one.
    let near = |typo: &str, target: &str| {
        typo.chars().count().abs_diff(target.chars().count()) <= 2
            && nearest_name(typo, iter::once(target)).is_some()
    };
    // The names after the rename are the project's words without the renamed ones, and the
    // new names.
    let near_miss = |name: &str, names: &[(&str, String)]| {
        let others: Vec<&str> = project_words
            .iter()
            .copied()
            .filter(|word| *word != name && names.iter().all(|(old, _)| old != word))
            .chain(names.iter().map(|(_, new)| new.as_str()))
            .filter(|word| *word != name)
            .collect();
        let outside = stub_words()
            .iter()
            .map(String::as_str)
            .chain(outside_words());
        others
            .iter()
            .any(|word| near(name, word) || near(word, name))
            || outside.into_iter().any(|word| near(name, word))
    };
    // Two renamed names never share a rendered line, where the order of two new names could
    // differ from the order of the old ones.
    let share_a_line = |first: &str, second: &str| {
        original
            .iter()
            .flat_map(|rendering| rendering.lines())
            .any(|line| {
                let mut words = line.split(|character: char| !is_name_character(character));
                let words: Vec<&str> = words.by_ref().collect();
                words.contains(&first) && words.contains(&second)
            })
    };
    let mut names: Vec<(&str, String)> = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let pool = match !in_findings.is_empty() && rng.below(4) != 0 {
            true => &in_findings,
            false => &candidates,
        };
        let old = pool[rng.below(pool.len())];
        if names
            .iter()
            .any(|(chosen, _)| *chosen == old || share_a_line(chosen, old))
            || near_miss(old, &names)
        {
            continue;
        }
        let new = (0..8)
            .filter_map(|_| fresh_name(db, old, &tokens, original, free, rng))
            .find(|new| {
                names.iter().all(|(_, chosen)| {
                    !chosen.contains(new.as_str()) && !new.contains(chosen.as_str())
                }) && !near_miss(new, &names)
            });
        if let Some(new) = new {
            names.push((old, new));
        }
    }
    if names.is_empty() {
        return None;
    }
    let backquote_share = match free && rng.below(3) == 0 {
        true => 2,
        false => 0,
    };
    let (edited, rename) = [backquote_share, 0]
        .into_iter()
        .map(|share| apply_rename(state, &files, &names, share, rng))
        .find(|(edited, _)| {
            edited
                .files
                .iter()
                .zip(&files)
                .all(|((_, text, _), tokens)| same_token_kinds(text, tokens))
        })?;
    let in_finding = names.iter().any(|(old, _)| in_findings.contains(old));
    Some(RenameEdit {
        edited,
        rename,
        free,
        in_finding,
    })
}

/// `state` with every identifier token spelled as an old name of `names` respelled as its new
/// name, each occurrence backquoted with chance one in `backquote_share` (never for 0), and the
/// [`Rename`] that describes the edit.
fn apply_rename(
    state: &ProjectState,
    files: &[Vec<(SyntaxKind, String, bool)>],
    names: &[(&str, String)],
    backquote_share: usize,
    rng: &mut dyn Entropy,
) -> (ProjectState, Rename) {
    let mut edited = state.clone();
    let mut occurrences = Vec::with_capacity(files.len());
    for ((_, text, _), tokens) in edited.files.iter_mut().zip(files) {
        let mut offset = 0;
        let mut renamed = String::with_capacity(text.len());
        let mut file_occurrences = Vec::new();
        for (kind, token, _) in tokens {
            let new = names
                .iter()
                .find(|(old, _)| *kind == SyntaxKind::IDENT && token == old)
                .map(|(_, new)| new);
            match new {
                Some(new) => {
                    let spelling = match backquote_share != 0 && rng.below(backquote_share) == 0 {
                        true => format!("`{new}`"),
                        false => new.clone(),
                    };
                    file_occurrences.push((offset, offset + token.len(), spelling.len()));
                    renamed.push_str(&spelling);
                }
                None => renamed.push_str(token),
            }
            offset += token.len();
        }
        *text = renamed;
        occurrences.push(file_occurrences);
    }
    let rename = Rename {
        names: names
            .iter()
            .map(|(old, new)| ((*old).to_owned(), new.clone()))
            .collect(),
        occurrences,
    };
    (edited, rename)
}

/// Whether `text` lexes to tokens of the kinds `tokens` lists, so a rename left the shape of
/// the file alone.
fn same_token_kinds(text: &str, tokens: &[(SyntaxKind, String, bool)]) -> bool {
    syntax::parse(text)
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| token.kind())
        .eq(tokens.iter().map(|(kind, _, _)| *kind))
}

/// Whether a character can be part of an R name.
fn is_name_character(character: char) -> bool {
    character.is_alphanumeric() || character == '.' || character == '_'
}

/// Whether `word` is a whole word of a finding line of a rendering.
fn occurs_in_finding(renderings: &[String], word: &str) -> bool {
    let is_word =
        |character: char| character.is_ascii_alphanumeric() || character == '.' || character == '_';
    renderings
        .iter()
        .flat_map(|rendering| rendering.lines())
        .filter_map(finding_range)
        .any(|(_, _, rest)| {
            rest.split(|character: char| !is_word(character))
                .any(|part| part == word)
        })
}

/// Whether every occurrence of `word` is an identifier token of the project's own: no string,
/// comment or other token contains it, no identifier is a prefix R's partial matching could
/// complete to it, and no shipped stub or export manifest names it, whether or not the project
/// attaches the package. A package the stubs know only by name leaves a read of its export
/// quiet, and the checker models some exports by name (data.table's `setnames`), so a rename
/// of such a read changes which function a call reaches.
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
        && !package_names().contains(word)
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

/// A new name for `old` that no token, stub or rendering contains, that no identifier of the
/// project is a prefix of, and that sorts as `old` does against every word of a rendered line
/// that holds `old`, or `None` when a few draws find none. Records list their fields by name
/// and symbols order by spelling, so a name that moved past another would reorder what a
/// rendering prints. Unless `free`, the name keeps `old`'s length and character classes; a free name is
/// any snake_case name of one to ten characters, some of them non-ASCII letters.
fn fresh_name<'a>(
    db: &RootDatabase,
    old: &str,
    tokens: &[&(SyntaxKind, String, bool)],
    original: &'a [String],
    free: bool,
    rng: &mut dyn Entropy,
) -> Option<String> {
    const LETTERS: &[char] = &['\u{e9}', '\u{f8}', '\u{df}', '\u{3bb}'];
    let is_word =
        |character: char| character.is_ascii_alphanumeric() || character == '.' || character == '_';
    let split_words = |text: &'a str| {
        text.split(move |character: char| !is_word(character))
            .filter(|word| !word.is_empty())
    };
    // Only a line that holds the name can list it among others, and a finding's prose never
    // sorts, so the order that matters is among the words of the code a finding quotes and of
    // a type line.
    let words: BTreeSet<&str> = original
        .iter()
        .flat_map(|rendering| rendering.lines())
        .filter(|line| split_words(line).any(|word| word == old))
        .flat_map(|line| match finding_range(line) {
            Some((_, _, message)) => message.split('`').skip(1).step_by(2).collect(),
            None => vec![line],
        })
        .flat_map(split_words)
        .collect();
    let below = words.range::<&str, _>(..old).next_back().copied();
    let above = words
        .range::<&str, _>((Bound::Excluded(old), Bound::Unbounded))
        .next()
        .copied();
    let letter = |rng: &mut dyn Entropy, digits: bool| match rng.below(12) {
        0 => LETTERS[rng.below(LETTERS.len())],
        1 if digits => (b'0' + rng.below(10) as u8) as char,
        2 if digits => '_',
        _ => (b'a' + rng.below(26) as u8) as char,
    };
    for _ in 0..64 {
        let new: String = match free {
            true => (0..1 + rng.below(10))
                .map(|index| letter(rng, index > 0))
                .collect(),
            false => {
                let kept = rng.below(old.len());
                old.char_indices()
                    .map(|(index, character)| match (index < kept, character) {
                        (true, _) => character,
                        (false, 'a'..='z') => (b'a' + rng.below(26) as u8) as char,
                        (false, '0'..='9') => (b'0' + rng.below(10) as u8) as char,
                        (false, other) => other,
                    })
                    .collect()
            }
        };
        let in_place = below.is_none_or(|below| below < new.as_str())
            && above.is_none_or(|above| new.as_str() < above);
        let lexes_as_name = syntax::parse(&new)
            .syntax_node()
            .first_token()
            .is_some_and(|token| token.kind() == SyntaxKind::IDENT && token.text() == new);
        let reshaped = !free || new.chars().next() != old.chars().next();
        let unused = new != old
            && reshaped
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

/// Runs [`check_stub_source`] on the stub source `generate` makes from each seed of
/// [`fuzz_seeds`], on a database that stays warm across seeds, each seed on a thread named after
/// the test and the seed. One child process then loads the sampled seeds' sources again
/// ([`compare_with_child_process`]), each first on a database of its own: a loader that keeps
/// state outside the database, such as a process-wide cache, gives every database of this
/// process the same stale answer, but not a process that never loaded the other sources.
pub fn for_each_stub_source(default: usize, mut generate: impl FnMut(u64) -> String + Send) {
    if let Some(seeds) = interning_child_seeds() {
        let mut out = io::stdout().lock();
        for &seed in seeds.iter().rev() {
            let text = generate(seed);
            for (index, source) in [text.clone(), stub_variant(&text)].iter().enumerate().rev() {
                let db = stub_database(source);
                let rendering = render_stub_library(&db);
                write!(
                    out,
                    "\n{CHILD_FRAME} {seed} 0 {index} {}\n{rendering}",
                    rendering.len()
                )
                .expect("the child writes its renderings to stdout");
            }
        }
        out.flush().expect("the child flushes its renderings");
        return;
    }
    let mut db = RootDatabase::default();
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    let mut samples = Vec::new();
    let first_of_run = fuzz_seeds(default).start;
    run_seeds(default, |seed| {
        let text = generate(seed);
        match on_seed_thread(seed, || check_stub_source(&mut db, &text)) {
            Ok(renderings) => {
                if (seed - first_of_run).is_multiple_of(CHILD_SAMPLE) {
                    samples.push((seed, vec![renderings]));
                }
                true
            }
            Err(_) => false,
        }
    });
    compare_with_child_process(&samples);
}

/// Loads `text` as a project's only stub source on `db`, a warm database whose stub input this
/// call owns, and checks the loader. It never panics; every problem names a line that holds
/// what the problem quotes from it ([`misplaced_stub_problems`]); every declaration's range
/// covers the name it declares ([`misplaced_declarations`]); and after a variant of the source
/// with other documentation and types loads, loading the source again gives the first library,
/// documentation included ([`compare_renderings`]). A few declared names are then called from
/// a project file, which must check without a panic. Returns the renderings of the source and
/// of its variant on the warm database.
pub fn check_stub_source(db: &mut RootDatabase, text: &str) -> Vec<String> {
    let problems: Vec<(usize, String)> = stub_source_problems(db, text)
        .into_iter()
        .map(|problem| (problem.line, problem.message))
        .collect();
    if let Err(misplaced) = misplaced_stub_problems(text, &problems) {
        panic!("{misplaced}\n---- stub:\n{text}");
    }
    let variant = stub_variant(text);
    let first = load_stub_source(db, text);
    let other = load_stub_source(db, &variant);
    let again = load_stub_source(db, text);
    if let Err(difference) = compare_renderings(slice::from_ref(&first), slice::from_ref(&again)) {
        panic!(
            "the stub loads differently after a variant of it\n{difference}\n---- stub:\n{text}\n---- variant:\n{variant}"
        );
    }
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
    vec![first, other]
}

/// `text` with every documentation line reworded and every `integer` read as `double`, so a
/// load of it changes the documentation and the types of the names `text` declares.
pub fn stub_variant(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| match line.trim_start().starts_with("#'") {
            true => line.replacen("#'", "#' Reworded:", 1),
            false => line.replace("integer", "double"),
        })
        .collect()
}

/// [`render_stub_source`], with its declaration ranges checked.
fn load_stub_source(db: &mut RootDatabase, text: &str) -> String {
    let rendering = render_stub_source(db, text);
    if let Err(misplaced) = misplaced_declarations(text, &rendering) {
        panic!("{misplaced}\n---- stub:\n{text}");
    }
    rendering
}

/// Installs `text` as the only stub source on `db` and renders the library: every fact it
/// holds, one sorted line each.
pub fn render_stub_source(db: &mut RootDatabase, text: &str) -> String {
    let sources = vec![(String::from("pkg"), text.to_owned())];
    match StubSources::try_get(db) {
        Some(input) => {
            input.set_sources(db).to(sources);
        }
        None => {
            StubSources::new(db, sources, Vec::new());
        }
    }
    render_stub_library(db)
}

/// A database that has loaded no stub source but `text`.
fn stub_database(text: &str) -> RootDatabase {
    let db = RootDatabase::default();
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    StubSources::new(
        &db,
        vec![(String::from("pkg"), text.to_owned())],
        Vec::new(),
    );
    db
}

/// Quotes in a stub problem that name the grammar, not text of the line.
const STUB_GRAMMAR_QUOTES: &[&str] = &[
    "@alias NAME<T> {TYPE}",
    "@type NAME {TYPE}",
    "@type NAME<T>",
    "@type",
    "@alias",
    "name : TYPE",
    ":",
    "@masked",
    "...",
];

/// `Ok` when every problem sits on a line of `text` that has content, and that line holds each
/// text the problem quotes from it, unless the line holds a backtick. Lines are counted here from the bytes: a line ends at `\n`,
/// a `\r` before it belongs to the break, and a final break starts no line.
pub fn misplaced_stub_problems(text: &str, problems: &[(usize, String)]) -> Result<(), String> {
    let lines: Vec<&str> = text
        .split_inclusive('\n')
        .map(|line| line.trim_end_matches('\n').trim_end_matches('\r'))
        .collect();
    for (line, message) in problems {
        let Some(&content) = lines.get(*line) else {
            return Err(format!(
                "a problem on line {line} of a stub of {} lines: {message}",
                lines.len()
            ));
        };
        let code = content.split_once('#').map_or(content, |(code, _)| code);
        if content.trim().is_empty() || content.trim_start().starts_with('#') {
            return Err(format!(
                "a problem on line {line}, which holds no declaration ({content:?}): {message}"
            ));
        }
        // A backtick in the line would end a quote early, so such a line is held only to
        // having content.
        let quotes = message
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|_| !content.contains('`'));
        for quote in quotes {
            let quoted = quote.strip_suffix(" :").unwrap_or(quote);
            if !STUB_GRAMMAR_QUOTES.contains(&quote) && !content.contains(quoted) {
                return Err(format!(
                    "a problem on line {line} quotes `{quoted}`, which the line ({code:?}) does not hold: {message}"
                ));
            }
        }
    }
    Ok(())
}

/// `Ok` when every declaration a library rendering lists (`value NAME at START..END`) covers
/// exactly the bytes of `NAME` in `text`, as a whole token.
pub fn misplaced_declarations(text: &str, rendering: &str) -> Result<(), String> {
    for line in rendering.lines() {
        let Some(rest) = line
            .strip_prefix("value ")
            .or_else(|| line.strip_prefix("type "))
        else {
            continue;
        };
        let Some((name, range)) = rest.rsplit_once(" at ") else {
            continue;
        };
        let bounds = range.split_once("..").and_then(|(start, end)| {
            Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
        });
        let covered = bounds.and_then(|(start, end)| text.get(start..end));
        if covered != Some(name) {
            return Err(format!(
                "`{name}` is declared at {range}, which covers {covered:?}"
            ));
        }
        // The covered bytes must also be a whole token: a short name occurs inside the
        // `@type` keyword too.
        let is_name_character =
            |character: char| character.is_alphanumeric() || "@._".contains(character);
        let before = bounds.and_then(|(start, _)| text.get(..start)?.chars().next_back());
        let after = bounds.and_then(|(_, end)| text.get(end..)?.chars().next());
        if before.is_some_and(is_name_character) || after.is_some_and(is_name_character) {
            return Err(format!(
                "`{name}` is declared at {range}, inside a longer token ({before:?} before it, {after:?} after it)"
            ));
        }
    }
    Ok(())
}

/// Every fact a stub library holds, one sorted line each.
fn render_stub_library(db: &RootDatabase) -> String {
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
            lines.push(format!(
                "{kind} {name} at {}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            ));
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

/// Derives a package's metadata from `NAMESPACE` and `DESCRIPTION` text and checks it on
/// `db`, a warm database whose metadata and project this call owns. No parser panics. The
/// `NAMESPACE` facts are the same in one piece as split at [`call_boundaries`] into two to four
/// chunks parsed on threads, which is how a large file parses. A project file that calls the
/// imported, exported and native names checks without a panic, and renders alike before and
/// after the database holds other metadata. Returns the derived facts, rendered.
pub fn check_package_metadata(db: &mut RootDatabase, namespace: &str, description: &str) -> String {
    let whole = parse_namespace_chunk(namespace, TextSize::from(0));
    let rendered = format!("{whole:#?}");
    for parts in 2..=4 {
        let chunks = call_boundaries(namespace, parts);
        let chunked = format!("{:#?}", parse_namespace_in_chunks(namespace, &chunks));
        if chunked != rendered {
            panic!(
                "NAMESPACE parses differently in {} chunks at {chunks:?}\n---- whole:\n{rendered}\n---- chunked:\n{chunked}\n---- NAMESPACE:\n{namespace}",
                chunks.len()
            );
        }
    }
    let spec = parse_namespace_spec(namespace);
    let metadata = DerivedMetadata {
        imports: normalized_imports(&whole.imports),
        dependencies: parse_description_dependencies(description),
        attached: parse_description_depends(description).into_iter().collect(),
        package: parse_description_package(description),
        native_symbols: whole.native_symbols.clone(),
        qualified_names: qualified_names(Some(&whole), Some(Vec::new())),
    };
    let facts = format!(
        "{metadata:#?}\n{spec:#?}\nlazy data {}\ncollate {:?}",
        parse_description_lazy_data(description),
        parse_description_collate(description),
    );
    let called: BTreeSet<&str> = metadata
        .imports
        .iter()
        .filter_map(|(_, name)| name.as_deref())
        .chain(metadata.native_symbols.names.iter().map(String::as_str))
        .chain(whole.exports.iter().map(|(name, _)| name.as_str()))
        .filter(|name| !name.contains('`'))
        .take(8)
        .collect();
    let probe: String = called
        .iter()
        .enumerate()
        .map(|(index, name)| format!("probe{index} <- `{name}`(1L)\n"))
        .collect();
    let file = SourceFile::new(db, probe, DocumentKind::Package);
    ProjectFiles::get(db).set_files(db).to(vec![file]);
    set_package_metadata(db, &metadata);
    let first = render_semantics(db, file);
    set_package_metadata(db, &DerivedMetadata::default());
    render_semantics(db, file);
    set_package_metadata(db, &metadata);
    let again = render_semantics(db, file);
    if first != again {
        panic!(
            "a project checks differently after other package metadata\n---- first:\n{first}---- again:\n{again}---- NAMESPACE:\n{namespace}\n---- DESCRIPTION:\n{description}"
        );
    }
    facts
}

/// The values of a [`PackageMetadata`] input, as a host derives them.
#[derive(Debug, Default)]
struct DerivedMetadata {
    imports: Vec<(String, Option<String>)>,
    dependencies: BTreeSet<String>,
    attached: BTreeSet<String>,
    package: Option<String>,
    native_symbols: NativeSymbols,
    qualified_names: Option<BTreeSet<String>>,
}

fn set_package_metadata(db: &mut RootDatabase, metadata: &DerivedMetadata) {
    let Some(input) = PackageMetadata::try_get(db) else {
        PackageMetadata::new(
            db,
            metadata.imports.clone(),
            metadata.dependencies.clone(),
            metadata.attached.clone(),
            metadata.package.clone(),
            metadata.native_symbols.clone(),
            metadata.qualified_names.clone(),
        );
        return;
    };
    input.set_imports(db).to(metadata.imports.clone());
    input.set_dependencies(db).to(metadata.dependencies.clone());
    input.set_attached(db).to(metadata.attached.clone());
    input.set_package(db).to(metadata.package.clone());
    input
        .set_native_symbols(db)
        .to(metadata.native_symbols.clone());
    input
        .set_qualified_names(db)
        .to(metadata.qualified_names.clone());
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
