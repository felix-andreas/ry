//! Fixture format: `#==== <group>` opens a group, `#---- <case>` a case whose
//! source runs to `#++++`, then the expectation runs to the next directive.
//!
//! The fuzz helpers live here too, so every crate's harness draws from one
//! generator and one set of mutation operators.

use {
    crate::{NodeOrToken, SyntaxNode},
    std::{
        collections::HashSet,
        env, fs,
        ops::Range,
        panic::{self, AssertUnwindSafe},
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    },
};

static INVARIANTS: AtomicBool = AtomicBool::new(false);

/// Switches on the expensive cross-checks that production skips: an incremental result is
/// recomputed from scratch and must be equal. Simulators and fuzzers turn it on for the whole
/// process, so a divergence panics at the step that caused it, with the inputs that reproduce it.
pub fn enable_invariants() {
    INVARIANTS.store(true, Ordering::Relaxed);
}

pub fn invariants_enabled() -> bool {
    INVARIANTS.load(Ordering::Relaxed)
}

/// A `cargo build` in the profile of the running test, for a test that needs another package's
/// binary (`libR.so`, `ry-rcmd`) beside the binaries under test in `profile_directory`. That
/// directory names the profile, except that `target/debug` is the `dev` profile.
///
/// The command drops the variables cargo sets for the test itself (`CARGO_PKG_NAME`,
/// `CARGO_MANIFEST_DIR`, `OUT_DIR` and the like). Build scripts such as ring's declare
/// `rerun-if-env-changed` on them, and cargo compares those against its own environment, so an
/// inherited value would rebuild most of the dependency tree here and again in the next plain
/// `cargo build`.
pub fn cargo_build(profile_directory: &Path) -> Command {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    let profile = profile_directory.file_name().and_then(|name| name.to_str());
    command.args([
        "build",
        "-q",
        "--profile",
        profile.filter(|&name| name != "debug").unwrap_or("dev"),
    ]);
    for (name, _) in env::vars_os() {
        let set_for_the_test = name.to_str().is_some_and(|name| {
            matches!(
                name,
                "OUT_DIR" | "CARGO_CRATE_NAME" | "CARGO_PRIMARY_PACKAGE" | "CARGO_TARGET_TMPDIR"
            ) || ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        });
        if set_for_the_test {
            command.env_remove(name);
        }
    }
    command
}

/// Runs every case of the suite under `suite_dir` through `render` and compares the output with
/// its expectation. The cases render in parallel through [`in_parallel`], and the report lists the
/// failures in file order, each with the command that reruns it.
///
/// `FIXTURE_FILTER` keeps the cases whose `group__case` id contains it, so `group__` runs a whole
/// group and a full id runs one case (and any case whose id extends it).
pub fn run_fixture_suite(suite_dir: &Path, render: &(dyn Fn(&str) -> String + Sync)) {
    let files = parse_fixture_files(suite_dir);
    assert!(
        !files.is_empty(),
        "no fixture files found under {}",
        suite_dir.display()
    );

    let filter = env::var("FIXTURE_FILTER")
        .ok()
        .filter(|filter| !filter.is_empty());
    let bless = env_var("BLESS").is_some_and(|value| value == "1");
    let mut seen_ids = HashSet::new();
    for case in files.iter().flat_map(|file| &file.cases) {
        assert!(
            seen_ids.insert(case.id.as_str()),
            "duplicate fixture id `{}` (fixture ids must be unique across the suite)",
            case.id
        );
    }

    let selected: Vec<(usize, &FixtureCase)> = files
        .iter()
        .enumerate()
        .flat_map(|(index, file)| file.cases.iter().map(move |case| (index, case)))
        .filter(|(_, case)| {
            filter
                .as_deref()
                .is_none_or(|filter| case.id.contains(filter))
        })
        .collect();
    let outcomes = in_parallel(&selected, |(_, case)| render(&case.source));
    let matched = !selected.is_empty();

    let mut failures = Vec::new();
    let mut blessed_edits: Vec<Vec<(Range<usize>, String)>> = vec![Vec::new(); files.len()];
    for ((file_index, case), outcome) in selected.iter().zip(outcomes) {
        let file = &files[*file_index];
        let rendered = match outcome {
            Ok(rendered) => normalize(&rendered),
            Err(message) => {
                failures.push(format!(
                    "fixture `{}` ({}) panicked: {message}\n    rerun: {}\n",
                    case.id,
                    file.path.display(),
                    fixture_replay_command(&case.id)
                ));
                continue;
            }
        };
        let expected = normalize(&case.expected);
        if rendered == expected {
            continue;
        }
        if bless {
            blessed_edits[*file_index].push((case.expected_span.clone(), rendered));
        } else {
            failures.push(format!(
                "fixture `{}` ({})\n---- expected ----\n{expected}\n---- actual ----\n{rendered}\n    rerun: {}\n",
                case.id,
                file.path.display(),
                fixture_replay_command(&case.id)
            ));
        }
    }

    for (file, edits) in files.into_iter().zip(blessed_edits) {
        if edits.is_empty() {
            continue;
        }
        let mut new_text = file.text;
        for (span, rendered) in edits.into_iter().rev() {
            let separator = if span.end == new_text.len() {
                "\n"
            } else {
                "\n\n"
            };
            new_text.replace_range(span, &format!("{rendered}{separator}"));
        }
        fs::write(&file.path, new_text)
            .unwrap_or_else(|error| panic!("cannot bless {}: {error}", file.path.display()));
    }

    if let Some(filter) = &filter {
        assert!(
            matched || sibling_suite_holds(suite_dir, filter),
            "FIXTURE_FILTER=`{filter}` matches no fixture case in any suite. Check the id.\n\
             A filter that matches nothing would otherwise run zero cases and report a pass."
        );
    }

    assert!(
        failures.is_empty(),
        "{} fixture failure(s):\n\n{}\n(the run used {}: a case rendered after others on the \
         same worker, so replaying a whole-suite failure needs the same count; set RY_BLESS=1 to \
         accept the new output)",
        failures.len(),
        failures.join("\n"),
        workers_replay()
    );
}

/// `work` applied to every item, returned in the order of `items` with a panic caught per item
/// and returned as its message, so one panicking item neither hides which item it was nor
/// discards the other results.
///
/// The items are dealt out statically: worker `w` of `n` takes items `w`, `w + n`, `w + 2n` and
/// so on, in that order. Work that keeps per-thread state between items (the fixture renderers
/// reuse one analysis database per thread) therefore sees the same history on every run with the
/// same worker count, which [`test_workers`] reads and [`workers_replay`] prints.
pub fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    work: impl Fn(&T) -> R + Sync,
) -> Vec<Result<R, String>> {
    let workers = test_workers().min(items.len().max(1));
    let work = &work;
    let mut results: Vec<(usize, Result<R, String>)> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..items.len())
                        .step_by(workers)
                        .map(|position| {
                            let outcome =
                                panic::catch_unwind(AssertUnwindSafe(|| work(&items[position])))
                                    .map_err(|payload| panic_message(payload.as_ref()));
                            (position, outcome)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| panic::resume_unwind(payload))
            })
            .collect()
    });
    results.sort_by_key(|(position, _)| *position);
    results.into_iter().map(|(_, result)| result).collect()
}

/// The worker count [`in_parallel`] uses: `RY_TEST_WORKERS` when set to a positive number, and
/// one per core otherwise.
pub fn test_workers() -> usize {
    env_var("TEST_WORKERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, |count| count.get()))
}

/// The variable assignment that replays a run with the partition this run used.
pub fn workers_replay() -> String {
    format!("RY_TEST_WORKERS={}", test_workers())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

/// `FIXTURE_FILTER=<id> cargo test -p <package> --test <binary> <test> -- --exact`.
fn fixture_replay_command(id: &str) -> String {
    format!("FIXTURE_FILTER={id} {}", cargo_test_command())
}

pub fn fixture_case_sources() -> Vec<(String, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits under crates/");
    let mut sources = Vec::new();
    for suite in FIXTURE_SUITES {
        for file in parse_fixture_files(&crates.join(suite)) {
            for case in file.cases {
                sources.push((format!("{suite}::{}", case.id), case.source));
            }
        }
    }
    sources
}

/// A suite missing from this list is invisible to the invariant batteries.
const FIXTURE_SUITES: [&str; 14] = [
    "format/tests/format",
    "format/tests/format-range",
    "ide/tests/ide",
    "semantics/tests/lints",
    "semantics/tests/lints-style",
    "semantics/tests/lowering",
    "semantics/tests/naming",
    "semantics/tests/typing",
    "semantics/tests/typing-imports",
    "semantics/tests/typing-scripts",
    "semantics/tests/typing-strict",
    "syntax/tests/errors",
    "syntax/tests/syntax",
    "syntax/tests/tsr",
];

/// Inputs without expectations, for the invariant batteries.
pub fn program_corpus_sources() -> Vec<(String, String)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs");
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "corpus")
        })
        .collect();
    files.sort();
    let mut sources = Vec::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut current: Option<(String, String)> = None;
        for line in text.lines() {
            if let Some(id) = line.strip_prefix("#---- ") {
                sources.extend(current.replace((id.to_owned(), String::new())));
            } else if let Some((_, body)) = &mut current {
                body.push_str(line);
                body.push('\n');
            }
        }
        sources.extend(current);
    }
    sources
}

/// Falls back to the `ROUGHLY_` prefix, which existing scripts still set.
pub fn env_var(suffix: &str) -> Option<String> {
    env::var(format!("RY_{suffix}"))
        .or_else(|_| env::var(format!("ROUGHLY_{suffix}")))
        .ok()
}

/// `None` for a single-file case (no `#~~~~ <path>` lines).
pub fn split_files(source: &str) -> Option<Vec<(String, String)>> {
    if !source.lines().any(|line| line.starts_with("#~~~~ ")) {
        return None;
    }
    let mut files: Vec<(String, String)> = Vec::new();
    for line in source.split_inclusive('\n') {
        if let Some(path) = line.strip_prefix("#~~~~ ") {
            files.push((path.trim().to_owned(), String::new()));
        } else if let Some((_, text)) = files.last_mut() {
            text.push_str(line);
        }
    }
    Some(files)
}

