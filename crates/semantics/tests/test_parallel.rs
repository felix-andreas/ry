//! Threads racing the package-interface fixpoint from different heads must not deadlock or
//! panic, and must answer byte-equal to a single thread.

use semantics::diagnostics::{file_diagnostics, strict_diagnostics};
use semantics::stubs::install_shipped_stubs;
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile, item_check, item_tree};
use std::time::{Duration, Instant};
use std::{slice, thread};

fn project(sources: &[String]) -> (RootDatabase, Vec<SourceFile>) {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let files: Vec<SourceFile> = sources
        .iter()
        .map(|source| SourceFile::new(&db, source.clone(), DocumentKind::Package))
        .collect();
    ProjectFiles::new(&db, files.clone());
    (db, files)
}

fn cyclic_sources(files: usize, functions_per_file: usize) -> Vec<String> {
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
    sources
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

/// The wall-time bound is generous: a hang here is a parallel-fixpoint bug, never load.
#[test]
fn concurrent_cycle_queries_agree_with_the_baseline() {
    let sources = cyclic_sources(6, 4);
    let (db, files) = project(&sources);
    let (baseline_db, baseline_files) = project(&sources);
    let baseline = all_diagnostics(&baseline_db, &baseline_files);

    let started = Instant::now();
    thread::scope(|scope| {
        let mut workers = Vec::new();
        for worker in 0..8 {
            let db = db.clone();
            let files = files.clone();
            workers.push(scope.spawn(move || {
                // Stagger entry points so threads enter the cycle from different heads.
                let mut ordered = files.clone();
                ordered.rotate_left(worker % files.len());
                all_diagnostics(&db, &ordered)
            }));
        }
        for (worker, handle) in workers.into_iter().enumerate() {
            let rendered = handle.join().expect("worker thread must not panic");
            let mut ordered = files.clone();
            ordered.rotate_left(worker % files.len());
            assert_eq!(
                rendered,
                all_diagnostics(&db, &ordered),
                "worker {worker} diverged"
            );
        }
    });
    assert!(
        started.elapsed() < Duration::from_secs(60),
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

#[test]
fn parallel_cold_prime_matches_sequential() {
    let mut sources = Vec::new();
    for index in 0..24 {
        sources.push(format!(
            "value_{index} <- {index}L\nuse_{index} <- function() value_{index} + shared_helper()\n"
        ));
    }
    sources.push("shared_helper <- function() 1L\n".to_owned());

    let (db, files) = project(&sources);
    thread::scope(|scope| {
        for chunk in files.chunks(4) {
            let db = db.clone();
            let chunk = chunk.to_vec();
            scope.spawn(move || {
                for &file in &chunk {
                    file_diagnostics(&db, file);
                }
            });
        }
    });

    let (sequential_db, sequential_files) = project(&sources);

    for (&file, &sequential_file) in files.iter().zip(&sequential_files) {
        assert_eq!(
            all_diagnostics(&db, slice::from_ref(&file)),
            all_diagnostics(&sequential_db, slice::from_ref(&sequential_file)),
        );
    }
}

#[test]
fn cyclic_group_answers_are_forcing_order_independent() {
    let cases: Vec<Vec<String>> = vec![
        // Each binding embeds the other, so the group hits the round cap and every member pins.
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
            let (db, files) = project(&sources);
            forcing(&db, &files);
            all_diagnostics(&db, &files)
        };
        let forward = build(&|_, _| {});
        let reverse = build(&|db, files| {
            for &file in files.iter().rev() {
                file_diagnostics(db, file);
                strict_diagnostics(db, file);
            }
        });
        let phased = build(&|db, files| {
            for &file in files {
                for &item in item_tree(db, file) {
                    item_check(db, item);
                }
            }
        });
        assert_eq!(forward, reverse, "reverse forcing changed the answers");
        assert_eq!(forward, phased, "phase pre-forcing changed the answers");
    }
}
