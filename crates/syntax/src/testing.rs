//! Fixture format: `#==== <group>` opens a group, `#---- <case>` a case whose
//! source runs to `#++++`, then the expectation runs to the next directive.
//!
//! The fuzz helpers live here too, so every crate's harness draws from one
//! generator and one set of mutation operators.

use rowan::NodeOrToken;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, fs};

use crate::SyntaxNode;

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

pub fn run_fixture_suite(suite_dir: &Path, render: &dyn Fn(&str) -> String) {
    let files = parse_fixture_files(suite_dir);
    assert!(
        !files.is_empty(),
        "no fixture files found under {}",
        suite_dir.display()
    );

    let filter = env::var("FIXTURE_FILTER").ok();
    let bless = env_var("BLESS").is_some_and(|value| value == "1");
    let mut seen_ids = HashSet::new();
    let mut failures = Vec::new();
    let mut matched = 0usize;

    for file in files {
        for case in &file.cases {
            assert!(
                seen_ids.insert(case.id.clone()),
                "duplicate fixture id `{}` (fixture ids must be unique across the suite)",
                case.id
            );
        }

        let mut blessed_edits: Vec<(Range<usize>, String)> = Vec::new();
        for case in &file.cases {
            if filter.as_deref().is_some_and(|filter| filter != case.id) {
                continue;
            }
            matched += 1;
            let rendered = normalize(&render(&case.source));
            let expected = normalize(&case.expected);
            if rendered == expected {
                continue;
            }
            if bless {
                blessed_edits.push((case.expected_span.clone(), rendered));
            } else {
                failures.push(format!(
                    "fixture `{}` ({})\n---- expected ----\n{expected}\n---- actual ----\n{rendered}\n",
                    case.id,
                    file.path.display()
                ));
            }
        }

        if !blessed_edits.is_empty() {
            let mut new_text = file.text;
            for (span, rendered) in blessed_edits.into_iter().rev() {
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
    }

    if let Some(filter) = &filter {
        assert!(
            matched > 0 || sibling_suite_holds(suite_dir, filter),
            "FIXTURE_FILTER=`{filter}` names no fixture case in any suite. Check the id.\n\
             A filter that matches nothing would otherwise run zero cases and report a pass."
        );
    }

    assert!(
        failures.is_empty(),
        "{} fixture failure(s):\n\n{}\n(set RY_BLESS=1 to accept the new output)",
        failures.len(),
        failures.join("\n")
    );
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
const FIXTURE_SUITES: [&str; 13] = [
    "format/tests/format",
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
    check_green_search(&parse.syntax_node(), input);

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

    // Erasure must turn a clean program into plain R: nothing inline survives, the
    // result is clean too, and erasing again changes nothing. A broken program has
    // no meaning to erase.
    if parse.errors().is_empty() {
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
        parse.green(),
        again.green(),
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

/// `FUZZ_ITERS` overrides each harness's default budget.
pub fn fuzz_iterations(default: usize) -> usize {
    env::var("FUZZ_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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
fn sibling_suite_holds(suite_dir: &Path, id: &str) -> bool {
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
                .any(|file| file.cases.iter().any(|case| case.id == id))
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

/// The green-tree searches agree with the red-tree walks they replace, for the whole tree
/// and for a subtree, and whatever subset of kinds is wanted.
fn check_green_search(root: &crate::SyntaxNode, input: &str) {
    let mut text = String::new();
    crate::for_each_token(root, |_, token| text.push_str(token));
    assert_eq!(
        text, input,
        "green token walk lost text for input {input:?}"
    );
    let descendants: Vec<crate::SyntaxNode> = root.descendants().collect();
    let mut visited = Vec::new();
    let mut green_text = String::new();
    let mut open = 0usize;
    crate::walk_green(root, |event| match event {
        crate::GreenEvent::Enter {
            node,
            parent,
            range,
        } => {
            open += 1;
            visited.push((node.kind(), parent.map(crate::GreenRef::kind), range));
        }
        crate::GreenEvent::Leave(_) => open -= 1,
        crate::GreenEvent::Token { text, range, .. } => {
            assert_eq!(usize::from(range.start()), green_text.len());
            green_text.push_str(text);
        }
    });
    assert_eq!(open, 0, "green walk left nodes open for input {input:?}");
    assert_eq!(
        green_text, input,
        "green walk lost text for input {input:?}"
    );
    let expected: Vec<_> = descendants
        .iter()
        .map(|node| {
            let parent = (node != root)
                .then(|| node.parent().map(|parent| parent.kind()))
                .flatten();
            (node.kind(), parent, node.text_range())
        })
        .collect();
    assert_eq!(
        visited, expected,
        "green node walk diverged for input {input:?}"
    );
    for node in &descendants {
        let first = descendants
            .iter()
            .find(|other| other.kind() == node.kind() && other.text_range() == node.text_range());
        assert_eq!(
            crate::node_at(root, node.kind(), node.text_range()).as_ref(),
            first,
            "node_at missed a node for input {input:?}"
        );
    }
    let subtrees = root.children().take(3);
    for node in std::iter::once(root.clone()).chain(subtrees) {
        for modulus in [1, 3, 7] {
            let wanted = |kind: crate::SyntaxKind| (kind as u16) % modulus == 0;
            let expected: Vec<crate::SyntaxNode> = node
                .descendants()
                .filter(|node| wanted(node.kind()))
                .collect();
            let mut parents = Vec::new();
            let found = crate::matching_nodes(&node, |green, parent| {
                let accepted = wanted(green.kind());
                if accepted {
                    parents.push(parent.map(crate::GreenRef::kind));
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
                "green search parents diverged for input {input:?}"
            );
            assert_eq!(found, expected, "green search diverged for input {input:?}");
        }
    }
}
