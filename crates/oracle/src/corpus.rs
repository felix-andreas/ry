//! Real R code as a second source of differential fuzzing programs, beside the grammar in
//! `fuzz`.
//!
//! The corpus is GNU R's own code, so it is extracted at run time and never committed: the
//! examples of every help page in the base packages (`tools::Rd2ex`), the regression tests in the
//! R source's `tests/` directory, the oracle's hand-written cases, and the tests of corpus
//! packages that ship them. Each file is split into snippets, runs of consecutive top-level
//! expressions free of anything that cannot run deterministically (graphics, files, the clock,
//! unseeded random numbers, addresses). A seed picks a snippet and [`mutate`] rewrites it.
//!
//! A snippet that GNU R and ry already disagree on would make every mutant of it rediscover the
//! same gap, so [`check_corpus_seed`] runs the unmutated snippet first and compares the mutant
//! only under the engines that agree on it.

use {
    crate::{
        Engine, EngineError, Outcome, diff, fuzz::Divergence, gnu_r_program, mutate::mutate,
        target_directory,
    },
    std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        process::Command,
    },
    syntax::{
        NodeOrToken, SyntaxNode, TextRange,
        kind::SyntaxKind::{
            ARGUMENT, ARGUMENT_LIST, AT, CALL_EXPR, COLON2, COLON3, DOLLAR, ERROR, IDENT, LITERAL,
            NAME, STRING,
        },
        parse,
        testing::SplitMix64,
    },
};

/// One self-contained piece of real R code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// The file it comes from, relative to its source (`rd/stats/lm.R`, `cases/x.R`), and its
    /// position among the file's snippets, as `rd/stats/lm.R#2`.
    pub name: String,
    pub text: String,
}

/// The snippets of every corpus file, in a fixed order, and what could not be gathered.
pub struct Corpus {
    pub snippets: Vec<Snippet>,
    /// Why a source is missing, such as a refused download of the R source.
    pub notes: Vec<String>,
}

/// Where the extracted corpus lives: `fuzz-corpus` in the cargo target directory.
pub fn corpus_directory() -> PathBuf {
    target_directory().join("fuzz-corpus")
}

/// Builds the parts of the corpus that are missing (with `download`, also fetching the R source
/// for its regression tests, once) and splits every file into snippets.
pub fn load_corpus(download: bool) -> Result<Corpus, String> {
    let directory = corpus_directory();
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut notes = Vec::new();
    let version = extract_examples(&directory)?;
    if let Some(note) = extract_regression_tests(&directory, &version, download)? {
        notes.push(note);
    }
    let files = corpus_files(&directory);
    let mut snippets = Vec::new();
    for (label, path) in files {
        let Ok(bytes) = std::fs::read(&path) else {
            notes.push(format!("{}: could not be read", path.display()));
            continue;
        };
        // A few of R's tests are latin1 on purpose; the oracle runs programs as UTF-8.
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        snippets.extend(
            split_snippets(&text)
                .into_iter()
                .enumerate()
                .map(|(index, text)| Snippet {
                    name: format!("{label}#{}", index + 1),
                    text,
                }),
        );
    }
    Ok(Corpus { snippets, notes })
}

/// The number of snippets from each source (`rd`, `r-tests`, `cases`, `packages`).
pub fn source_counts(snippets: &[Snippet]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for snippet in snippets {
        let source = snippet
            .name
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        *counts.entry(source).or_insert(0) += 1;
    }
    counts
}

/// The snippet `seed` picks, and its mutant for the same seed.
pub fn corpus_program(snippets: &[Snippet], seed: u64) -> Option<(&Snippet, String)> {
    if snippets.is_empty() {
        return None;
    }
    let mut rng = SplitMix64(PICK_STREAM ^ seed);
    let snippet = &snippets[rng.below(snippets.len())];
    let mutant = mutate(&snippet.text, seed);
    Some((snippet, mutant))
}

/// What one seed's snippet and mutant did under GNU R and each engine.
pub struct CorpusReport {
    pub seed: u64,
    pub source: String,
    pub base: String,
    pub mutant: String,
    /// Engines that disagree with GNU R on the unmutated snippet: a plain compatibility gap,
    /// not a fuzz finding, and their mutant is not compared.
    pub base_differs: Vec<Divergence>,
    /// Engines that agree on the snippet but not on its mutant: the fuzz findings.
    pub divergences: Vec<Divergence>,
    /// The mutant diverged, but GNU R gave two different outcomes on it, so it depends on the
    /// run (an address, the time) and is dropped.
    pub noisy: bool,
    /// Every mutation drawn for the seed was skipped, so there was no mutant to compare.
    pub unchanged: bool,
    pub unavailable: Vec<String>,
}

