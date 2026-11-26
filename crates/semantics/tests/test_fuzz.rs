//! Fuzz harness for the semantic pipeline. Each seed is one session, a project walked through
//! a few edits on a database that stays warm across seeds, so `FUZZ_SEED` replays a failure and
//! `FUZZ_ITERS` sets the budget.

use {
    rustc_hash::FxHashMap,
    semantics::{
        AnalysisSettings, DocumentKind, ProjectFiles, RootDatabase,
        metadata::resolve_namespace_conditionals,
        stubs::shipped_stub_sources,
        testing::{
            ProjectState, Workbench, check_metamorphic_relations, check_package_metadata,
            check_semantics_input, check_stub_source, command_database, compare_renamed,
            compare_renderings, fixture_state, for_each_session, for_each_stub_source,
            generate_session, insert_leading_trivia, misplaced_declarations, plan_rename,
            render_afresh, render_in_parallel, render_stub_source, stub_variant,
        },
    },
    std::{slice, sync::LazyLock},
    syntax::{
        SyntaxKind, SyntaxNode,
        ast::is_expression_kind,
        testing::{
            SplitMix64, fixture_case_sources, for_each_seed_on_named_threads, mutate_seed,
            program_corpus_sources, small_edit, split_files, token_soup,
        },
    },
};

const NAMES: &[&str] = &["a", "b", "f", "g", "x", "helper"];

/// `{n}` is the defined name, `{m}` another from the pool; the collisions are the point.
const TEMPLATES: &[&str] = &[
    "{n} <- {m}",
    "{n} <- {n} + 1L",
    "{n} <- function(p) {m}(p)",
    "{n} <- function(p) p + 1L",
    "{n} <- function() {n}()",
    "{n} <- c({m}, 1L)",
    "{n} <- list(x = 1, y = {m})",
    "{n} <- if ({m} > 1L) {m} else NULL",
    "{n} <- function(p) if (is.null(p)) 0L else p",
    "for (i in 1:3) {n} <- {m} + i",
    "{n} <- local({ t <- {m}; t })",
    "{n} <- \\(q) q + {m}",
    "print({m})",
    "{n} <- sum({m}, 1L)",
    "{n} <- {m}$field",
    "{n} <- {m}[[1L]]",
    "#: fn(x: integer) -> integer\n{n} <- function(x) x",
    "#: fn(x: integer) -> integer\n{n} <- function(x) {m}(x)",
    "#: integer\n{n} <- 1L",
    "#: @alias Alias {integer}\n\n#: Alias\n{n} <- {m}",
    "#: @type Point {list{v: double}}\n\n#: @new Point\n{n} <- list(v = 1)",
    "#: @trust fn(x: integer) -> character\n{n} <- function(x) x",
    "#: @param x {integer}\n#: @return {integer}\n{n} <- function(x) x",
    "#: @type Bad {integer}\n#: integer\n{n} <- 1L",
    "#: @alias Loop {Loop}\n\n#: Loop\n{n} <- 1L",
    "{n}: integer <- {m}",
    "{n}: list[named: double] = list()",
    "{n} <- function(x: integer, y = {m}) -> integer x",
    "{n} <- function<T>(x: T, ...: character) -> T {m}(x)",
    "#: fn(x: double) -> double\n{n} <- function(x: integer) x",
    "#: double\n{n}: integer <- {m}",
    "{n} <- lapply(1:3, \\(x: integer) -> integer {m}(x))",
    "{n} <- function() {\n  local_value: character <- {m}\n  local_value\n}",
    "{n} <- {m}[x > 1L, .(s = sum(y)), by = z]",
    "{n} <- with({m}, x + y)",
    "{n} <- stats::sd(c(1, 2))",
    "{n} <- nosuchpkg::mystery()",
    "{n} <- {m} |> print()",
    "{n} <- y ~ {m}",
    "repeat break",
    "while ({m} > 0L) {n} <- {n} - 1L",
    "{n} <- function(...) list(...)",
    "{n} <- switch({m}, a = 1L, b = 2L)",
    "{n} <- (x = {m}, y = 1L)",
    "{n} <- ({m}, \"a\")",
    "{n} <- ({m},)",
    "{n} <- {m}$x + {m}[[1L]]",
    "{n} <- if ({m}) :Some({m}) else :None",
    "{n} <- :Rect(width = {m}, height = 2)",
    "{n} <- match ({m}) {\n  :Some(v) if (v > 0L) => v\n  :Some(_) | :None => 0L\n}",
    "{n} <- match ({m}) {\n  (x = a, ..) => a\n  (p, q) => p\n  _ => {m}\n}",
    "{n} <- match ({m}) {\n  TRUE => 1L\n  FALSE => 2L\n}",
    "#: fn(o: :Some(integer) | :None) -> integer\n{n} <- function(o) match (o) {\n  :Some(v) => v\n}",
    "#: fn(r: :Ok(integer) | ..) -> integer\n{n} <- function(r) match (r) {\n  :Ok(v) => v\n  _ => {m}\n}",
    "#: @alias Shape {:Circle(radius: double) | :Empty}\n\n#: Shape\n{n} <- :Circle(radius = {m})",
    "#: @pure\n{n} <- function(p) {m}(p)",
    "#: @pure fn(p: integer) -> integer\n{n} <- function(p) p",
    "{n} <- @pure function(p) {m}(p)",
    "{n} <- @pure \\(q) q + {m}",
    "{n} <- {m}(1L)",
    "{n} <<- {m}",
    "names({n}) <- {m}",
    "{n} <- ({m} <- 1L) + 1L",
    "{n} <- memoise::memoise({m})",
    "if ({m}) {n} <- @pure function() 1L",
    "{n} <- local({\n  for (i in 1:2) {m} <- i\n  {m}\n})",
    "{n} <- [{m}, 1, 2]",
    "{n} <- []",
    "#: List<I64>\n{n} <- [1L, {m}]",
    "{n} <- [{m}] + [1] * {m}",
    "{n} <- !([{m}] > 1) & [TRUE]",
    "{n} <- {m}[[1]] + {m}[[{m}]]",
    "{n} <- {m}[{m} > 0]",
    "for (i in [{m}, 2L]) {n} <- i",
    "{n} <- List.map([{m}], \\(v) v * 2)",
    "{n} <- List.from_r({m})",
    "{n} <- [({m}, 1L), (2L, {m})]",
    "#: List<double[]>\n{n} <- [{m}]",
    // Two names one alternative lacks are reported at one range, in the order of their
    // symbols, so the child process of the session arms sees an order that follows ids.
    "{n} <- match ({m}) {\n  :Pair(zorp_{n}, quib_{n}) | :Empty => 1L\n  _ => 2L\n}",
];

