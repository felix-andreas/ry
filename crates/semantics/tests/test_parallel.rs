//! Parallel-read stress tests: salsa storage-handle clones let several
//! threads force the same queries concurrently. The dangerous shape is the
//! package-interface fixpoint, where concurrent threads enter the same
//! `global_scheme` cycle from different heads. That shape historically hung
//! parallel salsa fixpoints upstream. These tests gate any multi-core use of
//! the database: no deadlock (bounded wall time), no panic, and answers
//! byte-equal to the single-threaded ones.

use semantics::diagnostics::{file_diagnostics, strict_diagnostics};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

/// A project whose files form mutually recursive cross-file definition
/// cycles (each file's functions call the neighbours'), so every thread's
/// query enters the interface fixpoint.
fn cyclic_project(db: &RootDatabase, files: usize, functions_per_file: usize) -> Vec<SourceFile> {
    let mut sources = Vec::new();
    for file_index in 0..files {
        let mut source = String::new();
        for function_index in 0..functions_per_file {
            let next_file = (file_index + 1) % files;
            source.push_str(&format!(
                "f_{file_index}_{function_index} <- function(x) f_{next_file}_{function_index}(x) + g_{file_index}(x)\n"
            ));
        }
        // A self-referential growing binding rides the fixpoint round cap.
        source.push_str(&format!(
            "g_{file_index} <- function(x) if (x > 0) g_{}(x - 1) else x\n",
            (file_index + files - 1) % files
        ));
        sources.push(source);
    }
    let handles: Vec<SourceFile> = sources
        .into_iter()
        .map(|source| SourceFile::new(db, source, DocumentKind::Package))
        .collect();
    ProjectFiles::new(db, handles.clone());
    handles
}

fn all_diagnostics(db: &RootDatabase, files: &[SourceFile]) -> Vec<String> {
    let mut rendered = Vec::new();
    for &file in files {
        for diagnostic in file_diagnostics(db, file) {
            rendered.push(format!(
                "{}..{} {}",
                u32::from(diagnostic.range.start()),
                u32::from(diagnostic.range.end()),
                diagnostic.message
            ));
        }
        for diagnostic in strict_diagnostics(db, file) {
            rendered.push(format!(
                "strict {}..{} {}",
                u32::from(diagnostic.range.start()),
                u32::from(diagnostic.range.end()),
                diagnostic.message
            ));
        }
    }
    rendered
}

/// Eight threads race the same cyclic interface fixpoint; every thread's
/// answer must equal the single-threaded baseline, within a hard wall-time
/// bound (a hang here is a real parallel-fixpoint bug, never load).
#[test]
fn concurrent_cycle_queries_agree_with_the_baseline() {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files = cyclic_project(&db, 6, 4);

    let baseline_db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&baseline_db);
    let baseline_files = cyclic_project(&baseline_db, 6, 4);
    let baseline = all_diagnostics(&baseline_db, &baseline_files);

    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for worker in 0..8 {
            let db = db.clone();
            let files = files.clone();
            workers.push(scope.spawn(move || {
                // Stagger entry points so threads enter the cycle from
                // different heads.
                let mut ordered = files.clone();
                ordered.rotate_left(worker % files.len());
                all_diagnostics(&db, &ordered)
            }));
        }
        for (worker, handle) in workers.into_iter().enumerate() {
            let rendered = handle.join().expect("worker thread must not panic");
            let mut ordered = files.clone();
            ordered.rotate_left(worker % files.len());
            let expected = {
                let mut expected = Vec::new();
                let by_file: std::collections::HashMap<SourceFile, Vec<String>> = files
                    .iter()
                    .map(|&file| (file, all_diagnostics(&db, std::slice::from_ref(&file))))
                    .collect();
                for file in &ordered {
                    expected.extend(by_file[file].iter().cloned());
                }
                expected
            };
            assert_eq!(rendered, expected, "worker {worker} diverged");
        }
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "parallel fixpoint took implausibly long, so treat it as a hang"
    );

    let mut single = all_diagnostics(&db, &files);
    let mut baseline_sorted = baseline;
    single.sort();
    baseline_sorted.sort();
    assert_eq!(
        single, baseline_sorted,
        "parallel execution corrupted the memoized answers"
    );
}