pub fn check_parse_invariants(input: &str) {
    let parse = crate::parse(input);
    let reprinted = parse.text();
    assert_eq!(
        reprinted, input,
        "lossless round-trip violated for input {input:?}"
    );

    let (tokens, _errors) = crate::lex(input);
    let total: u32 = tokens.iter().map(|token| u32::from(token.len)).sum();
    assert_eq!(
        total as usize,
        input.len(),
        "token cover violated for input {input:?}"
    );
    for token in &tokens {
        assert!(
            u32::from(token.len) > 0,
            "zero-length token for input {input:?}"
        );
    }

    check_geometry(&parse.syntax_node(), input);
    check_navigation(&parse.syntax_node(), input);
    // Item trees are windows with a nonzero first index and base offset, which the root never
    // exercises; a window of a window covers nested rebasing.
    let tree = parse.tree();
    for node in tree.root().descendants().skip(1).step_by(7).take(4) {
        let window = Arc::new(tree.subtree(node));
        let window_root = SyntaxNode::new_root(window.clone());
        check_navigation(&window_root, window.text());
        if let Some(inner) = window.root().descendants().nth(1) {
            let nested = Arc::new(window.subtree(inner));
            check_navigation(&SyntaxNode::new_root(nested.clone()), nested.text());
            let detached = nested.detach();
            assert!(
                detached == *nested && detached.same_parents(&nested),
                "a detached window differs from its source for input {input:?}"
            );
        }
    }

    // One mistake must not fan out into a storm of errors.
    assert!(
        parse.errors().len() <= 2 * tokens.len() + 16,
        "error cascade: {} errors from {} tokens for input {input:?}",
        parse.errors().len(),
        tokens.len()
    );
    for error in parse.errors() {
        assert!(
            !error.message.is_empty(),
            "empty error message for input {input:?}"
        );
        assert!(
            u32::from(error.range.end()) as usize <= input.len(),
            "error range out of bounds for input {input:?}"
        );
    }

    // Whatever the tree marks as broken, a finding names: otherwise a consumer that trusts an
    // error-free parse reads a tree with holes in it.
    if parse.errors().is_empty() {
        let broken = parse
            .syntax_node()
            .descendants_with_tokens()
            .find(|element| match element {
                NodeOrToken::Node(node) => node.kind() == crate::SyntaxKind::ERROR,
                NodeOrToken::Token(token) => token.kind() == crate::SyntaxKind::ERROR_TOKEN,
            });
        assert!(
            broken.is_none(),
            "the tree has an error node no finding reports for input {input:?}"
        );
    }

    // Erasure must turn a clean program into plain R: nothing inline survives, the
    // result is clean too, and erasing again changes nothing. A broken program has
    // no meaning to erase, and neither has one with a type definition below the top level,
    // which parses and is the checker's `annotation` error.
    let nested_definition = parse.syntax_node().descendants().any(|node| {
        node.kind() == crate::SyntaxKind::TYPE_DEFINITION
            && node
                .parent()
                .is_some_and(|parent| parent.kind() != crate::SyntaxKind::SOURCE_FILE)
    });
    if parse.errors().is_empty() && !nested_definition {
        let erased = crate::erase_inline_annotations(&parse);
        let erased_parse = crate::parse(&erased);
        assert!(
            erased_parse.errors().is_empty(),
            "erasing a clean program introduced {:?} for input {input:?}: {erased:?}",
            erased_parse.errors()
        );
        assert!(
            !erased_parse
                .syntax_node()
                .descendants()
                .any(|node| crate::is_inline_annotation(&node)),
            "an inline annotation survived erasure for input {input:?}: {erased:?}"
        );
        assert_eq!(
            crate::erase_inline_annotations(&erased_parse),
            erased,
            "erasure is not idempotent for input {input:?}"
        );
    }

    let again = crate::parse(input);
    assert_eq!(
        parse.tree(),
        again.tree(),
        "non-deterministic parse for input {input:?}"
    );
    assert_eq!(
        parse.errors(),
        again.errors(),
        "non-deterministic errors for input {input:?}"
    );
}

/// A deterministic generator, so a fuzz failure reproduces from its seed.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound.max(1) as u64) as usize
    }

    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.next_u64() % denominator < numerator
    }
}

/// The choices a structured generator makes. The in-tree harnesses draw them from a seeded
/// [`SplitMix64`]; a coverage-guided target draws them from its input through [`ByteStream`],
/// so one generator serves both and libFuzzer's byte mutations become structural mutations.
pub trait Entropy {
    fn next_u64(&mut self) -> u64;

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound.max(1) as u64) as usize
    }

    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator as usize) < numerator as usize
    }
}

impl Entropy for SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        SplitMix64::next_u64(self)
    }
}

/// Fuzzer input read as a stream of choices. A small choice consumes one byte, so a byte
/// mutation changes one decision. An exhausted stream answers zero forever, which every
/// generator must treat as "stop growing".
pub struct ByteStream<'a> {
    bytes: &'a [u8],
}

impl<'a> ByteStream<'a> {
    pub fn new(bytes: &'a [u8]) -> ByteStream<'a> {
        ByteStream { bytes }
    }

    fn take(&mut self, count: usize) -> u64 {
        let (head, rest) = self.bytes.split_at(count.min(self.bytes.len()));
        self.bytes = rest;
        head.iter()
            .rev()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    }
}

impl Entropy for ByteStream<'_> {
    fn next_u64(&mut self) -> u64 {
        self.take(8)
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = bound.max(1) as u64;
        let value = if bound <= 1 << 8 {
            self.take(1)
        } else if bound <= 1 << 16 {
            self.take(2)
        } else {
            self.take(8)
        };
        (value % bound) as usize
    }
}

/// Runs `case` once per seed and panics at the end with the command that replays each seed
/// that failed. A run goes on past a failure, up to [`FAILURES_PER_RUN`] of them, so one long
/// run reports every distinct bug in its window instead of the first. The seeds are
/// [`fuzz_seeds`]: `0..default` normally, only `FUZZ_SEED` for a replay, and a window from it
/// when `FUZZ_ITERS` or `FUZZ_DEPTH` is set as well. Each seed is the harness's whole input:
/// the case builds its generator with `SplitMix64(seed)` and must draw nothing else, so the
/// replay reproduces the failure byte for byte.
pub fn for_each_seed(default: usize, mut case: impl FnMut(u64)) {
    run_seeds(default, |seed| {
        panic::catch_unwind(AssertUnwindSafe(|| case(seed))).is_ok()
    });
}

/// [`for_each_seed`] with each seed on its own thread, named after the test and the seed. A
/// stack overflow aborts the process before any replay can print, and its one line of output
/// names the overflowing thread, so the name carries the replay. The thread gets the default
/// stack, as libtest's test thread does, so a case overflows where it would there.
pub fn for_each_seed_on_named_threads(default: usize, mut case: impl FnMut(u64) + Send) {
    run_seeds(default, |seed| on_seed_thread(seed, || case(seed)).is_ok());
}

/// Runs `case` on a thread named after the running test and `seed`, as
/// [`for_each_seed_on_named_threads`] does, for a harness with its own seed loop. A panic comes
/// back as the `Err` of [`thread::Result`].
pub fn on_seed_thread<R: Send>(seed: u64, case: impl FnOnce() -> R + Send) -> thread::Result<R> {
    let test = running_test_name();
    thread::scope(|scope| {
        match thread::Builder::new()
            .name(format!("{test}{SEED_THREAD_MARKER}{seed}"))
            .spawn_scoped(scope, case)
        {
            Ok(handle) => handle.join(),
            Err(error) => panic!("could not start the thread for seed {seed}: {error}"),
        }
    })
}

/// The name of the running test, without the seed a seed thread's name adds.
pub fn running_test_name() -> String {
    let name = thread::current().name().unwrap_or("").to_owned();
    match name.split_once(SEED_THREAD_MARKER) {
        Some((test, _)) => test.to_owned(),
        None => name,
    }
}

/// What separates the test's name from the seed in a [`for_each_seed_on_named_threads`]
/// thread name, which [`replay_command`] strips to recover the test.
const SEED_THREAD_MARKER: &str = " FUZZ_SEED=";

/// Runs `passes` on each seed of [`fuzz_seeds`], and fails with a replay command for each seed
/// it returned `false` for, stopping at [`FAILURES_PER_RUN`].
pub fn run_seeds(default: usize, mut passes: impl FnMut(u64) -> bool) {
    let mut failed: Vec<u64> = Vec::new();
    for seed in fuzz_seeds(default) {
        if !passes(seed) {
            failed.push(seed);
            if failed.len() == FAILURES_PER_RUN {
                break;
            }
        }
    }
    if !failed.is_empty() {
        let replays: Vec<String> = failed
            .iter()
            .map(|seed| format!("    {}", replay_command(*seed)))
            .collect();
        panic!(
            "{} seed(s) failed; replay with\n{}",
            failed.len(),
            replays.join("\n")
        );
    }
}

/// Where a run stops collecting failing seeds: enough to tell distinct bugs apart, few enough
/// that a harness broken on every seed ends quickly.
pub const FAILURES_PER_RUN: usize = 8;

/// The seeds [`for_each_seed`] runs, for a harness that has to write its own replay command.
/// They start at `FUZZ_SEED` (0 when unset). Their count is `FUZZ_ITERS` when set, else
/// `default` times `FUZZ_DEPTH` when that is set, else one seed when `FUZZ_SEED` names a replay,
/// else `default`. `FUZZ_DEPTH` lets a campaign deepen a test whose loops have different
/// defaults while keeping their proportions.
pub fn fuzz_seeds(default: usize) -> Range<u64> {
    let first = variable("FUZZ_SEED");
    let count = match (variable("FUZZ_ITERS"), variable("FUZZ_DEPTH"), first) {
        (Some(iterations), _, _) => iterations,
        (None, Some(depth), _) => (default as u64).saturating_mul(depth),
        (None, None, Some(_)) => 1,
        (None, None, None) => default as u64,
    };
    let first = first.unwrap_or(0);
    first..first.saturating_add(count)
}

fn variable(name: &str) -> Option<u64> {
    env::var(name).ok().and_then(|value| value.parse().ok())
}

/// `FUZZ_SEED=<seed> cargo test -p <package> --test <binary> <test> -- --exact`.
pub fn replay_command(seed: u64) -> String {
    format!("FUZZ_SEED={seed} {}", cargo_test_command())
}

/// `cargo test -p <package> --test <binary> <test> -- --exact` for the running test, from what
/// cargo and the test harness tell it about itself.
fn cargo_test_command() -> String {
    let package = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "<package>".to_owned());
    let binary = env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .map(|stem| match stem.rsplit_once('-') {
            Some((name, hash)) if hash.chars().all(|c| c.is_ascii_hexdigit()) => name.to_owned(),
            _ => stem,
        })
        .unwrap_or_default();
    // A library's unit-test binary is named after the library, which need not match the package
    // (`ry-lang` builds `ry`), so an integration test is recognized by its source file instead.
    let integration = match env::var_os("CARGO_MANIFEST_DIR") {
        Some(manifest) if !binary.is_empty() => {
            let tests = Path::new(&manifest).join("tests");
            tests.join(format!("{binary}.rs")).exists()
                || tests.join(&binary).join("main.rs").exists()
        }
        _ => !binary.is_empty() && binary != package.replace('-', "_"),
    };
    let target = if integration {
        format!("--test {binary}")
    } else {
        "--lib".to_owned()
    };
    let test = running_test_name();
    format!("cargo test -p {package} {target} {test} -- --exact")
}

/// A harness's budget: `FUZZ_ITERS` when set, else `default` times `FUZZ_DEPTH` when that is
/// set, else `default`.
pub fn fuzz_iterations(default: usize) -> usize {
    match (variable("FUZZ_ITERS"), variable("FUZZ_DEPTH")) {
        (Some(iterations), _) => iterations as usize,
        (None, Some(depth)) => default.saturating_mul(depth as usize),
        (None, None) => default,
    }
}