const DIRECTIVES: &[&str] = &[
    "# typing: strict",
    "# typing: on",
    "# typing: off",
    "#: @strict",
];

const SOUP: &[&str] = &[
    "x",
    "f",
    "(",
    ")",
    "{",
    "}",
    "[",
    "[[",
    "]",
    ",",
    "\n",
    " ",
    "<-",
    "+",
    "if",
    "else",
    "function",
    "1L",
    "\"s\"",
    "%in%",
    "|>",
    "~",
    "$",
    "::",
    "#:",
    "@type",
    "fn",
    "list{",
    "NULL",
    "...",
    ":=",
    "<<-",
    "\\",
    "@new",
    "match",
    "=>",
    ":Some",
    ":None",
    "_",
    "..",
    "|",
    "@pure",
    "List<",
    ">",
    "List.from_r",
    "[1, 2]",
];

/// A program of colliding definitions: few names, so files of one project bind the same ones.
fn colliding_program(rng: &mut SplitMix64) -> String {
    let mut lines = Vec::new();
    if rng.chance(1, 6) {
        lines.push(DIRECTIVES[rng.below(DIRECTIVES.len())].to_owned());
        lines.push(String::new());
    }
    let item_count = 1 + rng.below(7);
    for _ in 0..item_count {
        let template = TEMPLATES[rng.below(TEMPLATES.len())];
        let defined = NAMES[rng.below(NAMES.len())];
        let mentioned = NAMES[rng.below(NAMES.len())];
        lines.push(template.replace("{n}", defined).replace("{m}", mentioned));
    }
    lines.join("\n")
}

fn soup(rng: &mut SplitMix64) -> String {
    let length = 1 + rng.below(60);
    let mut source = String::new();
    for _ in 0..length {
        source.push_str(SOUP[rng.below(SOUP.len())]);
    }
    source
}

/// Inputs that once broke an invariant, pinned without the gitignored fuzz corpus.
const REGRESSIONS: &[&str] = &[
    "foo$\n",
    "f)\nla<- function()",
    // Writes the syntactic write screen must see: a nested write, a destructure, a by-reference
    // rename, and a superassignment inside a nested function.
    "x <- (y <- 1L)\n",
    "c(a, b) %<-% list(1L, 2L)\n",
    "frame <- data.frame(a = 1L)\nsetnames(frame, \"a\", \"b\")\n",
    "dt <- 1\ndt[, `:=`(b = 2L)]\n",
    "dt <- 1\ndt[, ':='(b = 2L)][]\n",
    "frame <- data.frame(a = 1L)\n`setnames`(frame, \"a\", \"b\")\n",
    "setGeneric(\"area\", { y <- 1; function(obj) standardGeneric(\"area\") })\n",
    "x <- list(f = function() y <<- 1L)\n",
];

/// The files a session starts from or takes a statement from, besides the programs the grammar
/// generates: every fixture case of every suite (a multi-file case as its files), the mined
/// program corpus, colliding programs, token soup and the regressions.
static POOL: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut pool: Vec<String> = REGRESSIONS
        .iter()
        .map(|input| (*input).to_owned())
        .collect();
    for (_, source) in fixture_case_sources() {
        match split_files(&source) {
            Some(files) => pool.extend(files.into_iter().map(|(_, text)| text)),
            None => pool.push(source),
        }
    }
    pool.extend(
        program_corpus_sources()
            .into_iter()
            .map(|(_, source)| source),
    );
    let mut rng = SplitMix64(0xC011_1DE5);
    pool.extend((0..600).map(|_| colliding_program(&mut rng)));
    pool.extend((0..150).map(|_| soup(&mut rng)));
    pool
});

/// The default budget of [`fuzz_sessions`]; each seed is one session of two to four project
/// states.
const SESSIONS: usize = 600;

