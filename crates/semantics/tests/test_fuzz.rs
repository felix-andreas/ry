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
            ProjectState, Workbench, check_metamorphic_relations, check_semantics_input,
            check_stub_source, fixture_state, for_each_session, generate_session,
        },
    },
    std::sync::LazyLock,
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
    let mut cases = 0;
    for_each_seed_on_named_threads(METAMORPHIC_CASES, |seed| {
        let mut rng = SplitMix64(0x3E7A_3021 ^ seed);
        let states = generate_session(&mut rng, &POOL);
        let coverage = check_metamorphic_relations(&mut workbench, &states[0], &mut rng);
        cases += 1;
        leading_trivia += usize::from(coverage.leading_trivia);
        renames += usize::from(coverage.rename);
    });
    eprintln!("{cases} cases: {leading_trivia} with a line inserted, {renames} renamed");
    if cases >= METAMORPHIC_CASES {
        assert!(
            leading_trivia * 2 > cases && renames * 4 > cases,
            "{cases} cases: only {leading_trivia} with a line inserted and {renames} renamed"
        );
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
const STUB_REGRESSIONS: &[&str] = &[];

/// Generated, windowed and mutated `.Rtypes` text through the stub loader
/// ([`check_stub_source`]), on one database that stays warm across seeds.
#[test]
fn fuzz_stub_sources() {
    let shipped: Vec<String> = shipped_stub_sources()
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let mut db = RootDatabase::default();
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    for input in STUB_REGRESSIONS {
        check_stub_source(&mut db, input);
    }
    for_each_seed_on_named_threads(400, |seed| {
        let mut rng = SplitMix64(0x57AB_5EED ^ seed);
        let window = |rng: &mut SplitMix64| {
            let lines: Vec<&str> = shipped[rng.below(shipped.len())].lines().collect();
            let start = rng.below(lines.len().max(1));
            let end = (start + 1 + rng.below(40)).min(lines.len());
            lines.get(start..end).unwrap_or_default().join("\n") + "\n"
        };
        let input = match rng.below(3) {
            0 => token_soup(&mut rng, STUB_ALPHABET),
            1 => window(&mut rng),
            _ => {
                let seeds = [window(&mut rng), window(&mut rng)];
                mutate_seed(&mut rng, &[&seeds[0], &seeds[1]], 24)
            }
        };
        check_stub_source(&mut db, &input);
    });
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
