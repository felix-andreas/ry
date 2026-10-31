//! Every type the checker prints must be writable back: re-declaring an inferred scheme
//! as `#:` above its own definition must add no finding.

use salsa::Setter as _;
use semantics::diagnostics::{TypeRenderer, file_diagnostics};
use semantics::stubs::install_shipped_stubs;
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, item_check, item_spans,
};
use syntax::testing::fixture_case_sources;

/// `id :: name` schemes known not to round-trip; keep it empty.
const KNOWN_UNWRITABLE: [&str; 0] = [];

#[test]
fn rendered_schemes_are_writable_annotations() {
    // One database re-pointed with the salsa setter: a fresh one re-parses the stub corpus,
    // and the fuzz battery already pins setter-equals-fresh.
    let mut db = RootDatabase::default();
    install_shipped_stubs(&db);
    let file = SourceFile::new(&db, String::new(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);

    let mut checked = 0usize;
    let mut violations = Vec::new();
    for (id, source) in fixture_case_sources() {
        // The oracle "re-declaring adds no finding" needs a baseline with no findings.
        file.set_text(&mut db).to(source.clone());
        if !file_diagnostics(&db, file).is_empty() {
            continue;
        }

        for (name, scheme, insert_at) in rendered_schemes(&db, file) {
            checked += 1;
            let mut redeclared = source.clone();
            redeclared.insert_str(insert_at, &format!("#: {scheme}\n"));
            file.set_text(&mut db).to(redeclared);
            let added = file_diagnostics(&db, file);
            // Later schemes' offsets are read off the baseline text.
            file.set_text(&mut db).to(source.clone());
            if added.is_empty() {
                continue;
            }
            if KNOWN_UNWRITABLE.contains(&format!("{id} :: {name}").as_str()) {
                continue;
            }
            let mut report = format!("{id} :: {name}\n  rendered  #: {scheme}\n");
            for diagnostic in added {
                report.push_str(&format!("  {} {}\n", diagnostic.code, diagnostic.message));
            }
            violations.push(report);
        }
    }

    eprintln!(
        "round-trip: {checked} rendered schemes, {} unwritable",
        violations.len()
    );
    for violation in &violations {
        eprintln!("{violation}");
    }
    assert!(
        checked > 200,
        "expected the fixture corpus to yield schemes, got {checked}"
    );
    assert!(
        violations.is_empty(),
        "{} rendered scheme(s) do not survive being written back as the annotation \
         they describe",
        violations.len()
    );
}

/// Each named definition's rendered scheme and the start of its line, where `#:` attaches.
fn rendered_schemes(db: &RootDatabase, file: SourceFile) -> Vec<(String, String, usize)> {
    let text = file.text(db);
    let mut schemes = Vec::new();
    for span in item_spans(db, file) {
        let item = span.item;
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        let (Some(name), Some(check)) = (item.name(db).clone(), item_check(db, item)) else {
            continue;
        };
        let Some(scheme) = check.scheme else {
            continue;
        };
        // An already-annotated item echoes the user's text, and a second block would stack.
        let start = usize::from(span.range.start());
        let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
        if already_annotated(text, line_start) {
            continue;
        }
        let mut renderer = TypeRenderer::default();
        schemes.push((name, renderer.render_scheme(db, &scheme), line_start));
    }
    schemes
}

/// Ordinary comments may interleave with a multi-line `#:` block, so the whole run counts.
fn already_annotated(text: &str, line_start: usize) -> bool {
    text[..line_start]
        .lines()
        .rev()
        .take_while(|line| line.trim_start().starts_with('#'))
        .any(|line| line.trim_start().starts_with("#:"))
}
