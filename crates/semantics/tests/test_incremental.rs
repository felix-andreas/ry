use {
    salsa::{Database, Durability, Setter},
    semantics::{
        AnalysisSettings, DataFiles, Db, DocumentKind, Item, ProjectFiles, RootDatabase,
        SourceFile, SourcePaths,
        diagnostics::{Severity, derive_from_syntax, file_diagnostics, top_level_diagnostics},
        global_scheme,
        hir::Symbol,
        item_group, item_naming, item_tree,
        lints::{LintConfig, lint_file},
        load_index::{PackageFile, PackageSources, package_index},
        metadata::{PackageMetadata, attached_union},
        package_definitions, parse, session,
        stubs::install_shipped_stubs,
        testing::{ProbeDatabase, render_semantics},
        types::{Atomic, scalar},
    },
    std::{collections::BTreeMap, thread},
};

fn settle(db: &ProbeDatabase, files: &[SourceFile]) {
    for &file in files {
        file_diagnostics(db, file);
    }
}

fn definition<'db>(db: &'db ProbeDatabase, name: &str) -> Item<'db> {
    let files = ProjectFiles::get(db);
    *package_definitions(db, files)
        .get(&Symbol::new(name))
        .unwrap_or_else(|| panic!("`{name}` is defined"))
}

/// Each case settles its project, then edits its first file once per step and names the items
/// checked again after that step.
#[test]
fn an_edit_rechecks_the_edited_items_and_the_readers_whose_answers_change() {
    const READERS: &[&str] = &[
        "x <- function() 1L\n",
        "use_x <- function() x()\n",
        "other <- function() 3L\n",
    ];
    const RECURSIVE: &str =
        "count <- function(n) if (n > 0L) count(n - 1L) else 0L\nplain <- function() 1L\n";
    /// The edited first file's text, and the items checked again after it.
    type Step = (&'static str, &'static [&'static str]);
    let cases: &[(&str, &[&str], &[Step])] = &[
        (
            "a body edit keeping the scheme",
            READERS,
            &[("x <- function() 2L\n", &["x"])],
        ),
        (
            "a body edit changing the scheme",
            READERS,
            &[("x <- function() \"a\"\n", &["use_x", "x"])],
        ),
        (
            "a new definition nothing reads",
            READERS,
            &[("x <- function() 1L\nfresh <- 2L\n", &["fresh"])],
        ),
        (
            // A file's cyclic groups name items rather than positions in the file, so a
            // statement added before or after a recursive function leaves its check alone.
            "new statements beside a recursive function",
            &[RECURSIVE, "other <- function() count(2L)\n"],
            &[(
                "first <- 1L\ncount <- function(n) if (n > 0L) count(n - 1L) else 0L\n\
                 plain <- function() 1L\nlast <- 2L\n",
                &["first", "last"],
            )],
        ),
        (
            "defining a name that was missing",
            &[
                "x <- function() 1L\n",
                "use_later <- function() later()\n",
                "other <- function() 3L\n",
            ],
            &[(
                "x <- function() 1L\nlater <- function() 2L\n",
                &["later", "use_later"],
            )],
        ),
        (
            "editing an inline annotation",
            &[
                "count: integer <- 1L\n",
                "use_count <- function() count\n",
                "other <- function() 3L\n",
            ],
            &[("count: double <- 1L\n", &["count", "use_count"])],
        ),
        (
            "reference class fields",
            &[
                "Workbook <- setRefClass(\"Workbook\", fields = c(font = \"character\"))\n",
                "Sheet <- setRefClass(\"Sheet\", contains = \"Workbook\",\n  methods = list(widths = function() nchar(font) + size))\n",
                "other <- function() 3L\n",
            ],
            &[
                (
                    "Workbook <- setRefClass(\"Workbook\", fields = c(font = \"character\", unread = \"list\"))\n",
                    &["Workbook"],
                ),
                (
                    "Workbook <- setRefClass(\"Workbook\", fields = c(font = \"character\", size = \"numeric\"))\n",
                    &["Sheet", "Workbook"],
                ),
            ],
        ),
    ];
    for &(name, sources, steps) in cases {
        let mut db = ProbeDatabase::default();
        let files = db.project(sources);
        settle(&db, &files);
        db.checked_since();
        settle(&db, &files);
        assert!(
            db.checked_since().is_empty(),
            "{name}: settling twice rechecks nothing"
        );
        for &(text, expected) in steps {
            db.edit(files[0], text.to_owned());
            settle(&db, &files);
            assert_eq!(db.checked_since(), expected, "{name}");
        }
    }
}

fn settle_top_level(db: &ProbeDatabase, files: &[SourceFile]) {
    settle(db, files);
    for &file in files {
        top_level_diagnostics(db, file);
    }
}

#[test]
fn an_effect_added_to_a_pure_function_reclassifies_only_that_function() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "#: @pure\nscale <- function(x) x * 2\n",
        "scaled <- scale(2)\n",
    ]);
    settle_top_level(&db, &files);
    db.executions_since(&[]);

    db.edit(
        files[0],
        "#: @pure\nscale <- function(x) {\n  cat(x)\n  x * 2\n}\n".to_owned(),
    );
    settle_top_level(&db, &files);
    assert_eq!(
        db.executions_since(&["item_top_level", "item_syntax_flags", "item_check"]),
        [1, 1, 1],
        "the edited body is reclassified and its syntax flags are reread, but they come out \
         unchanged, so the caller's classification stays memoized"
    );
}