pub fn random_bytes(rng: &mut SplitMix64) -> String {
    let length = rng.below(160);
    let bytes: Vec<u8> = (0..length).map(|_| (rng.next_u64() & 0xFF) as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn token_soup(rng: &mut SplitMix64, alphabet: &[&str]) -> String {
    let mut text = String::new();
    for _ in 0..rng.below(48) {
        text.push_str(alphabet[rng.below(alphabet.len())]);
        if rng.below(3) == 0 {
            text.push(' ');
        }
    }
    text
}

/// One way of writing a fragment. A seed enables each with probability one half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FragmentFamily {
    /// Atoms joined by unary and binary operators with no parentheses, so the tree shows every
    /// precedence and associativity decision.
    Chain,
    /// A nested expression over every construct of the grammar.
    Construct,
    /// A [`FragmentFamily::Construct`] fragment whose tokens are separated by line breaks, semicolons,
    /// comments and nothing at all.
    Layout,
    /// One numeric literal, well formed or cut short.
    Number,
    /// One string literal: escapes of every form, raw strings, and both quotes.
    Text,
    /// A name of an unusual spelling in each position that takes a name.
    Name,
    /// A fragment of another family with one token deleted, repeated, swapped or replaced.
    Mutant,
    /// Tokens drawn blindly.
    Soup,
}

pub const FRAGMENT_FAMILIES: [FragmentFamily; 8] = [
    FragmentFamily::Chain,
    FragmentFamily::Construct,
    FragmentFamily::Layout,
    FragmentFamily::Number,
    FragmentFamily::Text,
    FragmentFamily::Name,
    FragmentFamily::Mutant,
    FragmentFamily::Soup,
];

/// The text of one fragment of `family`.
pub fn source_fragment(rng: &mut SplitMix64, family: FragmentFamily) -> String {
    match family {
        FragmentFamily::Chain => {
            let mut tokens = Vec::new();
            let links = 2 + rng.below(5);
            fragment_chain(rng, &mut tokens, links);
            tokens.join(" ")
        }
        FragmentFamily::Construct => {
            let mut tokens = Vec::new();
            fragment_expression(rng, &mut tokens, 3);
            tokens.join(" ")
        }
        FragmentFamily::Layout => {
            let mut tokens = Vec::new();
            fragment_expression(rng, &mut tokens, 3);
            let mut text = String::new();
            for token in &tokens {
                text.push_str(token);
                text.push_str(FRAGMENT_SEPARATORS[rng.below(FRAGMENT_SEPARATORS.len())]);
            }
            text
        }
        FragmentFamily::Number => fragment_number(rng),
        FragmentFamily::Text => fragment_string(rng),
        FragmentFamily::Name => {
            let template = FRAGMENT_NAME_TEMPLATES[rng.below(FRAGMENT_NAME_TEMPLATES.len())];
            let mut text = String::new();
            for character in template.chars() {
                match character {
                    '•' => text.push_str(FRAGMENT_NAMES[rng.below(FRAGMENT_NAMES.len())]),
                    other => text.push(other),
                }
            }
            text
        }
        FragmentFamily::Mutant => {
            let mut tokens = Vec::new();
            match rng.below(3) {
                0 => {
                    let links = 2 + rng.below(4);
                    fragment_chain(rng, &mut tokens, links);
                }
                _ => fragment_expression(rng, &mut tokens, 2),
            }
            for _ in 0..1 + rng.below(2) {
                let at = rng.below(tokens.len().max(1));
                match rng.below(5) {
                    0 if !tokens.is_empty() => {
                        tokens.remove(at);
                    }
                    1 if !tokens.is_empty() => {
                        let token = tokens[at].clone();
                        tokens.insert(at, token);
                    }
                    2 if tokens.len() > 1 => {
                        let other = rng.below(tokens.len());
                        tokens.swap(at, other);
                    }
                    3 if !tokens.is_empty() => {
                        tokens[at] =
                            FRAGMENT_ALPHABET[rng.below(FRAGMENT_ALPHABET.len())].to_owned();
                    }
                    _ => tokens.insert(
                        at,
                        FRAGMENT_ALPHABET[rng.below(FRAGMENT_ALPHABET.len())].to_owned(),
                    ),
                }
            }
            tokens.join(" ")
        }
        FragmentFamily::Soup => {
            let mut text = String::new();
            for _ in 0..1 + rng.below(8) {
                text.push_str(FRAGMENT_ALPHABET[rng.below(FRAGMENT_ALPHABET.len())]);
                if !rng.chance(1, 4) {
                    text.push(' ');
                }
            }
            text
        }
    }
}

/// Byte edits on one seed, plus splices of up to `max_donor` bytes from another.
pub fn mutate_seed(rng: &mut SplitMix64, seeds: &[&str], max_donor: usize) -> String {
    let mut text = seeds[rng.below(seeds.len())].as_bytes().to_vec();
    for _ in 0..(1 + rng.below(4)) {
        match rng.below(4) {
            0 if !text.is_empty() => {
                let at = rng.below(text.len());
                text.remove(at);
            }
            1 => {
                let at = rng.below(text.len() + 1);
                text.insert(at, (rng.next_u64() & 0x7F) as u8);
            }
            2 if !text.is_empty() => {
                let at = rng.below(text.len());
                text[at] = (rng.next_u64() & 0x7F) as u8;
            }
            _ => {
                let donor = seeds[rng.below(seeds.len())].as_bytes();
                let from = rng.below(donor.len());
                let to = (from + rng.below(max_donor)).min(donor.len());
                let at = rng.below(text.len() + 1);
                text.splice(at..at, donor[from..to].iter().copied());
            }
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

pub fn mutate_bytes(rng: &mut SplitMix64, source: &str) -> Vec<u8> {
    let mut bytes = source.as_bytes().to_vec();
    for _ in 0..(1 + rng.below(6)) {
        if bytes.is_empty() {
            break;
        }
        match rng.below(3) {
            0 => {
                let at = rng.below(bytes.len());
                bytes.remove(at);
            }
            1 => {
                let at = rng.below(bytes.len() + 1);
                bytes.insert(at, (rng.next_u64() & 0x7F) as u8);
            }
            _ => {
                let at = rng.below(bytes.len());
                bytes[at] = (rng.next_u64() & 0x7F) as u8;
            }
        }
    }
    bytes
}

/// One program in R and typed ry, grown from the grammar and the type grammar together: every
/// value has a sort the generator tracks, so an annotation and its value mostly agree and the
/// checker gets past name resolution into inference, generics, records, tags and `match`. About
/// one binding in six is ill-typed on purpose. The first alternative of every choice is the one
/// that stops growing, so an exhausted [`ByteStream`] yields a one-statement program.
pub fn generate_program(rng: &mut dyn Entropy) -> String {
    generate_project(rng, 1).pop().unwrap_or_default()
}

/// `files` programs of one project: a later file reads and calls what an earlier one defined,
/// and a name without a number may be defined in several files, which is what last-writer-wins
/// resolution and cross-file invalidation need.
pub fn generate_project(rng: &mut dyn Entropy, files: usize) -> Vec<String> {
    let mut program = Program {
        rng,
        values: Vec::new(),
        functions: Vec::new(),
        counter: 0,
    };
    (0..files)
        .map(|_| {
            let mut text = String::new();
            if rarely(&mut program, 12) {
                text.push_str(pick(&mut program, FILE_DIRECTIVES));
                text.push_str("\n\n");
            }
            for _ in 0..1 + program.rng.below(10) {
                text.push_str(&statement(&mut program, 0));
                text.push('\n');
            }
            text
        })
        .collect()
}

/// One edit of the size a keystroke or a refactoring step makes, so the result stays close to a
/// program the pipeline analyzes in depth: a statement deleted, duplicated, moved, replaced by
/// or joined with one of `donor`'s, a token deleted, duplicated or renamed to another name of
/// the file, two tokens joined, a line commented out, an annotation comment turned into a plain
/// one, or one character typed.
pub fn small_edit(rng: &mut dyn Entropy, source: &str, donor: &str) -> String {
    let statements = statement_ranges(source);
    let (tokens, _) = crate::lex(source);
    let mut spans: Vec<(crate::SyntaxKind, Range<usize>)> = Vec::with_capacity(tokens.len());
    let mut offset = 0;
    for token in &tokens {
        let end = offset + u32::from(token.len) as usize;
        spans.push((token.kind, offset..end));
        offset = end;
    }
    let mut text = source.to_owned();
    match rng.below(12) {
        0 if !statements.is_empty() => {
            text.replace_range(statements[rng.below(statements.len())].clone(), "");
        }
        1 if !statements.is_empty() => {
            let range = statements[rng.below(statements.len())].clone();
            let copy = format!("\n{}", &source[range.clone()]);
            text.insert_str(range.end, &copy);
        }
        2 if statements.len() > 1 => {
            let first = rng.below(statements.len() - 1);
            let second = first + 1 + rng.below(statements.len() - first - 1);
            let (first, second) = (statements[first].clone(), statements[second].clone());
            text.replace_range(second.clone(), &source[first.clone()]);
            text.replace_range(first, &source[second]);
        }
        3 | 4 => {
            let donated = statement_ranges(donor);
            let piece = match donated.is_empty() {
                true => donor,
                false => &donor[donated[rng.below(donated.len())].clone()],
            };
            match statements.is_empty() {
                true => text.push_str(piece),
                false if rng.below(2) == 0 => {
                    text.replace_range(statements[rng.below(statements.len())].clone(), piece);
                }
                false => {
                    let at = statements[rng.below(statements.len())].end;
                    text.insert_str(at, &format!("\n{piece}"));
                }
            }
        }
        5 if !spans.is_empty() => {
            text.replace_range(spans[rng.below(spans.len())].1.clone(), "");
        }
        6 if !spans.is_empty() => {
            let range = spans[rng.below(spans.len())].1.clone();
            text.insert_str(range.end, &source[range]);
        }
        7 => {
            let names: Vec<&Range<usize>> = spans
                .iter()
                .filter(|(kind, _)| *kind == crate::SyntaxKind::IDENT)
                .map(|(_, range)| range)
                .collect();
            if !names.is_empty() {
                let target = names[rng.below(names.len())].clone();
                let replacement = names[rng.below(names.len())].clone();
                text.replace_range(target, &source[replacement]);
            }
        }
        8 => {
            let separators: Vec<&Range<usize>> = spans
                .iter()
                .filter(|(kind, _)| {
                    matches!(
                        kind,
                        crate::SyntaxKind::WHITESPACE | crate::SyntaxKind::NEWLINE
                    )
                })
                .map(|(_, range)| range)
                .collect();
            if !separators.is_empty() {
                text.replace_range(separators[rng.below(separators.len())].clone(), "");
            }
        }
        9 => {
            let starts: Vec<usize> = std::iter::once(0)
                .chain(source.match_indices('\n').map(|(at, _)| at + 1))
                .collect();
            text.insert_str(starts[rng.below(starts.len())], "# ");
        }
        10 => {
            let annotations: Vec<usize> = source.match_indices("#:").map(|(at, _)| at).collect();
            if !annotations.is_empty() {
                let at = annotations[rng.below(annotations.len())];
                text.replace_range(at + 1..at + 2, "");
            }
        }
        _ => {
            const TYPED: &[&str] = &[
                "(", ")", "{", "}", "[", "]", ",", "\"", "'", "#", "\n", " ", "x", "1", ":", "=",
                "<", "-", "$", "@", "|", "\\", "`", ".",
            ];
            let at = source.floor_char_boundary(rng.below(source.len() + 1));
            text.insert_str(at, TYPED[rng.below(TYPED.len())]);
        }
    }
    text
}

/// The edit that turns `old` into `new` as [`crate::reparse`] takes it: the range of `old` it
/// deletes and the length it inserts there, with the longest common prefix and suffix cut off.
pub fn splice_of(old: &str, new: &str) -> (crate::TextRange, crate::TextSize) {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(before, after)| before == after)
        .count();
    let prefix = old.floor_char_boundary(prefix);
    let suffix = old.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(new.as_bytes()[prefix..].iter().rev())
        .take_while(|(before, after)| before == after)
        .count();
    let old_end = old.ceil_char_boundary(old.len() - suffix);
    let new_end = new.len() - (old.len() - old_end);
    (
        crate::TextRange::new(
            crate::TextSize::from(prefix as u32),
            crate::TextSize::from(old_end as u32),
        ),
        crate::TextSize::from((new_end - prefix) as u32),
    )
}

/// The ranges of a file's top-level statements, each with the `#:` and comment lines directly
/// above it, which belong to it.
pub fn statement_ranges(source: &str) -> Vec<Range<usize>> {
    let root = crate::parse(source).syntax_node();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut leading: Option<usize> = None;
    let mut line_breaks = 1;
    for child in root.children_with_tokens() {
        let (kind, range) = match &child {
            NodeOrToken::Token(token) => (token.kind(), token.text_range()),
            NodeOrToken::Node(node) => (node.kind(), node.text_range()),
        };
        let start = u32::from(range.start()) as usize;
        match kind {
            crate::SyntaxKind::WHITESPACE => {}
            crate::SyntaxKind::NEWLINE => {
                line_breaks += 1;
                if line_breaks > 1 {
                    leading = None;
                }
            }
            // A comment after code on its line is that statement's, not the next one's.
            crate::SyntaxKind::COMMENT
            | crate::SyntaxKind::ANNOTATION
            | crate::SyntaxKind::ANNOTATION_DIRECTIVE => {
                if line_breaks > 0 {
                    leading.get_or_insert(start);
                }
                line_breaks = 0;
            }
            _ if matches!(child, NodeOrToken::Node(_)) => {
                ranges.push(leading.take().unwrap_or(start)..u32::from(range.end()) as usize);
                line_breaks = 0;
            }
            _ => {}
        }
    }
    ranges
}

/// An evenly spread sample of the corpus's `.R` files.
pub fn corpus_sample(count: usize) -> Option<Vec<String>> {
    let root = corpus_dir()?;
    let mut files = Vec::new();
    let mut stack = vec![root.join("r-base"), root.join("cran")];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "R") {
                files.push(path);
            }
        }
    }
    files.sort();
    let step = (files.len() / count.max(1)).max(1);
    let sample: Vec<String> = files
        .iter()
        .step_by(step)
        .take(count)
        .filter_map(|path| fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    (!sample.is_empty()).then_some(sample)
}

/// The gitignored corpus of real R sources that `scripts/fetch-corpus.rs`
/// fetches, or `None` when it is absent.
pub fn corpus_dir() -> Option<PathBuf> {
    let root = env_var("CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    root.is_dir().then_some(root)
}

struct FixtureFile {
    path: PathBuf,
    text: String,
    cases: Vec<FixtureCase>,
}

struct FixtureCase {
    id: String,
    source: String,
    expected: String,
    expected_span: Range<usize>,
}

fn parse_fixture_files(suite_dir: &Path) -> Vec<FixtureFile> {
    let mut paths = Vec::new();
    collect_fixture_files(suite_dir, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
            parse_fixture_file(path, text)
        })
        .collect()
}

fn collect_fixture_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read fixture dir {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_fixture_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "test")
        {
            out.push(path);
        }
    }
}