/// Runs `seed`'s snippet and then its mutant under `reference` and `engines`, in files written
/// to `directory`.
pub fn check_corpus_seed(
    seed: u64,
    snippets: &[Snippet],
    reference: &dyn Engine,
    engines: &[&dyn Engine],
    directory: &Path,
) -> Result<CorpusReport, EngineError> {
    let (snippet, mutant) = corpus_program(snippets, seed)
        .ok_or_else(|| EngineError::Failed("the fuzz corpus is empty".to_string()))?;
    let base_path = write_program(directory, &format!("corpus_{seed}_base.R"), &snippet.text)?;
    let reference_base = reference.run(&base_path)?;
    let (base_differs, mut unavailable) = compare(reference, &reference_base, engines, &base_path);
    remove_program(&base_path);
    let mut report = CorpusReport {
        seed,
        source: snippet.name.clone(),
        base: snippet.text.clone(),
        mutant: mutant.clone(),
        base_differs,
        divergences: Vec::new(),
        noisy: false,
        unchanged: mutant == snippet.text,
        unavailable: Vec::new(),
    };
    let agreeing: Vec<&dyn Engine> = engines
        .iter()
        .copied()
        .filter(|engine| {
            let name = engine.name();
            !report
                .base_differs
                .iter()
                .any(|divergence| divergence.engine == name)
                && !unavailable.iter().any(|reason| reason.starts_with(&name))
        })
        .collect();
    if report.unchanged || agreeing.is_empty() {
        report.unavailable = unavailable;
        return Ok(report);
    }
    let mutant_path = write_program(directory, &format!("corpus_{seed}.R"), &mutant)?;
    let reference_mutant = reference.run(&mutant_path)?;
    let (divergences, more_unavailable) =
        compare(reference, &reference_mutant, &agreeing, &mutant_path);
    unavailable.extend(more_unavailable);
    if !divergences.is_empty() {
        let again = reference.run(&mutant_path)?;
        report.noisy = again != reference_mutant;
    }
    remove_program(&mutant_path);
    if !report.noisy {
        report.divergences = divergences;
    }
    report.unavailable = unavailable;
    Ok(report)
}

/// The failure report for one corpus seed: where the snippet comes from, the mutant, every
/// engine's difference, and the commands that reproduce it.
pub fn render_corpus_divergences(report: &CorpusReport) -> String {
    let engines: Vec<&str> = report
        .divergences
        .iter()
        .map(|divergence| divergence.engine.as_str())
        .collect();
    let mut text = format!(
        "seed {seed} (from {source}) diverges under {names}\nreproduce: cargo run -q -p oracle -- fuzz --corpus --seed {seed} --engines {list}\nprogram:   cargo run -q -p oracle -- fuzz --corpus --seed {seed} --print > corpus_{seed}.R\n--- program ---\n{program}",
        seed = report.seed,
        source = report.source,
        names = engines.join(", "),
        list = engines.join(","),
        program = report.mutant,
    );
    for divergence in &report.divergences {
        text.push_str(&format!(
            "--- {} ---\n{}\n",
            divergence.engine, divergence.detail
        ));
    }
    text
}

/// Splits R source into snippets: runs of consecutive top-level expressions, cut at blank
/// lines into paragraphs, packed up to a modest size, with every paragraph that parses badly or
/// does something nondeterministic left out (and ending the run, since what follows may need
/// it).
pub fn split_snippets(source: &str) -> Vec<String> {
    let parsed = parse(source);
    let errors: Vec<TextRange> = parsed.errors().iter().map(|error| error.range).collect();
    let root = parsed.syntax_node();
    let mut snippets = Vec::new();
    let mut current: Vec<TextRange> = Vec::new();
    let mut seeded = false;
    for paragraph in paragraphs(source, &root) {
        let first = paragraph[0].text_range().start();
        let last = paragraph[paragraph.len() - 1].text_range().end();
        let whole = TextRange::new(first, last);
        let broken = errors.iter().any(|error| overlaps(*error, whole))
            || paragraph
                .iter()
                .any(|statement| statement.descendants().any(|node| node.kind() == ERROR));
        let check = deny(&paragraph);
        if broken || check.denied.is_some() {
            flush(source, &mut current, &mut snippets);
            seeded = false;
            continue;
        }
        let statements = current.len() + paragraph.len();
        let bytes = current
            .first()
            .map_or(0, |start| usize::from(last - start.start()));
        if !current.is_empty() && (statements > MAX_STATEMENTS || bytes > MAX_BYTES) {
            flush(source, &mut current, &mut snippets);
            seeded = false;
        }
        if check.draws_random && !seeded && !check.sets_seed {
            flush(source, &mut current, &mut snippets);
            continue;
        }
        seeded = seeded || check.sets_seed;
        current.extend(paragraph.iter().map(significant_range));
    }
    flush(source, &mut current, &mut snippets);
    snippets
}

