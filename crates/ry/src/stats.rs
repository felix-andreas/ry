//! `ry debug analysis-stats`: a workspace performance diagnosis. Runs
//! the full analysis pipeline over a workspace through the same queries the
//! language server uses. It prints where the time goes, as per-phase totals,
//! the slowest files, and an incremental typing probe. It also prints where the
//! memory goes, as per-phase resident-set growth. A slow or memory-hungry
//! workspace can then be diagnosed, and reported, with one command instead of
//! guesswork.

use crate::cli::CommandError;
use crate::config::Config;
use crate::diagnostics::document_diagnostics;

use semantics::check::CHECK_EXECUTIONS;
use semantics::infer::RESOLVE_CALLS;
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SLOWEST_FILES: usize = 10;
const TYPING_BURST: usize = 10;

pub fn analysis_stats(target: Option<&Path>) -> Result<(), CommandError> {
    let target = target.unwrap_or(Path::new("."));
    let mut config = Config::discover(target).map_err(|error| {
        crate::cli::report(&error);
        CommandError
    })?;
    crate::cli::warn_unknown_config_keys(&config);
    let target = std::fs::canonicalize(target).map_err(|error| {
        crate::cli::error(&format!("failed to resolve {}: {error}", target.display()));
        CommandError
    })?;
    let root = crate::cli::analysis_root_for_target(&target);

    // A diagnosis with the type checker off measures nothing interesting;
    // force it on and say so.
    let typing_was_off = !config.check.typing;
    config.check.typing = true;

    let mut stub_sources = semantics::stubs::shipped_stub_sources();
    match crate::cli::discover_project_stubs(&root) {
        Ok(project_stubs) => {
            for stub in project_stubs {
                stub_sources.push((stub.stem, stub.text));
            }
        }
        Err((path, error)) => {
            crate::cli::warn(&format!(
                "failed to read override stub {}: {error}",
                path.display()
            ));
        }
    }
    let namespace_imports = std::fs::read_to_string(root.join("NAMESPACE"))
        .ok()
        .map(|source| semantics::metadata::parse_namespace_imports(&source))
        .unwrap_or_default();
    let description_source = std::fs::read_to_string(root.join("DESCRIPTION")).ok();
    let collate = description_source
        .as_deref()
        .map(semantics::metadata::parse_description_collate)
        .unwrap_or_default();
    let collate_rank: HashMap<&str, usize> = collate
        .iter()
        .enumerate()
        .map(|(rank, name)| (name.as_str(), rank))
        .collect();

    // Discover and order files exactly as the CLI and the server do. Package
    // files come first, in Collate order when it is declared, then by
    // root-relative path. Scripts follow. The `[check] exclude` scope applies
    // identically.
    let exclude = crate::cli::exclude_matcher(&config, &target)?;
    let r_path = root.join("R");
    let mut entries: Vec<(bool, bool, usize, String, PathBuf)> = Vec::new();
    for path in crate::cli::collect_r_files(&target, exclude.as_ref())? {
        // Sorting and classification are different questions, and collapsing
        // them into one flag is what made this instrument measure a different
        // program than `check`: package files sort ahead of scripts by
        // `R/`-membership, while sharing a namespace also covers
        // `tests/testthat/`.
        let sorts_first = path.starts_with(&r_path);
        let is_package = crate::cli::shares_a_namespace(&path, &root);
        let rank = path
            .file_name()
            .and_then(|name| collate_rank.get(name.to_string_lossy().as_ref()))
            .copied()
            .unwrap_or(usize::MAX);
        let key = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push((sorts_first, is_package, rank, key, path));
    }
    entries.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });
    if entries.is_empty() {
        crate::cli::error(&format!("no R files under {}", target.display()));
        return Err(CommandError);
    }

    // Phase 1: load. Reading the sources and building the rope-backed salsa
    // inputs, exactly as the server loads a workspace from disk.
    let rss_baseline = resident_set_bytes();
    let mut db = RootDatabase::default();
    semantics::stubs::StubSources::new(
        &db,
        stub_sources,
        semantics::stubs::shipped_export_manifests(),
    );
    let metadata = semantics::metadata::PackageMetadata::new(
        &db,
        semantics::metadata::normalized_imports(&namespace_imports),
        description_source
            .as_deref()
            .map(semantics::metadata::parse_description_dependencies)
            .unwrap_or_default(),
        std::collections::BTreeSet::new(),
        None,
    );
    let mut records: Vec<FileRecord> = Vec::with_capacity(entries.len());
    let mut package_count = 0usize;
    let mut load_total = Duration::ZERO;
    for (_, is_package, _, _, path) in entries {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                crate::cli::warn(&format!("failed to read {}: {error}", path.display()));
                continue;
            }
        };
        let loc = source.lines().count();
        let kind = if is_package {
            DocumentKind::Package
        } else {
            DocumentKind::Script
        };
        package_count += usize::from(is_package);
        let start = Instant::now();
        let file = SourceFile::new(&db, source.clone(), kind);
        load_total += start.elapsed();
        records.push(FileRecord {
            file,
            path,
            loc,
            source,
            typecheck: Duration::ZERO,
        });
    }
    ProjectFiles::new(&db, records.iter().map(|record| record.file).collect());
    let rss_after_load = resident_set_bytes();

    // The remaining phases are staged fetches. Memoization makes the
    // attribution honest, because each stage reuses everything the stages
    // before it computed, so its wall time is its own marginal cost. Package-wide folds and per-symbol
    // interface schemes have no line of their own: they run on demand inside
    // the first stage that needs them, which is exactly where an editor pays
    // for them.
    let total_loc: usize = records.iter().map(|record| record.loc).sum();
    let mut parse_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = semantics::parse(&db, record.file);
        parse_total += start.elapsed();
    }
    let rss_after_parse = resident_set_bytes();
    // Attach facts ride the parse memos just warmed; they must land before
    // the naming stage so conditional stub namespaces resolve exactly as the
    // real hosts see them.
    let attached =
        semantics::metadata::attached_union(&db, records.iter().map(|record| record.file));
    if !attached.is_empty() {
        use salsa::Setter;
        metadata.set_attached(&mut db).to(attached);
    }
    let mut lower_total = Duration::ZERO;
    let mut item_count = 0usize;
    for record in &records {
        let start = Instant::now();
        for &item in semantics::item_tree(&db, record.file) {
            item_count += 1;
            let _ = semantics::item_hir(&db, item);
            let _ = semantics::item_naming(&db, item);
        }
        lower_total += start.elapsed();
    }
    let rss_after_lower = resident_set_bytes();
    let mut typecheck_total = Duration::ZERO;
    for record in &mut records {
        let start = Instant::now();
        for &item in semantics::item_tree(&db, record.file) {
            let _ = semantics::item_check(&db, item);
        }
        record.typecheck = start.elapsed();
        typecheck_total += record.typecheck;
    }
    let rss_after_typecheck = resident_set_bytes();
    // The diagnostics phase split by marginal cost: the semantic render
    // (unresolved/unused/type findings and their messages), then the lints,
    // then the final assembly document_diagnostics adds on top (config
    // gating, severity escalation, sorting) riding the memos just warmed.
    let mut semantic_render_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = semantics::diagnostics::file_diagnostics(&db, record.file);
        semantic_render_total += start.elapsed();
    }
    let mut lint_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = semantics::lints::lint_file(&db, record.file, &config.lint);
        lint_total += start.elapsed();
    }
    let mut assemble_total = Duration::ZERO;
    let mut diagnostic_count = 0usize;
    for record in &records {
        let start = Instant::now();
        diagnostic_count += document_diagnostics(&db, record.file, &config).len();
        assemble_total += start.elapsed();
    }
    let render_total = semantic_render_total + lint_total + assemble_total;
    let rss_after_render = resident_set_bytes();
    let cold_total = load_total + parse_total + lower_total + typecheck_total + render_total;

    println!("workspace: {}", root.display());
    println!(
        "  {} package files + {} scripts, {} LoC, {} top-level items",
        package_count,
        records.len() - package_count,
        total_loc,
        item_count,
    );
    if typing_was_off {
        println!("  note: [check] typing is off in the configuration; stats force it on");
    }
    println!();
    println!("cold analysis (one full pass, phase marginal cost, resident-set growth):");
    let phase = |name: &str, duration: Duration, memory: Option<(u64, u64)>| {
        let memory = match memory {
            Some((before, after)) => {
                format!("  {:>+9.1} MiB", (after as f64 - before as f64) / MEBIBYTE)
            }
            None => String::new(),
        };
        println!(
            "  {name:<26} {:>10.1} ms  ({:>4.1}%){memory}",
            duration.as_secs_f64() * 1e3,
            duration.as_secs_f64() / cold_total.as_secs_f64().max(f64::EPSILON) * 100.0,
        );
    };
    let delta = |before: Option<u64>, after: Option<u64>| Some((before?, after?));
    phase(
        "load (read + inputs)",
        load_total,
        delta(rss_baseline, rss_after_load),
    );
    phase("parse", parse_total, delta(rss_after_load, rss_after_parse));
    phase(
        "lower + naming",
        lower_total,
        delta(rss_after_parse, rss_after_lower),
    );
    phase(
        "typecheck (+interfaces)",
        typecheck_total,
        delta(rss_after_lower, rss_after_typecheck),
    );
    phase(
        "diagnostics (render +lint)",
        render_total,
        delta(rss_after_typecheck, rss_after_render),
    );
    phase("  semantic render", semantic_render_total, None);
    phase("  lints", lint_total, None);
    phase("  assembly (gate + sort)", assemble_total, None);
    let total_memory = match delta(rss_baseline, rss_after_render) {
        Some((before, after)) => {
            format!("  {:>+9.1} MiB", (after as f64 - before as f64) / MEBIBYTE)
        }
        None => String::new(),
    };
    println!(
        "  {:<26} {:>10.1} ms        {total_memory}   ({} diagnostics)",
        "total",
        cold_total.as_secs_f64() * 1e3,
        diagnostic_count,
    );
    if let Some(peak) = peak_resident_set_bytes() {
        println!(
            "  peak resident set          {:>10.1} MiB",
            peak as f64 / MEBIBYTE
        );
    }

    println!();
    println!("slowest files (typecheck):");
    let mut by_typecheck: Vec<usize> = (0..records.len()).collect();
    by_typecheck.sort_by_key(|&index| std::cmp::Reverse(records[index].typecheck));
    for &index in by_typecheck.iter().take(SLOWEST_FILES) {
        let record = &records[index];
        println!(
            "  {:>10.1} ms  {:>6} LoC  {}",
            record.typecheck.as_secs_f64() * 1e3,
            record.loc,
            record
                .path
                .strip_prefix(&root)
                .unwrap_or(&record.path)
                .display(),
        );
    }

    // The incremental probe: a burst of appended keystrokes on
    // representative files. The slowest file gets the detailed attribution;
    // a median-sized and a small file get one line each, so a workspace
    // whose largest file dominates still shows what ordinary files feel
    // like.
    let mut probe_indexes: Vec<usize> = Vec::new();
    if let Some(&slowest) = by_typecheck.first() {
        probe_indexes.push(slowest);
    }
    let mut by_loc: Vec<usize> = (0..records.len()).collect();
    by_loc.sort_by_key(|&index| records[index].loc);
    for candidate in [by_loc.get(by_loc.len() / 2), by_loc.get(by_loc.len() / 10)]
        .into_iter()
        .flatten()
    {
        if !probe_indexes.contains(candidate) {
            probe_indexes.push(*candidate);
        }
    }
    for (position, &index) in probe_indexes.iter().enumerate() {
        typing_burst(&mut db, &records, index, &config, &root, position == 0);
    }

    Ok(())
}