#[test]
fn removing_a_purity_marker_reclassifies_its_callers() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "#: @pure\nscale <- function(x) x * 2\n",
        "scaled <- scale(2)\n",
        "other <- 1\n",
    ]);
    settle_top_level(&db, &files);
    db.executions_since(&[]);

    assert!(top_level_diagnostics(&db, files[1]).is_empty());
    db.executions_since(&[]);
    db.edit(files[0], "scale <- function(x) x * 2\n".to_owned());
    settle_top_level(&db, &files);
    assert_eq!(top_level_diagnostics(&db, files[1]).len(), 1);
    assert_eq!(
        db.executions_since(&["item_top_level"]),
        [1],
        "only the caller is reclassified: `scale`'s HIR, which has no annotations, is unchanged"
    );
}

#[test]
fn a_new_definition_reprojects_the_name_maps_and_checks_only_the_new_item() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "x <- function() 1L\n",
        "use_x <- function() x()\nalso <- function() use_x()\n",
        "other <- function() also()\n",
    ]);
    settle(&db, &files);
    db.executions_since(&[]);

    db.edit(files[0], "x <- function() 1L\nfresh <- 2L\n".to_owned());
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&[
            "item_links",
            "item_check",
            "file_interface_groups",
            "duplicate_binding_map",
            "package_definition_index",
        ]),
        [5, 1, 1, 1, 1],
        "each item projects the maps again and only the new one is checked; the edited file \
         walks its groups again, and the fold runs once and its duplicates backdate"
    );
}

/// An item's links ask whether its file belongs to the project, which reads the file list and
/// not what the project's files bind, so a name a script adds reaches no package item.
#[test]
fn a_new_name_in_a_script_links_no_package_item_again() {
    let mut db = ProbeDatabase::default();
    let package = SourceFile::new(
        &db,
        "x <- function() 1L\nuse_x <- function() x()\n".to_owned(),
        DocumentKind::Package,
    );
    let script = SourceFile::new(&db, "result <- x()\n".to_owned(), DocumentKind::Script);
    ProjectFiles::new(&db, vec![package, script]);
    settle(&db, &[package, script]);
    db.executions_since(&[]);

    db.edit(script, "result <- x()\nfresh <- 2L\n".to_owned());
    settle(&db, &[package, script]);
    assert_eq!(
        db.executions_since(&["item_links"]),
        [2],
        "the script's two items resolve their names, and neither package item does"
    );
}

/// A check constructs names no naming lists, such as the S3 method of an operand's class. In a
/// package file the package layer answers whether the file can bind one, so the check reads the
/// binders of its own file alone and lowers no item of a file it cannot see.
#[test]
fn a_package_check_reads_the_binders_of_its_own_file_alone() {
    let db = ProbeDatabase::default();
    install_shipped_stubs(&db);
    let package = SourceFile::new(
        &db,
        "stamp <- function() Sys.Date() + 1L\n".to_owned(),
        DocumentKind::Package,
    );
    let neighbour = SourceFile::new(
        &db,
        "width <- function() 3L\n".to_owned(),
        DocumentKind::Package,
    );
    let test = SourceFile::new(
        &db,
        "test_that(\"it adds\", {\n  value <- 1L\n  expect_equal(value, 1L)\n})\n".to_owned(),
        DocumentKind::Test,
    );
    ProjectFiles::new(&db, vec![package, neighbour, test]);
    file_diagnostics(&db, package);
    let [names, file_binders, frame_binders, lowered] =
        db.executions_since(&["package_name", "file_binders", "frame_binder", "item_hir"])[..]
    else {
        panic!("four counts");
    };
    assert!(
        names > 0,
        "the check constructs a name and asks the package layer"
    );
    assert_eq!(
        (file_binders, frame_binders, lowered),
        (1, 0, 1),
        "the check reads its own file's binders, keeps no memo for a name the package layer \
         does not bind, and lowers its own item alone"
    );
}

/// A test file has no layer over its own top-level names, so a name its check constructs is
/// answered by the file's binders.
#[test]
fn a_test_file_asks_its_own_binders_for_a_constructed_name() {
    let db = ProbeDatabase::default();
    install_shipped_stubs(&db);
    let test = SourceFile::new(
        &db,
        "stamp <- function() Sys.Date() + 1L\n".to_owned(),
        DocumentKind::Test,
    );
    ProjectFiles::new(&db, vec![test]);
    file_diagnostics(&db, test);
    assert!(
        db.executions_since(&["frame_binder"])[0] > 0,
        "the file's binders answer a constructed name"
    );
}