#[test]
fn fuzz_sessions() {
    for_each_session(SESSIONS, |seed| {
        generate_session(&mut SplitMix64(0x5E55_10A5 ^ seed), &POOL)
    });
}

/// Every fixture case as the project its suite checks it as, then with one small edit. The
/// cases are dealt out to two tests, so the default run checks them on two cores.
#[test]
fn fixture_sources_hold_invariants() {
    check_fixture_sources(0);
}

#[test]
fn fixture_sources_hold_invariants_odd() {
    check_fixture_sources(1);
}

fn check_fixture_sources(parity: usize) {
    let cases = fixture_case_sources();
    assert!(
        cases.len() > 2_000,
        "expected every fixture suite, found {} cases",
        cases.len()
    );
    for_each_session(cases.len().div_ceil(2), |seed| {
        let (id, source) = &cases[(seed as usize * 2 + parity) % cases.len()];
        let kind = if id.starts_with("semantics/tests/typing-scripts::") {
            DocumentKind::Script
        } else {
            DocumentKind::Package
        };
        let mut state = fixture_state(source, kind);
        state.strict = id.starts_with("semantics/tests/typing-strict::");
        let mut rng = SplitMix64(0xF1C5_7A7E ^ seed);
        let mut edited = state.clone();
        let target = rng.below(edited.files.len());
        edited.files[target].1 = small_edit(&mut rng, &edited.files[target].1, "probe <- 1L\n");
        vec![state, edited]
    });
}

/// The mined corpus as package projects of eight files, each then with one small edit.
#[test]
fn program_corpus_holds_invariants() {
    let sources = program_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    let projects: Vec<&[(String, String)]> = sources.chunks(8).collect();
    for_each_session(projects.len(), |seed| {
        let state = ProjectState {
            files: projects[seed as usize % projects.len()]
                .iter()
                .enumerate()
                .map(|(key, (_, source))| (key, source.clone(), DocumentKind::Package))
                .collect(),
            strict: false,
            prelude: true,
        };
        let mut rng = SplitMix64(0xC0A9_05E5 ^ seed);
        let mut edited = state.clone();
        let target = rng.below(edited.files.len());
        let donor = &sources[rng.below(sources.len())].1;
        edited.files[target].1 = small_edit(&mut rng, &edited.files[target].1, donor);
        vec![state, edited]
    });
}

/// The default budget of [`edits_that_change_no_meaning_only_move_findings`].
const METAMORPHIC_CASES: usize = 400;

/// The metamorphic relations over the first state of a generated session. A run of the default
/// budget must find a place for each edit in most cases, or its preconditions have drifted so
/// far that the oracle checks nothing.
#[test]
fn edits_that_change_no_meaning_only_move_findings() {
    let mut workbench = Workbench::default();
    let mut leading_trivia = 0;
    let mut renames = 0;
    let mut free_renames = 0;
    let mut renames_in_findings = 0;
    let mut cases = 0;
    for_each_seed_on_named_threads(METAMORPHIC_CASES, |seed| {
        let (state, mut rng) = metamorphic_case(seed);
        let coverage = check_metamorphic_relations(&mut workbench, &state, &mut rng);
        cases += 1;
        leading_trivia += usize::from(coverage.leading_trivia);
        renames += usize::from(coverage.rename);
        free_renames += usize::from(coverage.free_rename);
        renames_in_findings += usize::from(coverage.rename_in_finding);
    });
    eprintln!(
        "{cases} cases: {leading_trivia} with a line inserted, {renames} renamed, \
         {free_renames} of them freely and {renames_in_findings} of a name in a finding"
    );
    if cases >= METAMORPHIC_CASES {
        assert!(
            leading_trivia * 2 > cases
                && renames * 4 > cases
                && free_renames * 8 > cases
                && renames_in_findings * 8 > cases,
            "{cases} cases: only {leading_trivia} with a line inserted, {renames} renamed, \
             {free_renames} freely and {renames_in_findings} of a name in a finding"
        );
    }
}

/// The project of a metamorphic case, and the generator that goes on to place its edits.
fn metamorphic_case(seed: u64) -> (ProjectState, SplitMix64) {
    let mut rng = SplitMix64(0x3E7A_3021 ^ seed);
    let mut state = generate_session(&mut rng, &POOL).swap_remove(0);
    if rng.below(2) == 0 {
        add_unresolved_reads(&mut rng, &mut state);
    }
    (state, rng)
}

/// The trivia relation reports an insertion whose findings stay where they were: the
/// rendering of the unedited project stands in for that of the edited one.
#[test]
fn trivia_oracle_catches_unmoved_findings() {
    let mut db = command_database();
    let caught = (0..METAMORPHIC_CASES as u64).any(|seed| {
        let (state, mut rng) = metamorphic_case(seed);
        let original = render_afresh(&mut db, &state);
        insert_leading_trivia(&state, &original, &mut rng)
            .is_some_and(|trivia| compare_renderings(&trivia.expected, &original).is_err())
    });
    assert!(
        caught,
        "no case of {METAMORPHIC_CASES} reported findings that did not move"
    );
}