/// Why `source` must not run in a differential test, when something in it reads the
/// environment, the clock, files or a graphics device, draws unseeded random numbers, or loads
/// a package outside base R.
pub fn denied_reason(source: &str) -> Option<String> {
    let parsed = parse(source);
    let root = parsed.syntax_node();
    let statements: Vec<SyntaxNode> = root.children().collect();
    let check = deny(&statements);
    if let Some(reason) = check.denied {
        return Some(reason);
    }
    (check.draws_random && !check.sets_seed).then(|| "random numbers without a seed".to_string())
}

/// Keeps the stream that picks a snippet apart from the mutator's for the same seed.
const PICK_STREAM: u64 = 0x636f_7270_7573_0001;
/// A snippet grows up to this many top-level expressions or bytes, so most finish quickly under
/// a debug build and a failing one stays readable.
const MAX_STATEMENTS: usize = 30;
const MAX_BYTES: usize = 4000;

/// The base packages a snippet may load or name with `::`. Loading graphics draws nothing; its
/// plotting functions are denied by name.
const LOADABLE_PACKAGES: &[&str] = &[
    "base",
    "compiler",
    "datasets",
    "grDevices",
    "graphics",
    "methods",
    "splines",
    "stats",
    "stats4",
    "tools",
    "utils",
];

/// Identifiers that make a snippet nondeterministic or unsafe to run, wherever they appear as a
/// value or a function (an argument name or a `$` field of the same spelling is fine).
const DENIED: &[&str] = &[
    // The clock, the session and the machine.
    "Sys.time",
    "Sys.Date",
    "date",
    "proc.time",
    "system.time",
    "Sys.sleep",
    "Sys.getenv",
    "Sys.setenv",
    "Sys.unsetenv",
    "Sys.info",
    "Sys.getpid",
    "Sys.setlocale",
    "Sys.getlocale",
    "Sys.timezone",
    "Sys.setLanguage",
    "R.version",
    "R.Version",
    "R.version.string",
    "version",
    "sessionInfo",
    "R.home",
    ".libPaths",
    "commandArgs",
    "l10n_info",
    "setTimeLimit",
    "setSessionTimeLimit",
    "Sys.chmod",
    "Sys.umask",
    "Sys.which",
    "Sys.readlink",
    "nsl",
    "installed.packages",
    "system.file",
    "Rprof",
    "Rprofmem",
    ".Options",
    "searchpaths",
    // Processes and the network.
    "system",
    "system2",
    "shell",
    "pipe",
    "url",
    "download.file",
    "browseURL",
    "socketConnection",
    "socketAccept",
    "serverSocket",
    "make.socket",
    "curlGetHeaders",
    // Files and connections.
    "file",
    "gzfile",
    "bzfile",
    "xzfile",
    "unz",
    "fifo",
    "gzcon",
    "sink",
    "source",
    "sys.source",
    "save",
    "save.image",
    "load",
    "saveRDS",
    "readRDS",
    "write",
    "write.table",
    "write.csv",
    "write.csv2",
    "dump",
    "unlink",
    "file.remove",
    "file.rename",
    "file.create",
    "file.copy",
    "file.append",
    "file.symlink",
    "file.link",
    "file.exists",
    "file.info",
    "file.access",
    "file.mtime",
    "file.size",
    "dir.create",
    "dir.exists",
    "list.files",
    "list.dirs",
    "dir",
    "tempfile",
    "tempdir",
    "setwd",
    "getwd",
    "normalizePath",
    "Sys.glob",
    "path.expand",
    "readline",
    "menu",
    "stdin",
    "showConnections",
    "getAllConnections",
    "closeAllConnections",
    "readBin",
    "writeBin",
    "readChar",
    "writeChar",
    "Sys.setFileTime",
    "file.choose",
    // Packages and the session.
    "quit",
    "q",
    "install.packages",
    "update.packages",
    "remove.packages",
    "available.packages",
    "demo",
    "example",
    "help",
    "vignette",
    "browser",
    "debug",
    "debugonce",
    "undebug",
    "trace",
    "untrace",
    "recover",
    "dyn.load",
    "dyn.unload",
    "library.dynam",
    ".Internal",
    ".Call",
    ".External",
    ".External2",
    ".C",
    ".Fortran",
    "getNativeSymbolInfo",
    // Memory and addresses.
    "gc",
    "gcinfo",
    "gctorture",
    "gctorture2",
    "object.size",
    "tracemem",
    "untracemem",
    "retracemem",
    "mem.maxVSize",
    "mem.maxNSize",
    "memory.profile",
    "reg.finalizer",
    "environment",
    "Cstack_info",
    // Graphics.
    "plot",
    "par",
    "lines",
    "points",
    "hist",
    "image",
    "abline",
    "text",
    "title",
    "legend",
    "axis",
    "box",
    "polygon",
    "segments",
    "arrows",
    "rect",
    "curve",
    "matplot",
    "matlines",
    "matpoints",
    "barplot",
    "boxplot",
    "pie",
    "contour",
    "persp",
    "pairs",
    "coplot",
    "layout",
    "mtext",
    "grid",
    "symbols",
    "stars",
    "sunflowerplot",
    "dotchart",
    "stripchart",
    "mosaicplot",
    "assocplot",
    "fourfoldplot",
    "spineplot",
    "cdplot",
    "smoothScatter",
    "filled.contour",
    "heatmap",
    "biplot",
    "qqnorm",
    "qqline",
    "qqplot",
    "screeplot",
    "termplot",
    "interaction.plot",
    "lag.plot",
    "monthplot",
    "ts.plot",
    "cpgram",
    "x11",
    "X11",
    "png",
    "pdf",
    "jpeg",
    "bmp",
    "tiff",
    "svg",
    "postscript",
    "xfig",
    "pictex",
    "bitmap",
    "palette",
    "locator",
    "identify",
    "rug",
    "frame",
    "split.screen",
    "screen",
    "erase.screen",
    "bxp",
    "clip",
    "polypath",
    "rasterImage",
    "recordPlot",
    "replayPlot",
    "xspline",
    "strwidth",
    "strheight",
    "graphics.off",
    "colorRampPalette",
    "windows",
    "quartz",
    "plot.new",
    "plot.window",
    "plot.xy",
    "plot.design",
    "plot.default",
    "plot.ts",
    "plot.ecdf",
];