fn parse_fixture_file(path: PathBuf, text: String) -> FixtureFile {
    struct OpenCase<'a> {
        name: String,
        source: Vec<&'a str>,
        expected: Option<Range<usize>>,
    }
    let close = |case: OpenCase, group: Option<&str>| {
        let group = group.unwrap_or_else(|| {
            panic!(
                "{}: case `{}` appears before any `#====` group",
                path.display(),
                case.name
            )
        });
        let expected_span = case.expected.unwrap_or_else(|| {
            panic!(
                "{}: case `{}` has no `#++++` expectation",
                path.display(),
                case.name
            )
        });
        FixtureCase {
            id: format!("{group}__{}", case.name),
            source: case.source.join("\n").trim_end().to_owned(),
            expected: text[expected_span.clone()].to_owned(),
            expected_span,
        }
    };

    let mut cases = Vec::new();
    let mut group: Option<String> = None;
    let mut open: Option<OpenCase> = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(name) = trimmed.strip_prefix("#====") {
            cases.extend(open.take().map(|case| close(case, group.as_deref())));
            group = Some(name.trim().to_owned());
        } else if let Some(name) = trimmed.strip_prefix("#----") {
            cases.extend(open.take().map(|case| close(case, group.as_deref())));
            open = Some(OpenCase {
                name: name.trim().to_owned(),
                source: Vec::new(),
                expected: None,
            });
        } else if let Some(case) = &mut open {
            if trimmed.starts_with("#++++") {
                case.expected = Some(offset..offset);
            } else if let Some(expected) = &mut case.expected {
                expected.end = offset;
            } else {
                case.source.push(trimmed);
            }
        }
    }
    cases.extend(open.take().map(|case| close(case, group.as_deref())));

    FixtureFile { path, text, cases }
}

/// One test binary drives several suites, so a focused filter legitimately
/// matches nothing here when the case lives in a sibling suite.
fn sibling_suite_holds(suite_dir: &Path, filter: &str) -> bool {
    let Some(parent) = suite_dir.parent() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.is_dir()
            && path != suite_dir
            && parse_fixture_files(&path)
                .iter()
                .any(|file| file.cases.iter().any(|case| case.id.contains(filter)))
    })
}

fn normalize(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

fn check_geometry(node: &SyntaxNode, input: &str) {
    let range = node.text_range();
    let mut cursor = range.start();
    for child in node.children_with_tokens() {
        let child_range = child.text_range();
        assert_eq!(
            child_range.start(),
            cursor,
            "child ranges not contiguous under {:?} for input {input:?}",
            node.kind()
        );
        cursor = child_range.end();
        if let NodeOrToken::Node(child) = child {
            check_geometry(&child, input);
        }
    }
    if node.children_with_tokens().next().is_some() {
        assert_eq!(
            cursor,
            range.end(),
            "children do not cover {:?} for input {input:?}",
            node.kind()
        );
    }
}

/// The flat tree's shortcuts (binary searches over starts, climbing parents, borrowed walks)
/// agree with the plain child-by-child definitions a nested tree would use.
fn check_navigation(root: &SyntaxNode, input: &str) {
    let mut text = String::new();
    for token in root.node_ref().tokens() {
        text.push_str(token.text());
    }
    assert_eq!(text, input, "token walk lost text for input {input:?}");
    let descendants: Vec<SyntaxNode> = root.descendants().collect();
    let walked: Vec<_> = root
        .node_ref()
        .descendants_with_tokens()
        .map(|element| (element.kind(), element.range()))
        .collect();
    let elements: Vec<crate::SyntaxElement> = root.descendants_with_tokens().collect();
    let expected: Vec<_> = elements
        .iter()
        .map(|element| (element.kind(), element.text_range()))
        .collect();
    assert_eq!(
        walked, expected,
        "element walk diverged for input {input:?}"
    );
    for node in descendants.iter().take(8) {
        let enclosed: Vec<bool> = root
            .node_ref()
            .descendants_with_tokens()
            .map(|element| node.node_ref().encloses(element))
            .collect();
        let expected: Vec<bool> = elements
            .iter()
            .map(|element| {
                let parent = match element {
                    NodeOrToken::Node(child) => Some(child.clone()),
                    NodeOrToken::Token(token) => token.parent(),
                };
                std::iter::successors(parent, SyntaxNode::parent).any(|ancestor| ancestor == *node)
            })
            .collect();
        assert_eq!(enclosed, expected, "enclosure diverged for input {input:?}");
    }
    for node in &descendants {
        let first = descendants
            .iter()
            .find(|other| other.kind() == node.kind() && other.text_range() == node.text_range());
        assert_eq!(
            crate::node_at(root, node.kind(), node.text_range()).as_ref(),
            first,
            "node_at missed a node for input {input:?}"
        );
        check_siblings(node, input);
    }
    let subtrees = root.children().take(3);
    for node in std::iter::once(root.clone()).chain(subtrees) {
        for modulus in [1, 3, 7] {
            let wanted = |kind: crate::SyntaxKind| (kind as u16).is_multiple_of(modulus);
            let expected: Vec<SyntaxNode> = node
                .descendants()
                .filter(|node| wanted(node.kind()))
                .collect();
            let mut parents = Vec::new();
            let found = crate::matching_nodes(&node, |candidate, parent| {
                let accepted = wanted(candidate.kind());
                if accepted {
                    parents.push(parent.map(crate::NodeRef::kind));
                }
                accepted
            });
            let expected_parents: Vec<Option<crate::SyntaxKind>> = expected
                .iter()
                .map(|found| {
                    (*found != node)
                        .then(|| found.parent().map(|parent| parent.kind()))
                        .flatten()
                })
                .collect();
            assert_eq!(
                parents, expected_parents,
                "node search parents diverged for input {input:?}"
            );
            assert_eq!(found, expected, "node search diverged for input {input:?}");
        }
    }
    let end = u32::from(root.text_range().end());
    for offset in 0..=end {
        let offset = crate::TextSize::from(offset);
        let fast: Vec<_> = root.token_at_offset(offset).collect();
        assert_eq!(
            fast,
            nested_token_at_offset(&NodeOrToken::Node(root.clone()), offset),
            "token_at_offset at {offset:?} diverged for input {input:?}"
        );
    }
}

/// Sibling, last-child, token and range lookups against the children list read front to back.
fn check_siblings(node: &SyntaxNode, input: &str) {
    let children: Vec<crate::SyntaxElement> = node.children_with_tokens().collect();
    assert_eq!(
        node.last_child_or_token(),
        children.last().cloned(),
        "last_child_or_token diverged for input {input:?}"
    );
    assert_eq!(
        node.children_with_tokens().count(),
        node.node_ref().children().rev().count(),
        "reverse children diverged for input {input:?}"
    );
    for (position, child) in children.iter().enumerate() {
        assert_eq!(
            child.prev_sibling_or_token(),
            position
                .checked_sub(1)
                .map(|previous| children[previous].clone()),
            "prev_sibling_or_token diverged for input {input:?}"
        );
        assert_eq!(
            child.index(),
            position,
            "index diverged for input {input:?}"
        );
        if let NodeOrToken::Token(token) = child {
            assert_eq!(
                token.next_token(),
                nested_next_token(token),
                "next_token diverged for input {input:?}"
            );
        }
    }
    let mut ranges: Vec<crate::TextRange> =
        children.iter().map(|child| child.text_range()).collect();
    ranges.extend(
        children
            .iter()
            .map(|child| crate::TextRange::empty(child.text_range().start())),
    );
    ranges.push(crate::TextRange::empty(node.text_range().end()));
    for range in ranges {
        let ordering = |child: &crate::SyntaxElement| {
            let child = child.text_range();
            if child.end() <= range.start() {
                std::cmp::Ordering::Less
            } else if range.end() <= child.start() {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        };
        let position = children
            .binary_search_by(ordering)
            .unwrap_or_else(|position| position.saturating_sub(1));
        let expected = children
            .get(position)
            .filter(|child| child.text_range().contains_range(range))
            .cloned();
        assert_eq!(
            node.child_or_token_at_range(range),
            expected,
            "child_or_token_at_range({range:?}) diverged under {node:?} for input {input:?}"
        );
    }
}

/// The next token the way a nested tree finds it: the first token of the next sibling, or of
/// the next sibling of the nearest ancestor that has one.
fn nested_next_token(token: &crate::SyntaxToken) -> Option<crate::SyntaxToken> {
    let first_token = |element: crate::SyntaxElement| match element {
        NodeOrToken::Node(node) => node.first_token(),
        NodeOrToken::Token(token) => Some(token),
    };
    match token.next_sibling_or_token() {
        Some(element) => first_token(element),
        None => token
            .parent_ancestors()
            .find_map(|ancestor| ancestor.next_sibling_or_token())
            .and_then(first_token),
    }
}

fn nested_token_at_offset(
    element: &crate::SyntaxElement,
    offset: crate::TextSize,
) -> Vec<crate::SyntaxToken> {
    let node = match element {
        NodeOrToken::Token(token) => return vec![token.clone()],
        NodeOrToken::Node(node) => node,
    };
    if node.text_range().is_empty() {
        return Vec::new();
    }
    node.children_with_tokens()
        .filter(|child| {
            let range = child.text_range();
            !range.is_empty() && range.start() <= offset && offset <= range.end()
        })
        .flat_map(|child| nested_token_at_offset(&child, offset))
        .collect()
}

/// What [`generate_project`] knows about the program so far.
struct Program<'a> {
    rng: &'a mut dyn Entropy,
    values: Vec<(String, Sort)>,
    functions: Vec<GeneratedFunction>,
    counter: usize,
}

struct GeneratedFunction {
    name: String,
    parameters: Vec<Sort>,
    result: Sort,
}

/// The types the generator can both spell and build a value of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Integer,
    Double,
    Character,
    Logical,
    Integers,
    Doubles,
    Characters,
    Named,
    Record,
    Tuple,
    Maybe,
    List,
    Function,
}

const SORTS: [Sort; 13] = [
    Sort::Integer,
    Sort::Double,
    Sort::Character,
    Sort::Logical,
    Sort::Integers,
    Sort::Doubles,
    Sort::Characters,
    Sort::Named,
    Sort::Record,
    Sort::Tuple,
    Sort::Maybe,
    Sort::List,
    Sort::Function,
];

const FILE_DIRECTIVES: &[&str] = &[
    "# typing: on",
    "# typing: strict",
    "# typing: off",
    "#: @strict",
];

const WORDS: &[&str] = &[
    "total", "count", "label", "items", "helper", "point", "pair", "maybe", "scale", "step",
    "result", "value",
];