/// A subscript's argument names belong to its method, which reads them by spelling: renaming a
/// table's `roll` changes a rolling join into an equi-join and drops its finding (`FUZZ_SEED=177`
/// before the renamer knew).
#[test]
fn renames_leave_subscript_argument_names_alone() {
    let db = command_database();
    let state = fixture_state(
        "joined <- events[windows, on = .(id, t >= lo), roll = TRUE]\n\
         roll <- joined\n",
        DocumentKind::Test,
    );
    let original = vec![String::new()];
    for seed in 0..64 {
        let Some(rename) = plan_rename(&db, &state, &original, &mut SplitMix64(seed)) else {
            continue;
        };
        let (_, text, _) = &rename.edited.files[0];
        assert!(
            text.contains("roll = TRUE"),
            "seed {seed} renamed a subscript's argument name:\n{text}"
        );
    }
}

/// The rename relation reports a renamed project that lost a finding: the real rendering of the
/// renamed project, less one finding line.
#[test]
fn rename_oracle_catches_a_lost_finding() {
    let mut db = command_database();
    let caught = (0..METAMORPHIC_CASES as u64).any(|seed| {
        let (state, mut rng) = metamorphic_case(seed);
        let original = render_afresh(&mut db, &state);
        // The trivia edit draws first, so the rename is the one the relation test makes.
        insert_leading_trivia(&state, &original, &mut rng);
        let Some(rename) = plan_rename(&db, &state, &original, &mut rng) else {
            return false;
        };
        let mut renamed = render_afresh(&mut db, &rename.edited);
        let Some(rendering) = renamed
            .iter_mut()
            .find(|rendering| rendering.lines().any(is_finding_line))
        else {
            return false;
        };
        *rendering = without_first_finding(rendering);
        compare_renamed(&rename.rename, &original, &renamed).is_err()
    });
    assert!(
        caught,
        "no case of {METAMORPHIC_CASES} reported a renamed project that lost a finding"
    );
}

/// The parallel relation reports a worker whose rendering of one file lists the same lines in
/// another order: the real rendering of the first worker with one file's lines reversed.
#[test]
fn parallel_oracle_catches_a_reordered_rendering() {
    let mut db = command_database();
    let caught = (0..SESSIONS as u64).any(|seed| {
        let state = generate_session(&mut SplitMix64(0x5E55_10A5 ^ seed), &POOL).swap_remove(0);
        let sequential = render_afresh(&mut db, &state);
        let mut parallel = render_in_parallel(&mut db, &state);
        let Some(rendering) = parallel.first_mut().and_then(|worker| {
            worker.iter_mut().find(|rendering| {
                let lines: Vec<&str> = rendering.lines().collect();
                lines.windows(2).any(|pair| pair[0] != pair[1])
            })
        }) else {
            return false;
        };
        *rendering = rendering
            .lines()
            .rev()
            .map(|line| format!("{line}\n"))
            .collect();
        parallel
            .iter()
            .any(|worker| compare_renderings(&sequential, worker).is_err())
    });
    assert!(
        caught,
        "no session of {SESSIONS} reported a reordered parallel rendering"
    );
}

/// Whether a line of a [`render_semantics`] rendering is a finding (`START..END [code] ...`).
fn is_finding_line(line: &str) -> bool {
    line.split_once(' ').is_some_and(|(range, rest)| {
        range.split_once("..").is_some_and(|(start, end)| {
            start.parse::<usize>().is_ok() && end.parse::<usize>().is_ok()
        }) && rest.starts_with('[')
    })
}

/// `rendering` without its first finding line.
fn without_first_finding(rendering: &str) -> String {
    let mut dropped = false;
    rendering
        .split_inclusive('\n')
        .filter(|line| {
            let drop = !dropped && is_finding_line(line);
            dropped |= drop;
            !drop
        })
        .collect()
}

/// Appends reads of names nothing defines to a file of `state`, so a rename meets `unresolved`
/// findings, which generated programs rarely have. The names vary in length and first letter,
/// because a rename to a free name crosses both.
fn add_unresolved_reads(rng: &mut SplitMix64, state: &mut ProjectState) {
    let Some(file) = (!state.files.is_empty()).then(|| rng.below(state.files.len())) else {
        return;
    };
    for index in 0..1 + rng.below(3) {
        let name: String = (0..3 + rng.below(8))
            .map(|position| match position > 0 && rng.below(5) == 0 {
                true => '_',
                false => (b'a' + rng.below(26) as u8) as char,
            })
            .collect();
        let read = match rng.below(3) {
            0 => format!("unresolved_{index} <- {name}"),
            1 => format!("{name}(1L)"),
            _ => format!("reader_{index} <- function(x) {name}(x)"),
        };
        let text = &mut state.files[file].1;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&read);
        text.push('\n');
    }
}

/// The words and punctuation of the `.Rtypes` grammar, for token soup.
const STUB_ALPHABET: &[&str] = &[
    "name",
    "zoo",
    "[.zoo",
    "%+%",
    " : ",
    ":",
    "fn(",
    ")",
    "(",
    "[x]",
    "x: ",
    "...: ",
    "Any",
    "integer",
    "double[]",
    "list{",
    "}",
    "<T>",
    "T",
    " -> ",
    "|",
    ", ",
    "@type ",
    "@alias ",
    "@pure",
    "@pure(FUN)",
    "@masked",
    "@opaque(x)",
    "#' ",
    "# ",
    "\n",
    "\n\n",
    " ",
    "`",
    "\"",
    "é",
];