/// Concurrent cold priming of disjoint files must scale without corrupting
/// state: the parallel answers equal a fresh sequential database's.
#[test]
fn parallel_cold_prime_matches_sequential() {
    let mut sources = Vec::new();
    for index in 0..24 {
        sources.push(format!(
            "value_{index} <- {index}L\nuse_{index} <- function() value_{index} + shared_helper()\n"
        ));
    }
    sources.push("shared_helper <- function() 1L\n".to_owned());

    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(&db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&db, files.clone());

    std::thread::scope(|scope| {
        for chunk in files.chunks(4) {
            let db = db.clone();
            let chunk = chunk.to_vec();
            scope.spawn(move || {
                for &file in &chunk {
                    let _ = file_diagnostics(&db, file);
                }
            });
        }
    });

    let sequential_db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&sequential_db);
    let sequential_files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(&sequential_db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&sequential_db, sequential_files.clone());

    for (&file, &sequential_file) in files.iter().zip(&sequential_files) {
        assert_eq!(
            all_diagnostics(&db, std::slice::from_ref(&file)),
            all_diagnostics(&sequential_db, std::slice::from_ref(&sequential_file)),
        );
    }
}

/// Cyclic-group answers must not depend on which member is queried first:
/// forward forcing, reverse forcing, and per-item phase pre-forcing must all
/// render identical diagnostics. (The canonical per-group fixpoint guarantees
/// this. Before it, the salsa cycle head decided where the round cap pinned,
/// and the head is whichever query arrives first.)
#[test]
fn cyclic_group_answers_are_forcing_order_independent() {
    let cases: Vec<Vec<String>> = vec![
        // A growing pair: each binding embeds the other, so the group rides
        // the round cap and every member pins.
        vec![
            "a <- list(v = 1, w = b)\nuse_a <- function() a$v + 1\n".to_owned(),
            "b <- list(v = 2, w = a)\nuse_b <- function() b$v + 1\n".to_owned(),
        ],
        // Mutually recursive functions plus a growing accumulator.
        vec![
            "f <- function(x) g(x)\nacc <- c(acc, f)\n".to_owned(),
            "g <- function(x) f(x) + h(x)\nh <- function(x) if (x > 0) g(x - 1) else acc\n"
                .to_owned(),
        ],
        // An oscillating value pair with downstream readers.
        vec![
            "p <- if (TRUE) q else 1L\n".to_owned(),
            "q <- if (TRUE) p else \"s\"\nr <- q\ns <- p\n".to_owned(),
        ],
    ];
    for sources in cases {
        let build = |forcing: &dyn Fn(&RootDatabase, &[SourceFile])| -> Vec<String> {
            let db = RootDatabase::default();
            semantics::stubs::install_shipped_stubs(&db);
            let files: Vec<SourceFile> = sources
                .iter()
                .map(|source| SourceFile::new(&db, source.clone(), DocumentKind::Package))
                .collect();
            ProjectFiles::new(&db, files.clone());
            forcing(&db, &files);
            all_diagnostics(&db, &files)
        };
        let forward = build(&|_, _| {});
        let reverse = build(&|db, files| {
            for &file in files.iter().rev() {
                let _ = file_diagnostics(db, file);
                let _ = strict_diagnostics(db, file);
            }
        });
        let phased = build(&|db, files| {
            for &file in files {
                for &item in semantics::item_tree(db, file) {
                    let _ = semantics::item_check(db, item);
                }
            }
        });
        assert_eq!(forward, reverse, "reverse forcing changed the answers");
        assert_eq!(forward, phased, "phase pre-forcing changed the answers");
    }
}