/// `(hole, sort, type, literals)`: the hole names the sort in a template as `@{hole}`.
const SORT_TABLE: &[(&str, Sort, &str, &[&str])] = &[
    (
        "I",
        Sort::Integer,
        "integer",
        &[
            "1L",
            "0L",
            "2L",
            "10L",
            "-1L",
            "100000L",
            "0x1FL",
            "NA_integer_",
        ],
    ),
    (
        "D",
        Sort::Double,
        "double",
        &[
            "1.5", "0", "2", "1e3", "-0.5", ".5", "pi", "Inf", "NA_real_",
        ],
    ),
    (
        "C",
        Sort::Character,
        "character",
        &[
            "\"a\"",
            "\"\"",
            "\"hello world\"",
            "'single'",
            "\"é\"",
            "r\"(raw)\"",
            "NA_character_",
        ],
    ),
    ("L", Sort::Logical, "logical", &["TRUE", "FALSE", "NA"]),
    (
        "IV",
        Sort::Integers,
        "integer[]",
        &[
            "c(1L, 2L, 3L)",
            "1:3",
            "integer(0)",
            "seq_len(4L)",
            "c(a = 1L, b = 2L)",
        ],
    ),
    (
        "DV",
        Sort::Doubles,
        "double[]",
        &["c(1.5, 2)", "numeric(0)", "c(0.1, NA)", "c(x = 1, y = 2)"],
    ),
    (
        "CV",
        Sort::Characters,
        "character[]",
        &["c(\"a\", \"b\")", "character(0)", "letters"],
    ),
    (
        "N",
        Sort::Named,
        "list{name: character, size: integer}",
        &["list(name = \"a\", size = 1L)"],
    ),
    (
        "R",
        Sort::Record,
        "Record{x: double, y: double}",
        &["(x = 1, y = 2)", "(x = 0.5, y = -1)"],
    ),
    (
        "U",
        Sort::Tuple,
        "Tuple{integer, character}",
        &["(1L, \"a\")"],
    ),
    (
        "M",
        Sort::Maybe,
        ":Some(integer) | :None",
        &[":None", ":Some(1L)"],
    ),
    ("X", Sort::List, "List<I64>", &["[1L, 2L]", "[1L]"]),
    (
        "F",
        Sort::Function,
        "fn(value: integer) -> integer",
        &["function(value) value", "\\(value) value + 1L"],
    ),
];

/// Expressions by the sort they produce; a hole is filled with an expression of its sort.
const EXPRESSIONS: &[(Sort, &str)] = &[
    (Sort::Integer, "@{I} + @{I}"),
    (Sort::Integer, "@{I} * @{I}"),
    (Sort::Integer, "@{I} - @{I}"),
    (Sort::Integer, "@{I} %/% @{I}"),
    (Sort::Integer, "@{I} %% @{I}"),
    (Sort::Integer, "-@{I}"),
    (Sort::Integer, "(@{I})"),
    (Sort::Integer, "length(@{CV})"),
    (Sort::Integer, "length(@{DV})"),
    (Sort::Integer, "nchar(@{C})"),
    (Sort::Integer, "as.integer(@{D})"),
    (Sort::Integer, "@{IV}[[1L]]"),
    (Sort::Integer, "@{IV}[[@{I}]]"),
    (Sort::Integer, "@{N}$size"),
    (Sort::Integer, "@{N}[[\"size\"]]"),
    (Sort::Integer, "sum(@{IV})"),
    (Sort::Integer, "max(@{I}, @{I})"),
    (Sort::Integer, "abs(@{I})"),
    (Sort::Integer, "if (@{L}) @{I} else @{I}"),
    (Sort::Integer, "switch(@{C}, a = @{I}, @{I})"),
    (Sort::Integer, "@{F}(@{I})"),
    (Sort::Integer, "(@{IV} |> length())"),
    (Sort::Integer, "(\\(inner) inner + 1L)(@{I})"),
    (Sort::Integer, "local({\n  inner <- @{I}\n  inner * 2L\n})"),
    (Sort::Integer, "tryCatch(@{I}, error = function(e) @{I})"),
    (
        Sort::Integer,
        "match (@{M}) {\n  :Some(found) => found\n  :None => @{I}\n}",
    ),
    (Sort::Integer, "match (@{U}) {\n  (first, _) => first\n}"),
    (Sort::Integer, "Result.with_default(List.first(@{X}), @{I})"),
    (
        Sort::Integer,
        "List.walk(@{X}, @{I}, function(state, item) state + item)",
    ),
    (Sort::Integer, "List.len(@{X})"),
    (Sort::Double, "@{D} + @{D}"),
    (Sort::Double, "@{D} * @{D}"),
    (Sort::Double, "@{D} / @{D}"),
    (Sort::Double, "@{I} / @{I}"),
    (Sort::Double, "@{D} + @{I}"),
    (Sort::Double, "@{D}^2"),
    (Sort::Double, "sqrt(@{D})"),
    (Sort::Double, "exp(@{D})"),
    (Sort::Double, "mean(@{DV})"),
    (Sort::Double, "sum(@{DV})"),
    (Sort::Double, "@{R}$x"),
    (Sort::Double, "@{R}$y"),
    (Sort::Double, "as.numeric(@{I})"),
    (Sort::Double, "@{DV}[[1L]]"),
    (Sort::Double, "if (@{L}) @{D} else @{D}"),
    (Sort::Double, "(@{D} |> sqrt())"),
    (Sort::Double, "stats::sd(@{DV})"),
    (
        Sort::Double,
        "match (@{R}) {\n  (x = horizontal, ..) => horizontal\n}",
    ),
    (Sort::Character, "paste(@{C}, @{C})"),
    (Sort::Character, "paste0(@{C}, @{I})"),
    (Sort::Character, "toupper(@{C})"),
    (Sort::Character, "trimws(@{C})"),
    (Sort::Character, "sprintf(\"%d items\", @{I})"),
    (Sort::Character, "format(@{D})"),
    (Sort::Character, "@{N}$name"),
    (Sort::Character, "as.character(@{I})"),
    (Sort::Character, "substr(@{C}, 1L, 2L)"),
    (Sort::Character, "@{CV}[[1L]]"),
    (Sort::Character, "paste(@{CV}, collapse = \", \")"),
    (Sort::Character, "if (@{L}) @{C} else @{C}"),
    (
        Sort::Character,
        "match (@{U}) {\n  (_, second) => second\n}",
    ),
    (Sort::Logical, "@{I} > @{I}"),
    (Sort::Logical, "@{D} <= @{D}"),
    (Sort::Logical, "@{C} == @{C}"),
    (Sort::Logical, "!@{L}"),
    (Sort::Logical, "@{L} && @{L}"),
    (Sort::Logical, "@{L} || @{L}"),
    (Sort::Logical, "isTRUE(@{L})"),
    (Sort::Logical, "nchar(@{C}) > 0L"),
    (Sort::Logical, "any(@{IV} > @{I})"),
    (Sort::Logical, "all(@{DV} >= 0)"),
    (Sort::Logical, "@{I} %in% @{IV}"),
    (Sort::Logical, "is.na(@{D})"),
    (Sort::Logical, "length(@{CV}) == 0L"),
    (Sort::Logical, "startsWith(@{C}, @{C})"),
    (Sort::Logical, "is.null(@{N}$name)"),
    (Sort::Logical, "identical(@{IV}, @{IV})"),
    (
        Sort::Logical,
        "match (@{M}) {\n  :Some(_) => TRUE\n  :None => FALSE\n}",
    ),
    (Sort::Integers, "c(@{I}, @{I})"),
    (Sort::Integers, "c(@{IV}, @{I})"),
    (Sort::Integers, "seq_len(@{I})"),
    (Sort::Integers, "@{I}:@{I}"),
    (Sort::Integers, "@{IV}[@{IV} > @{I}]"),
    (Sort::Integers, "@{IV}[-1L]"),
    (Sort::Integers, "rev(@{IV})"),
    (Sort::Integers, "sort(@{IV})"),
    (Sort::Integers, "unique(@{IV})"),
    (Sort::Integers, "@{IV} + 1L"),
    (Sort::Integers, "@{IV} * @{I}"),
    (Sort::Integers, "seq_along(@{CV})"),
    (Sort::Integers, "which(@{DV} > @{D})"),
    (Sort::Integers, "nchar(@{CV})"),
    (Sort::Integers, "as.integer(@{DV})"),
    (Sort::Integers, "vapply(@{CV}, nchar, integer(1))"),
    (Sort::Integers, "rep(@{I}, 3L)"),
    (Sort::Integers, "if (@{L}) @{IV} else @{IV}"),
    (Sort::Doubles, "c(@{D}, @{D})"),
    (Sort::Doubles, "c(@{DV}, @{DV})"),
    (Sort::Doubles, "@{DV} * 2"),
    (Sort::Doubles, "@{DV} + @{D}"),
    (Sort::Doubles, "@{IV} / 2"),
    (Sort::Doubles, "as.numeric(@{IV})"),
    (Sort::Doubles, "sqrt(@{DV})"),
    (Sort::Doubles, "cumsum(@{DV})"),
    (Sort::Doubles, "rev(@{DV})"),
    (Sort::Doubles, "@{DV}[@{DV} > @{D}]"),
    (Sort::Doubles, "@{DV} / sum(@{DV})"),
    (
        Sort::Doubles,
        "vapply(@{IV}, function(item) item / 2, numeric(1))",
    ),
    (Sort::Doubles, "unname(@{DV})"),
    (Sort::Characters, "c(@{C}, @{C})"),
    (Sort::Characters, "c(@{CV}, @{C})"),
    (Sort::Characters, "paste0(@{CV}, @{C})"),
    (Sort::Characters, "toupper(@{CV})"),
    (Sort::Characters, "as.character(@{IV})"),
    (Sort::Characters, "sort(@{CV})"),
    (Sort::Characters, "rev(@{CV})"),
    (Sort::Characters, "unique(@{CV})"),
    (Sort::Characters, "names(@{N})"),
    (Sort::Characters, "strsplit(@{C}, \",\")[[1L]]"),
    (Sort::Characters, "sprintf(\"%s!\", @{CV})"),
    (Sort::Characters, "@{CV}[nchar(@{CV}) > @{I}]"),
    (Sort::Named, "list(name = @{C}, size = @{I})"),
    (Sort::Named, "if (@{L}) @{N} else @{N}"),
    (Sort::Record, "(x = @{D}, y = @{D})"),
    (Sort::Record, "if (@{L}) @{R} else @{R}"),
    (
        Sort::Record,
        "match (@{R}) {\n  (x = horizontal, y = vertical) => (x = vertical, y = horizontal)\n}",
    ),
    (Sort::Tuple, "(@{I}, @{C})"),
    (Sort::Tuple, "if (@{L}) @{U} else @{U}"),
    (Sort::Maybe, ":Some(@{I})"),
    (Sort::Maybe, "if (@{L}) :Some(@{I}) else :None"),
    (
        Sort::Maybe,
        "match (@{M}) {\n  :Some(found) => :Some(found + 1L)\n  :None => :None\n}",
    ),
    (Sort::List, "[@{I}, @{I}]"),
    (Sort::List, "[@{I}]"),
    (Sort::List, "List.map(@{X}, function(item) item + 1L)"),
    (Sort::List, "List.map(@{X}, \\(item) item * @{I})"),
    (Sort::List, "List.from_r(@{IV})"),
    (Sort::List, "List.append(@{X}, @{I})"),
    (Sort::List, "List.concat(@{X}, @{X})"),
    (Sort::List, "List.keep_if(@{X}, function(item) item > @{I})"),
    (Sort::Function, "function(value) value + @{I}"),
    (Sort::Function, "\\(value) value * 2L"),
    (Sort::Function, "function(value: integer) -> integer value"),
    (Sort::Function, "@pure function(value) value + 1L"),
    (
        Sort::Function,
        "function(value) {\n  doubled <- value * 2L\n  doubled\n}",
    ),
    (
        Sort::Function,
        "local({\n  offset <- @{I}\n  function(value) value + offset\n})",
    ),
    (Sort::Function, "if (@{L}) @{F} else @{F}"),
];

