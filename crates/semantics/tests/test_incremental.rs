//! Incrementality contracts, observed through which queries salsa executes
//! after an edit: an edit re-checks the edited item and, only when its
//! exported scheme changed, the items that read it.

use semantics::diagnostics::file_diagnostics;
use semantics::testing::ProbeDatabase;
use semantics::types::{Atomic, scalar, unknown};
use semantics::{DocumentKind, Item, ProjectFiles, SourceFile, global_scheme, package_definitions};

fn settle(db: &ProbeDatabase, files: &[SourceFile]) {
    for &file in files {
        file_diagnostics(db, file);
    }
}

fn definition<'db>(db: &'db ProbeDatabase, name: &str) -> Item<'db> {
    let files = ProjectFiles::get(db);
    *package_definitions(db, files)
        .get(name)
        .unwrap_or_else(|| panic!("`{name}` is defined"))
}

#[test]
fn the_probe_sees_item_checks() {
    let db = ProbeDatabase::default();
    let files = db.project(&["x <- function() 1L\n", "use_x <- function() x()\n"]);
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["use_x", "x"]);
    settle(&db, &files);
    assert!(db.checked_since().is_empty(), "a re-read executes nothing");
}

#[test]
fn a_body_edit_keeping_the_scheme_rechecks_only_the_edited_item() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "x <- function() 1L\n",
        "use_x <- function() x()\n",
        "other <- function() 3L\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    db.edit(files[0], "x <- function() 2L\n".to_owned());
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["x"]);
}

#[test]
fn a_body_edit_changing_the_scheme_rechecks_its_readers_only() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "x <- function() 1L\n",
        "use_x <- function() x()\n",
        "other <- function() 3L\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    db.edit(files[0], "x <- function() \"a\"\n".to_owned());
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["use_x", "x"]);
}

#[test]
fn a_new_definition_rechecks_nothing_that_does_not_read_it() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "x <- function() 1L\n",
        "use_x <- function() x()\n",
        "other <- function() 3L\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    db.edit(files[0], "x <- function() 1L\nfresh <- 2L\n".to_owned());
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["fresh"]);
}

#[test]
fn defining_a_missing_name_rechecks_its_readers() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "x <- function() 1L\n",
        "use_later <- function() later()\n",
        "other <- function() 3L\n",
    ]);
    settle(&db, &files);
    assert!(
        !file_diagnostics(&db, files[1]).is_empty(),
        "`later` is unresolved"
    );
    db.checked_since();

    db.edit(
        files[0],
        "x <- function() 1L\nlater <- function() 2L\n".to_owned(),
    );
    settle(&db, &files);
    assert!(
        file_diagnostics(&db, files[1]).is_empty(),
        "`later` resolves"
    );
    assert_eq!(db.checked_since(), ["later", "use_later"]);
}

// Inserting a statement shifts every later position, but a read whose
// resolution is unchanged keeps its check; a read the insertion intercepts
// re-checks.
#[test]
fn inserting_a_statement_rechecks_only_the_reads_it_intercepts() {
    for kind in [DocumentKind::Package, DocumentKind::Script] {
        let mut db = ProbeDatabase::default();
        let file = SourceFile::new(&db, "base <- 1L\nderived <- base + 1L\n".to_owned(), kind);
        ProjectFiles::new(&db, vec![file]);
        settle(&db, &[file]);
        db.checked_since();

        db.edit(
            file,
            "base <- 1L\ninserted <- 2L\nderived <- base + 1L\n".to_owned(),
        );
        settle(&db, &[file]);
        assert_eq!(db.checked_since(), ["inserted"], "{kind:?}");

        db.edit(
            file,
            "base <- 1L\ninserted <- 2L\nbase <- 1.5\nderived <- base + 1L\n".to_owned(),
        );
        settle(&db, &[file]);
        assert_eq!(db.checked_since(), ["base", "derived"], "{kind:?}");
    }
}

#[test]
fn a_reexport_chain_follows_its_base() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&["a <- b\n", "b <- c\n", "c <- 1L\n"]);
    let integer = scalar(&db, Atomic::Integer);
    assert_eq!(global_scheme(&db, definition(&db, "a")).body, integer);

    db.edit(files[2], "c <- \"s\"\n".to_owned());
    let character = scalar(&db, Atomic::Character);
    assert_eq!(global_scheme(&db, definition(&db, "a")).body, character);
}

// Nothing concrete enters the cycle, so the fixpoint pins it to Unknown
// rather than spinning to the round cap.
#[test]
fn a_reexport_cycle_pins_to_unknown() {
    let db = ProbeDatabase::default();
    db.project(&["a <- b\n", "b <- a\n"]);
    for name in ["a", "b"] {
        assert_eq!(global_scheme(&db, definition(&db, name)).body, unknown(&db));
    }
}

// Each link resolves through the next one's scheme, so both demand and
// revalidation recurse as deep as the chain. On the stack the shipped binary
// gives analysis threads that must neither overflow nor be cut short to a
// stale Unknown.
#[test]
fn a_deep_reexport_chain_resolves_fully() {
    const LINKS: usize = 4096;
    const ANALYSIS_STACK_SIZE: usize = 64 * 1024 * 1024;
    let run = || {
        let mut db = ProbeDatabase::default();
        let chain: String = (0..LINKS)
            .map(|index| format!("link_{index} <- link_{}\n", index + 1))
            .collect();
        let files = db.project(&[&format!("{chain}link_{LINKS} <- 1L\n")]);
        let head = definition(&db, "link_0");
        assert_eq!(global_scheme(&db, head).body, scalar(&db, Atomic::Integer));

        // Revalidated without re-executing: nothing the chain reads changed.
        db.edit(
            files[0],
            format!("{chain}link_{LINKS} <- 1L\n# trailing comment\n"),
        );
        let head = definition(&db, "link_0");
        assert_eq!(global_scheme(&db, head).body, scalar(&db, Atomic::Integer));

        // Re-executed end to end: the base's type changed.
        db.edit(files[0], format!("{chain}link_{LINKS} <- 1.5\n"));
        let head = definition(&db, "link_0");
        assert_eq!(global_scheme(&db, head).body, scalar(&db, Atomic::Double));
    };
    std::thread::Builder::new()
        .stack_size(ANALYSIS_STACK_SIZE)
        .spawn(run)
        .expect("spawn the analysis thread")
        .join()
        .expect("the chain resolves");
}
