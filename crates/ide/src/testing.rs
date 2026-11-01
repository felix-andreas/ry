//! The editor features' fuzz contract, shared by the in-tree harness and the coverage-guided
//! `fuzz/` target so both check exactly the same properties.

use crate::{DefinitionTarget, DocumentSymbol};
use semantics::stubs::install_shipped_stubs;
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::collections::{BTreeSet, HashSet};
use syntax::{SyntaxKind, TextRange, TextSize, lex};

/// Runs every feature at sampled offsets of `source`: nothing may panic, every range must lie
/// inside the text it indexes, and wherever the cursor sits on a name, every range a feature
/// returns must spell that name. Shared with the coverage-guided `ide` target.
pub fn check_ide_invariants(source: &str) {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    let files = ProjectFiles::new(&db, vec![file]);
    let completion_offsets = one_offset_per_context(source);
    for offset in sample_offsets(source) {
        let offset = TextSize::from(offset as u32);
        let at = |what: &str| format!("{what} in {source:?} at {offset:?}");

        let hover = crate::hover(&db, files, file, offset);
        assert_eq!(
            hover,
            crate::hover(&db, files, file, offset),
            "{}",
            at("non-deterministic hover")
        );
        if let Some(hover) = &hover {
            check_range(&db, file, hover.range, &at("hover"));
        }

        crate::hover_debug(&db, file, offset);

        let cursor_name = name_at(source, offset);

        let definition = crate::definition(&db, files, file, offset);
        check_target(&db, definition.as_ref(), &at("definition"));
        check_names(
            &db,
            definition_range(definition.as_ref()),
            cursor_name,
            &at("definition"),
        );

        let references = crate::references(&db, files, file, offset, true);
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

        let rename = crate::rename(&db, files, file, offset);
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
            let back = crate::references(&db, files, file, target_range.start(), true);
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
        for signature in crate::signature_help(&db, file, offset)
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
            for item in crate::completion(&db, files, file, offset)
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

        let type_definition = crate::type_definition(&db, files, file, offset);
        check_target(&db, type_definition.as_ref(), &at("type definition"));

        let selection = TextRange::new(offset, offset);
        let actions = crate::code_actions(&db, file, selection);
        for action in &actions {
            for edit in &action.edits {
                check_range(&db, file, edit.range, &at("code action edit"));
            }
        }
    }

    let hints = crate::inlay_hints(&db, file, None);
    for hint in &hints {
        assert!(
            usize::from(hint.offset) <= source.len(),
            "inlay hint offset {:?} escapes {source:?}",
            hint.offset
        );
    }
    for symbol in crate::document_symbols(&db, file) {
        check_symbol(&db, file, &symbol);
    }
    for symbol in crate::workspace_symbols(&db, files, "a") {
        check_range(
            &db,
            symbol.target.file,
            symbol.target.range,
            "workspace symbol",
        );
    }
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

fn token_boundaries(source: &str) -> BTreeSet<TextSize> {
    let mut offsets = BTreeSet::from([TextSize::from(0)]);
    let mut at = 0usize;
    for token in lex(source).0 {
        at += usize::from(token.len);
        offsets.insert(TextSize::from(at.min(source.len()) as u32));
    }
    offsets
}

/// The kind of token the cursor sits after decides what completion may offer.
fn one_offset_per_context(source: &str) -> BTreeSet<TextSize> {
    let mut seen = HashSet::new();
    let mut offsets = BTreeSet::from([TextSize::from(0), TextSize::from(source.len() as u32)]);
    let mut at = 0usize;
    for token in lex(source).0 {
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
    for token in lex(source).0 {
        let end = start + usize::from(token.len);
        // A backtick-quoted name lexes as IDENT, backticks included.
        if (start..end).contains(&offset) && token.kind == SyntaxKind::IDENT {
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
    ranges: impl IntoIterator<Item = (SourceFile, TextRange)>,
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

fn definition_range(target: Option<&DefinitionTarget>) -> Option<(SourceFile, TextRange)> {
    match target {
        Some(DefinitionTarget::Project(navigation)) => Some((navigation.file, navigation.range)),
        _ => None,
    }
}

fn check_range(db: &RootDatabase, file: SourceFile, range: TextRange, what: &str) {
    let length = file.text(db).len();
    assert!(
        usize::from(range.end()) <= length,
        "{what}: range {range:?} escapes its file (length {length})"
    );
}

fn check_target(db: &RootDatabase, target: Option<&DefinitionTarget>, what: &str) {
    // A stub target's range indexes a stub source, not a project file.
    if let Some(DefinitionTarget::Project(navigation)) = target {
        check_range(db, navigation.file, navigation.range, what);
    }
}

fn check_symbol(db: &RootDatabase, file: SourceFile, symbol: &DocumentSymbol) {
    check_range(db, file, symbol.range, "document symbol");
    check_range(db, file, symbol.selection, "document symbol selection");
    for child in &symbol.children {
        check_symbol(db, file, child);
    }
}