#[test]
fn a_new_helper_definition_rediagnoses_no_test_file_that_does_not_read_it() {
    let mut db = ProbeDatabase::default();
    let helper = SourceFile::new(
        &db,
        "helper_a <- function() 1L\n".to_owned(),
        DocumentKind::TestHelper,
    );
    let test = SourceFile::new(
        &db,
        "helper_a()\nmisspeled()\n".to_owned(),
        DocumentKind::Test,
    );
    ProjectFiles::new(&db, vec![helper, test]);
    settle(&db, &[helper, test]);
    db.executions_since(&["file_diagnostics"]);

    db.edit(
        helper,
        "helper_a <- function() 1L\nfresh <- function() 2L\n".to_owned(),
    );
    settle(&db, &[helper, test]);
    assert_eq!(
        db.executions_since(&["file_diagnostics"]),
        [1],
        "only the edited helper's diagnostics re-run"
    );
}

#[test]
fn a_sourced_helper_edit_rechecks_only_the_script_reads_it_changes() {
    let mut db = ProbeDatabase::default();
    let helpers = SourceFile::new(
        &db,
        "scale_to <- function(x) x * 2L\n".to_owned(),
        DocumentKind::Script,
    );
    let script = SourceFile::new(
        &db,
        "source(\"lib/helpers.R\")\nresult <- scale_to(1L)\nunrelated <- 3L\n".to_owned(),
        DocumentKind::Script,
    );
    SourcePaths::new(
        &db,
        [
            ("lib/helpers.R".to_owned(), helpers),
            ("run.R".to_owned(), script),
        ]
        .into(),
    );
    ProjectFiles::new(&db, vec![helpers, script]);
    settle(&db, &[helpers, script]);
    db.checked_since();

    db.edit(
        helpers,
        "scale_to <- function(x) x * 2L\nfresh <- function() 2L\n".to_owned(),
    );
    settle(&db, &[helpers, script]);
    assert_eq!(db.checked_since(), ["fresh"]);

    db.edit(
        helpers,
        "scale_to <- function(x) x * 2.5\nfresh <- function() 2L\n".to_owned(),
    );
    settle(&db, &[helpers, script]);
    assert_eq!(db.checked_since(), ["result", "scale_to"]);
}

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

// Demand and revalidation recurse as deep as the chain, on the shipped analysis stack size.
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

        db.edit(
            files[0],
            format!("{chain}link_{LINKS} <- 1L\n# trailing comment\n"),
        );
        let head = definition(&db, "link_0");
        assert_eq!(global_scheme(&db, head).body, scalar(&db, Atomic::Integer));

        db.edit(files[0], format!("{chain}link_{LINKS} <- 1.5\n"));
        let head = definition(&db, "link_0");
        assert_eq!(global_scheme(&db, head).body, scalar(&db, Atomic::Double));
    };
    thread::Builder::new()
        .stack_size(ANALYSIS_STACK_SIZE)
        .spawn(run)
        .expect("spawn the analysis thread")
        .join()
        .expect("the chain resolves");
}

