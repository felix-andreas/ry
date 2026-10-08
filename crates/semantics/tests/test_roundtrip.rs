//! The annotation round-trip oracle: every type the checker prints must be a
//! type a user can write back.
//!
//! `#: TYPE` asserts the annotated value is compatible with `TYPE`, and the
//! checker has just proved the value *has* the type it rendered. Re-declaring
//! an inferred scheme above its own definition must therefore add no
//! finding. Anything else means a rendering nobody can copy out of a hover or
//! a `expected X, found Y` message and paste into an annotation.
//!
//! This is the only test that compares the renderer against the annotation
//! grammar. Every other suite reads the rendering as a *string*, so a type that
//! prints beautifully and parses back as something else is invisible to all of
//! them. That is not hypothetical. An unquoted record field name containing a
//! comma re-parsed as a different type entirely, which produced a bogus
//! mismatch plus a bogus unknown-type error. A constraint spelling the grammar
//! did not accept also made every scheme carrying it unwritable.
//!
//! Run one case with `FIXTURE_FILTER=group__case cargo test -p semantics
//! --test test_roundtrip -- --nocapture`.

use salsa::Setter as _;
use semantics::diagnostics::TypeRenderer;
use semantics::diagnostics::file_diagnostics;
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, item_check, item_spans,
};
use std::fmt::Write as _;

/// Schemes that are known not to round-trip, named individually so that a new
/// unwritable rendering of any kind fails this test rather than blending into a
/// category. **Empty, and worth keeping empty**: the last entries were seven
/// defaulted-parameter schemes the checker inferred and then rejected when they
/// were written back, which is fixed.
const KNOWN_UNWRITABLE: [&str; 0] = [];

#[test]
fn rendered_schemes_are_writable_annotations() {
    // One database throughout, re-pointed at each probe with the salsa setter.
    // A fresh database costs a re-parse of the whole shipped stub corpus, which
    // would be almost the entire runtime of a test that builds a thousand of
    // them; the semantics fuzz battery already asserts that setting the text
    // gives what a fresh database would.
    let mut db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, String::new(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);

    let mut checked = 0usize;
    let mut violations = Vec::new();
    for (id, source) in syntax::testing::fixture_case_sources() {
        // A source that does not check cleanly to begin with has nothing to
        // round-trip: the oracle is "re-declaring adds no finding", which needs
        // a baseline with no findings to add to.
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
            // Every later scheme in this source is read off the baseline text,
            // so put it back before moving on.
            file.set_text(&mut db).to(source.clone());
            if added.is_empty() {
                continue;
            }
            if KNOWN_UNWRITABLE.contains(&format!("{id} :: {name}").as_str()) {
                continue;
            }
            let mut report = format!("{id} :: {name}\n  rendered  #: {scheme}\n");
            for diagnostic in added {
                let _ = writeln!(report, "  {} {}", diagnostic.code, diagnostic.message);
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

/// Each named definition's rendered scheme, with the byte offset its
/// annotation would be inserted at. That offset is the start of the line the
/// item begins on, so the `#:` block attaches to it.
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
        // An item already carrying an annotation is not a rendering the
        // checker chose. It is the user's text echoed back, and inserting a
        // second block above it stacks two annotations on one definition.
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

/// Whether the run of comment lines immediately above `line_start` holds a `#:`
/// block. A block may be several lines (`@forall` / `@param` / `@return`), and
/// ordinary comments may be interleaved, so the whole contiguous run counts.
fn already_annotated(text: &str, line_start: usize) -> bool {
    text[..line_start]
        .lines()
        .rev()
        .take_while(|line| line.trim_start().starts_with('#'))
        .any(|line| line.trim_start().starts_with("#:"))
}
