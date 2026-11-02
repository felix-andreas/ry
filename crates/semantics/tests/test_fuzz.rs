//! Fuzz harness for the semantic pipeline. `FUZZ_ITERS` scales the per-generator budget;
//! runs are deterministic per seed.

use {
    rustc_hash::FxHashMap,
    salsa::Setter as _,
    semantics::{
        DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::file_diagnostics,
        metadata::resolve_namespace_conditionals,
        stubs::install_shipped_stubs,
        testing::{check_semantics_input, check_semantics_invariants, render_semantics as render},
    },
    std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    syntax::{
        SyntaxKind, SyntaxNode,
        ast::is_expression_kind,
        testing::{
            SplitMix64, for_each_seed, fuzz_iterations, mutate_seed, program_corpus_sources,
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
    "#: Array<integer>\n{n} <- [1L, {m}]",
    "{n} <- [{m}] + [1] * {m}",
    "{n} <- !([{m}] > 1) & [TRUE]",
    "{n} <- {m}[[1]] + {m}[[{m}]]",
    "{n} <- {m}[{m} > 0]",
    "for (i in [{m}, 2L]) {n} <- i",
    "{n} <- List.map([{m}], \\(v) v * 2)",
    "{n} <- as.Array({m})",
    "{n} <- [({m}, 1L), (2L, {m})]",
    "#: Array<double[]>\n{n} <- [{m}]",
];

const DIRECTIVES: &[&str] = &[
    "# typing: strict",
    "# typing: on",
    "# typing: off",
    "#: @strict",
];

const SOUP: &[&str] = &[
    "x", "f", "(", ")", "{", "}", "[", "[[", "]", ",", "\n", " ", "<-", "+", "if", "else",
    "function", "1L", "\"s\"", "%in%", "|>", "~", "$", "::", "#:", "@type", "fn", "list{", "NULL",
    "...", ":=", "<<-", "\\", "@new", "match", "=>", ":Some", ":None", "_", "..", "|", "@pure",
    "Array<", ">", "as.Array", "[1, 2]",
];

fn iterations() -> usize {
    fuzz_iterations(250)
}

fn generate_program(rng: &mut SplitMix64) -> String {
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

fn generate_soup(rng: &mut SplitMix64) -> String {
    let length = 1 + rng.below(60);
    let mut source = String::new();
    for _ in 0..length {
        source.push_str(SOUP[rng.below(SOUP.len())]);
    }
    source
}

/// Prints the failing inputs on panic: a bare salsa or checker panic names no source.
fn run_single_file(budget: usize, seed: u64, generate: fn(&mut SplitMix64) -> String) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let source = generate(&mut rng);
        let kind = if rng.chance(1, 4) {
            DocumentKind::Script
        } else {
            DocumentKind::Package
        };
        let edited_source = generate_program(&mut rng);
        let result = catch_unwind(|| check_semantics_invariants(&source, &edited_source, kind));
        if let Err(payload) = result {
            eprintln!(
                "---- pipeline panicked ({kind:?}) ----\n{source}\n---- edited to ----\n{edited_source}\n----"
            );
            resume_unwind(payload);
        }
    }
}

/// Prints the failing inputs on panic, as [`run_single_file`] does.
fn run_multi_file(budget: usize, seed: u64) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let first_source = generate_program(&mut rng);
        let second_source = generate_program(&mut rng);
        let edited_source = generate_program(&mut rng);
        let result =
            catch_unwind(|| check_multi_file_edit(&first_source, &second_source, &edited_source));
        if let Err(payload) = result {
            eprintln!(
                "---- multi-file pipeline panicked ----\n{first_source}\n---- second file ----\n{second_source}\n---- second file edited to ----\n{edited_source}\n----"
            );
            resume_unwind(payload);
        }
    }
}

/// Edits the second of two package files and back, checking each state against a fresh
/// database.
fn check_multi_file_edit(first_source: &str, second_source: &str, edited_source: &str) {
    let mut db = RootDatabase::default();
    install_shipped_stubs(&db);
    let first = SourceFile::new(&db, first_source.to_owned(), DocumentKind::Package);
    let second = SourceFile::new(&db, second_source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![first, second]);
    let before = (render(&db, first), render(&db, second));

    second.set_text(&mut db).to(edited_source.to_owned());
    let after_first = render(&db, first);
    let after_second = render(&db, second);

    let fresh_db = RootDatabase::default();
    install_shipped_stubs(&fresh_db);
    let fresh_first = SourceFile::new(&fresh_db, first_source.to_owned(), DocumentKind::Package);
    let fresh_second = SourceFile::new(&fresh_db, edited_source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&fresh_db, vec![fresh_first, fresh_second]);
    assert_eq!(
        after_first,
        render(&fresh_db, fresh_first),
        "cross-file incremental output diverges (untouched file)"
    );
    assert_eq!(
        after_second,
        render(&fresh_db, fresh_second),
        "cross-file incremental output diverges (edited file)"
    );

    second.set_text(&mut db).to(second_source.to_owned());
    assert_eq!(
        (render(&db, first), render(&db, second)),
        before,
        "cross-file output not restored after editing back"
    );
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
    "frame <- data.frame(a = 1L)\n`setnames`(frame, \"a\", \"b\")\n",
    "setGeneric(\"area\", { y <- 1; function(obj) standardGeneric(\"area\") })\n",
    "x <- list(f = function() y <<- 1L)\n",
];

/// One shared database (a fresh one re-parses the stubs per case), so this asserts only
/// never-panic and range geometry; the generated arms cover determinism and equivalence.
#[test]
fn program_corpus_holds_invariants() {
    let sources = program_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let files: Vec<(String, SourceFile)> = sources
        .iter()
        .map(|(id, source)| {
            (
                id.clone(),
                SourceFile::new(&db, source.clone(), DocumentKind::Package),
            )
        })
        .collect();
    ProjectFiles::new(&db, files.iter().map(|(_, file)| *file).collect());
    for (id, file) in &files {
        let length = file.text(&db).len();
        let diagnostics = catch_unwind(AssertUnwindSafe(|| file_diagnostics(&db, *file)))
            .unwrap_or_else(|_| panic!("program corpus case `{id}` panicked the pipeline"));
        for diagnostic in diagnostics {
            let start = u32::from(diagnostic.range.start()) as usize;
            let end = u32::from(diagnostic.range.end()) as usize;
            assert!(
                start <= end && end <= length,
                "program corpus case `{id}`: range {start}..{end} escapes the file (length {length})"
            );
        }
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_semantics_input(input);
    }
}

#[test]
fn fuzz_generated_programs() {
    run_single_file(iterations(), 0xDECA_FBAD, generate_program);
}

#[test]
fn fuzz_token_soup() {
    run_single_file(iterations() / 2, 0xBEEF_FACE, generate_soup);
}

#[test]
fn fuzz_multi_file() {
    run_multi_file(iterations() / 5, 0xCAFE_D00D);
}

#[test]
#[ignore = "long fuzz run for nightly/manual use"]
fn fuzz_deep() {
    run_single_file(iterations() * 20, 0x5EED_0001, generate_program);
    run_single_file(iterations() * 10, 0x5EED_0002, generate_soup);
    run_multi_file(iterations() * 4, 0x5EED_0003);
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
    for_each_seed(400, |seed| {
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