/// Denied names that are also common variable names, denied only where they are called.
const COMMON_WORDS: &[&str] = &[
    "q", "dir", "file", "frame", "screen", "box", "text", "title", "grid", "lines", "points",
    "image", "date", "write", "source", "load", "save", "debug", "trace", "help", "pipe", "url",
    "shell", "system", "dump", "menu", "rect", "symbols", "stars", "axis", "layout", "clip",
    "curve", "legend", "pie", "rug", "segments", "arrows", "polygon", "contour", "pairs",
    "example", "demo", "browser", "version",
];

/// Prefixes of identifiers that are always denied: graphics devices and every `Sys.` function.
const DENIED_PREFIXES: &[&str] = &["dev.", "Sys.", "grid.", "plot."];

/// Functions that draw random numbers, allowed only after a `set.seed` in the same snippet.
const RANDOM: &[&str] = &[
    "rnorm",
    "runif",
    "rbinom",
    "rpois",
    "rexp",
    "rgamma",
    "rbeta",
    "rchisq",
    "rt",
    "rf",
    "rcauchy",
    "rlogis",
    "rlnorm",
    "rweibull",
    "rgeom",
    "rhyper",
    "rnbinom",
    "rmultinom",
    "rsignrank",
    "rwilcox",
    "sample",
    "sample.int",
    "jitter",
    "simulate",
    "rWishart",
    "r2dtable",
    "kmeans",
    ".Random.seed",
    "RNGkind",
];

/// Readers that are safe only on in-memory text (`text =` or a `textConnection`).
const READERS: &[&str] = &[
    "readLines",
    "scan",
    "read.table",
    "read.csv",
    "read.csv2",
    "read.delim",
    "read.delim2",
    "read.fwf",
    "count.fields",
    "read.DIF",
    "readRenviron",
    "parse",
    "dget",
];

/// Writers that are safe only when they write to standard output.
const WRITERS: &[&str] = &[
    "cat",
    "writeLines",
    "print",
    "dput",
    "capture.output",
    "sink",
];

/// Loaders whose first argument must name a base package.
const LOADERS: &[&str] = &[
    "library",
    "require",
    "requireNamespace",
    "loadNamespace",
    "attachNamespace",
];

/// What [`deny`] found in a paragraph.
struct Check {
    denied: Option<String>,
    draws_random: bool,
    sets_seed: bool,
}