#[test]
fn turning_inline_annotations_off_rechecks_only_items_that_have_them() {
    let mut db = ProbeDatabase::default();
    let settings = AnalysisSettings::new(&db, true, true, true);
    let files = db.project(&[
        "typed <- function(x: integer) -> integer x\n",
        "untyped <- function(x) x + 1L\n",
        "#: integer\ncommented <- 1L\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    settings.set_inline_annotations(&mut db).to(false);
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["typed"]);
}

#[test]
fn turning_the_prelude_off_rechecks_only_items_that_use_the_library() {
    let mut db = ProbeDatabase::default();
    install_shipped_stubs(&db);
    let settings = AnalysisSettings::new(&db, true, true, true);
    let files = db.project(&[
        "lengths <- function(x) List.len(x)\n",
        "plain <- function(x) length(x)\n",
        "qualified <- function(x) std::List.len(x)\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    settings.set_prelude(&mut db).to(false);
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["lengths"]);
}

#[test]
fn an_edit_to_a_low_durability_file_never_revisits_the_others_trees() {
    let mut db = ProbeDatabase::default();
    let files: Vec<SourceFile> = [
        "x <- function() 1L\n",
        "use_x <- function() x()\n",
        "other <- function() 3L\n",
    ]
    .into_iter()
    .map(|text| {
        SourceFile::builder(text.to_owned(), DocumentKind::Package)
            .durability(Durability::MEDIUM)
            .new(&db)
    })
    .collect();
    let _project = ProjectFiles::builder(files.clone())
        .durability(Durability::LOW)
        .new(&db);
    settle(&db, &files);
    files[0]
        .set_text(&mut db)
        .with_durability(Durability::LOW)
        .to("x <- function() 2L\n".to_owned());
    settle(&db, &files);
    db.checked_since();

    let parses = db.trees().parses();
    db.edit(files[0], "x <- function() 3L\n".to_owned());
    settle(&db, &files);
    assert_eq!(
        db.trees().parses() - parses,
        1,
        "only the edited file parses"
    );
    assert_eq!(
        db.executions_since(&["item_spans", "item_hir"]),
        [1, 1],
        "only the edited file re-derives its items from its tree"
    );
}

/// An item keeps the stamp of its syntax while that syntax is unchanged, wherever the item
/// moves in its file. Its stamp is read again after every edit to the file and comes out
/// equal, so nothing derived from the item's syntax runs again. The `#:` block of an item is
/// a sibling of the item, attached by a per-file walk that comes out equal while no block
/// changes, so the annotated item's annotations stay memoized too.
#[test]
fn an_edit_lowers_only_the_items_whose_syntax_changed() {
    let mut db = ProbeDatabase::default();
    let source = |second: &str| {
        format!(
            "first <- function() 1L\n{second}\n#: @pure\nthird <- function(x) x + 1L\nfourth <- 4L\n\
             fourth <- 4L\n"
        )
    };
    let files = db.project(&[&source("second <- function() 2L")]);
    let derived = [
        "item_stamp",
        "item_syntax_flags",
        "item_hir",
        "item_annotations",
        "item_naming",
    ];
    settle(&db, &files);
    assert_eq!(db.executions_since(&derived), [5, 5, 5, 5, 5]);

    db.edit(files[0], source("second <- function() 20L"));
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&derived),
        [5, 1, 1, 1, 1],
        "an edit inside one item"
    );

    db.edit(
        files[0],
        format!(
            "# moved\n\nzeroth <- 0L\n{}",
            source("second <- function() 20L")
        ),
    );
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&derived),
        [6, 1, 1, 1, 1],
        "a new item above the others, which all move"
    );

    db.edit(files[0], source("second <- function() 20L"));
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&derived),
        [5, 0, 0, 0, 0],
        "the new item removed again"
    );

    db.edit(
        files[0],
        "first <- function() 1L\nsecond <- function() 20L\n#: @pure\nthird <- function(x) x + 1L\n\
         fourth <- 4L\n",
    );
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&derived),
        [4, 0, 0, 0, 0],
        "the second of two equal statements removed"
    );

    db.edit(
        files[0],
        "first <- function() 10L\nsecond <- function() 20L\n#: @pure\nthird <- function(x) x + 1L\n\
         fourth <- 40L\n",
    );
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&derived),
        [4, 4, 4, 4, 2],
        "edits at both ends of the file leave one changed region that spans it, so every item \
         lowers again, and the two in the middle lower to what they were"
    );
}

#[test]
fn an_edit_to_a_low_durability_file_never_revisits_the_others_items() {
    let mut db = ProbeDatabase::default();
    let files: Vec<SourceFile> = [
        "x <- function() 1L\n",
        "use_x <- function() x()\nif (TRUE) slot <- 1L\ny <- function() use_x()\n",
        "other <- function() 3L\nmore <- function() other()\nlast <- 4L\n",
    ]
    .into_iter()
    .map(|text| {
        SourceFile::builder(text.to_owned(), DocumentKind::Package)
            .durability(Durability::MEDIUM)
            .new(&db)
    })
    .collect();
    // LOW, as hosts create it: the project-wide maps read it, so they are LOW before any
    // file's first edit and can backdate through that file's drop from MEDIUM.
    let _project = ProjectFiles::builder(files.clone())
        .durability(Durability::LOW)
        .new(&db);
    settle(&db, &files);
    db.checked_since();
    files[0]
        .set_text(&mut db)
        .with_durability(Durability::LOW)
        .to("x <- function() 2L\n".to_owned());
    settle(&db, &files);
    assert_eq!(
        db.checked_since(),
        ["x"],
        "a file's first edit drops its durability without re-executing the rest of the project"
    );

    db.edit(files[0], "x <- function() 3L\n".to_owned());
    settle(&db, &files);
    // Each file's group walk verifies the links it reaches, so validations of `item_links`
    // follow the items a check demands. What must not happen is any item recomputing them.
    assert_eq!(
        db.executions_since(&["item_links", "item_bound_names"]),
        [0, 0],
        "the edited item binds only its own name, which its memoized syntax flags say and which \
         backdate; its naming backdates too, so nothing recomputes links or the edges they hold, \
         and none of the five items in the untouched files reruns either"
    );
}

/// Each file has every kind of syntax a later pass reads: a lint, a `#:` comment, and a
/// statement annotated both ways. Two files far apart both bind `shared`, so reporting the
/// duplicate reads the other file's facts, and the trailing script reports an unused binding.
fn wide_project(db: &ProbeDatabase) -> Vec<SourceFile> {
    let mut files: Vec<SourceFile> = (0..200)
        .map(|index| {
            let mut source = wide_project_source(index);
            if index == 0 || index == 199 {
                source.push_str("shared <- 1L\n");
            }
            SourceFile::new(db, source, DocumentKind::Package)
        })
        .collect();
    files.push(SourceFile::new(
        db,
        "unused <- 1L\nunused <- 2L\nprint(unused)\n".to_owned(),
        DocumentKind::Script,
    ));
    ProjectFiles::new(db, files.clone());
    files
}

