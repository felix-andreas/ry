use {
    salsa::{Database, Durability, Setter},
    semantics::{
        AnalysisSettings, DataFiles, DocumentKind, Item, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::{derive_from_syntax, file_diagnostics, top_level_diagnostics},
        global_scheme,
        hir::Symbol,
        interface_group, item_naming, item_tree,
        lints::{LintConfig, lint_file},
        load_index::{PackageFile, PackageSources, package_index},
        metadata::{PackageMetadata, attached_union},
        package_definitions,
        stubs::install_shipped_stubs,
        testing::{ProbeDatabase, render_semantics},
        types::{Atomic, scalar, unknown},
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
        db.executions_since(&["item_top_level", "item_declared_pure", "item_check"]),
        [1, 0, 1],
        "the edited body is reclassified, its unchanged `#: @pure` block is not reread, and \
         the caller's classification stays memoized"
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

/// A name being typed is a new top-level definition on every keystroke. The project-wide
/// definition map changes each time, so whatever reads it directly re-runs for every item
/// checked afterwards; the interface edges and the duplicate check read per-name projections
/// instead, and the edited file is the only one whose edges re-run.
#[test]
fn a_new_definition_rebuilds_no_other_files_interface_edges() {
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
            "item_interface_edges",
            "duplicate_binding_map",
            "package_definition_index",
        ]),
        [1, 1, 1],
        "only the new item's edges are derived; the fold runs once and its duplicates backdate"
    );
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

#[test]
fn a_reexport_cycle_pins_to_unknown() {
    let db = ProbeDatabase::default();
    db.project(&["a <- b\n", "b <- a\n"]);
    for name in ["a", "b"] {
        assert_eq!(global_scheme(&db, definition(&db, name)).body, unknown(&db));
    }
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
    let settings = AnalysisSettings::new(&db, true);
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
fn editing_an_inline_annotation_rechecks_the_item_and_its_readers_only() {
    let mut db = ProbeDatabase::default();
    let files = db.project(&[
        "count: integer <- 1L\n",
        "use_count <- function() count\n",
        "other <- function() 3L\n",
    ]);
    settle(&db, &files);
    db.checked_since();

    db.edit(files[0], "count: double <- 1L\n".to_owned());
    settle(&db, &files);
    assert_eq!(db.checked_since(), ["count", "use_count"]);
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

    db.edit(files[0], "x <- function() 3L\n".to_owned());
    settle(&db, &files);
    assert_eq!(
        db.validations_since(&["parse", "item_syntax"]),
        [0, 0],
        "the untouched files' trees sit below memos that verify by durability alone"
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
    // Each checked item's interface group verifies the edges it reaches, so validations of
    // `item_interface_reads` follow the items a check demands. What must not happen is any
    // untouched item recomputing its reads.
    assert_eq!(
        db.executions_since(&["item_interface_reads", "item_top_level_names"]),
        [0, 1],
        "only the edited item re-derives its top-level names, for its own file's frame binders; \
         its naming backdates, so nothing recomputes interface reads, and none of the five items \
         in the untouched files reruns either"
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

/// The order `ry check` runs in: derive from each tree, name every item, then report.
fn cold_check(db: &mut ProbeDatabase, files: &[SourceFile], lints: &LintConfig) {
    for &file in files {
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

#[test]
fn a_cold_check_parses_each_file_once() {
    let mut db = ProbeDatabase::default();
    let files = wide_project(&db);
    let lints = LintConfig::default();
    cold_check(&mut db, &files, &lints);
    assert_eq!(db.executions_since(&["parse"]), [files.len()]);
    let codes = |file: SourceFile| -> Vec<&'static str> {
        file_diagnostics(&db, file)
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
        lint_file(&db, files[0], &lints)
            .iter()
            .any(|diagnostic| diagnostic.code == "object-system"),
        "the bare `setClass` call is still reported"
    );
}

#[test]
fn an_edit_after_a_cold_check_reparses_and_relints_only_the_edited_file() {
    let mut db = ProbeDatabase::default();
    let files = wide_project(&db);
    let lints = LintConfig::default();
    cold_check(&mut db, &files, &lints);
    db.checked_since();

    let edited = wide_project_source(0).replace("x + 0L", "x + 9L");
    db.edit(files[0], edited);
    report(&mut db, &files, &lints);
    assert_eq!(
        db.executions_since(&["parse", "file_syntax_lints", "item_check"]),
        [1, 1, 1]
    );
}

fn wide_project_source(index: usize) -> String {
    format!(
        "f{index} <- function(x) x + {index}L\n#: integer\nn{index} <- T\n#: integer\nm{index}: integer <- 1L\nsetClass(\"A{index}\")\n"
    )
}

#[test]
fn a_file_that_defines_set_class_itself_is_not_linted_for_calling_it() {
    let db = ProbeDatabase::default();
    let files = db.project(&["setClass <- function(name) name\nsetClass(\"A\")\n"]);
    assert!(
        lint_file(&db, files[0], &LintConfig::default())
            .iter()
            .all(|diagnostic| diagnostic.code != "object-system")
    );
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
    ]);
    let attached = attached_union(&db, files.iter().copied());
    PackageMetadata::new(
        &db,
        Vec::new(),
        Default::default(),
        attached,
        None,
        Default::default(),
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
    assert_eq!(db.checked_since(), ["prices"]);
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
    for reader in ["rev", "sort", "as.Array"] {
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
        interface_group(&db, project, items[1]).is_some(),
        "the loop reads the slot it writes, so the group fixpoint resolves it, not salsa"
    );
    assert!(interface_group(&db, project, items[0]).is_none());
    round_trip_edit(
        DocumentKind::Package,
        &["if (TRUE) b <- 3L\nwhile (b > 0L) b <- b - 1L\n"],
        0,
        "while (b > 0L) b <- b - 1L\n",
    );
}