/// Stub sources that once broke the loader.
const STUB_REGRESSIONS: &[&str] = &[
    // A short type name also occurs inside the `@type` or `@alias` keyword, where its
    // declaration range once pointed.
    "@type t\n",
    "@alias a {integer}\n",
    // A keyword glued to a name is not that keyword: `@typet` declares no type `t`.
    "@typet\n@aliasa {integer}\n",
    // An alias that expands back into itself once overflowed the stack, and one that does so
    // through two positions took exponential time, when a declaration used it.
    "@alias z {z | integer}\nx : z\n",
    "@alias z {list[named: w]}\n@alias w {fn(y: z) -> Any | list[named: z]}\nx : fn(a: z) -> w\n",
];

/// Generated, windowed and mutated `.Rtypes` text through the stub loader
/// ([`check_stub_source`]), on one database that stays warm across seeds. Half the sources come
/// from the grammar of the shipped stubs ([`generated_stub`]), so most load declarations.
#[test]
fn fuzz_stub_sources() {
    let shipped: Vec<String> = shipped_stub_sources()
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let mut db = stub_test_database();
    for input in STUB_REGRESSIONS {
        check_stub_source(&mut db, input);
    }
    for_each_stub_source(STUB_SOURCES, |seed| stub_source(&shipped, seed));
}

/// The default budget of [`fuzz_stub_sources`].
const STUB_SOURCES: usize = 400;

/// The stub source of a seed: token soup, a window of a shipped stub, two windows mutated into
/// one, or a generated stub, perhaps with one small edit.
fn stub_source(shipped: &[String], seed: u64) -> String {
    let mut rng = SplitMix64(0x57AB_5EED ^ seed);
    let window = |rng: &mut SplitMix64| {
        let lines: Vec<&str> = shipped[rng.below(shipped.len())].lines().collect();
        let start = rng.below(lines.len().max(1));
        let end = (start + 1 + rng.below(40)).min(lines.len());
        lines.get(start..end).unwrap_or_default().join("\n") + "\n"
    };
    match rng.below(6) {
        0 => token_soup(&mut rng, STUB_ALPHABET),
        1 => window(&mut rng),
        2 => {
            let seeds = [window(&mut rng), window(&mut rng)];
            mutate_seed(&mut rng, &[&seeds[0], &seeds[1]], 24)
        }
        _ => {
            let source = generated_stub(&mut rng);
            match rng.chance(1, 3) {
                true => {
                    let insertion = STUB_ALPHABET[rng.below(STUB_ALPHABET.len())];
                    small_edit(&mut rng, &source, insertion)
                }
                false => source,
            }
        }
    }
}

/// The stub range check reports a declaration whose range starts one byte late: the real
/// rendering of the library with every declared range's start moved by one.
#[test]
fn stub_range_oracle_catches_a_shifted_range() {
    let shipped: Vec<String> = shipped_stub_sources()
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let mut db = stub_test_database();
    let caught = (0..STUB_SOURCES as u64).any(|seed| {
        let text = stub_source(&shipped, seed);
        let rendering = render_stub_source(&mut db, &text);
        let shifted: String = rendering
            .lines()
            .map(|line| {
                let shifted = line.rsplit_once(" at ").and_then(|(head, range)| {
                    let (start, end) = range.split_once("..")?;
                    Some(format!(
                        "{head} at {}..{end}",
                        start.parse::<usize>().ok()? + 1
                    ))
                });
                shifted.unwrap_or_else(|| line.to_owned()) + "\n"
            })
            .collect();
        misplaced_declarations(&text, &shifted).is_err()
    });
    assert!(
        caught,
        "no stub of {STUB_SOURCES} reported a declaration range that starts late"
    );
}

/// The stub history relation reports a loader that keeps the library of the source it loaded
/// before: the real rendering of the variant stands in for the second load of the source.
#[test]
fn stub_history_oracle_catches_a_stale_library() {
    let shipped: Vec<String> = shipped_stub_sources()
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let mut db = stub_test_database();
    let caught = (0..STUB_SOURCES as u64).any(|seed| {
        let text = stub_source(&shipped, seed);
        let first = render_stub_source(&mut db, &text);
        let stale = render_stub_source(&mut db, &stub_variant(&text));
        compare_renderings(slice::from_ref(&first), slice::from_ref(&stale)).is_err()
    });
    assert!(
        caught,
        "no stub of {STUB_SOURCES} reported a library left from the source before"
    );
}

/// A database for stub sources: it installs none of the shipped stubs.
fn stub_test_database() -> RootDatabase {
    let db = RootDatabase::default();
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    db
}

/// Generated `NAMESPACE` and `DESCRIPTION` text through the metadata parsers and a project that
/// uses what they import ([`check_package_metadata`]), on one database that stays warm across
/// seeds. A third of the `NAMESPACE` files are mutated or token soup, so the parsers meet
/// broken directives.
#[test]
fn fuzz_package_metadata() {
    let mut db = command_database();
    for_each_seed_on_named_threads(50, |seed| {
        let mut rng = SplitMix64(0x3E7A_DA7A ^ seed);
        let generated = generated_namespace(&mut rng);
        let namespace = match rng.below(6) {
            0 => token_soup(&mut rng, NAMESPACE_DIRECTIVES),
            1 => mutate_seed(&mut rng, &[generated.as_str()], 16),
            _ => generated,
        };
        let description = generated_description(&mut rng);
        check_package_metadata(&mut db, &namespace, &description);
    });
}