/// The order `ry check` runs in: derive from each tree while holding it, name every item, then
/// report.
fn cold_check(db: &mut ProbeDatabase, files: &[SourceFile], lints: &LintConfig) {
    for &file in files {
        let _held = db.trees().hold(file);
        derive_from_syntax(db, file, lints);
    }
    // `ry check` writes the attached namespaces here, so later passes verify memos instead of
    // reusing them within one revision.
    db.synthetic_write(Durability::LOW);
    for &file in files {
        for &item in item_tree(db, file) {
            item_naming(db, item);
        }
    }
    report(db, files, lints);
}

fn report(db: &mut ProbeDatabase, files: &[SourceFile], lints: &LintConfig) {
    for &file in files {
        file_diagnostics(db, file);
        lint_file(db, file, lints);
    }
}

/// A cold check parses each file once, both on a host that keeps every tree and on a
/// diagnostics-only host, which keeps two trees here, never one per file. The diagnostics-only
/// host parses once because the pass that derives from a tree holds it and no later pass reads
/// one, and it reports what the tree-keeping host reports.
#[test]
fn a_cold_check_parses_each_file_once_on_either_host() {
    let mut bounded = ProbeDatabase::diagnostics_only();
    let files = wide_project(&bounded);
    let lints = LintConfig::default();
    cold_check(&mut bounded, &files, &lints);
    assert!(bounded.trees().stored() <= 2);
    assert_eq!(bounded.trees().parses(), files.len() as u64);
    let codes = |file: SourceFile| -> Vec<&'static str> {
        file_diagnostics(&bounded, file)
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect()
    };
    assert!(
        codes(files[0]).contains(&"duplicate"),
        "{:?}",
        codes(files[0])
    );
    assert!(
        codes(files[200]).contains(&"unused"),
        "{:?}",
        codes(files[200])
    );
    assert!(
        lint_file(&bounded, files[0], &lints)
            .iter()
            .any(|diagnostic| diagnostic.code == "object-system"),
        "the bare `setClass` call is reported"
    );

    let mut keeping = ProbeDatabase::default();
    let kept_files = wide_project(&keeping);
    cold_check(&mut keeping, &kept_files, &lints);
    assert_eq!(keeping.trees().parses(), kept_files.len() as u64);
    let expected = diagnostics_of(&keeping, &kept_files);
    assert!(!expected.is_empty(), "the project has findings to compare");
    assert_eq!(diagnostics_of(&bounded, &files), expected);
}

/// A held file's tree survives the reads of more files than the store keeps, and is dropped like
/// any other once the hold ends.
#[test]
fn a_held_tree_outlives_the_reads_of_other_files() {
    let db = ProbeDatabase::diagnostics_only();
    let files = db.project(&["a <- 1L\n", "b <- 2L\n", "c <- 3L\n", "d <- 4L\n"]);
    let trees = db.trees();
    {
        let _held = trees.hold(files[0]);
        for &file in &files {
            parse(&db, file);
        }
        let parses = trees.parses();
        parse(&db, files[0]);
        assert_eq!(trees.parses(), parses, "the held file is not parsed again");
    }
    for &file in &files[1..] {
        parse(&db, file);
    }
    let parses = trees.parses();
    parse(&db, files[0]);
    assert_eq!(trees.parses(), parses + 1, "the released file was dropped");
}

#[test]
fn an_edit_after_a_cold_check_reparses_and_relints_only_the_edited_file() {
    let mut db = ProbeDatabase::default();
    let files = wide_project(&db);
    let lints = LintConfig::default();
    cold_check(&mut db, &files, &lints);
    db.checked_since();

    let edited = wide_project_source(0).replace("x + 0L", "x + 9L");
    let parses = db.trees().parses();
    db.edit(files[0], edited);
    report(&mut db, &files, &lints);
    assert_eq!(db.trees().parses() - parses, 1);
    assert_eq!(
        db.executions_since(&["file_syntax_lints", "item_check"]),
        [1, 1]
    );
}

fn wide_project_source(index: usize) -> String {
    format!(
        "f{index} <- function(x) x + {index}L\n#: integer\nn{index} <- T\n#: integer\nm{index}: integer <- 1L\nsetClass(\"A{index}\")\n"
    )
}

fn package(
    db: &ProbeDatabase,
    files: &[(&str, &str)],
    namespace: &str,
) -> (PackageSources, Vec<SourceFile>) {
    let files: Vec<PackageFile> = files
        .iter()
        .map(|(path, text)| PackageFile {
            path: (*path).to_owned(),
            file: SourceFile::new(db, (*text).to_owned(), DocumentKind::Package),
        })
        .collect();
    let handles = files.iter().map(|file| file.file).collect();
    let sources = PackageSources::new(
        db,
        files,
        namespace.to_owned(),
        "Package: demo\nVersion: 1.0\n".to_owned(),
        false,
    );
    (sources, handles)
}

