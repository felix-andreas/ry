use {
    salsa::{Database, Durability, Setter},
    semantics::{
        AnalysisSettings, DataFiles, DocumentKind, Item, ProjectFiles, SourceFile,
        diagnostics::{derive_from_syntax, file_diagnostics, top_level_diagnostics},
        global_scheme, item_naming, item_tree, limit_resident_trees,
        lints::{LintConfig, lint_file},
        load_index::{PackageFile, PackageSources, package_index},
        metadata::{PackageMetadata, attached_union},
        package_definitions,
        stubs::install_shipped_stubs,
        testing::ProbeDatabase,
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
    assert_eq!(
        db.validations_since(&["item_interface_reads", "item_top_level_names"]),
        [1, 0],
        "the project-wide name maps fold per-file memos, so an edit verifies only the edited \
         file's one item and none of the five in the untouched files, and a function item \
         writes only its own name, so the conditional writers never name it"
    );
}

/// More files than the tree caches hold, so a cold pass really evicts trees. Each file has
/// every kind of syntax a later pass reads: a lint, a `#:` comment, and a statement
/// annotated both ways.
/// Two files far apart both bind `shared`, so reporting the duplicate reads the other file's
/// facts after its tree is evicted, and the trailing script reports an unused binding.
fn evicting_project(db: &ProbeDatabase) -> Vec<SourceFile> {
    let mut files: Vec<SourceFile> = (0..200)
        .map(|index| {
            let mut source = evicting_project_source(index);
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

/// The order `ry check` runs in: derive from each tree and evict, name every item, then
/// report.
fn cold_check(db: &mut ProbeDatabase, files: &[SourceFile], lints: &LintConfig) {
    for &file in files {
        derive_from_syntax(db, file, lints);
        db.trigger_lru_eviction();
    }
    // `ry check` writes the attached namespaces here, so later passes verify memos over
    // evicted trees instead of reusing them within one revision.
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
        db.trigger_lru_eviction();
    }
}

#[test]
fn a_cold_check_that_evicts_trees_parses_each_file_once() {
    let mut db = ProbeDatabase::default();
    limit_resident_trees(&mut db);
    let files = evicting_project(&db);
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
fn an_edit_after_eviction_reparses_and_relints_only_the_edited_file() {
    let mut db = ProbeDatabase::default();
    limit_resident_trees(&mut db);
    let files = evicting_project(&db);
    let lints = LintConfig::default();
    cold_check(&mut db, &files, &lints);
    db.checked_since();

    let edited = evicting_project_source(0).replace("x + 0L", "x + 9L");
    db.edit(files[0], edited);
    report(&mut db, &files, &lints);
    assert_eq!(
        db.executions_since(&["parse", "file_syntax_lints", "item_check"]),
        [1, 1, 1]
    );
}

fn evicting_project_source(index: usize) -> String {
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
    "item_load",
    "file_items",
    "item_immediate_reads",
    "package_index",
];

#[test]
fn a_body_edit_rederives_one_item_of_the_load_index() {
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
        [1, 1, 0, 1],
        "only the edited item's load facts and its own file's layer run again"
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
    assert_eq!(db.executions_since(LOAD_QUERIES), [0, 0, 0, 1]);
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
        [0, 1, 0, 1],
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