/// `NAMESPACE` directives of every kind the parsers read, with some that R rejects.
const NAMESPACE_DIRECTIVES: &[&str] = &[
    "export(f)\n",
    "export(g, `h i`)\n",
    "export(\"quoted\")\n",
    "exportPattern(\"^[a-z]\")\n",
    "exportMethods(show)\n",
    "exportClasses(Thing)\n",
    "import(stats)\n",
    "import(utils, except = c(head, tail))\n",
    "importFrom(stats, median, sd)\n",
    "importFrom(\"utils\", \"head\")\n",
    "importClassesFrom(methods, show)\n",
    "importMethodsFrom(methods, show)\n",
    "S3method(print, thing)\n",
    "S3method(base::format, thing, format_thing)\n",
    "useDynLib(pkg, .registration = TRUE, .fixes = \"C_\")\n",
    "useDynLib(pkg, alias = routine, other)\n",
    "if (getRversion() >= \"4.0.0\") {\n  export(newer)\n}\n",
    "if (.Platform$OS.type == \"windows\") importFrom(utils, winDialog)\n",
    "# comment\n",
    "\n",
    "export(\n  f,\n  g\n)\n",
    "export(",
    ")",
    "importFrom(",
    "é",
];

/// A `NAMESPACE` of up to 60 directives, long enough that most split into chunks.
fn generated_namespace(rng: &mut SplitMix64) -> String {
    (0..rng.below(60))
        .map(|_| NAMESPACE_DIRECTIVES[rng.below(NAMESPACE_DIRECTIVES.len() - 4)])
        .collect()
}

/// A `DESCRIPTION` whose fields come in any order, some of them continued, empty or repeated.
fn generated_description(rng: &mut SplitMix64) -> String {
    const FIELDS: &[&str] = &[
        "Package: pkg\n",
        "Package:\n",
        "Imports: stats, utils (>= 4.0.0),\n    methods\n",
        "Depends: R (>= 4.1.0), data.table\n",
        "Suggests: testthat (>= 3.0.0)\n",
        "LazyData: true\n",
        "LazyData: no\n",
        "Collate: 'b.R' 'a.R'\n",
        "Collate.unix: 'unix/a.R'\n",
        "Collate.windows: \"windows/a.R\"\n",
        "Title: A Package\n",
        "  continued\n",
        "Broken\n",
        "\n",
    ];
    (0..rng.below(10))
        .map(|_| FIELDS[rng.below(FIELDS.len())])
        .collect()
}

/// Names a generated stub declares: short ones that also occur inside keywords, dotted and
/// operator names, and one beyond ASCII.
const STUB_NAMES: &[&str] = &[
    "t", "a", "ty", "x", "f", "zoo", "is.zoo", "[.zoo", "%+%", "as_thing", "média",
];

/// Type names a generated stub defines, short ones among them because they also occur inside
/// the `@type` and `@alias` keywords.
const STUB_NOMINALS: &[&str] = &["t", "a", "pe", "Shape", "zoo", "data.thing"];

/// A `.Rtypes` source from the grammar the shipped stubs use: documentation lines, `@type` and
/// `@alias` definitions (generic or not), and declarations of values and functions with
/// optional and variadic parameters, unions, generics and the `@pure`, `@masked` and `@opaque`
/// markers.
fn generated_stub(rng: &mut SplitMix64) -> String {
    let mut nominals: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    for _ in 0..1 + rng.below(10) {
        if rng.chance(1, 3) {
            lines.push(format!("#' Documents the next line, {}.", rng.below(100)));
        }
        let name = STUB_NAMES[rng.below(STUB_NAMES.len())];
        match rng.below(6) {
            0 => {
                let nominal = STUB_NOMINALS[rng.below(STUB_NOMINALS.len())].to_owned();
                lines.push(match rng.below(4) {
                    0 => format!("@type {nominal}"),
                    1 => format!("@type {nominal}<T>"),
                    2 => format!(
                        "@type {nominal} {{{}}}",
                        stub_type(rng, &nominals, false, 2)
                    ),
                    _ => format!(
                        "@alias {nominal}<T> {{{}}}",
                        stub_type(rng, &nominals, true, 2)
                    ),
                });
                nominals.push(nominal);
            }
            1 => {
                let alias = STUB_NOMINALS[rng.below(STUB_NOMINALS.len())].to_owned();
                lines.push(format!(
                    "@alias {alias} {{{}}}",
                    stub_type(rng, &nominals, false, 2)
                ));
                nominals.push(alias);
            }
            _ => {
                let generic = rng.chance(1, 3);
                let marker = match rng.below(6) {
                    0 => "@pure ",
                    1 => "@pure(f) ",
                    2 => "@masked ",
                    3 => "@opaque(x) ",
                    _ => "",
                };
                let binders = if generic { "<T> " } else { "" };
                let mut parameters: Vec<String> = Vec::new();
                for (index, parameter) in ["x", "f", "n"].iter().enumerate().take(rng.below(4)) {
                    let ty = match *parameter == "f" {
                        true => format!("fn({}) -> Any", stub_type(rng, &nominals, generic, 1)),
                        false => stub_type(rng, &nominals, generic, 2),
                    };
                    parameters.push(match index > 0 && rng.chance(1, 2) {
                        true => format!("[{parameter}]: {ty}"),
                        false => format!("{parameter}: {ty}"),
                    });
                }
                if rng.chance(1, 3) {
                    parameters.push("...: Any".to_owned());
                }
                let body = match rng.chance(1, 5) {
                    true => stub_type(rng, &nominals, false, 2),
                    false => format!(
                        "{binders}fn({}) -> {}",
                        parameters.join(", "),
                        stub_type(rng, &nominals, generic, 2)
                    ),
                };
                lines.push(format!("{name} : {marker}{body}"));
            }
        }
    }
    lines.join("\n") + "\n"
}