const LOAD_QUERIES: &[&str] = &[
    "file_items",
    "file_binds",
    "package_bindings",
    "item_immediate_reads",
    "package_index",
];

#[test]
fn a_body_edit_rederives_one_file_of_the_load_index() {
    let mut db = ProbeDatabase::default();
    let (sources, files) = package(
        &db,
        &[
            (
                "R/a.R",
                "first <- function() 1\nsecond <- function() 2\nvalue <- first()\n",
            ),
            ("R/b.R", "third <- function() 3\nsetClass(\"Point\")\n"),
        ],
        "export(first)\n",
    );
    package_index(&db, sources);
    db.executions_since(&[]);

    db.edit(
        files[0],
        "first <- function() 10\nsecond <- function() 2\nvalue <- first()\n",
    );
    let index = package_index(&db, sources);
    assert_eq!(
        db.executions_since(LOAD_QUERIES),
        [1, 1, 0, 0, 1],
        "only the edited file's layer runs again, and its bindings stop at the firewall"
    );
    assert_eq!(
        index.files[0].items.items[0].hash,
        item_hash("first <- function() 10")
    );
}

#[test]
fn a_namespace_edit_rederives_no_item() {
    let mut db = ProbeDatabase::default();
    let (sources, _) = package(
        &db,
        &[("R/a.R", "first <- function() 1\nvalue <- first()\n")],
        "export(first)\n",
    );
    package_index(&db, sources);
    db.executions_since(&[]);

    sources
        .set_namespace(&mut db)
        .to("export(first)\nexport(value)\n".to_owned());
    let index = package_index(&db, sources);
    assert_eq!(db.executions_since(LOAD_QUERIES), [0, 0, 0, 0, 1]);
    assert_eq!(index.namespace.exports, ["first", "value"]);
}

#[test]
fn moving_a_statement_down_keeps_its_load_facts() {
    let mut db = ProbeDatabase::default();
    let (sources, files) = package(
        &db,
        &[("R/a.R", "value <- helper()\nhelper <- function() 1\n")],
        "",
    );
    package_index(&db, sources);
    db.executions_since(&[]);

    db.edit(files[0], "\n\nvalue <- helper()\nhelper <- function() 1\n");
    package_index(&db, sources);
    assert_eq!(
        db.executions_since(LOAD_QUERIES),
        [1, 1, 0, 0, 1],
        "positions are per-file facts, so only the file layer runs again"
    );
}

fn item_hash(text: &str) -> u64 {
    semantics::load_index::fnv1a(text.as_bytes())
}

#[test]
fn a_data_file_edit_rechecks_only_the_reads_of_that_file() {
    let mut db = ProbeDatabase::default();
    install_shipped_stubs(&db);
    let files = db.project(&[
        "library(data.table)\n",
        "prices <- fread(\"prices.csv\")\n",
        "trades <- fread(\"trades.csv\")\n",
        "frame <- DataFrame.read_csv(\"prices.csv\")\n",
        "quotes <- DataFrame.read_csv(\"trades.csv\")\n",
    ]);
    let attached = attached_union(&db, files.iter().copied());
    PackageMetadata::new(
        &db,
        Vec::new(),
        Default::default(),
        attached,
        None,
        Default::default(),
        None,
    );
    let layouts = |prices: &str| {
        BTreeMap::from([
            (
                "prices.csv".to_owned(),
                sniff::sniff_head(prices.as_bytes()),
            ),
            (
                "trades.csv".to_owned(),
                sniff::sniff_head(b"id,size\n1,2\n"),
            ),
        ])
    };
    let data_files = DataFiles::new(&db, layouts("id,price\n1,2.5\n"));
    settle(&db, &files);
    db.checked_since();

    data_files
        .set_layouts(&mut db)
        .to(layouts("id,price,ticker\n1,2.5,a\n"));
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["frame", "prices"]);
    let scheme = global_scheme(&db, definition(&db, "prices"));
    assert_eq!(
        semantics::diagnostics::TypeRenderer::default().render_scheme(&db, &scheme),
        "data.table{id: integer[], price: double[], ticker: character[]}"
    );
}

/// Renders every file, edits `files[edited]` to `text`, and renders again after editing it
/// back. Breaking a cycle while a member's value stays the same re-executes that member with
/// only older inputs, which salsa rejects unless the cycle never formed inside salsa.
fn round_trip_edit(kind: DocumentKind, sources: &[&str], edited: usize, text: &str) {
    let mut db = RootDatabase::default();
    install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(&db, (*source).to_owned(), kind))
        .collect();
    ProjectFiles::new(&db, files.clone());
    let render_all = |db: &RootDatabase| -> Vec<String> {
        files
            .iter()
            .map(|&file| render_semantics(db, file))
            .collect()
    };
    let before = render_all(&db);
    files[edited].set_text(&mut db).to(text.to_owned());
    render_all(&db);
    files[edited]
        .set_text(&mut db)
        .to(sources[edited].to_owned());
    assert_eq!(
        render_all(&db),
        before,
        "output not restored after editing back"
    );
}