/// Whole statements the sorts cannot express: type definitions, generics, the object systems,
/// tables, the standard library, tests and code the lints report. `@{n}` and `@{m}` are fresh
/// names and `@{T}` a fresh type or class name, each the same throughout one snippet.
const SNIPPETS: &[&str] = &[
    "#: @alias @{T} {integer | NULL}\n\n#: @{T}\n@{n} <- if (@{L}) @{I} else NULL\nif (!is.null(@{n})) print(@{n} + 1L)",
    "alias @{T} = character[]\n@{n}: @{T} <- @{CV}",
    "alias @{T}<V> = list[named: V]\n@{n}: @{T}<double> <- list(a = @{D})",
    "type @{T} = Record{x: double, y: double}\n@{n}: @{T} <- @@{T}(@{R})\n@{m} <- function(at: @{T}) -> double at$x + at$y\nprint(@{m}(@{n}))",
    "#: @type @{T} {list{w: double, h: double}}\n\n#: @new @{T}\n@{n} <- list(w = @{D}, h = @{D})\n\n#: fn(size: @{T}) -> double\n@{m} <- function(size) size$w * size$h",
    "#: @alias @{T} {:Circle(radius: double) | :Rect(width: double, height: double)}\n\n#: fn(shape: @{T}) -> double\n@{n} <- function(shape) match (shape) {\n  :Circle(radius = r) => r * r\n  :Rect(width = w, height = h) => w * h\n}\n@{m} <- @{n}(:Circle(radius = @{D}))",
    "type @{T} = :Node(Tuple{integer, @{T}}) | :End\n@{n} <- function(chain: @{T}) -> integer match (chain) {\n  :Node((head, rest)) => head + @{n}(rest)\n  :End => 0L\n}\n@{m} <- @{n}(@@{T}(:Node((@{I}, @@{T}(:End)))))",
    "#: <T> fn(x: T[]) -> T\n@{n} <- function(x) x[[1L]]\n@{m} <- @{n}(@{CV})",
    "@{n} <- function<T>(x: T) -> T x\n@{m} <- @{n}(@{R})",
    "@{n} <- function<T: numeric>(x: T, y: T) -> T x + y\n@{m} <- @{n}(@{I}, @{I})",
    "#: @forall T\n#: @param x {T}\n#: @param [times] {integer}\n#: @return {list[T]}\n@{n} <- function(x, times = 2L) rep(list(x), times)\n@{m} <- @{n}(@{C})",
    "#: fn(...: character) -> character\n@{n} <- function(...) paste(..., sep = \"\")\n@{m} <- @{n}(@{C}, @{C})",
    "#: @trust fn(x: integer) -> character\n@{n} <- function(x) x\n@{m} <- @{n}(@{I})",
    "@{n} <- function(text: Str) Str.to_i64(text)\n@{m} <- Result.and_then(@{n}(\"4\"), function(parsed) if (parsed > 0) :Ok(parsed) else :Err(:NotPositive))",
    "@{n} <- match (List.get(@{X}, @{I})) {\n  :Ok(item) => item\n  :Err(_) => @{I}\n}",
    "@{n} <- Result.map(List.find_first(@{X}, function(item) item > @{I}), function(item) item * 2L)",
    "@{n} <- structure(list(amount = @{D}), class = \"@{T}\")\nprint.@{T} <- function(x, ...) {\n  cat(format(x$amount), \"\\n\")\n  invisible(x)\n}\nformat.@{T} <- function(x, ...) paste(\"<\", x$amount, \">\")\n@{m} <- function(x, ...) UseMethod(\"@{m}\")\n@{m}.@{T} <- function(x, ...) x$amount * 2\n@{m}.default <- function(x, ...) NA_real_\nprint(@{m}(@{n}))",
    "`+.@{T}` <- function(e1, e2) structure(list(amount = e1$amount + e2$amount), class = \"@{T}\")\n@{n} <- structure(list(amount = @{D}), class = \"@{T}\")\n@{m} <- @{n} + @{n}",
    "setClass(\"@{T}\", representation(amount = \"numeric\", label = \"character\"))\nsetGeneric(\"@{m}\", function(object, ...) standardGeneric(\"@{m}\"))\nsetMethod(\"@{m}\", \"@{T}\", function(object, ...) object@amount * 2)\n@{n} <- new(\"@{T}\", amount = @{D}, label = @{C})\nprint(@{m}(@{n}))\nprint(@{n}@label)",
    "@{T} <- R6::R6Class(\"@{T}\",\n  public = list(\n    amount = NULL,\n    initialize = function(amount) {\n      self$amount <- amount\n    },\n    double = function() self$amount * 2\n  ),\n  private = list(secret = 1L)\n)\n@{n} <- @{T}$new(@{D})\n@{m} <- @{n}$double()",
    "@{T} <- setRefClass(\"@{T}\", fields = list(amount = \"numeric\"), methods = list(\n  bump = function() {\n    amount <<- amount + 1\n    invisible(.self)\n  }\n))\n@{n} <- @{T}$new(amount = @{D})\n@{n}$bump()",
    "library(data.table)\n@{n} <- data.table(key = @{CV}, amount = @{DV})\n@{m} <- @{n}[amount > @{D}, .(total = sum(amount)), by = key]\n@{n}[, share := amount / sum(amount)]\nsetnames(@{n}, \"share\", \"part\")",
    "@{n} <- data.frame(key = @{CV}, amount = @{DV}, stringsAsFactors = FALSE)\n@{n}$double <- @{n}$amount * 2\n@{m} <- @{n}[@{n}$amount > @{D}, c(\"key\", \"amount\")]\nprint(nrow(@{m}))",
    "@{n} <- lapply(@{IV}, function(item) item * @{I})\n@{m} <- Reduce(`+`, @{n}, 0L)",
    "@{n} <- Map(function(left, right) left + right, @{IV}, @{DV})\n@{m} <- do.call(rbind, @{n})",
    "@{n} <- function(kind = c(\"mean\", \"median\"), ...) {\n  kind <- match.arg(kind)\n  on.exit(message(\"done\"), add = TRUE)\n  if (missing(...)) return(NULL)\n  switch(kind, mean = mean(c(...)), median = stats::median(c(...)))\n}\n@{m} <- @{n}(\"mean\", @{D}, @{D})",
    "@{n} <- function() {\n  hits <- 0L\n  function() {\n    hits <<- hits + 1L\n    hits\n  }\n}\n@{m} <- @{n}()\nprint(@{m}())",
    "@{n} <- function(n) if (n <= 1L) 1L else n * @{n}(n - 1L)\n@{m} <- function(n) if (n == 0L) TRUE else @{n}(n) > 0L",
    "@{n} <- tryCatch({\n  warning(@{C})\n  @{I}\n}, warning = function(w) conditionMessage(w), error = function(e) NULL, finally = cat(\"\"))",
    "@{n} <- y ~ a + b\n@{m} <- stats::lm(@{n}, data = data.frame(y = @{DV}, a = @{DV}, b = @{DV}))",
    "test_that(\"@{n} holds\", {\n  @{n} <- @{I}\n  expect_equal(@{n}, @{I})\n  expect_true(@{L})\n  expect_error(stop(@{C}), \"a\")\n})",
    "@{n} = @{I}\nT -> @{m}\nif (@{L} == TRUE) print(1:length(@{CV}))\nsapply(@{IV}, function(item) item)",
    "@{n} <- @{IV}\nfor (index in seq_along(@{n})) @{n}[index] <- @{n}[index] + 1L\nnames(@{n}) <- @{CV}\nattr(@{n}, \"note\") <- @{C}\nlevels(@{n}) <- NULL",
    "@{n} <- new.env()\nassign(\"@{m}\", @{I}, envir = @{n})\n@{m} <- get(\"@{m}\", envir = @{n})\nlocal({\n  @{m} <- @{C}\n  @{m}\n})",
    "@{n} <- nosuchpkg::mystery(@{I})\n@{m} <- @{n}$field[[1L]](@{C})",
    "@{n}: integer <- 1L\n@{n}: character <- \"a\"",
    "#: double\n@{n}: integer <- @{I}",
];

fn statement(program: &mut Program, depth: usize) -> String {
    let kinds = if depth >= 2 { 4 } else { 10 };
    match program.rng.below(kinds) {
        0 | 1 => binding(program, depth),
        2 => {
            let sort = any_sort(program);
            format!("print({})", expression(program, sort, 1))
        }
        3 => replacement(program, depth),
        4 | 5 => function_definition(program, depth),
        6 | 7 => control_flow(program, depth),
        _ => {
            let template = pick(program, SNIPPETS);
            let (name, other, class) = (fresh_name(program), fresh_name(program), {
                program.counter += 1;
                format!("Kind{}", program.counter)
            });
            let text = template
                .replace("@{n}", &name)
                .replace("@{m}", &other)
                .replace("@{T}", &class);
            expand(program, &text, 1)
        }
    }
}

fn binding(program: &mut Program, depth: usize) -> String {
    let sort = any_sort(program);
    let value_sort = if rarely(program, 6) {
        any_sort(program)
    } else {
        sort
    };
    let value = expression(program, value_sort, 0);
    let name = fresh_name(program);
    let annotation = type_text(sort);
    let text = match program.rng.below(8) {
        0 | 1 => format!("{name} <- {value}"),
        2 => format!("#: {annotation}\n{name} <- {value}"),
        3 => format!("{name}: {annotation} <- {value}"),
        4 => format!("{name}: {annotation} = {value}"),
        5 => format!("#: @if-unknown {annotation}\n{name} <- {value}"),
        6 if depth > 0 => format!("{name} <<- {value}"),
        _ => format!("{name} = {value}"),
    };
    define_value(program, name, sort);
    text
}

/// A write through a replacement form to a value in scope, or a binding when nothing is.
fn replacement(program: &mut Program, depth: usize) -> String {
    if program.values.is_empty() {
        return binding(program, depth);
    }
    let (name, sort) = program.values[program.rng.below(program.values.len())].clone();
    let template = match sort {
        Sort::Integers => pick(
            program,
            &[
                "@{v}[[1L]] <- @{I}",
                "@{v}[@{v} > @{I}] <- 0L",
                "names(@{v}) <- @{CV}",
                "@{v}[[length(@{v}) + 1L]] <- @{I}",
                "attr(@{v}, \"unit\") <- @{C}",
            ],
        ),
        Sort::Doubles => pick(
            program,
            &[
                "@{v}[2L] <- @{D}",
                "@{v}[is.na(@{v})] <- 0",
                "names(@{v})[1L] <- @{C}",
            ],
        ),
        Sort::Characters => pick(
            program,
            &[
                "@{v}[[1L]] <- @{C}",
                "@{v} <- c(@{v}, @{C})",
                "substr(@{v}, 1L, 1L) <- @{C}",
            ],
        ),
        Sort::Named => pick(
            program,
            &[
                "@{v}$size <- @{I}",
                "@{v}[[\"name\"]] <- @{C}",
                "@{v}$size <- @{v}$size + 1L",
            ],
        ),
        Sort::Integer => "@{v} <- @{v} + @{I}",
        Sort::Double => "@{v} <- @{v} * @{D}",
        Sort::Character => "@{v} <- paste0(@{v}, @{C})",
        Sort::Logical => "@{v} <- !@{v}",
        Sort::Record => "@{v} <- (x = @{v}$y, y = @{D})",
        Sort::Tuple => "@{v} <- (@{I}, @{C})",
        Sort::Maybe => "@{v} <- :Some(@{I})",
        Sort::List => "@{v} <- List.append(@{v}, @{I})",
        Sort::Function => "@{v} <- @{F}",
    };
    expand(program, &template.replace("@{v}", &name), 1)
}