fn stub_type(rng: &mut SplitMix64, nominals: &[String], generic: bool, depth: usize) -> String {
    let leaves = ["integer", "double[]", "character", "logical", "Any", "NULL"];
    match rng.below(if depth == 0 { 3 } else { 7 }) {
        0 if generic => "T".to_owned(),
        1 if !nominals.is_empty() => nominals[rng.below(nominals.len())].clone(),
        3 => format!(
            "list{{v: {}}}",
            stub_type(rng, nominals, generic, depth - 1)
        ),
        4 => format!(
            "list[named: {}]",
            stub_type(rng, nominals, generic, depth - 1)
        ),
        5 => format!(
            "{} | {}",
            stub_type(rng, nominals, generic, depth - 1),
            stub_type(rng, nominals, generic, depth - 1)
        ),
        6 => format!(
            "fn(y: {}) -> Any",
            stub_type(rng, nominals, generic, depth - 1)
        ),
        _ => leaves[rng.below(leaves.len())].to_owned(),
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_semantics_input(input);
    }
}

#[test]
#[ignore = "long fuzz run for nightly/manual use"]
fn fuzz_deep() {
    for_each_session(SESSIONS * 20, |seed| {
        generate_session(&mut SplitMix64(0x5EED_0001 ^ seed), &POOL)
    });
}

/// A generated `NAMESPACE`: directives, `{` blocks and `if` directives nested at random, each
/// condition with the value the host would report for it.
enum Directive {
    Plain(String),
    Block(Vec<Directive>),
    If {
        condition: String,
        value: bool,
        then_branch: Box<Directive>,
        else_branch: Option<Box<Directive>>,
    },
}

/// Resolving the `if` directives of a random `NAMESPACE` with the values R's
/// `parseNamespaceFile` would see leaves exactly the directives on the branches it would take,
/// in file order; values that are missing, short or long leave the file unresolved.
#[test]
fn namespace_conditionals_resolve_to_the_taken_branches() {
    for_each_seed_on_named_threads(400, |seed| {
        let mut rng = SplitMix64(seed);
        let mut counter = 0;
        let directives: Vec<Directive> = (0..1 + rng.below(6))
            .map(|_| namespace_directive(&mut rng, 0, &mut counter))
            .collect();
        let mut source = String::new();
        for directive in &directives {
            render_directive(&mut rng, directive, &mut source);
            source.push('\n');
        }
        let mut expected = Vec::new();
        let mut values = FxHashMap::default();
        for directive in &directives {
            taken_plain(directive, &mut expected);
            conditional_values(directive, &source, &mut values);
        }
        let resolved = resolve_namespace_conditionals(&source, &values);
        assert!(resolved.resolved, "unresolved:\n{source}");
        assert_eq!(resolved.conditionals.len(), values.len(), "{source}");
        assert_eq!(
            flat_directives(&resolved.text),
            expected,
            "{source}\n=>\n{}",
            resolved.text
        );

        // Taking one directive's values away, or giving it one value too few or too many,
        // leaves that directive in the text and marks the file unresolved.
        if let Some(key) = resolved
            .conditionals
            .get(rng.below(resolved.conditionals.len()))
        {
            let mut damaged = values.clone();
            match rng.below(3) {
                0 => {
                    damaged.remove(key);
                }
                1 => {
                    if let Some(entry) = damaged.get_mut(key) {
                        entry.pop();
                    }
                }
                _ => {
                    if let Some(entry) = damaged.get_mut(key) {
                        entry.push(rng.chance(1, 2));
                    }
                }
            }
            let partial = resolve_namespace_conditionals(&source, &damaged);
            assert!(!partial.resolved, "damaged values resolved:\n{source}");
            assert!(partial.text.contains(key.as_str()), "{source}");
        }

        // Mutated text with arbitrary values never panics.
        let mutated = mutate_seed(&mut rng, &[source.as_str()], 24);
        let mut arbitrary = FxHashMap::default();
        for key in resolve_namespace_conditionals(&mutated, &arbitrary).conditionals {
            let values = (0..rng.below(4)).map(|_| rng.chance(1, 2)).collect();
            arbitrary.insert(key, values);
        }
        resolve_namespace_conditionals(&mutated, &arbitrary);
    });
}