#[test]
fn removing_a_cross_file_cycle_and_restoring_it_revalidates() {
    // `g <- rev(g)` reads the other file's `g`, which reads itself; emptying that file makes
    // the first `g` read itself instead.
    round_trip_edit(
        DocumentKind::Package,
        &["g <- rev(g)\nf <- g$x\n", "g <- g + 1L\n"],
        1,
        "",
    );
    for reader in ["rev", "sort", "List.from_r"] {
        round_trip_edit(
            DocumentKind::Package,
            &[
                &format!("g <- {reader}(g)\nf <- g$x + g[[1L]]\n"),
                "g <- g + 1L\n",
            ],
            1,
            "",
        );
    }
}

#[test]
fn a_cycle_through_an_earlier_writer_revalidates() {
    // The added last `g` is the winner the first one reads, and it reads the first one back
    // as the earlier writer in its file: a cycle a graph of winners alone would miss.
    round_trip_edit(
        DocumentKind::Package,
        &["g <- rev(g)\nf <- g$x\n"],
        0,
        "g <- rev(g)\nf <- g$x\ng <- g\n",
    );
}

#[test]
fn a_script_cycle_through_deferred_reads_revalidates() {
    let source = "f <- function() rev(g())\nk <- f\ng <- function() h()\n";
    let cyclic = "f <- function() rev(g())\nk <- f\ng <- function() f()\n";
    round_trip_edit(DocumentKind::Script, &[source], 0, cyclic);
    round_trip_edit(DocumentKind::Test, &[source], 0, cyclic);
}

#[test]
fn a_statement_reading_its_own_conditional_write_is_a_cyclic_group() {
    let db = ProbeDatabase::default();
    let files = db.project(&["if (TRUE) b <- 3L\nwhile (b > 0L) b <- b - 1L\n"]);
    let items = item_tree(&db, files[0]);
    let project = ProjectFiles::get(&db);
    assert!(
        item_group(&db, project, items[1]).is_some(),
        "the loop reads the slot it writes, so the group fixpoint resolves it, not salsa"
    );
    assert!(item_group(&db, project, items[0]).is_none());
    round_trip_edit(
        DocumentKind::Package,
        &["if (TRUE) b <- 3L\nwhile (b > 0L) b <- b - 1L\n"],
        0,
        "while (b > 0L) b <- b - 1L\n",
    );
}

const CYCLIC_PROJECT: &[&str] = &[
    "x <- function() 1L\nping <- function(n) if (n > 0L) pong(n - 1L) else 0L\n",
    "use_x <- function() x()\npong <- function(n) ping(n)\n",
    "other <- function() also()\nbad <- function() use_x() + \"a\"\n",
];

fn diagnostics_of(db: &ProbeDatabase, files: &[SourceFile]) -> Vec<String> {
    files
        .iter()
        .flat_map(|&file| file_diagnostics(db, file))
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect()
}

#[test]
fn a_check_resolves_the_names_it_spells_through_its_links_and_one_group_walk_per_file() {
    if syntax::testing::invariants_enabled() {
        return;
    }
    let db = ProbeDatabase::default();
    let files = db.project(CYCLIC_PROJECT);
    settle(&db, &files);
    assert_eq!(
        db.executions_since(&["item_links", "frame_binder", "file_interface_groups"]),
        [6, 0, 3],
        "each item resolves its names in one query, none through a query per name, and each \
         file finds its items' cyclic groups in one walk"
    );
}

#[test]
fn a_diagnostics_only_check_reports_what_an_editor_check_reports() {
    let editor = ProbeDatabase::default();
    let editor_files = editor.project(CYCLIC_PROJECT);
    let command = ProbeDatabase::diagnostics_only();
    let command_files = command.project(CYCLIC_PROJECT);
    let expected = diagnostics_of(&editor, &editor_files);
    assert!(!expected.is_empty(), "the project has findings to compare");
    assert_eq!(diagnostics_of(&command, &command_files), expected);
}

/// Invariants are process-wide, so the cross-check runs in a child process instead of
/// switching them on under the tests above that count query executions.
#[test]
fn links_and_file_groups_agree_with_their_from_scratch_forms_under_invariants() {
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "link_invariants_child", "--nocapture"])
        .env("RY_LINK_INVARIANTS_CHILD", "1")
        .status()
        .expect("child test runs");
    assert!(status.success(), "the link cross-check failed");
}