fn deny(statements: &[SyntaxNode]) -> Check {
    let mut check = Check {
        denied: None,
        draws_random: false,
        sets_seed: false,
    };
    for statement in statements {
        for element in statement.descendants_with_tokens() {
            match element {
                NodeOrToken::Token(token) if token.kind() == IDENT => {
                    if is_tag_or_field(&token) {
                        continue;
                    }
                    let name = token.text().trim_matches('`');
                    if next_significant_is_namespace(&token) {
                        if !LOADABLE_PACKAGES.contains(&name) {
                            check.denied = Some(format!("package `{name}`"));
                        }
                    } else if (DENIED.contains(&name)
                        && (is_callee(&token) || !COMMON_WORDS.contains(&name)))
                        || DENIED_PREFIXES
                            .iter()
                            .any(|prefix| name.starts_with(prefix))
                    {
                        check.denied = Some(format!("`{name}`"));
                    } else if RANDOM.contains(&name) {
                        check.draws_random = true;
                    } else if name == "set.seed" {
                        check.sets_seed = true;
                    }
                }
                NodeOrToken::Node(node) if node.kind() == CALL_EXPR => {
                    if let Some(reason) = denied_call(&node) {
                        check.denied = Some(reason);
                    }
                }
                _ => {}
            }
            if check.denied.is_some() {
                return check;
            }
        }
    }
    check
}

/// Why a call with an allowed name is still denied: a loader of a package outside base R, an
/// `options()` call that changes more than warnings, a reader of a file, a writer to a file.
fn denied_call(call: &SyntaxNode) -> Option<String> {
    let callee = call.first_child()?;
    if callee.kind() != NAME {
        return None;
    }
    let name = callee.text().trim().trim_matches('`').to_string();
    let arguments: Vec<SyntaxNode> = call
        .children()
        .find(|child| child.kind() == ARGUMENT_LIST)
        .map(|list| {
            list.children()
                .filter(|child| child.kind() == ARGUMENT)
                .collect()
        })
        .unwrap_or_default();
    let tags: Vec<String> = arguments.iter().filter_map(argument_tag).collect();
    if LOADERS.contains(&name.as_str()) {
        let package = arguments
            .iter()
            .find(|argument| argument_tag(argument).is_none_or(|tag| tag == "package"))
            .map(argument_value_text)
            .unwrap_or_default();
        let package = package.trim_matches(['"', '\'', '`']);
        return (!LOADABLE_PACKAGES.contains(&package)).then(|| format!("{name}({package})"));
    }
    if name == "options" {
        let changes_printing = arguments
            .iter()
            .any(|argument| match argument_tag(argument) {
                Some(tag) => tag != "warn",
                None => !argument.first_child().is_some_and(|value| {
                    value.kind() == LITERAL
                        && value
                            .first_token()
                            .is_some_and(|token| token.kind() == STRING)
                }),
            });
        return changes_printing.then(|| "options() that change printing".to_string());
    }
    if READERS.contains(&name.as_str()) {
        let in_memory = tags.iter().any(|tag| tag == "text")
            || call.descendants().any(|node| {
                node.kind() == CALL_EXPR
                    && node
                        .first_child()
                        .is_some_and(|callee| callee.text().trim() == "textConnection")
            });
        return (!in_memory).then(|| format!("{name}() of a file"));
    }
    if WRITERS.contains(&name.as_str()) {
        let positional = arguments
            .iter()
            .filter(|argument| argument_tag(argument).is_none())
            .count();
        let to_file = tags.iter().any(|tag| tag == "file" || tag == "con")
            || (name == "writeLines" && positional >= 2);
        return to_file.then(|| format!("{name}() to a file"));
    }
    None
}

/// The name of `name = value` or `"name" = value`.
fn argument_tag(argument: &SyntaxNode) -> Option<String> {
    let tagged = argument
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .any(|token| token.kind() == syntax::SyntaxKind::EQ);
    if !tagged {
        return None;
    }
    let tag = argument.first_child()?;
    Some(tag.text().trim().trim_matches(['"', '\'', '`']).to_string())
}

fn argument_value_text(argument: &SyntaxNode) -> String {
    argument
        .children()
        .last()
        .map(|value| value.text().trim().to_string())
        .unwrap_or_default()
}

/// An identifier that names an argument (`f(text = 1)`) or a field (`x$text`, `x@text`) is not
/// a reference to the function of that name.
fn is_tag_or_field(token: &syntax::SyntaxToken) -> bool {
    let after_accessor =
        previous_significant(token).is_some_and(|previous| matches!(previous.kind(), DOLLAR | AT));
    let is_tag = token.parent().is_some_and(|name| {
        name.kind() == NAME
            && name.parent().is_some_and(|argument| {
                argument.kind() == ARGUMENT
                    && argument.first_child().as_ref() == Some(&name)
                    && argument_tag(&argument).is_some()
            })
    });
    after_accessor || is_tag
}