fn namespace_directive(rng: &mut SplitMix64, depth: usize, counter: &mut usize) -> Directive {
    const PLAIN: &[&str] = &[
        "export(alpha)",
        "export(\"beta\", gamma)",
        "importFrom(stats, setNames)",
        "import(methods)",
        "S3method(print, widget)",
        "S3method(format, widget, format_widget)",
        "useDynLib(pkg, .registration = TRUE)",
        "exportPattern(\"^[^\\\\.]\")",
    ];
    match rng.below(if depth >= 3 { 1 } else { 4 }) {
        0 | 1 if depth >= 3 || rng.chance(1, 2) => {
            Directive::Plain(PLAIN[rng.below(PLAIN.len())].to_owned())
        }
        2 => Directive::Block(
            (0..rng.below(4))
                .map(|_| namespace_directive(rng, depth + 1, counter))
                .collect(),
        ),
        _ => {
            const CONDITIONS: &[&str] = &[
                "getRversion() >= \"4.1.0\"",
                ".Platform$OS.type == \"windows\"",
                "requireNamespace(\"x\", quietly = TRUE)",
                "TRUE",
            ];
            *counter += 1;
            // The counter keeps two directives' texts apart, since values are keyed by text.
            let condition = format!(
                "{} && {} > 0",
                CONDITIONS[rng.below(CONDITIONS.len())],
                counter
            );
            Directive::If {
                condition,
                value: rng.chance(1, 2),
                then_branch: Box::new(namespace_directive(rng, depth + 1, counter)),
                else_branch: rng
                    .chance(1, 2)
                    .then(|| Box::new(namespace_directive(rng, depth + 1, counter))),
            }
        }
    }
}

fn render_directive(rng: &mut SplitMix64, directive: &Directive, out: &mut String) {
    match directive {
        Directive::Plain(text) => out.push_str(text),
        Directive::Block(inner) => {
            out.push('{');
            for directive in inner {
                out.push_str(if rng.chance(1, 2) { "\n  " } else { "; " });
                render_directive(rng, directive, out);
            }
            if rng.chance(1, 3) {
                out.push_str(" # end of block");
            }
            out.push_str("\n}");
        }
        Directive::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str(&format!("if ({condition}) "));
            // An `else` must not start a line at top level, so a plain `then` branch followed
            // by `else` is braced.
            let brace = else_branch.is_some() && !matches!(**then_branch, Directive::Block(_));
            if brace {
                out.push_str("{ ");
            }
            render_directive(rng, then_branch, out);
            if brace {
                out.push_str(" }");
            }
            if let Some(else_branch) = else_branch {
                out.push_str(" else ");
                render_directive(rng, else_branch, out);
            }
        }
    }
}

/// The plain directives `parseNamespaceFile` keeps, in order.
fn taken_plain(directive: &Directive, out: &mut Vec<String>) {
    match directive {
        Directive::Plain(text) => out.push(text.clone()),
        Directive::Block(inner) => inner
            .iter()
            .for_each(|directive| taken_plain(directive, out)),
        Directive::If {
            value,
            then_branch,
            else_branch,
            ..
        } => {
            if *value {
                taken_plain(then_branch, out);
            } else if let Some(else_branch) = else_branch {
                taken_plain(else_branch, out);
            }
        }
    }
}

/// The values the host reports for each `if` directive the file reaches through `{` blocks
/// alone, keyed by its source text: its condition, then depth first the conditions on the
/// branch it takes.
fn conditional_values(
    directive: &Directive,
    source: &str,
    values: &mut FxHashMap<String, Vec<bool>>,
) {
    match directive {
        Directive::Plain(_) => {}
        Directive::Block(inner) => {
            for directive in inner {
                conditional_values(directive, source, values);
            }
        }
        Directive::If { condition, .. } => {
            let mut walked = Vec::new();
            branch_values(directive, &mut walked);
            let text = if_text(source, condition);
            values.insert(text, walked);
        }
    }
}

fn branch_values(directive: &Directive, out: &mut Vec<bool>) {
    match directive {
        Directive::Plain(_) => {}
        Directive::Block(inner) => inner
            .iter()
            .for_each(|directive| branch_values(directive, out)),
        Directive::If {
            value,
            then_branch,
            else_branch,
            ..
        } => {
            out.push(*value);
            if *value {
                branch_values(then_branch, out);
            } else if let Some(else_branch) = else_branch {
                branch_values(else_branch, out);
            }
        }
    }
}

/// The source text of the `if` directive with this condition, as the parser spans it.
fn if_text(source: &str, condition: &str) -> String {
    let root = syntax::parse(source).syntax_node();
    root.descendants()
        .find(|node| {
            node.kind() == SyntaxKind::IF_EXPR
                && node
                    .text()
                    .to_string()
                    .starts_with(&format!("if ({condition})"))
        })
        .map(|node| node.text().to_string())
        .unwrap_or_default()
}

/// The top-level expressions of a `NAMESPACE`, `{` blocks flattened, as text.
fn flat_directives(source: &str) -> Vec<String> {
    let root = syntax::parse(source).syntax_node();
    let mut out = Vec::new();
    let mut pending: Vec<SyntaxNode> = root
        .children()
        .filter(|node| is_expression_kind(node.kind()))
        .collect();
    pending.reverse();
    while let Some(node) = pending.pop() {
        if node.kind() == SyntaxKind::BRACE_EXPR {
            let mut inner: Vec<SyntaxNode> = node
                .children()
                .filter(|child| is_expression_kind(child.kind()))
                .collect();
            inner.reverse();
            pending.extend(inner);
        } else {
            out.push(node.text().to_string());
        }
    }
    out
}