fn function_definition(program: &mut Program, depth: usize) -> String {
    let name = fresh_name(program);
    let parameters: Vec<(String, Sort)> = (0..program.rng.below(4))
        .map(|index| {
            (
                format!("{}{index}", pick(program, &["arg", "input", "n"])),
                any_sort(program),
            )
        })
        .collect();
    let optional = match parameters.last() {
        Some((_, sort)) if rarely(program, 3) => Some(expression(program, *sort, 2)),
        _ => None,
    };
    let dots = rarely(program, 6);
    let result = any_sort(program);
    let signature = GeneratedFunction {
        name: name.clone(),
        parameters: parameters.iter().map(|(_, sort)| *sort).collect(),
        result,
    };
    // Registered before the body one time in four, so the body may recurse.
    let recursive = rarely(program, 4);
    if recursive {
        define_function(program, signature);
    }
    let scope = program.values.len();
    program.values.extend(parameters.iter().cloned());
    let mut lines: Vec<String> = (0..program.rng.below(3))
        .map(|_| statement(program, depth + 1))
        .collect();
    if dots {
        lines.push("extra <- list(...)".to_owned());
    }
    if rarely(program, 5) {
        lines.push(format!(
            "if ({}) return({})",
            expression(program, Sort::Logical, 2),
            expression(program, result, 2)
        ));
    }
    let last = expression(program, result, 1);
    lines.push(match program.rng.below(4) {
        0 | 1 => last,
        2 => format!("return({last})"),
        _ => format!("invisible({last})"),
    });
    program.values.truncate(scope);
    if !recursive {
        define_function(
            program,
            GeneratedFunction {
                name: name.clone(),
                parameters: parameters.iter().map(|(_, sort)| *sort).collect(),
                result,
            },
        );
    }

    let style = program.rng.below(5);
    let last_parameter = parameters.len().saturating_sub(1);
    let mut plain = Vec::new();
    let mut inline = Vec::new();
    let mut compact = Vec::new();
    let mut expanded = String::new();
    for (index, (parameter, sort)) in parameters.iter().enumerate() {
        let annotation = type_text(*sort);
        match &optional {
            Some(default) if index == last_parameter => {
                plain.push(format!("{parameter} = {default}"));
                inline.push(format!("{parameter}: {annotation} = {default}"));
                compact.push(format!("[{parameter}]: {annotation}"));
                expanded.push_str(&format!("#: @param [{parameter}] {{{annotation}}}\n"));
            }
            _ => {
                plain.push(parameter.clone());
                inline.push(format!("{parameter}: {annotation}"));
                compact.push(format!("{parameter}: {annotation}"));
                expanded.push_str(&format!("#: @param {parameter} {{{annotation}}}\n"));
            }
        }
    }
    if dots {
        plain.push("...".to_owned());
        inline.push("...".to_owned());
        compact.push("...: Any".to_owned());
    }
    let returns = type_text(result);
    let body = block(&lines);
    match style {
        0 | 1 => format!("{name} <- function({}) {body}", plain.join(", ")),
        2 => format!(
            "#: fn({}) -> {returns}\n{name} <- function({}) {body}",
            compact.join(", "),
            plain.join(", ")
        ),
        3 => format!(
            "{expanded}#: @return {{{returns}}}\n{name} <- function({}) {body}",
            plain.join(", ")
        ),
        _ => format!(
            "{name} <- function({}) -> {returns} {body}",
            inline.join(", ")
        ),
    }
}

fn control_flow(program: &mut Program, depth: usize) -> String {
    let scope = program.values.len();
    let condition = expression(program, Sort::Logical, 1);
    let text = match program.rng.below(7) {
        0 => format!("if ({condition}) {}", nested_block(program, depth)),
        1 => format!(
            "if ({condition}) {} else {}",
            nested_block(program, depth),
            nested_block(program, depth)
        ),
        2 => {
            let sequence = expression(program, Sort::Integers, 1);
            let variable = fresh_name(program);
            program.values.push((variable.clone(), Sort::Integer));
            format!(
                "for ({variable} in {sequence}) {}",
                nested_block(program, depth)
            )
        }
        3 => {
            let body = nested_block(program, depth);
            format!(
                "while ({condition}) {}",
                body.replacen('{', "{\n  if (FALSE) next", 1)
            )
        }
        4 => {
            let body = nested_block(program, depth);
            format!("repeat {}", body.replacen('{', "{\n  if (TRUE) break", 1))
        }
        5 => format!(
            "tryCatch({}, error = function(e) {}, finally = {})",
            nested_block(program, depth),
            nested_block(program, depth),
            nested_block(program, depth)
        ),
        _ => {
            let subject = expression(program, Sort::Maybe, 1);
            format!(
                "match ({subject}) {{\n  :Some(found) if (found > 0L) => {}\n  _ => {}\n}}",
                nested_block(program, depth).replace('\n', "\n  "),
                nested_block(program, depth).replace('\n', "\n  ")
            )
        }
    };
    program.values.truncate(scope);
    text
}

/// A braced block of statements whose bindings stay inside it, since it may not run.
fn nested_block(program: &mut Program, depth: usize) -> String {
    let scope = program.values.len();
    let lines: Vec<String> = (0..1 + program.rng.below(3))
        .map(|_| statement(program, depth + 1))
        .collect();
    program.values.truncate(scope);
    block(&lines)
}

fn block(lines: &[String]) -> String {
    let mut text = String::from("{\n");
    for line in lines {
        text.push_str("  ");
        text.push_str(&line.replace('\n', "\n  "));
        text.push('\n');
    }
    text.push('}');
    text
}

/// An expression of `sort`: a literal, a value in scope, a call of a function in scope, or a
/// template of [`EXPRESSIONS`]. Past `depth` 3 only the first two, which bounds its size.
fn expression(program: &mut Program, sort: Sort, depth: usize) -> String {
    let choice = if depth >= 3 {
        program.rng.below(2)
    } else {
        program.rng.below(8)
    };
    match choice {
        0 => literal(program, sort),
        1 | 2 => {
            let candidates: Vec<usize> = (0..program.values.len())
                .filter(|index| program.values[*index].1 == sort)
                .collect();
            match candidates.is_empty() {
                true => literal(program, sort),
                false => program.values[candidates[program.rng.below(candidates.len())]]
                    .0
                    .clone(),
            }
        }
        3 => {
            let candidates: Vec<usize> = (0..program.functions.len())
                .filter(|index| program.functions[*index].result == sort)
                .collect();
            if candidates.is_empty() {
                return literal(program, sort);
            }
            let index = candidates[program.rng.below(candidates.len())];
            let name = program.functions[index].name.clone();
            let parameters = program.functions[index].parameters.clone();
            let arguments: Vec<String> = parameters
                .iter()
                .map(|parameter| expression(program, *parameter, depth + 1))
                .collect();
            format!("{name}({})", arguments.join(", "))
        }
        _ => {
            let candidates: Vec<&str> = EXPRESSIONS
                .iter()
                .filter(|(result, _)| *result == sort)
                .map(|(_, template)| *template)
                .collect();
            let template = candidates[program.rng.below(candidates.len())];
            expand(program, template, depth + 1)
        }
    }
}

/// Fills each `@{hole}` of `template` with an expression of the hole's sort.
fn expand(program: &mut Program, template: &str, depth: usize) -> String {
    let mut text = String::new();
    let mut rest = template;
    while let Some(at) = rest.find("@{") {
        text.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('}') else {
            break;
        };
        let hole = &rest[at + 2..at + end];
        match SORT_TABLE.iter().find(|(name, ..)| *name == hole) {
            Some((_, sort, ..)) => text.push_str(&expression(program, *sort, depth)),
            None => text.push_str(&rest[at..at + end + 1]),
        }
        rest = &rest[at + end + 1..];
    }
    text.push_str(rest);
    text
}

fn literal(program: &mut Program, sort: Sort) -> String {
    let literals = SORT_TABLE
        .iter()
        .find(|(_, candidate, ..)| *candidate == sort)
        .map_or(&[][..], |(.., literals)| literals);
    pick(program, literals).to_owned()
}

fn type_text(sort: Sort) -> &'static str {
    SORT_TABLE
        .iter()
        .find(|(_, candidate, ..)| *candidate == sort)
        .map_or("Any", |(_, _, text, _)| text)
}

fn any_sort(program: &mut Program) -> Sort {
    SORTS[program.rng.below(SORTS.len())]
}

/// A name with a number is unique in the project; one in three has none, so files collide.
fn fresh_name(program: &mut Program) -> String {
    let word = pick(program, WORDS);
    if rarely(program, 3) {
        return word.to_owned();
    }
    program.counter += 1;
    format!("{word}{}", program.counter)
}

/// A redefinition replaces what the name was, so later reads use its latest sort.
fn define_value(program: &mut Program, name: String, sort: Sort) {
    program.values.retain(|(existing, _)| *existing != name);
    program.functions.retain(|function| function.name != name);
    program.values.push((name, sort));
}

fn define_function(program: &mut Program, function: GeneratedFunction) {
    program
        .values
        .retain(|(existing, _)| *existing != function.name);
    program
        .functions
        .retain(|existing| existing.name != function.name);
    program.functions.push(function);
}

fn pick<'a>(program: &mut Program, pool: &[&'a str]) -> &'a str {
    pool.get(program.rng.below(pool.len()))
        .copied()
        .unwrap_or("")
}

/// True once in `one_in` draws, and never on an exhausted stream, whose zeros must stop growth.
fn rarely(program: &mut Program, one_in: usize) -> bool {
    program.rng.below(one_in) == one_in - 1
}

const FRAGMENT_ATOMS: &[&str] = &[
    "a",
    "b",
    "x",
    "1",
    "2L",
    "\"s\"",
    "TRUE",
    "NULL",
    "NA",
    "Inf",
    ".5",
    "1i",
    "..1",
    "...",
    "f",
    "T",
    "NA_integer_",
    "0x10",
    "1e3",
    "'q'",
    "`a b`",
];

/// Binary operators, each precedence level of R's grammar present, the common ones twice.
const FRAGMENT_BINARY: &[&str] = &[
    "^", "**", "-", "+", "-", "+", ":", "%%", "%in%", "%o%", "%||%", "|>", "*", "/", "*", "<", ">",
    "<=", ">=", "==", "!=", "&", "&&", "|", "||", "~", "->", "->>", "<-", "<-", "<<-", "=", "?",
    ":=", "$", "@", "::", ":::",
];

const FRAGMENT_UNARY: &[&str] = &["-", "+", "!", "~", "?", "-", "!"];

const FRAGMENT_NAMES: &[&str] = &[
    "a", ".a", "..a", "a.b", "a_b", "a1", ".1a", "_a", "a_", "..1", "...", "..", ".", "é", "naïve",
    "日本", "α1", "`a b`", "`a\\`b`", "`a\\nb`", "`if`", "`1`", "``", "`é`", "\"s\"", "'s'", "if",
    "NULL", "TRUE", "function", "in", "T", "NA", "Inf", "break", "a\u{a0}b", "ａ", "x1L", "e1",
    "`\\x41`", "`a\\\\b`", "1a", ".2", "`...`", "`_`",
];

/// Where a name can stand; `•` is the hole.
const FRAGMENT_NAME_TEMPLATES: &[&str] = &[
    "•",
    "• <- 1",
    "1 -> •",
    "• = 1",
    "x$•",
    "x@•",
    "x$•$•",
    "f(• = 1)",
    "f(• = )",
    "f(•)",
    "function(•) 1",
    "function(• = 2, •) •",
    "\\(•) •",
    "•::•",
    "•:::•",
    "•(1)",
    "x$•(1)",
    "x[• = 1]",
    "x[[•]]",
    "for (• in x) •",
    "• |> f()",
    "x |> •()",
    "x |> f(• = _)",
    "• + •",
    "-•",
    "• %in% •",
    "~•",
    "?•",
    "• ? •",
    "•$• <- •",
    "c(• = 1, • = 2)",
];

const FRAGMENT_SEPARATORS: &[&str] = &[
    " ", " ", " ", " ", "", "\n", "\n", " \n ", ";", " # c\n", "\n\n", "\t", "\n# c\n", "  ",
];

const FRAGMENT_ALPHABET: &[&str] = &[
    "a", "b", "x", "f", "1", "2L", "\"s\"", "TRUE", "NULL", "(", ")", "{", "}", "[", "]", "[[",
    "]]", ",", ";", "\n", "if", "else", "for", "in", "while", "repeat", "function", "\\", "break",
    "next", "+", "-", "*", "/", "^", "**", ":", "::", ":::", "$", "@", "~", "?", "!", "!=", "==",
    "<", ">", "<=", ">=", "&", "&&", "|", "||", "|>", "<-", "<<-", "->", "->>", "=", ":=", "%%",
    "%in%", "%", "_", "...", "..1", ".", "#", "'", "\"", "`", "\\(", "0x", "1e", "1i", ".5",
    "r\"(", ")\"", "=>", "<", "-", "=", ">", "|",
];