/// One typing burst on the record at `index`: appended `#` keystrokes (each
/// revision differs by one byte, so item identity holds and the burst
/// measures steady-state revalidation). `detailed` adds the recompute
/// attribution and the workspace revalidate sweep.
fn typing_burst(
    db: &mut RootDatabase,
    records: &[FileRecord],
    index: usize,
    config: &Config,
    root: &Path,
    detailed: bool,
) {
    use salsa::Setter;
    let record = &records[index];
    let mut text = record.source.clone();
    text.push('\n');
    let checks_before = CHECK_EXECUTIONS.load(Ordering::Relaxed);
    let resolves_before = RESOLVE_CALLS.load(Ordering::Relaxed);
    let mut keystroke_times = Vec::with_capacity(TYPING_BURST);
    for _ in 0..TYPING_BURST {
        text.push('#');
        let start = Instant::now();
        record.file.set_text(db).to(text.clone());
        let _ = document_diagnostics(db, record.file, config);
        keystroke_times.push(start.elapsed());
    }
    let checks_after_burst = CHECK_EXECUTIONS.load(Ordering::Relaxed);
    let resolves_after_burst = RESOLVE_CALLS.load(Ordering::Relaxed);
    let sweep_start = Instant::now();
    for other in records {
        let _ = document_diagnostics(db, other.file, config);
    }
    let sweep = sweep_start.elapsed();
    let checks_after_sweep = CHECK_EXECUTIONS.load(Ordering::Relaxed);

    keystroke_times.sort();
    let median = keystroke_times[keystroke_times.len() / 2];
    let worst = *keystroke_times.last().expect("burst is non-empty");
    let display_path = record.path.strip_prefix(root).unwrap_or(&record.path);
    if !detailed {
        println!(
            "  typing burst on {} ({} LoC): {:.1} ms median, {:.1} ms max",
            display_path.display(),
            record.loc,
            median.as_secs_f64() * 1e3,
            worst.as_secs_f64() * 1e3,
        );
    } else {
        println!();
        println!(
            "incremental (typing burst: {TYPING_BURST} appended keystrokes on {}):",
            display_path.display(),
        );
        println!(
            "  edited-file diagnostics   {:>10.1} ms median, {:.1} ms max",
            median.as_secs_f64() * 1e3,
            worst.as_secs_f64() * 1e3,
        );
        println!(
            "  workspace revalidate      {:>10.1} ms  (after the last keystroke)",
            sweep.as_secs_f64() * 1e3
        );
        println!(
            "  item rechecks             {:>10.1} per keystroke, {} in the revalidate sweep",
            (checks_after_burst - checks_before) as f64 / TYPING_BURST as f64,
            checks_after_sweep - checks_after_burst,
        );
        println!(
            "  resolve steps             {:>10.1} per keystroke",
            (resolves_after_burst - resolves_before) as f64 / TYPING_BURST as f64,
        );
        // The raw from-scratch parse of the edited text, to attribute the
        // keystroke latency: everything above this line is item-tree rebuild
        // plus revalidation.
        let parse_start = Instant::now();
        let parses = 5;
        for _ in 0..parses {
            let _ = syntax::parse(&text);
        }
        println!(
            "  raw parse of the file     {:>10.1} ms  (the latency floor)",
            parse_start.elapsed().as_secs_f64() * 1e3 / parses as f64
        );
    }
    // Restore the original text so the next probe measures in isolation.
    record.file.set_text(db).to(record.source.clone());
    let _ = document_diagnostics(db, record.file, config);
}

struct FileRecord {
    file: SourceFile,
    path: PathBuf,
    loc: usize,
    source: String,
    typecheck: Duration,
}

const MEBIBYTE: f64 = 1024.0 * 1024.0;

// The current resident set, so each phase's growth attributes the retained
// memory (memoized values dominate; the resident set also keeps
// allocator-held freed pages, so a phase with heavy transient allocation
// reads slightly high). It is `None` where the kernel does not expose it, and
// the memory column is then simply omitted.
fn resident_set_bytes() -> Option<u64> {
    proc_status_field("VmRSS:")
}

fn peak_resident_set_bytes() -> Option<u64> {
    proc_status_field("VmHWM:")
}

fn proc_status_field(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with(field))?;
    let kibibytes: u64 = line
        .strip_prefix(field)?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kibibytes * 1024)
}
