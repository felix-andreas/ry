//! Reload property and index robustness under the contracts in `loader::testing`. Packages and
//! edits are random; `FUZZ_ITERS` scales the budget.

use {
    loader::{
        Loader, NamespaceConditions,
        testing::{ReloadLoaders, check_load_index_input, check_random_reload},
    },
    syntax::testing::{SplitMix64, for_each_seed, fuzz_iterations, mutate_seed, random_bytes},
};

#[test]
fn a_reload_matches_a_fresh_load() {
    let mut loaders = ReloadLoaders::default();
    for_each_seed(400, |seed| {
        check_random_reload(&mut loaders, &mut SplitMix64(seed));
    });
}

#[test]
fn arbitrary_text_never_breaks_the_index() {
    let seeds = [
        "f <- function(x: int) -> int { x }\n",
        "setMethod(\"show\", \"A\", function(object) cat(\"é\"))\n",
        "x <- 1; y <- function() x\nif (TRUE) z <- 2\n",
    ];
    for_each_seed(300, |seed| {
        let mut rng = SplitMix64(seed);
        let text = match rng.below(3) {
            0 => random_bytes(&mut rng),
            _ => mutate_seed(&mut rng, &seeds, 24),
        };
        check_load_index_input(&text);
    });
}

/// What the console asks before each command: a disk edit to a loaded package is reported once,
/// so is an edit to a package whose first load failed or was refused, and a plan read after an
/// edit absorbs it.
#[test]
fn changed_roots_reports_each_disk_edit_once() {
    let directory = tempfile::tempdir().expect("temp dir");
    let root = directory.path();
    std::fs::create_dir_all(root.join("R")).expect("create R/");
    std::fs::write(root.join("DESCRIPTION"), "Package: demo\nVersion: 1.0\n").expect("write");
    std::fs::write(root.join("R/a.R"), "f <- function() 1\n").expect("write");
    std::fs::write(root.join("R/b.R"), "g <- function( 1\n").expect("write");
    let mut loader = Loader::default();
    assert!(
        loader
            .plan(root, false, &NamespaceConditions::default())
            .is_err(),
        "a syntax error fails the plan"
    );
    assert!(
        loader.changed_roots().is_empty(),
        "nothing changed since the attempt"
    );
    std::fs::write(root.join("R/b.R"), "g <- function() 1\n").expect("write");
    assert_eq!(
        loader.changed_roots(),
        vec![root.to_path_buf()],
        "a failed first load is retried after an edit"
    );
    assert!(loader.changed_roots().is_empty(), "reported once");
    // A plan that is never committed, as when strict mode refuses it, is watched the same way.
    loader
        .plan(root, false, &NamespaceConditions::default())
        .expect("plans");
    std::fs::write(root.join("R/a.R"), "f <- function() 22\n").expect("write");
    assert_eq!(loader.changed_roots(), vec![root.to_path_buf()]);
    loader
        .plan(root, false, &NamespaceConditions::default())
        .expect("plans");
    loader.commit(root);
    assert!(
        loader.changed_roots().is_empty(),
        "nothing changed since the load"
    );
    // Each edit changes the size, so it shows even within one timestamp tick.
    let mut rng = SplitMix64(7);
    let mut text = String::from("f <- function() 22\n");
    for step in 0..fuzz_iterations(20) {
        let edited = rng.chance(1, 2);
        if edited {
            text.push_str("# edit\n");
            let file = if rng.chance(1, 3) { "R/b.R" } else { "R/a.R" };
            std::fs::write(root.join(file), &text).expect("write");
        }
        let changed = loader.changed_roots();
        assert_eq!(changed.len(), usize::from(edited), "step {step}");
        assert!(
            loader.changed_roots().is_empty(),
            "step {step}: reported twice"
        );
    }
}