/// `links` operands joined by binary operators, each operand an atom with unary operators and
/// postfix forms, or an open-ended construct (`if`, a function) that takes everything after it.
fn fragment_chain(rng: &mut SplitMix64, tokens: &mut Vec<String>, links: usize) {
    for index in 0..links {
        if index > 0 {
            tokens.push(FRAGMENT_BINARY[rng.below(FRAGMENT_BINARY.len())].to_owned());
        }
        while rng.chance(1, 4) {
            tokens.push(FRAGMENT_UNARY[rng.below(FRAGMENT_UNARY.len())].to_owned());
        }
        match rng.below(14) {
            0 => {
                tokens.extend(["if", "(", "a", ")"].map(str::to_owned));
                if rng.chance(1, 2) {
                    let inner = 1 + rng.below((links - index).max(1));
                    fragment_chain(rng, tokens, inner);
                    tokens.push("else".to_owned());
                }
            }
            1 => tokens.extend(["function", "(", "x", ")"].map(str::to_owned)),
            2 => tokens.extend(["\\", "(", "x", ")"].map(str::to_owned)),
            3 => {
                tokens.extend(["for", "(", "i", "in", "x", ")"].map(str::to_owned));
            }
            4 => tokens.extend(["while", "(", "a", ")"].map(str::to_owned)),
            5 => tokens.push("repeat".to_owned()),
            _ => {
                tokens.push(FRAGMENT_ATOMS[rng.below(FRAGMENT_ATOMS.len())].to_owned());
                while rng.chance(1, 5) {
                    let postfix: &[&str] = match rng.below(5) {
                        0 => &["(", ")"],
                        1 => &["(", "y", ")"],
                        2 => &["[", "1", "]"],
                        3 => &["[[", "1", "]]"],
                        _ => &["$", "y"],
                    };
                    tokens.extend(postfix.iter().map(|token| (*token).to_owned()));
                }
                continue;
            }
        }
        // The open-ended construct's body is the next operand.
        tokens.push(FRAGMENT_ATOMS[rng.below(FRAGMENT_ATOMS.len())].to_owned());
    }
}

/// A nested expression, as tokens. `depth` bounds the nesting.
fn fragment_expression(rng: &mut SplitMix64, tokens: &mut Vec<String>, depth: usize) {
    let push = |tokens: &mut Vec<String>, token: &str| tokens.push(token.to_owned());
    if depth == 0 {
        push(tokens, FRAGMENT_ATOMS[rng.below(FRAGMENT_ATOMS.len())]);
        return;
    }
    match rng.below(20) {
        0 | 1 => {
            let links = 2 + rng.below(3);
            fragment_chain(rng, tokens, links);
        }
        2 => {
            push(tokens, FRAGMENT_UNARY[rng.below(FRAGMENT_UNARY.len())]);
            fragment_expression(rng, tokens, depth - 1);
        }
        3 | 4 => {
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, FRAGMENT_BINARY[rng.below(FRAGMENT_BINARY.len())]);
            fragment_expression(rng, tokens, depth - 1);
        }
        5 => {
            push(tokens, "(");
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, ")");
        }
        6 => {
            push(tokens, "{");
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, [";", "\n", ";", "\n", ";;", "\n\n"][rng.below(6)]);
                }
                fragment_expression(rng, tokens, depth - 1);
            }
            push(tokens, "}");
        }
        7 | 8 => {
            push(tokens, "if");
            push(tokens, "(");
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, ")");
            fragment_expression(rng, tokens, depth - 1);
            if rng.chance(2, 3) {
                push(tokens, "else");
                fragment_expression(rng, tokens, depth - 1);
            }
        }
        9 => {
            push(tokens, "for");
            push(tokens, "(");
            push(tokens, FRAGMENT_NAMES[rng.below(8)]);
            push(tokens, "in");
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, ")");
            fragment_expression(rng, tokens, depth - 1);
        }
        10 => {
            if rng.chance(1, 2) {
                push(tokens, "while");
                push(tokens, "(");
                fragment_expression(rng, tokens, depth - 1);
                push(tokens, ")");
            } else {
                push(tokens, "repeat");
            }
            fragment_expression(rng, tokens, depth - 1);
        }
        11 | 12 => {
            push(tokens, ["function", "function", "\\"][rng.below(3)]);
            push(tokens, "(");
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, ",");
                }
                push(
                    tokens,
                    ["x", "y", "...", "a.b", "`a b`", ".z"][rng.below(6)],
                );
                if rng.chance(1, 3) {
                    push(tokens, "=");
                    fragment_expression(rng, tokens, depth - 1);
                }
            }
            push(tokens, ")");
            fragment_expression(rng, tokens, depth - 1);
        }
        13..=15 => {
            fragment_expression(rng, tokens, depth - 1);
            let (open, close) = [("(", ")"), ("(", ")"), ("[", "]"), ("[[", "]]")][rng.below(4)];
            push(tokens, open);
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, ",");
                }
                match rng.below(8) {
                    0 => {}
                    1 => {
                        push(
                            tokens,
                            ["n", "\"n\"", "`n m`", "NULL", "if", "'n'"][rng.below(6)],
                        );
                        push(tokens, "=");
                        if rng.chance(3, 4) {
                            fragment_expression(rng, tokens, depth - 1);
                        }
                    }
                    _ => fragment_expression(rng, tokens, depth - 1),
                }
            }
            push(tokens, close);
        }
        16 => {
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, ["$", "@", "$", "::", ":::"][rng.below(5)]);
            push(tokens, FRAGMENT_NAMES[rng.below(FRAGMENT_NAMES.len())]);
        }
        17 => {
            fragment_expression(rng, tokens, depth - 1);
            push(tokens, "|>");
            push(tokens, "f");
            push(tokens, "(");
            if rng.chance(1, 2) {
                push(tokens, "y");
                push(tokens, "=");
                push(tokens, "_");
            }
            push(tokens, ")");
        }
        18 => push(
            tokens,
            ["break", "next", "NULL", "function(x) x"][rng.below(4)],
        ),
        _ => push(tokens, FRAGMENT_ATOMS[rng.below(FRAGMENT_ATOMS.len())]),
    }
}

/// A numeric literal built part by part, any part of which may be missing or doubled.
fn fragment_number(rng: &mut SplitMix64) -> String {
    const FIXED: &[&str] = &[
        "0x",
        "1e",
        "1e+",
        "1e-",
        "0x1p",
        "0x1p+",
        "1.5L",
        "1e3L",
        "1e-3L",
        "0x10L",
        "1i",
        "1e-2i",
        "100000000000L",
        "2147483647L",
        "2147483648L",
        "1e400",
        "1e-400",
        "0x1.8p1",
        "0x.8p1",
        "0x1.p1",
        "0x1.8",
        "1..2",
        "1.2.3",
        "1 .2",
        "5.",
        ".5",
        ".",
        "0x1g",
        "1e5e5",
        "1L2",
        "1iL",
        "1Li",
        "0x1i",
        "0x1L",
        "1l",
        "1E5",
        "1e+05L",
        "0X1F",
        "0x1P3",
        "1_000",
        "00012",
        "0.0L",
        "1e0L",
        "0xFFFFFFFFFFFFFFFFFFFF",
        "123456789012345678901234567890",
        "4.9e-324",
        "1.0e+00i",
        ".5e",
        "0.5e1L",
        "1.0L",
        "0x1p-1074",
        "1e1000L",
        "0xfp2L",
        "0b101",
        "1e 5",
        "1 e5",
        "1e5.5",
        "0x1.1",
        "9007199254740993",
        "0.1e-1i",
        ".e5",
        "0x.p1",
        "1L.5",
        "TRUE1",
        "1TRUE",
        "1x",
        "0xg",
        ".5.5",
        "1.1.1.1",
        "..5",
        "0x1.2.3p1",
    ];
    if rng.chance(1, 3) {
        return FIXED[rng.below(FIXED.len())].to_owned();
    }
    let mut text = String::new();
    let hex = rng.chance(1, 4);
    let digits = if hex {
        "0123456789abcdefABCDEF"
    } else {
        "0123456789"
    };
    let run = |rng: &mut SplitMix64, text: &mut String, most: usize| {
        for _ in 0..rng.below(most + 1) {
            text.push(char::from(digits.as_bytes()[rng.below(digits.len())]));
        }
    };
    if hex {
        text.push_str(["0x", "0X"][rng.below(2)]);
    }
    run(rng, &mut text, 4);
    if rng.chance(1, 2) {
        text.push('.');
        run(rng, &mut text, 4);
        if rng.chance(1, 8) {
            text.push('.');
            run(rng, &mut text, 2);
        }
    }
    if rng.chance(1, 2) {
        let markers = if hex {
            ["p", "P", "p", "e"]
        } else {
            ["e", "E", "e", "p"]
        };
        text.push_str(markers[rng.below(4)]);
        text.push_str(["", "", "+", "-"][rng.below(4)]);
        for _ in 0..rng.below(4) {
            text.push(char::from(b'0' + rng.below(10) as u8));
        }
    }
    text.push_str(["", "", "", "L", "i", "l", "I"][rng.below(7)]);
    text
}

/// A string literal: a plain or a raw one, from pieces that each exercise one rule of the lexer.
fn fragment_string(rng: &mut SplitMix64) -> String {
    const PIECES: &[&str] = &[
        "a",
        " ",
        "é",
        "日",
        "😀",
        "\\n",
        "\\t",
        "\\\\",
        "\\\"",
        "\\'",
        "\\`",
        "\\a",
        "\\b",
        "\\f",
        "\\v",
        "\\r",
        "\\0",
        "\\x41",
        "\\x4",
        "\\xg",
        "\\x",
        "\\xe4",
        "\\101",
        "\\7",
        "\\18",
        "\\400",
        "\\377",
        "\\u00e4",
        "\\u{e4}",
        "\\u{1F600}",
        "\\U0001F600",
        "\\U{110000}",
        "\\u{}",
        "\\u",
        "\\u{e4",
        "\\ue4z",
        "\\U{0}",
        "\\u0000",
        "\\x00",
        "\\q",
        "\\ ",
        "\\\n",
        "\n",
        "\t",
        "'",
        "\"",
        "#",
        "\\U0010FFFF",
        "\\u{00e4}x",
        "\\xc3\\xa9",
        "\\8",
        "\\e",
        "%",
        "\\N",
    ];
    if rng.chance(1, 4) {
        let dashes = "-".repeat(rng.below(3));
        let (open, close) = [("(", ")"), ("[", "]"), ("{", "}")][rng.below(3)];
        let quote = ["\"", "'"][rng.below(2)];
        let mut text = format!("{}{quote}{dashes}{open}", ["r", "R"][rng.below(2)]);
        for _ in 0..rng.below(5) {
            text.push_str(
                [
                    "a", "\\", "\\n", "\"", "'", ")", "]", "}", "-", ")\"", "\n", "é", ")-", "(",
                ][rng.below(14)],
            );
        }
        // A closing delimiter that does not match leaves the string open.
        match rng.below(8) {
            0 => text.push_str(&format!("{close}{quote}")),
            1 => text.push_str(&format!(")-{quote}")),
            2 => text.push_str(&format!("{close}{dashes}")),
            _ => text.push_str(&format!("{close}{dashes}{quote}")),
        }
        return text;
    }
    let quote = ["\"", "\"", "'"][rng.below(3)];
    let mut text = quote.to_owned();
    for _ in 0..rng.below(6) {
        let piece = PIECES[rng.below(PIECES.len())];
        // A bare quote of the string's own kind ends it, which is a case of its own.
        if piece == quote && !rng.chance(1, 6) {
            continue;
        }
        text.push_str(piece);
    }
    if !rng.chance(1, 12) {
        text.push_str(quote);
    }
    text
}