#[test]
fn link_invariants_child() {
    if std::env::var_os("RY_LINK_INVARIANTS_CHILD").is_none() {
        return;
    }
    syntax::testing::enable_invariants();
    let db = ProbeDatabase::default();
    let files = db.project(CYCLIC_PROJECT);
    settle(&db, &files);
    assert!(
        db.executions_since(&["package_name"])[0] > 0,
        "the cross-check consulted the per-name projections"
    );

    // Each way a file binds a method name that a check of the same file constructs. Its layer
    // must list the name, or the cross-check in the constructed-name lookup panics. The last
    // source binds none of the names it constructs.
    let sources = [
        "`+.Date` <- function(e1, e2) e1\nstamp <- function() Sys.Date() + 1L\n",
        "if (interactive()) sum.matrix <- function(x, ...) 1L\ntotal <- function() sum(matrix(1:4, 2))\n",
        "Summary.matrix <- Arith.matrix <- function(...) 1L\nshifted <- function() matrix(1:4, 2) + sum(matrix(1:4, 2))\n",
        "tryCatch(`-.POSIXct` <- function(e1, e2) 1L, error = identity)\nelapsed <- function() Sys.time() - Sys.time()\n",
        "for (step in 1:2) `+.matrix` <- function(e1, e2) e1\nadded <- function() matrix(1:4, 2) + 1L\n",
        "unbound <- function() Sys.time() - Sys.time() + sum(matrix(1:4, 2))\n",
    ];
    for kind in [DocumentKind::Package, DocumentKind::TestHelper] {
        let db = ProbeDatabase::default();
        install_shipped_stubs(&db);
        let files: Vec<SourceFile> = sources
            .iter()
            .map(|&source| SourceFile::new(&db, source.to_owned(), kind))
            .collect();
        ProjectFiles::new(&db, files.clone());
        settle(&db, &files);
    }

    // Every read of an item's stamp compares the syntax the stamp names with the item's node
    // in the file as it stands, through edits that move, repeat, add and remove statements.
    let mut db = ProbeDatabase::default();
    let files = db.project(&["first <- 1L\nsecond <- function() first\nfirst <- 1L\n"]);
    settle(&db, &files);
    for text in [
        "first <- 1L\nfirst <- 1L\nsecond <- function() first\n",
        "first <- 1L\nsecond <- function() first\n",
        "# note\nsecond <- function() first\nfirst <- 1L\nfirst <- 1L\n",
        "second <- function() first\nsecond <- function() first\n",
        "",
        "first <- 1L\n",
    ] {
        db.edit(files[0], text);
        settle(&db, &files);
    }
}

#[test]
fn an_edit_that_keeps_the_dollar_methods_rechecks_no_dollar_reader() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "helper <- function() 1L\n`$.vs` <- function(x, name) name\n",
        "weights <- function() c(1L, 2L)$weight\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    db.edit(
        files[0],
        "helper <- function() 2L\n`$.vs` <- function(x, name) name\n".to_owned(),
    );
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["helper"]);

    db.edit(files[0], "helper <- function() 2L\n".to_owned());
    settle(&db, &files);
    assert!(
        db.checked_since().contains(&"weights".to_owned()),
        "removing the last `$` method rechecks a reader of `$` on an atomic vector"
    );
}

/// The codes of a console command's errors.
fn command_codes(db: &RootDatabase, file: SourceFile) -> Vec<&'static str> {
    file_diagnostics(db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

/// Pushes `text` as the session's next command and returns it with the earlier commands whose
/// checks its bindings may change.
fn run_command(db: &mut RootDatabase, text: &str) -> (SourceFile, Vec<SourceFile>) {
    let file = session::command_file(db, text.to_owned());
    let contributions = session::prepare(db, file);
    (file, session::commit(db, file, contributions))
}

#[test]
fn a_console_command_reads_the_interface_of_the_commands_before_it() {
    let mut db = RootDatabase::default();
    install_shipped_stubs(&db);
    AnalysisSettings::new(&db, true, true, true);
    ProjectFiles::new(&db, Vec::new());
    let (_, readers) = run_command(&mut db, "x: integer <- 1L\n");
    assert!(readers.is_empty());
    let (reader, _) = run_command(&mut db, "f <- function() -> integer x + 1L\n");
    assert_eq!(command_codes(&db, reader), Vec::<&str>::new());
    let pending = session::command_file(&db, "y <- undefined_name\n".to_owned());
    session::prepare(&db, pending);
    assert!(
        file_diagnostics(&db, pending)
            .iter()
            .any(|diagnostic| diagnostic.code == "unresolved")
    );

    // Only a function body sees a later binder, so an immediate read is no reader of `x`.
    let (_, readers) = run_command(&mut db, "z: integer <- x\n");
    assert!(readers.is_empty());
    // A function calling `f` changes type with it, so it is a reader too.
    let (caller, _) = run_command(&mut db, "g <- function() -> integer f()\n");
    let (_, readers) = run_command(&mut db, "x <- \"a\"\n");
    assert_eq!(readers, [reader, caller]);
    assert_eq!(command_codes(&db, reader), ["type-mismatch"]);

    session::truncate(&mut db, 4);
    assert_eq!(session::len(&db), 4);
    assert_eq!(command_codes(&db, reader), Vec::<&str>::new());
}
