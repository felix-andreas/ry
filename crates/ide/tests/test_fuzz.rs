//! Every feature runs at sampled offsets and must not panic or produce a range outside the text
//! it indexes. `FUZZ_ITERS` scales the mutation budget.

use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use syntax::TextSize;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

const SEEDS: &[&str] = &[
    "x <- 1L\ny <- x + 1\n",
    "f <- function(a, b = 2) a + b\ng <- function() f(1)\n",
    "#: fn(x: integer) -> double\nhalf <- function(x) x / 2\n",
    "make <- function() {\n  total <- 0\n  add <- function(n) total <<- total + n\n  add\n}\n",
    "lst <- list(a = 1, b = \"s\")\nvalue <- lst$a\n",
    "for (i in 1:10) print(i)\n",
    "if (x > 1) y <- 2 else y <- 3\n",
    "`odd name` <- 5\nuse <- `odd name`\n",
    "broken <- function( {\n",
    "s <- \"unterminated\n",
];

fn iterations() -> usize {
    std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300)
}

fn sweep(source: &str) {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    let files = ProjectFiles::new(&db, vec![file]);
    let completion_offsets = one_offset_per_context(source);
    for offset in sample_offsets(source) {
        let offset = TextSize::from(offset as u32);
        let at = |what: &str| format!("{what} in {source:?} at {offset:?}");

        let hover = ide::hover(&db, files, file, offset);
        assert_eq!(
            hover,
            ide::hover(&db, files, file, offset),
            "{}",
            at("non-deterministic hover")
        );
        if let Some(hover) = &hover {
            check_range(&db, file, hover.range, &at("hover"));
        }

        let _ = ide::hover_debug(&db, file, offset);

        let cursor_name = name_at(source, offset);

        let definition = ide::definition(&db, files, file, offset);
        check_target(&db, definition.as_ref(), &at("definition"));
        check_names(
            &db,
            definition_range(definition.as_ref()),
            cursor_name,
            &at("definition"),
        );

        let references = ide::references(&db, files, file, offset, true);
        for occurrence in &references {
            check_range(&db, occurrence.file, occurrence.range, &at("reference"));
        }
        check_names(
            &db,
            references
                .iter()
                .map(|occurrence| (occurrence.file, occurrence.range)),
            cursor_name,
            &at("reference"),
        );

        let rename = ide::rename(&db, files, file, offset);
        for occurrence in rename.iter().flatten() {
            check_range(&db, occurrence.file, occurrence.range, &at("rename edit"));
        }
        check_names(
            &db,
            rename
                .iter()
                .flatten()
                .map(|occurrence| (occurrence.file, occurrence.range)),
            cursor_name,
            &at("rename edit"),
        );

        // A consistently shifted set of ranges passes every containment check yet corrupts the
        // file; the round trip catches it.
        if let Some((target_file, target_range)) = definition_range(definition.as_ref())
            && target_file == file
            && cursor_name.is_some()
        {
            let back = ide::references(&db, files, file, target_range.start(), true);
            assert!(
                back.iter().any(|occurrence| {
                    occurrence.file == file && occurrence.range.contains(offset)
                }),
                "{}: definition led to {target_range:?} but its references do not come back to \
                 the cursor",
                at("round trip")
            );
        }

        // Parameter spans index the rendered label, not the file.
        for signature in ide::signature_help(&db, file, offset)
            .iter()
            .flat_map(|help| &help.signatures)
        {
            for parameter in &signature.parameters {
                assert!(
                    usize::from(parameter.end()) <= signature.label.len(),
                    "{}: parameter span {parameter:?} escapes label {:?}",
                    at("signature help"),
                    signature.label
                );
            }
        }

        // Completion dominates this harness's cost and depends on the syntactic context, not the
        // exact byte, so one offset per context suffices.
        if completion_offsets.contains(&offset) {
            for item in ide::completion(&db, files, file, offset)
                .iter()
                .flat_map(|result| &result.items)
            {
                assert!(
                    !item.label.is_empty(),
                    "{}: empty completion label",
                    at("completion")
                );
            }
        }

        let type_definition = ide::type_definition(&db, files, file, offset);
        check_target(&db, type_definition.as_ref(), &at("type definition"));

        let selection = syntax::TextRange::new(offset, offset);
        let actions = ide::code_actions(&db, file, selection);
        for action in &actions {
            for edit in &action.edits {
                check_range(&db, file, edit.range, &at("code action edit"));
            }
        }
    }

    let hints = ide::inlay_hints(&db, file, None);
    for hint in &hints {
        assert!(
            usize::from(hint.offset) <= source.len(),
            "inlay hint offset {:?} escapes {source:?}",
            hint.offset
        );
    }
    for symbol in ide::document_symbols(&db, file) {
        check_symbol(&db, file, &symbol);
    }
    let _ = ide::workspace_symbols(&db, files, "a");
}

/// Token boundaries are where position decisions happen; the stride keeps off-boundary and
/// mid-character offsets without paying for every byte.
fn sample_offsets(source: &str) -> Vec<usize> {
    const STRIDE: usize = 7;
    let mut offsets: Vec<usize> = token_boundaries(source)
        .iter()
        .map(|&offset| usize::from(offset))
        .collect();
    offsets.extend((0..=source.len()).step_by(STRIDE));
    offsets.push(source.len());
    offsets.sort_unstable();
    offsets.dedup();
    offsets.retain(|&offset| offset <= source.len());
    offsets
}

