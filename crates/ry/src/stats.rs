//! `ry debug analysis-stats`: where time and memory go across a workspace,
//! measured through the same queries the language server uses.

use crate::cli::{self, CommandError};
use crate::config::Config;
use crate::diagnostics::document_diagnostics;
use semantics::check::CHECK_EXECUTIONS;
use semantics::diagnostics::file_diagnostics;
use semantics::infer::RESOLVE_CALLS;
use semantics::lints::lint_file;
use semantics::metadata::{
    PackageMetadata, attached_union, normalized_imports, parse_description_collate,
    parse_description_dependencies, parse_namespace_imports,
};
use semantics::stubs::{StubSources, shipped_export_manifests, shipped_stub_sources};
use semantics::{AnalysisSettings, DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SLOWEST_FILES: usize = 10;
const TYPING_BURST: usize = 10;

pub fn analysis_stats(target: Option<&Path>) -> Result<(), CommandError> {
    let target = target.unwrap_or(Path::new("."));
    let mut config = Config::discover(target).map_err(|error| {
        cli::report(&error);
        CommandError
    })?;
    cli::warn_config(&config);
    let target = fs::canonicalize(target).map_err(|error| {
        cli::error(&format!("failed to resolve {}: {error}", target.display()));
        CommandError
    })?;
    let root = cli::analysis_root_for_target(&target);

    let typing_was_off = !config.check.typing;
    config.check.typing = true;

    let mut stub_sources = shipped_stub_sources();
    match cli::read_project_stubs(&root) {
        Ok(project_stubs) => {
            stub_sources.extend(project_stubs.into_iter().map(|stub| (stub.stem, stub.text)));
        }
        Err((path, error)) => {
            cli::warn(&format!(
                "failed to read override stub {}: {error}",
                path.display()
            ));
        }
    }
    let namespace_imports = fs::read_to_string(root.join("NAMESPACE"))
        .ok()
        .map(|source| parse_namespace_imports(&source))
        .unwrap_or_default();
    let description_source = fs::read_to_string(root.join("DESCRIPTION")).ok();
    let collate = description_source
        .as_deref()
        .map(parse_description_collate)
        .unwrap_or_default();

    // File order and the `[check] exclude` scope must match the CLI and server.
    let exclude = cli::exclude_matcher(&config, &target)?;
    let mut paths = cli::collect_r_files(&target, exclude.as_ref())?;
    let order = cli::project_order(&root, &collate);
    paths.sort_by_cached_key(|path| order(path));
    if paths.is_empty() {
        cli::error(&format!("no R files under {}", target.display()));
        return Err(CommandError);
    }

    let rss_baseline = proc_status_field("VmRSS:");
    let mut db = RootDatabase::default();
    StubSources::new(&db, stub_sources, shipped_export_manifests());
    AnalysisSettings::new(&db, config.check.inline_annotations);
    let metadata = PackageMetadata::new(
        &db,
        normalized_imports(&namespace_imports),
        description_source
            .as_deref()
            .map(parse_description_dependencies)
            .unwrap_or_default(),
        BTreeSet::new(),
        None,
    );
    let mut records: Vec<FileRecord> = Vec::with_capacity(paths.len());
    let mut package_count = 0usize;
    let mut load_total = Duration::ZERO;
    for path in paths {
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                cli::warn(&format!("failed to read {}: {error}", path.display()));
                continue;
            }
        };
        let loc = source.lines().count();
        let is_package = cli::shares_a_namespace(&path, &root);
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
    let rss_after_load = proc_status_field("VmRSS:");

    // Memoization makes each stage's wall time its own marginal cost; package-wide
    // folds run inside the first stage that needs them, as in an editor.
    let total_loc: usize = records.iter().map(|record| record.loc).sum();
    let mut parse_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = semantics::parse(&db, record.file);
        parse_total += start.elapsed();
    }
    let rss_after_parse = proc_status_field("VmRSS:");
    // Must land before naming so conditional stub namespaces resolve as the real
    // hosts see them.
    let attached = attached_union(&db, records.iter().map(|record| record.file));
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
    let rss_after_lower = proc_status_field("VmRSS:");
    let mut typecheck_total = Duration::ZERO;
    for record in &mut records {
        let start = Instant::now();
        for &item in semantics::item_tree(&db, record.file) {
            let _ = semantics::item_check(&db, item);
        }
        record.typecheck = start.elapsed();
        typecheck_total += record.typecheck;
    }
    let rss_after_typecheck = proc_status_field("VmRSS:");
    let mut semantic_render_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = file_diagnostics(&db, record.file);
        semantic_render_total += start.elapsed();
    }
    let mut lint_total = Duration::ZERO;
    for record in &records {
        let start = Instant::now();
        let _ = lint_file(&db, record.file, &config.lint);
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
    let rss_after_render = proc_status_field("VmRSS:");
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
    let growth = |before: Option<u64>, after: Option<u64>| match (before, after) {
        (Some(before), Some(after)) => {
            format!("  {:>+9.1} MiB", (after as f64 - before as f64) / MEBIBYTE)
        }
        _ => String::new(),
    };
    let phase = |name: &str, duration: Duration, memory: String| {
        println!(
            "  {name:<26} {:>10.1} ms  ({:>4.1}%){memory}",
            duration.as_secs_f64() * 1e3,
            duration.as_secs_f64() / cold_total.as_secs_f64().max(f64::EPSILON) * 100.0,
        );
    };
    phase(
        "load (read + inputs)",
        load_total,
        growth(rss_baseline, rss_after_load),
    );
    phase(
        "parse",
        parse_total,
        growth(rss_after_load, rss_after_parse),
    );
    phase(
        "lower + naming",
        lower_total,
        growth(rss_after_parse, rss_after_lower),
    );
    phase(
        "typecheck (+interfaces)",
        typecheck_total,
        growth(rss_after_lower, rss_after_typecheck),
    );
    phase(
        "diagnostics (render +lint)",
        render_total,
        growth(rss_after_typecheck, rss_after_render),
    );
    phase("  semantic render", semantic_render_total, String::new());
    phase("  lints", lint_total, String::new());
    phase("  assembly (gate + sort)", assemble_total, String::new());
    let total_memory = growth(rss_baseline, rss_after_render);
    println!(
        "  {:<26} {:>10.1} ms        {total_memory}   ({} diagnostics)",
        "total",
        cold_total.as_secs_f64() * 1e3,
        diagnostic_count,
    );
    if let Some(peak) = proc_status_field("VmHWM:") {
        println!(
            "  peak resident set          {:>10.1} MiB",
            peak as f64 / MEBIBYTE
        );
    }

    print_memo_census(&db);

    println!();
    println!("slowest files (typecheck):");
    let mut by_typecheck: Vec<usize> = (0..records.len()).collect();
    by_typecheck.sort_by_key(|&index| Reverse(records[index].typecheck));
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

    // Also probe a median and a small file: the largest file can dominate.
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

/// Salsa's own census: how many memos each query holds and their inline size plus
/// bookkeeping (dependency edges are the bulk of the metadata). Heap behind a value is
/// not counted, so read it beside the phase growth above.
fn print_memo_census(db: &RootDatabase) {
    let info = <dyn salsa::Database>::memory_usage(db);
    let mut queries: Vec<_> = info.queries.iter().collect();
    queries.sort_by_key(|(_, query)| Reverse(query.size_of_metadata() + query.size_of_fields()));
    println!();
    println!("memo census (count, inline value size, salsa metadata):");
    for (name, query) in queries.iter().take(MEMO_CENSUS_ROWS) {
        println!(
            "  {name:<40} {:>9} {:>9.1} MiB {:>9.1} MiB",
            query.count(),
            query.size_of_fields() as f64 / MEBIBYTE,
            query.size_of_metadata() as f64 / MEBIBYTE,
        );
    }
    let mut structs: Vec<_> = info.structs.iter().filter(|entry| entry.count() > 0).collect();
    structs.sort_by_key(|entry| Reverse(entry.size_of_metadata() + entry.size_of_fields()));
    for entry in structs.iter().take(MEMO_CENSUS_ROWS) {
        println!(
            "  {:<40} {:>9} {:>9.1} MiB {:>9.1} MiB",
            entry.debug_name(),
            entry.count(),
            entry.size_of_fields() as f64 / MEBIBYTE,
            entry.size_of_metadata() as f64 / MEBIBYTE,
        );
    }
}

const MEMO_CENSUS_ROWS: usize = 16;

/// Appended `#` keystrokes keep item identity, so the burst measures
/// steady-state revalidation.
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
        // Everything above this is item-tree rebuild plus revalidation.
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

/// In bytes. The resident set includes allocator-held freed pages, so a phase with
/// heavy transient allocation reads slightly high.
fn proc_status_field(field: &str) -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
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