fn is_callee(token: &syntax::SyntaxToken) -> bool {
    token.parent().is_some_and(|name| {
        name.kind() == NAME
            && name.parent().is_some_and(|call| {
                call.kind() == CALL_EXPR && call.first_child().as_ref() == Some(&name)
            })
    })
}

fn previous_significant(token: &syntax::SyntaxToken) -> Option<syntax::SyntaxToken> {
    let mut previous = token.prev_token();
    while let Some(candidate) = &previous
        && candidate.kind().is_trivia()
    {
        previous = candidate.prev_token();
    }
    previous
}

fn next_significant_is_namespace(token: &syntax::SyntaxToken) -> bool {
    let mut next = token.next_token();
    while let Some(candidate) = &next
        && candidate.kind().is_trivia()
    {
        next = candidate.next_token();
    }
    next.is_some_and(|candidate| matches!(candidate.kind(), COLON2 | COLON3))
}

/// The top-level expressions of a file, grouped into paragraphs at blank lines.
fn paragraphs(source: &str, root: &SyntaxNode) -> Vec<Vec<SyntaxNode>> {
    let mut paragraphs: Vec<Vec<SyntaxNode>> = Vec::new();
    let mut previous_end: Option<usize> = None;
    for statement in root.children() {
        let range = significant_range(&statement);
        let start = usize::from(range.start());
        let blank_line_before = previous_end.is_none_or(|end| {
            let gap = &source[end..start];
            let lines: Vec<&str> = gap.split('\n').collect();
            lines.len() > 2
                && lines[1..lines.len() - 1]
                    .iter()
                    .any(|line| line.trim().is_empty())
        });
        match paragraphs.last_mut() {
            Some(paragraph) if !blank_line_before => paragraph.push(statement),
            _ => paragraphs.push(vec![statement]),
        }
        previous_end = Some(usize::from(range.end()));
    }
    paragraphs
}

/// Whether an error range touches the paragraph; an empty error range marks a position.
fn overlaps(error: TextRange, paragraph: TextRange) -> bool {
    if error.is_empty() {
        paragraph.contains_inclusive(error.start())
    } else {
        error.start() < paragraph.end() && paragraph.start() < error.end()
    }
}

fn significant_range(node: &SyntaxNode) -> TextRange {
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|token| !token.kind().is_trivia());
    let Some(first) = tokens.next() else {
        return TextRange::empty(node.text_range().start());
    };
    let last = tokens.last().unwrap_or_else(|| first.clone());
    TextRange::new(first.text_range().start(), last.text_range().end())
}

/// Turns the gathered statements into one snippet, if it parses on its own.
fn flush(source: &str, current: &mut Vec<TextRange>, snippets: &mut Vec<String>) {
    let (Some(first), Some(last)) = (current.first(), current.last()) else {
        return;
    };
    let text = format!(
        "{}\n",
        &source[usize::from(first.start())..usize::from(last.end())]
    );
    current.clear();
    if parse(&text).errors().is_empty() {
        snippets.push(text);
    }
}

/// Every corpus file, labelled by its source, in sorted order.
fn corpus_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    collect_r_files(&directory.join("rd"), "rd", &mut files);
    collect_r_files(&directory.join("r-tests"), "r-tests", &mut files);
    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("cases");
    collect_r_files(&cases, "cases", &mut files);
    if let Some(library) = package_library()
        && let Ok(entries) = std::fs::read_dir(&library)
    {
        let mut packages: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        packages.sort();
        for package in packages {
            let Some(name) = package.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            collect_r_files(
                &package.join("tests"),
                &format!("packages/{name}"),
                &mut files,
            );
        }
    }
    files.sort();
    files
}

/// The installed corpus packages: `$RY_CORPUS_DIR/packages/library`, else `.corpus/packages/
/// library` in the workspace.
fn package_library() -> Option<PathBuf> {
    let corpus = std::env::var_os("RY_CORPUS_DIR")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.corpus"));
    let library = corpus.join("packages").join("library");
    library.is_dir().then_some(library)
}

fn collect_r_files(directory: &Path, label: &str, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if path.is_dir() {
            collect_r_files(&path, &format!("{label}/{name}"), files);
        } else if name.ends_with(".R") {
            files.push((format!("{label}/{name}"), path));
        }
    }
}