fn token_boundaries(source: &str) -> std::collections::BTreeSet<TextSize> {
    let mut offsets = std::collections::BTreeSet::from([TextSize::from(0)]);
    let mut at = 0usize;
    for token in syntax::lex(source).0 {
        at += usize::from(token.len);
        offsets.insert(TextSize::from(at.min(source.len()) as u32));
    }
    offsets
}

/// The kind of token the cursor sits after decides what completion may offer.
fn one_offset_per_context(source: &str) -> std::collections::BTreeSet<TextSize> {
    let mut seen = std::collections::HashSet::new();
    let mut offsets =
        std::collections::BTreeSet::from([TextSize::from(0), TextSize::from(source.len() as u32)]);
    let mut at = 0usize;
    for token in syntax::lex(source).0 {
        at += usize::from(token.len);
        if seen.insert(token.kind) {
            offsets.insert(TextSize::from(at.min(source.len()) as u32));
        }
    }
    offsets
}

fn name_at(source: &str, offset: TextSize) -> Option<&str> {
    let offset = usize::from(offset);
    let mut start = 0usize;
    for token in syntax::lex(source).0 {
        let end = start + usize::from(token.len);
        // A backtick-quoted name lexes as IDENT, backticks included.
        if (start..end).contains(&offset) && token.kind == syntax::SyntaxKind::IDENT {
            return source.get(start..end);
        }
        start = end;
    }
    None
}

/// A range shifted by one byte is in bounds and self-consistent yet destroys code, so each range
/// must spell the cursor's name. Backticks are normalized: `` `x` `` and `x` name one binding.
fn check_names(
    db: &RootDatabase,
    ranges: impl IntoIterator<Item = (SourceFile, syntax::TextRange)>,
    cursor_name: Option<&str>,
    what: &str,
) {
    let Some(cursor_name) = cursor_name.map(unquote) else {
        return;
    };
    for (file, range) in ranges {
        let text = file.text(db);
        let Some(spelled) = text.get(usize::from(range.start())..usize::from(range.end())) else {
            continue;
        };
        assert_eq!(
            unquote(spelled),
            cursor_name,
            "{what}: range {range:?} covers {spelled:?}, not the name under the cursor"
        );
    }
}

fn unquote(name: &str) -> &str {
    name.strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
        .unwrap_or(name)
}

fn definition_range(
    target: Option<&ide::DefinitionTarget>,
) -> Option<(SourceFile, syntax::TextRange)> {
    match target {
        Some(ide::DefinitionTarget::Project(navigation)) => {
            Some((navigation.file, navigation.range))
        }
        _ => None,
    }
}

fn check_range(db: &RootDatabase, file: SourceFile, range: syntax::TextRange, what: &str) {
    let length = file.text(db).len();
    assert!(
        usize::from(range.end()) <= length,
        "{what}: range {range:?} escapes its file (length {length})"
    );
}

fn check_target(db: &RootDatabase, target: Option<&ide::DefinitionTarget>, what: &str) {
    // A stub target's range indexes a stub source, not a project file.
    if let Some(ide::DefinitionTarget::Project(navigation)) = target {
        check_range(db, navigation.file, navigation.range, what);
    }
}

fn check_symbol(db: &RootDatabase, file: SourceFile, symbol: &ide::DocumentSymbol) {
    check_range(db, file, symbol.range, "document symbol");
    check_range(db, file, symbol.selection, "document symbol selection");
    for child in &symbol.children {
        check_symbol(db, file, child);
    }
}

#[test]
fn seeds_never_panic() {
    for seed in SEEDS {
        sweep(seed);
    }
}

#[test]
fn fuzz_seed_mutations() {
    let mut rng = SplitMix64(0x01DE_F022);
    for _ in 0..iterations() {
        let seed = SEEDS[rng.below(SEEDS.len())];
        let mut text = seed.as_bytes().to_vec();
        for _ in 0..(1 + rng.below(4)) {
            match rng.below(4) {
                0 if !text.is_empty() => {
                    let at = rng.below(text.len());
                    text.remove(at);
                }
                1 => {
                    let at = rng.below(text.len() + 1);
                    text.insert(at, (rng.next() & 0x7F) as u8);
                }
                2 if !text.is_empty() => {
                    let at = rng.below(text.len());
                    text[at] = (rng.next() & 0x7F) as u8;
                }
                _ => {
                    let donor = SEEDS[rng.below(SEEDS.len())].as_bytes();
                    let from = rng.below(donor.len());
                    let to = (from + rng.below(24)).min(donor.len());
                    let at = rng.below(text.len() + 1);
                    for (index, &byte) in donor[from..to].iter().enumerate() {
                        text.insert(at + index, byte);
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&text).into_owned();
        sweep(&text);
    }
}

/// `FUZZ_ITERS=5000 cargo test -p ide --release --test test_fuzz -- --ignored fuzz_deep`
#[test]
#[ignore = "deep fuzz; run explicitly with a large FUZZ_ITERS"]
fn fuzz_deep() {
    let mut rng = SplitMix64(0x01DE_F023);
    for _ in 0..iterations().max(5000) {
        let pieces = 1 + rng.below(12);
        let mut text = String::new();
        for _ in 0..pieces {
            text.push_str(SEEDS[rng.below(SEEDS.len())]);
        }
        sweep(&text);
    }
}
