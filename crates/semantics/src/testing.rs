//! The semantic pipeline's fuzz invariant battery, shared by the in-tree
//! property harness and the coverage-guided `fuzz/` target so the contract
//! has one home. On every input: never panic (salsa fixpoints converge),
//! deterministic rendering across fresh databases, in-bounds diagnostic and
//! lint ranges, and incremental equivalence. Editing through the salsa setter
//! equals a fresh database on the edited text, and editing back restores the
//! original output.

use crate::diagnostics::{TypeRenderer, file_diagnostics, strict_diagnostics};
use crate::lints::{LintConfig, LintLevel, NameStyle, lint_file};
use crate::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, item_check, item_tree,
};
use salsa::Setter as _;

/// One canonical rendering of everything the pipeline produces for a file:
/// exported schemes, diagnostics (typing and strict), and every lint under
/// an everything-on configuration, so the determinism and incremental
/// invariants cover the lint layer too. It asserts range geometry inline.
pub fn render_semantics(db: &RootDatabase, file: SourceFile) -> String {
    let mut output = String::new();
    for &item in item_tree(db, file) {
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        let Some(name) = item.name(db).clone() else {
            continue;
        };
        let Some(check) = item_check(db, item) else {
            continue;
        };
        let Some(scheme) = check.scheme else {
            continue;
        };
        let mut renderer = TypeRenderer::default();
        output.push_str(&name);
        output.push_str(": ");
        output.push_str(&renderer.render_scheme(db, &scheme));
        output.push('\n');
    }
    let lint_config = LintConfig {
        naming_style: Some(NameStyle::Snake),
        unused_parameter: LintLevel::Warn,
        shadows_builtin: LintLevel::Warn,
        shadows_namespace: LintLevel::Warn,
        ..LintConfig::default()
    };
    let length = file.text(db).len();
    for diagnostic in file_diagnostics(db, file)
        .into_iter()
        .chain(strict_diagnostics(db, file))
        .chain(lint_file(db, file, &lint_config))
    {
        let start = u32::from(diagnostic.range.start()) as usize;
        let end = u32::from(diagnostic.range.end()) as usize;
        assert!(
            start <= end && end <= length,
            "diagnostic range {start}..{end} escapes the file (length {length})"
        );
        output.push_str(&format!(
            "{start}..{end} [{}] {}\n",
            diagnostic.code, diagnostic.message
        ));
    }
    output
}

fn fresh_output(source: &str, kind: DocumentKind) -> String {
    let db = RootDatabase::default();
    crate::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), kind);
    ProjectFiles::new(&db, vec![file]);
    render_semantics(&db, file)
}

/// The full battery for one (source, edit) pair under one document kind.
pub fn check_semantics_invariants(source: &str, edited_source: &str, kind: DocumentKind) {
    let first = fresh_output(source, kind);

    let mut db = RootDatabase::default();
    crate::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), kind);
    ProjectFiles::new(&db, vec![file]);
    let before = render_semantics(&db, file);
    // This IS the determinism check: `before` and `first` come from two
    // independently built databases. A third build only bought a second
    // comparison of the same pair, and each one costs a full re-parse of the
    // shipped stubs (~113 ms against ~1.4 ms for the rest of a round), which is
    // where this battery spends nearly all of its time.
    assert_eq!(
        before, first,
        "non-deterministic pipeline output across fresh databases for:\n{source}"
    );
    file.set_text(&mut db).to(edited_source.to_owned());
    let incremental = render_semantics(&db, file);
    let fresh = fresh_output(edited_source, kind);
    assert_eq!(
        incremental, fresh,
        "incremental output diverges from a fresh build after editing\n---- from:\n{source}\n---- to:\n{edited_source}"
    );
    file.set_text(&mut db).to(source.to_owned());
    let back = render_semantics(&db, file);
    assert_eq!(
        back, before,
        "output not restored after editing back\n---- source:\n{source}\n---- via:\n{edited_source}"
    );
}

/// Single-input entry for the coverage-guided target: the document kind and
/// the edited variant derive deterministically from the input bytes, so one
/// byte stream exercises the whole battery including the incremental arm.
pub fn check_semantics_input(input: &str) {
    let hash = input.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    });
    let kind = if hash % 4 == 0 {
        DocumentKind::Script
    } else {
        DocumentKind::Package
    };
    // The edit truncates at a hash-derived char boundary and continues with
    // a small live statement, driving the splice cache and re-validation.
    let cut = input.floor_char_boundary((hash as usize >> 8) % (input.len() + 1));
    let edited = format!("{}\nprobe <- 1L\n", &input[..cut]);
    check_semantics_invariants(input, &edited, kind);
}