/// Writes the examples of every base package's help pages to `rd/<package>/<topic>.R` with
/// GNU R's `tools::Rd2ex`, unless they are there, and returns GNU R's version.
fn extract_examples(directory: &Path) -> Result<String, String> {
    let examples = directory.join("rd");
    let version_file = directory.join("gnu-r-version.txt");
    if examples.is_dir()
        && let Ok(version) = std::fs::read_to_string(&version_file)
    {
        return Ok(version.trim().to_string());
    }
    let partial = directory.join(format!("rd.partial-{}", std::process::id()));
    let script = directory.join(format!("extract-{}.R", std::process::id()));
    std::fs::write(&script, EXTRACT_EXAMPLES)
        .map_err(|error| format!("{}: {error}", script.display()))?;
    let output = Command::new(gnu_r_program())
        .args(["--vanilla", "--slave", "-f"])
        .arg(&script)
        .arg("--args")
        .arg(&partial)
        .output()
        .map_err(|error| format!("GNU R could not extract the help examples: {error}"))?;
    if let Err(error) = std::fs::remove_file(&script) {
        eprintln!("could not remove {}: {error}", script.display());
    }
    if !output.status.success() {
        return Err(format!(
            "GNU R could not extract the help examples: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if examples.is_dir() {
        // Another run finished first; its copy is as good as this one.
        remove_directory(&partial);
    } else {
        std::fs::rename(&partial, &examples)
            .map_err(|error| format!("{}: {error}", examples.display()))?;
    }
    std::fs::write(&version_file, &version)
        .map_err(|error| format!("{}: {error}", version_file.display()))?;
    Ok(version)
}

const EXTRACT_EXAMPLES: &str = r#"out <- commandArgs(TRUE)[1]
for (pkg in c("base", "compiler", "datasets", "methods", "splines", "stats", "stats4", "tools", "utils")) {
  db <- tools::Rd_db(pkg)
  dir.create(file.path(out, pkg), recursive = TRUE, showWarnings = FALSE)
  for (name in names(db)) {
    file <- file.path(out, pkg, sub("\\.Rd$", ".R", basename(name)))
    tryCatch(tools::Rd2ex(db[[name]], file, commentDontrun = TRUE, commentDonttest = TRUE),
             error = function(e) NULL)
  }
}
cat(format(getRversion()))
"#;

/// Fetches the R source of `version` from CRAN once and keeps its `tests/*.R` in `r-tests`.
/// Returns why they are missing when they are, which a failed download records so later runs
/// do not try again.
fn extract_regression_tests(
    directory: &Path,
    version: &str,
    download: bool,
) -> Result<Option<String>, String> {
    let tests = directory.join("r-tests");
    let refused = directory.join("r-tests-unavailable.txt");
    if tests.is_dir() {
        return Ok(None);
    }
    if let Ok(reason) = std::fs::read_to_string(&refused) {
        return Ok(Some(format!(
            "R's regression tests are unavailable: {}",
            reason.trim()
        )));
    }
    if !download {
        return Ok(Some(
            "R's regression tests are not downloaded (`oracle fuzz --corpus` fetches them)"
                .to_string(),
        ));
    }
    let partial = directory.join(format!("download-{}", std::process::id()));
    std::fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let major = version.split('.').next().unwrap_or("4");
    let url = format!("https://cran.r-project.org/src/base/R-{major}/R-{version}.tar.gz");
    let archive = partial.join("R.tar.gz");
    let fetched = Command::new("curl")
        .args(["-sSfL", "--max-time", "600", "-o"])
        .arg(&archive)
        .arg(&url)
        .output();
    let failure = match fetched {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "{url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => Some(format!("curl: {error}")),
    };
    if let Some(reason) = failure {
        remove_directory(&partial);
        std::fs::write(&refused, &reason)
            .map_err(|error| format!("{}: {error}", refused.display()))?;
        return Ok(Some(format!(
            "R's regression tests are unavailable: {reason}"
        )));
    }
    let pattern = format!("R-{version}/tests/*.R");
    let unpacked = Command::new("tar")
        .arg("xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&partial)
        .args(["--wildcards", &pattern])
        .output()
        .map_err(|error| format!("tar: {error}"))?;
    if !unpacked.status.success() {
        remove_directory(&partial);
        return Err(format!(
            "tar could not unpack {pattern}: {}",
            String::from_utf8_lossy(&unpacked.stderr)
        ));
    }
    let source = partial.join(format!("R-{version}")).join("tests");
    std::fs::rename(&source, &tests).map_err(|error| format!("{}: {error}", tests.display()))?;
    remove_directory(&partial);
    Ok(None)
}

fn remove_directory(directory: &Path) {
    if let Err(error) = std::fs::remove_dir_all(directory) {
        eprintln!("could not remove {}: {error}", directory.display());
    }
}

fn write_program(directory: &Path, name: &str, text: &str) -> Result<PathBuf, EngineError> {
    let path = directory.join(name);
    std::fs::write(&path, text)
        .map_err(|error| EngineError::Failed(format!("{}: {error}", path.display())))?;
    Ok(path)
}

fn remove_program(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        eprintln!("could not remove {}: {error}", path.display());
    }
}

/// Every engine's difference from the reference's outcome on `path`, and the engines that could
/// not run.
fn compare(
    reference: &dyn Engine,
    reference_outcome: &Outcome,
    engines: &[&dyn Engine],
    path: &Path,
) -> (Vec<Divergence>, Vec<String>) {
    let mut divergences = Vec::new();
    let mut unavailable = Vec::new();
    for engine in engines {
        match engine.run(path) {
            Ok(outcome) => {
                if let Some(detail) = diff(
                    &reference.name(),
                    reference_outcome,
                    &engine.name(),
                    &outcome,
                ) {
                    divergences.push(Divergence {
                        engine: engine.name(),
                        detail,
                    });
                }
            }
            Err(EngineError::Unavailable(reason)) => {
                unavailable.push(format!("{}: {reason}", engine.name()))
            }
            Err(EngineError::Failed(reason) | EngineError::Crashed(reason)) => {
                divergences.push(Divergence {
                    engine: engine.name(),
                    detail: reason,
                })
            }
        }
    }
    (divergences, unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_split_at_blank_lines_and_drop_what_is_denied() {
        let source = "x <- 1:3 # numbers\ny <- x * 2\n\nplot(x, y)\nz <- 1\n\nprint(y)\n\nf(1,\n";
        let snippets = split_snippets(source);
        assert_eq!(
            snippets,
            vec!["x <- 1:3 # numbers\ny <- x * 2\n", "print(y)\n"]
        );
    }

    #[test]
    fn long_runs_are_cut_into_several_snippets() {
        let source: String = (0..70)
            .map(|index| format!("v{index} <- {index}\n"))
            .collect();
        let snippets = split_snippets(&source);
        assert_eq!(snippets.len(), 1, "one paragraph is never cut");
        let source: String = (0..70)
            .map(|index| format!("v{index} <- {index}\n\n"))
            .collect();
        let snippets = split_snippets(&source);
        assert_eq!(snippets.len(), 3);
        assert!(
            snippets
                .iter()
                .all(|snippet| parse(snippet).errors().is_empty())
        );
    }

    #[test]
    fn random_numbers_need_a_seed_in_the_same_snippet() {
        assert_eq!(split_snippets("x <- rnorm(3)\n"), Vec::<String>::new());
        assert_eq!(
            split_snippets("set.seed(1)\n\nx <- rnorm(3)\n"),
            vec!["set.seed(1)\n\nx <- rnorm(3)\n"]
        );
    }

    #[test]
    fn the_deny_list_reads_calls_not_names() {
        for denied in [
            "Sys.time()",
            "x <- date()",
            "dev.off()",
            "library(ggplot2)",
            "requireNamespace(\"rlang\")",
            "rlang::abort('x')",
            "options(digits = 3)",
            "readLines('a.txt')",
            "cat(1, file = 'a.txt')",
            "writeLines('a', con)",
            "f <- function(e) environment(e)",
            "x <- runif(2)",
            "graphics::plot(1)",
        ] {
            assert!(denied_reason(denied).is_some(), "`{denied}` was allowed");
        }
        for allowed in [
            "f(text = 1)",
            "x$file",
            "library(stats)",
            "require(\"methods\")",
            "require(graphics)",
            "utils::head(1:3)",
            "options(warn = 1)",
            "getOption(\"digits\")",
            "readLines(textConnection(\"a\\nb\"))",
            "scan(text = \"1 2\")",
            "cat(1, \"\\n\")",
            "writeLines(\"a\")",
            "set.seed(2); sample(5)",
        ] {
            assert_eq!(denied_reason(allowed), None, "`{allowed}` was denied");
        }
    }

    #[test]
    fn the_oracle_cases_give_snippets_and_the_pick_is_deterministic() {
        let mut files = Vec::new();
        let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("cases");
        collect_r_files(&cases, "cases", &mut files);
        files.sort();
        let snippets: Vec<Snippet> = files
            .iter()
            .filter_map(|(label, path)| {
                let text = std::fs::read_to_string(path).ok()?;
                Some(
                    split_snippets(&text)
                        .into_iter()
                        .map(|text| Snippet {
                            name: label.clone(),
                            text,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        assert!(snippets.len() > 300, "only {} snippets", snippets.len());
        for seed in 0..50 {
            let (snippet, mutant) = corpus_program(&snippets, seed).expect("a snippet");
            let (again, mutant_again) = corpus_program(&snippets, seed).expect("a snippet");
            assert_eq!((snippet, &mutant), (again, &mutant_again));
            assert!(parse(&mutant).errors().is_empty(), "seed {seed}:\n{mutant}");
        }
    }
}
