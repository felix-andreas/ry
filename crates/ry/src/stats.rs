//! `ry debug analysis-stats`: where time and memory go across a workspace,
//! measured through the same queries the language server uses.

use {
    crate::{
        cli::{self, CommandError},
        config::Config,
        diagnostics::document_diagnostics,
        installed::InstalledPackages,
        scope_oracle::{Binding, ScopeOracle, Site},
    },
    ide::{DefinitionTarget, HoverDefinition},
    rustc_hash::{FxHashMap, FxHashSet},
    salsa::{Durability, Setter},
    semantics::{
        AnalysisSettings, DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        check::CHECK_EXECUTIONS,
        diagnostics::file_diagnostics,
        infer::resolve_calls,
        lints::lint_file,
        metadata::{
            PackageMetadata, attached_union, normalized_imports, parse_description_collate,
            parse_description_dependencies, parse_namespace,
        },
        stubs::{StubSources, shipped_export_manifests, shipped_stub_sources},
    },
    std::{
        cmp::Reverse,
        collections::{BTreeMap, BTreeSet},
        fs,
        path::{Path, PathBuf},
        sync::atomic::Ordering,
        time::{Duration, Instant},
    },
    syntax::{
        SyntaxElement,
        SyntaxKind::{
            BINARY_EXPR, CALL_EXPR, EQ, FOR_EXPR, FUNCTION_DEF, IDENT, LESS_MINUS, NAME, PARAMETER,
        },
        SyntaxNode, SyntaxToken, TextRange, TextSize,
        ast::BinaryExpr,
    },
};

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
    let namespace_source = fs::read_to_string(root.join("NAMESPACE")).ok();
    let namespace_file = namespace_source
        .as_deref()
        .map(parse_namespace)
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
    // Every input is MEDIUM, as in the server, so the request timings below pay what the
    // server pays to verify memos after a keystroke.
    let _: StubSources = StubSources::builder(stub_sources, shipped_export_manifests())
        .durability(Durability::MEDIUM)
        .new(&db);
    let _: AnalysisSettings = AnalysisSettings::builder(
        config.check.inline_annotations,
        config.check.strict,
        config.check.prelude,
    )
    .durability(Durability::MEDIUM)
    .new(&db);
    let metadata = PackageMetadata::builder(
        normalized_imports(&namespace_file.imports),
        description_source
            .as_deref()
            .map(parse_description_dependencies)
            .unwrap_or_default(),
        BTreeSet::new(),
        None,
        namespace_file.native_symbols,
    )
    .durability(Durability::MEDIUM)
    .new(&db);
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
        let kind = cli::document_kind(&path, &root);
        let is_package = kind == DocumentKind::Package;
        package_count += usize::from(is_package);
        let start = Instant::now();
        // As in the server: files change at MEDIUM until the user edits one, and an edit
        // writes LOW, so the request timings below see the server's revalidation cost.
        let file = SourceFile::builder(source.clone(), kind)
            .durability(Durability::MEDIUM)
            .new(&db);
        load_total += start.elapsed();
        records.push(FileRecord {
            file,
            path,
            loc,
            source,
            typecheck: Duration::ZERO,
        });
    }
    // A singleton that queries reach through `ProjectFiles::get`. LOW, as in the server, so a
    // file's first edit does not re-execute the project.
    let _project = ProjectFiles::builder(records.iter().map(|record| record.file).collect())
        .durability(Durability::LOW)
        .new(&db);
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

    print_memo_census(&mut db);

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
    if let Some(&median) = by_loc.get(by_loc.len() / 2) {
        editor_requests(&mut db, &records, median, &config);
        editor_coverage(&db, &records, &root);
        completion_coverage(&mut db, &records, &root);
        reference_coverage(&mut db, &records);
        hierarchy_coverage(&db, &records);
    }

    Ok(())
}

/// Editor requests at identifiers sampled across the package files, on a settled database and
/// right after a keystroke in another file, the moment a request pays for revalidation.
fn editor_requests(db: &mut RootDatabase, records: &[FileRecord], edited: usize, config: &Config) {
    let Some(files) = ProjectFiles::try_get(db) else {
        return;
    };
    let positions: Vec<(SourceFile, TextSize)> = records
        .iter()
        .enumerate()
        .filter(|(index, record)| {
            *index != edited
                && record.file.kind(db) == &DocumentKind::Package
                && index % REQUEST_FILE_STRIDE == 0
        })
        .flat_map(|(_, record)| {
            identifier_offsets(&record.source)
                .into_iter()
                .step_by(REQUEST_POSITION_STRIDE)
                .map(move |offset| (record.file, offset))
        })
        .take(REQUEST_SAMPLES)
        .collect();
    let Some(edited) = records.get(edited) else {
        return;
    };
    let mut text = edited.source.clone();
    text.push('\n');
    println!();
    println!(
        "editor requests ({} positions; after a keystroke in another file):",
        positions.len()
    );
    for kind in RequestKind::ALL {
        let mut settled = Vec::with_capacity(positions.len());
        let mut after_edit = Vec::with_capacity(positions.len());
        for &(file, offset) in &positions {
            settled.push(time_request(db, files, kind, file, offset));
            text.push('#');
            edited
                .file
                .set_text(db)
                .with_durability(Durability::LOW)
                .to(text.clone());
            after_edit.push(time_request(db, files, kind, file, offset));
        }
        settled.sort();
        after_edit.sort();
        println!(
            "  {:<12} settled {:>7.1} ms p50 {:>7.1} ms p99   after edit {:>7.1} ms p50 {:>7.1} ms p99",
            kind.label(),
            percentile(&settled, 50),
            percentile(&settled, 99),
            percentile(&after_edit, 50),
            percentile(&after_edit, 99),
        );
    }
    edited
        .file
        .set_text(db)
        .with_durability(Durability::LOW)
        .to(edited.source.clone());
    let _ = document_diagnostics(db, edited.file, config);
}

/// How often hover and goto-definition answer at identifiers in package files, by the role the
/// identifier plays, with a few unanswered positions per role to show where the gaps are.
fn editor_coverage(db: &RootDatabase, records: &[FileRecord], root: &Path) {
    let Some(files) = ProjectFiles::try_get(db) else {
        return;
    };
    let mut tallies: BTreeMap<IdentifierRole, CoverageTally> = BTreeMap::new();
    let mut sampled = 0usize;
    // Answers that name an installed package count only when the library really has the name,
    // as the server would show it.
    let mut installed = InstalledPackages::new(root, None);
    for record in records {
        if record.file.kind(db) != &DocumentKind::Package {
            continue;
        }
        let syntax_root = semantics::parse(db, record.file).syntax_node();
        for offset in identifier_offsets(&record.source)
            .into_iter()
            .step_by(COVERAGE_POSITION_STRIDE)
        {
            if sampled == COVERAGE_SAMPLES {
                break;
            }
            let Some(role) = identifier_role(&record.source, usize::from(offset)) else {
                continue;
            };
            sampled += 1;
            let tally = tallies.entry(role).or_default();
            tally.total += 1;
            let hovered = ide::hover(db, files, record.file, offset);
            let hover_text = hovered
                .as_ref()
                .map(|hover| hover.lines.join(" | "))
                .unwrap_or_default();
            let documented = match hovered.and_then(|hover| hover.definition) {
                Some(HoverDefinition::Package(target)) => {
                    installed.documentation(&target).is_some()
                }
                _ => false,
            };
            // A hover that only says `Unknown` tells the reader nothing.
            let hover = documented || (!hover_text.is_empty() && hover_text != "Unknown");
            let definition = ide::definition(db, files, record.file, offset)
                .iter()
                .any(|target| match target {
                    DefinitionTarget::Package(target) => installed.definition(target).is_some(),
                    _ => true,
                });
            tally.hover += usize::from(hover);
            tally.definition += usize::from(definition);
            // A call head asks inside its parentheses, as typing `(` would.
            let signature_offset = match role {
                IdentifierRole::CallHead => record.source[usize::from(offset)..]
                    .find('(')
                    .map(|open| offset + TextSize::from(open as u32 + 1)),
                // A parameter's default sits in a definition, where signature help has no call
                // to describe.
                IdentifierRole::ArgumentName => syntax_root
                    .token_at_offset(offset)
                    .right_biased()
                    .filter(|token| !is_declaration(token, &record.source[usize::from(offset)..]))
                    .map(|_| offset),
                _ => None,
            };
            let mut signature = true;
            if let Some(signature_offset) = signature_offset {
                signature =
                    ide::signature_help_with(db, record.file, signature_offset, &mut installed)
                        .is_some();
                tally.signature_total += 1;
                tally.signature += usize::from(signature);
            }
            if !(hover && definition && signature) && tally.misses.len() < COVERAGE_EXAMPLES {
                let line_start = record.source[..usize::from(offset)]
                    .rfind('\n')
                    .map_or(0, |index| index + 1);
                let line_end = record.source[line_start..]
                    .find('\n')
                    .map_or(record.source.len(), |index| line_start + index);
                let line = record.source[..line_start].matches('\n').count() + 1;
                tally.misses.push(format!(
                    "{}{}{} {}:{line}: {}   [{hover_text}]",
                    if hover { ' ' } else { 'H' },
                    if definition { ' ' } else { 'D' },
                    if signature { ' ' } else { 'S' },
                    record
                        .path
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                    record.source[line_start..line_end].trim(),
                ));
            }
        }
    }
    println!();
    println!("editor coverage ({sampled} identifiers in package files):");
    for (role, tally) in &tallies {
        let share = |answered: usize| answered as f64 * 100.0 / tally.total.max(1) as f64;
        let signature = if tally.signature_total > 0 {
            format!(
                "   signature {:>5.1}%",
                tally.signature as f64 * 100.0 / tally.signature_total as f64
            )
        } else {
            String::new()
        };
        println!(
            "  {:<16} {:>6}   hover {:>5.1}%   definition {:>5.1}%{signature}",
            role.label(),
            tally.total,
            share(tally.hover),
            share(tally.definition),
        );
        for miss in &tally.misses {
            println!("      {miss}");
        }
    }
}

/// How often completion offers the identifier the code actually wrote among its first
/// [`COMPLETION_TOP`] items, after cutting the identifier back in the file to the prefix a user
/// would have typed: half the name, and the first [`COMPLETION_PREFIX`] characters. Declarations
/// are skipped, since nothing can complete a name before it exists.
fn completion_coverage(db: &mut RootDatabase, records: &[FileRecord], root: &Path) {
    let Some(files) = ProjectFiles::try_get(db) else {
        return;
    };
    let mut tallies: BTreeMap<IdentifierRole, CompletionTally> = BTreeMap::new();
    let mut planned_per_role: BTreeMap<IdentifierRole, usize> = BTreeMap::new();
    let mut latencies = Vec::new();
    let mut installed = InstalledPackages::new(root, None);
    for record in records {
        if record.file.kind(db) != &DocumentKind::Package {
            continue;
        }
        let root = semantics::parse(db, record.file).syntax_node();
        let mut samples = Vec::new();
        for offset in identifier_offsets(&record.source)
            .into_iter()
            .step_by(COMPLETION_POSITION_STRIDE)
        {
            let start = usize::from(offset);
            let Some(role) = identifier_role(&record.source, start) else {
                continue;
            };
            let planned = planned_per_role.entry(role).or_insert(0usize);
            if *planned == COMPLETION_SAMPLES_PER_ROLE {
                continue;
            }
            let Some(token) = root
                .token_at_offset(offset)
                .right_biased()
                .filter(|token| token.kind() == IDENT)
            else {
                continue;
            };
            let word = token.text();
            if word.chars().count() <= COMPLETION_PREFIX
                || is_declaration(&token, &record.source[start..])
                || (role == IdentifierRole::ArgumentName
                    && names_a_dots_element(db, record.file, offset, word, &mut installed))
            {
                continue;
            }
            *planned += 1;
            samples.push((offset, word.to_owned(), role));
        }
        for (offset, word, role) in samples {
            let start = usize::from(offset);
            let length = word.chars().count();
            let tally = tallies.entry(role).or_default();
            tally.total += 1;
            for (cut, typed) in [
                (PrefixCut::Short, COMPLETION_PREFIX),
                (PrefixCut::Half, length.div_ceil(2).max(COMPLETION_PREFIX)),
            ] {
                let prefix_end = start
                    + word
                        .char_indices()
                        .nth(typed)
                        .map_or(word.len(), |(index, _)| index);
                let mut text = String::with_capacity(record.source.len());
                text.push_str(&record.source[..prefix_end]);
                text.push_str(&record.source[start + word.len()..]);
                record
                    .file
                    .set_text(db)
                    .with_durability(Durability::LOW)
                    .to(text);
                let cursor = TextSize::from(prefix_end as u32);
                let began = Instant::now();
                let result = ide::completion_with(db, files, record.file, cursor, &mut installed);
                latencies.push(began.elapsed());
                let labels: Vec<String> = result
                    .map(|result| {
                        result
                            .items
                            .into_iter()
                            .take(COMPLETION_TOP)
                            .map(|item| item.label)
                            .collect()
                    })
                    .unwrap_or_default();
                let offered = labels
                    .iter()
                    .any(|label| label.trim_end_matches(" = ").trim_matches('`') == word);
                match cut {
                    PrefixCut::Short => tally.offered_short += usize::from(offered),
                    PrefixCut::Half => tally.offered_half += usize::from(offered),
                }
                if offered || cut == PrefixCut::Short || tally.misses.len() == COVERAGE_EXAMPLES {
                    continue;
                }
                let line_start = record.source[..start]
                    .rfind('\n')
                    .map_or(0, |index| index + 1);
                let line_end = record.source[line_start..]
                    .find('\n')
                    .map_or(record.source.len(), |index| line_start + index);
                let line = record.source[..line_start].matches('\n').count() + 1;
                tally.misses.push(format!(
                    "{}:{line}: {word}   [{}]   {}{}",
                    record
                        .path
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                    labels.join(", "),
                    record.source[line_start..line_end].trim(),
                    // An argument name is only as good as the signature the call resolves to.
                    match role {
                        IdentifierRole::ArgumentName => format!(
                            "   signature: {}",
                            ide::signature_help_with(db, record.file, cursor, &mut installed)
                                .and_then(|help| help.signatures.into_iter().next())
                                .map_or_else(|| "none".to_owned(), |signature| signature.label)
                        ),
                        _ => String::new(),
                    },
                ));
            }
        }
        record
            .file
            .set_text(db)
            .with_durability(Durability::LOW)
            .to(record.source.clone());
    }
    latencies.sort();
    let sampled: usize = tallies.values().map(|tally| tally.total).sum();
    println!();
    println!(
        "completion coverage ({sampled} identifiers, top {COMPLETION_TOP} after typing half the name or {COMPLETION_PREFIX} characters; {:.1} ms p50 {:.1} ms p99):",
        percentile(&latencies, 50),
        percentile(&latencies, 99),
    );
    for (role, tally) in &tallies {
        let share = |offered: usize| offered as f64 * 100.0 / tally.total.max(1) as f64;
        println!(
            "  {:<16} {:>6}   half {:>5.1}%   {COMPLETION_PREFIX} characters {:>5.1}%",
            role.label(),
            tally.total,
            share(tally.offered_half),
            share(tally.offered_short),
        );
        for miss in &tally.misses {
            println!("      {miss}");
        }
    }
}

/// Find-references and rename at the package's own bindings, checked against
/// [`ScopeOracle`], a resolver that reads only syntax and R's scoping rules. References count when
/// the set they find is exactly the resolver's; rename counts when renaming the binding and then
/// renaming it back restores every file.
fn reference_coverage(db: &mut RootDatabase, records: &[FileRecord]) {
    let Some(files) = ProjectFiles::try_get(db) else {
        return;
    };
    let PackageSites {
        package,
        index,
        oracle,
        ..
    } = package_sites(db, records);
    let mut expected: BTreeMap<&Binding, BTreeSet<Span>> = BTreeMap::new();
    let mut undecided: FxHashSet<Span> = FxHashSet::default();
    for site in &oracle.sites {
        match &site.binding {
            Some(binding) => {
                expected
                    .entry(binding)
                    .or_default()
                    .insert(span(site.file, site.range));
            }
            None => {
                undecided.insert(span(site.file, site.range));
            }
        }
    }
    let candidates: Vec<&Site> = oracle
        .sites
        .iter()
        .filter(|site| {
            site.binding
                .as_ref()
                .is_some_and(|binding| oracle.is_project(binding))
        })
        .collect();
    let stride = candidates.len().div_ceil(REFERENCE_SAMPLES).max(1);
    let mut tallies: BTreeMap<&str, ReferenceTally> = BTreeMap::new();
    let mut latencies = Vec::new();
    let location = |file: usize, start: TextSize| {
        let Some(record) = package.get(file) else {
            return String::new();
        };
        let line = record
            .source
            .get(..usize::from(start))
            .map_or(0, |before| before.matches('\n').count())
            + 1;
        let name = record
            .path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        format!("{name}:{line}")
    };
    for (position, site) in candidates.iter().step_by(stride).enumerate() {
        let Some(binding) = &site.binding else {
            continue;
        };
        let kind = match binding {
            Binding::Local { .. } => "local",
            Binding::Global(_) => "package-level",
        };
        let tally = tallies.entry(kind).or_default();
        let record = package[site.file];
        let began = Instant::now();
        let found: BTreeSet<Span> =
            ide::references(db, files, record.file, site.range.start(), true)
                .into_iter()
                .filter_map(|occurrence| {
                    Some(span(*index.get(&occurrence.file)?, occurrence.range))
                })
                .filter(|occurrence| !undecided.contains(occurrence))
                .collect();
        latencies.push(began.elapsed());
        let wanted = expected.get(binding).cloned().unwrap_or_default();
        tally.references_total += 1;
        if found == wanted {
            tally.references += 1;
        } else if tally.reference_misses.len() < COVERAGE_EXAMPLES {
            let missing: Vec<String> = wanted
                .difference(&found)
                .take(3)
                .map(|&(file, start, _)| location(file, start))
                .collect();
            let extra: Vec<String> = found
                .difference(&wanted)
                .take(3)
                .map(|&(file, start, _)| location(file, start))
                .collect();
            tally.reference_misses.push(format!(
                "{}: {}   expected {}, found {}   missing [{}]   extra [{}]",
                location(site.file, site.range.start()),
                site.name,
                wanted.len(),
                found.len(),
                missing.join(", "),
                extra.join(", "),
            ));
        }
        // Renaming edits every file it touches, so it runs on every other sample.
        // A non-syntactic name would need quoting the harness's new spelling does not add.
        let syntactic = site
            .name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '.' | '_'));
        if position % RENAME_EVERY != 0 || !syntactic {
            continue;
        }
        tally.rename_total += 1;
        match rename_round_trip(db, files, records, &package, site) {
            Ok(()) => tally.rename += 1,
            Err(reason) => {
                if tally.rename_misses.len() < COVERAGE_EXAMPLES {
                    tally.rename_misses.push(format!(
                        "{}: {}   {reason}",
                        location(site.file, site.range.start()),
                        site.name
                    ));
                }
            }
        }
    }
    latencies.sort();
    let sampled: usize = tallies.values().map(|tally| tally.references_total).sum();
    println!();
    println!(
        "references and rename ({sampled} bindings, checked against a lexical resolver; references {:.1} ms p50 {:.1} ms p99):",
        percentile(&latencies, 50),
        percentile(&latencies, 99),
    );
    for (kind, tally) in &tallies {
        let share = |count: usize, total: usize| count as f64 * 100.0 / total.max(1) as f64;
        println!(
            "  {kind:<16} {:>6}   references {:>5.1}%   rename {:>5.1}% of {}",
            tally.references_total,
            share(tally.references, tally.references_total),
            share(tally.rename, tally.rename_total),
            tally.rename_total,
        );
        for miss in tally.reference_misses.iter().chain(&tally.rename_misses) {
            println!("      {miss}");
        }
    }
}

/// The package files with their syntax trees and the lexical resolver's answer for every name,
/// which the reference and call hierarchy roles check the editor against.
struct PackageSites<'a> {
    package: Vec<&'a FileRecord>,
    index: FxHashMap<SourceFile, usize>,
    roots: Vec<SyntaxNode>,
    oracle: ScopeOracle,
}

fn package_sites<'a>(db: &RootDatabase, records: &'a [FileRecord]) -> PackageSites<'a> {
    let package: Vec<&FileRecord> = records
        .iter()
        .filter(|record| record.file.kind(db) == &DocumentKind::Package)
        .collect();
    let index = package
        .iter()
        .enumerate()
        .map(|(index, record)| (record.file, index))
        .collect();
    let roots: Vec<SyntaxNode> = package
        .iter()
        .map(|record| semantics::parse(db, record.file).syntax_node())
        .collect();
    let oracle = ScopeOracle::build(&roots);
    PackageSites {
        package,
        index,
        roots,
        oracle,
    }
}

/// Call hierarchy and workspace symbols for the package's top-level definitions, checked
/// against the lexical resolver: the incoming calls of a function must be exactly the call
/// sites that resolve to it, each under the top-level item that contains it, its outgoing calls
/// exactly the calls in its body that resolve to another project function, and searching a
/// definition's name, or the first half of it, must list the definition.
fn hierarchy_coverage(db: &RootDatabase, records: &[FileRecord]) {
    let Some(files) = ProjectFiles::try_get(db) else {
        return;
    };
    let PackageSites {
        package,
        index,
        roots,
        oracle,
    } = package_sites(db, records);
    let mut definitions: Vec<Definition> = Vec::new();
    for (file, root) in roots.iter().enumerate() {
        for statement in root.children() {
            let Some(binary) = BinaryExpr::cast(statement.clone()) else {
                continue;
            };
            let (Some(target), Some(operator)) = (binary.lhs(), binary.operator()) else {
                continue;
            };
            if target.kind() != NAME || !matches!(operator.kind(), LESS_MINUS | EQ) {
                continue;
            }
            definitions.push(Definition {
                file,
                name: target.text().to_string().trim_matches('`').to_owned(),
                name_range: target.text_range(),
                range: statement.text_range(),
                is_function: binary
                    .rhs()
                    .is_some_and(|value| value.kind() == FUNCTION_DEF),
            });
        }
    }
    let mut definition_count: FxHashMap<&str, usize> = FxHashMap::default();
    for definition in &definitions {
        *definition_count.entry(&definition.name).or_default() += 1;
    }
    // A name defined twice has no single function for the hierarchy to show.
    let functions: FxHashMap<&str, &Definition> = definitions
        .iter()
        .filter(|definition| {
            definition.is_function && definition_count.get(definition.name.as_str()) == Some(&1)
        })
        .map(|definition| (definition.name.as_str(), definition))
        .collect();
    let caller_name = |file: usize, offset: TextSize| {
        definitions
            .iter()
            .find(|definition| definition.file == file && definition.range.contains(offset))
            .map_or_else(
                || "(top level)".to_owned(),
                |definition| definition.name.clone(),
            )
    };
    let mut incoming_expected: FxHashMap<&str, BTreeSet<(usize, TextSize, String)>> =
        FxHashMap::default();
    let mut outgoing_expected: FxHashMap<&str, BTreeSet<(usize, TextSize, String)>> =
        FxHashMap::default();
    let mut undecided: FxHashSet<(usize, TextSize)> = FxHashSet::default();
    for site in &oracle.sites {
        let Some(Binding::Global(callee)) = &site.binding else {
            if site.binding.is_none() {
                undecided.insert((site.file, site.range.start()));
            }
            continue;
        };
        let Some(root) = roots.get(site.file) else {
            continue;
        };
        if !functions.contains_key(callee.as_str()) || !is_call_head(root, site.range) {
            continue;
        }
        let caller = caller_name(site.file, site.range.start());
        incoming_expected
            .entry(callee.as_str())
            .or_default()
            .insert((site.file, site.range.start(), caller.clone()));
        if let Some(&definition) = functions.get(caller.as_str())
            && definition.file == site.file
        {
            outgoing_expected
                .entry(definition.name.as_str())
                .or_default()
                .insert((site.file, site.range.start(), callee.clone()));
        }
    }
    let location = |file: usize, start: TextSize| {
        let Some(record) = package.get(file) else {
            return String::new();
        };
        let line = record
            .source
            .get(..usize::from(start))
            .map_or(0, |before| before.matches('\n').count())
            + 1;
        let name = record
            .path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        format!("{name}:{line}")
    };
    let mut sampled: Vec<&Definition> = functions.values().copied().collect();
    sampled.sort_by_key(|definition| (definition.file, definition.range.start()));
    let stride = sampled.len().div_ceil(HIERARCHY_SAMPLES).max(1);
    let mut incoming = HierarchyTally::default();
    let mut outgoing = HierarchyTally::default();
    let mut latencies = Vec::new();
    for definition in sampled.iter().step_by(stride) {
        let Some(record) = package.get(definition.file) else {
            continue;
        };
        let began = Instant::now();
        let item = ide::call_hierarchy_item(db, files, record.file, definition.name_range.start());
        let (found_incoming, found_outgoing) = match &item {
            Some(item) => (
                ide::incoming_calls(db, files, item),
                ide::outgoing_calls(db, files, item),
            ),
            None => (Vec::new(), Vec::new()),
        };
        latencies.push(began.elapsed());
        // Incoming ranges sit in each caller's file, outgoing ones in the definition's.
        let flatten = |calls: Vec<ide::CallHierarchyCall>,
                       in_file: Option<usize>|
         -> BTreeSet<(usize, TextSize, String)> {
            let mut flat = BTreeSet::new();
            for call in calls {
                let Some(file) = in_file.or_else(|| index.get(&call.item.file).copied()) else {
                    continue;
                };
                for range in call.ranges {
                    if !undecided.contains(&(file, range.start())) {
                        flat.insert((file, range.start(), call.item.name.clone()));
                    }
                }
            }
            flat
        };
        let found_incoming = flatten(found_incoming, None);
        let found_outgoing = flatten(found_outgoing, Some(definition.file));
        let empty = BTreeSet::new();
        let wanted_incoming = incoming_expected
            .get(definition.name.as_str())
            .unwrap_or(&empty);
        let wanted_outgoing = outgoing_expected
            .get(definition.name.as_str())
            .unwrap_or(&empty);
        for (tally, wanted, found) in [
            (&mut incoming, wanted_incoming, &found_incoming),
            (&mut outgoing, wanted_outgoing, &found_outgoing),
        ] {
            tally.total += 1;
            if wanted == found {
                tally.matched += 1;
            } else if tally.misses.len() < COVERAGE_EXAMPLES {
                let describe = |set: &BTreeSet<(usize, TextSize, String)>| {
                    set.iter()
                        .take(3)
                        .map(|(file, start, name)| format!("{} {name}", location(*file, *start)))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                tally.misses.push(format!(
                    "{}: {}   expected {}, found {}   missing [{}]   extra [{}]",
                    location(definition.file, definition.name_range.start()),
                    definition.name,
                    wanted.len(),
                    found.len(),
                    describe(&wanted.difference(found).cloned().collect()),
                    describe(&found.difference(wanted).cloned().collect()),
                ));
            }
        }
    }
    let mut symbols = HierarchyTally::default();
    let mut prefixes = HierarchyTally::default();
    let mut symbol_latencies = Vec::new();
    let stride = definitions.len().div_ceil(HIERARCHY_SAMPLES).max(1);
    for definition in definitions.iter().step_by(stride) {
        let Some(record) = package.get(definition.file) else {
            continue;
        };
        let half = definition.name.chars().count().div_ceil(2).max(3);
        let prefix: String = definition.name.chars().take(half).collect();
        for (tally, query) in [
            (&mut symbols, definition.name.as_str()),
            (&mut prefixes, prefix.as_str()),
        ] {
            let began = Instant::now();
            let results = ide::workspace_symbols(db, files, query);
            symbol_latencies.push(began.elapsed());
            tally.total += 1;
            let listed = results.iter().any(|symbol| {
                symbol.target.file == record.file && symbol.target.range == definition.name_range
            });
            if listed {
                tally.matched += 1;
            } else if results.len() == ide::COMPLETION_LIMIT {
                // More names share the prefix than a search lists, so a user types on; the
                // search is not wrong to leave this one out.
                tally.total -= 1;
                tally.crowded += 1;
            } else if tally.misses.len() < COVERAGE_EXAMPLES {
                tally.misses.push(format!(
                    "{}: {}   query {query:?} lists {} symbols, not this definition",
                    location(definition.file, definition.name_range.start()),
                    definition.name,
                    results.len(),
                ));
            }
        }
    }
    latencies.sort();
    symbol_latencies.sort();
    let share = |tally: &HierarchyTally| tally.matched as f64 * 100.0 / tally.total.max(1) as f64;
    println!();
    println!(
        "call hierarchy ({} functions, checked against a lexical resolver; {:.1} ms p50 {:.1} ms p99):",
        incoming.total,
        percentile(&latencies, 50),
        percentile(&latencies, 99),
    );
    println!("  incoming calls   {:>5.1}%", share(&incoming));
    for miss in &incoming.misses {
        println!("      {miss}");
    }
    println!("  outgoing calls   {:>5.1}%", share(&outgoing));
    for miss in &outgoing.misses {
        println!("      {miss}");
    }
    println!(
        "workspace symbols ({} definitions; {:.1} ms p50 {:.1} ms p99):",
        symbols.total,
        percentile(&symbol_latencies, 50),
        percentile(&symbol_latencies, 99),
    );
    println!("  exact name       {:>5.1}%", share(&symbols));
    for miss in &symbols.misses {
        println!("      {miss}");
    }
    println!(
        "  name prefix      {:>5.1}%   ({} prefixes match more than {} names, left out)",
        share(&prefixes),
        prefixes.crowded,
        ide::COMPLETION_LIMIT,
    );
    for miss in &prefixes.misses {
        println!("      {miss}");
    }
}

const HIERARCHY_SAMPLES: usize = 300;

/// A top-level `name <- value` statement of a package file.
struct Definition {
    file: usize,
    name: String,
    name_range: TextRange,
    range: TextRange,
    is_function: bool,
}

#[derive(Default)]
struct HierarchyTally {
    total: usize,
    matched: usize,
    crowded: usize,
    misses: Vec<String>,
}

/// Whether the name at `range` is the head of a call, `f` in `f(x)`.
fn is_call_head(root: &SyntaxNode, range: TextRange) -> bool {
    let SyntaxElement::Token(token) = root.covering_element(range) else {
        return false;
    };
    token.parent().is_some_and(|name| {
        name.kind() == NAME
            && name.parent().is_some_and(|call| {
                call.kind() == CALL_EXPR && call.first_child().as_ref() == Some(&name)
            })
    })
}

/// Renames the binding at `site` to a fresh name and back, and restores the package's files
/// whatever happens.
fn rename_round_trip(
    db: &mut RootDatabase,
    files: ProjectFiles,
    records: &[FileRecord],
    package: &[&FileRecord],
    site: &Site,
) -> Result<(), String> {
    let record = package[site.file];
    let renamed = format!("ry_renamed_{}", site.name);
    let first = ide::rename(db, files, record.file, site.range.start())
        .ok_or_else(|| "refused".to_owned())?;
    let edits = rename_edits(db, &first, &site.name, &renamed)?;
    let mut touched: Vec<SourceFile> = edits.iter().map(|(file, _)| *file).collect();
    set_texts(db, edits);
    // Every occurrence before the site in its file grew by the same amount.
    let shift = first
        .iter()
        .filter(|occurrence| {
            occurrence.file == record.file && occurrence.range.end() <= site.range.start()
        })
        .count()
        * (renamed.len() - site.name.len());
    let moved = site.range.start() + TextSize::from(shift as u32);
    let outcome = match ide::rename(db, files, record.file, moved) {
        None => Err("refused after the first rename".to_owned()),
        Some(back) if back.len() != first.len() => Err(format!(
            "renamed {} occurrences, then {} back",
            first.len(),
            back.len()
        )),
        Some(back) => rename_edits(db, &back, &renamed, &site.name).map(|edits| {
            touched.extend(edits.iter().map(|(file, _)| *file));
            set_texts(db, edits);
        }),
    };
    let changed = touched.iter().find_map(|&file| {
        let text: &str = file.text(db);
        let original = original_text(records, file);
        let differs_at = text
            .bytes()
            .zip(original.bytes())
            .position(|(left, right)| left != right)
            .or_else(|| (text.len() != original.len()).then(|| text.len().min(original.len())))?;
        let line = original[..differs_at].matches('\n').count() + 1;
        let path = records
            .iter()
            .find(|record| record.file == file)
            .and_then(|record| record.path.file_name())
            .map_or(std::borrow::Cow::Borrowed(""), |name| {
                name.to_string_lossy()
            });
        Some(format!("renaming back changed {path}:{line}"))
    });
    restore(db, records, &touched);
    outcome?;
    changed.map_or(Ok(()), Err)
}

/// Each edited file's new text, provided every occurrence spells `from`. A backticked
/// occurrence keeps its backticks, so every occurrence grows by the same amount.
fn rename_edits(
    db: &RootDatabase,
    occurrences: &[ide::Occurrence],
    from: &str,
    to: &str,
) -> Result<Vec<(SourceFile, String)>, String> {
    let mut by_file: FxHashMap<SourceFile, Vec<TextRange>> = FxHashMap::default();
    for occurrence in occurrences {
        by_file
            .entry(occurrence.file)
            .or_default()
            .push(occurrence.range);
    }
    let mut edits = Vec::new();
    for (file, mut ranges) in by_file {
        let mut text = file.text(db).to_string();
        ranges.sort_by_key(|range| Reverse(range.start()));
        for range in ranges {
            let span = std::ops::Range::<usize>::from(range);
            let spelled = text.get(span.clone()).unwrap_or_default();
            if spelled.trim_matches('`') != from {
                return Err(format!("an occurrence spells `{spelled}`"));
            }
            if spelled.starts_with('`') {
                text.replace_range(span, &format!("`{to}`"));
            } else {
                text.replace_range(span, to);
            }
        }
        edits.push((file, text));
    }
    Ok(edits)
}

fn set_texts(db: &mut RootDatabase, edits: Vec<(SourceFile, String)>) {
    for (file, text) in edits {
        file.set_text(db).with_durability(Durability::LOW).to(text);
    }
}

fn original_text(records: &[FileRecord], file: SourceFile) -> &str {
    records
        .iter()
        .find(|record| record.file == file)
        .map_or("", |record| record.source.as_str())
}

fn restore(db: &mut RootDatabase, records: &[FileRecord], touched: &[SourceFile]) {
    for record in records
        .iter()
        .filter(|record| touched.contains(&record.file))
    {
        record
            .file
            .set_text(db)
            .with_durability(Durability::LOW)
            .to(record.source.clone());
    }
}

/// A file index and a range: an occurrence in one of the package's files.
type Span = (usize, TextSize, TextSize);

fn span(file: usize, range: TextRange) -> Span {
    (file, range.start(), range.end())
}

const REFERENCE_SAMPLES: usize = 400;
const RENAME_EVERY: usize = 2;

#[derive(Default)]
struct ReferenceTally {
    references_total: usize,
    references: usize,
    reference_misses: Vec<String>,
    rename_total: usize,
    rename: usize,
    rename_misses: Vec<String>,
}

/// `list(total = 1)` or `tryCatch(warning = ...)`: an argument name the known signature does not
/// declare, which `...` takes as a new name that nothing could complete.
fn names_a_dots_element(
    db: &RootDatabase,
    file: SourceFile,
    offset: TextSize,
    word: &str,
    installed: &mut InstalledPackages,
) -> bool {
    let Some(signature) = ide::signature_help_with(db, file, offset, installed)
        .and_then(|help| help.signatures.into_iter().next())
    else {
        return false;
    };
    let mut has_dots = false;
    for span in &signature.parameters {
        let Some(parameter) = signature
            .label
            .get(usize::from(span.start())..usize::from(span.end()))
        else {
            continue;
        };
        let name = parameter
            .trim_start_matches('[')
            .split([']', ':', ' '])
            .next()
            .unwrap_or_default()
            .trim();
        if name == word {
            return false;
        }
        has_dots |= name == "...";
    }
    has_dots
}

/// A parameter, a loop variable, or an assignment target introduces the name it spells.
fn is_declaration(token: &SyntaxToken, rest: &str) -> bool {
    let after = rest[token.text().len()..].trim_start_matches([' ', '\t']);
    if after.starts_with("<-") || after.starts_with("<<-") || after.starts_with(":=") {
        return true;
    }
    token
        .parent_ancestors()
        .take(2)
        .any(|node| match node.kind() {
            PARAMETER | FOR_EXPR => true,
            // `x = 1` as a statement, not `f(x = 1)`.
            BINARY_EXPR => after.starts_with('=') && !after.starts_with("=="),
            _ => false,
        })
}

const COMPLETION_PREFIX: usize = 2;
const COMPLETION_TOP: usize = 5;
const COMPLETION_POSITION_STRIDE: usize = 5;
const COMPLETION_SAMPLES_PER_ROLE: usize = 400;

#[derive(Default)]
struct CompletionTally {
    total: usize,
    offered_half: usize,
    offered_short: usize,
    /// Positions where half the name was not enough.
    misses: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrefixCut {
    /// The first [`COMPLETION_PREFIX`] characters.
    Short,
    /// Half the name, rounded up: completion that misses here saves less than half the typing.
    Half,
}

const COVERAGE_SAMPLES: usize = 20_000;
const COVERAGE_POSITION_STRIDE: usize = 3;
const COVERAGE_EXAMPLES: usize = 12;

#[derive(Default)]
struct CoverageTally {
    total: usize,
    hover: usize,
    definition: usize,
    /// Call heads and argument names only: how many were asked for signature help, and how many
    /// got a signature.
    signature_total: usize,
    signature: usize,
    misses: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum IdentifierRole {
    Namespaced,
    Field,
    ArgumentName,
    CallHead,
    Variable,
}

impl IdentifierRole {
    fn label(self) -> &'static str {
        match self {
            IdentifierRole::Namespaced => "pkg::name",
            IdentifierRole::Field => "$ or @ field",
            IdentifierRole::ArgumentName => "argument name",
            IdentifierRole::CallHead => "call head",
            IdentifierRole::Variable => "variable",
        }
    }
}

/// The role of the identifier starting at `start`, from the surrounding text, or `None` for a
/// keyword or constant that no editor request answers.
fn identifier_role(source: &str, start: usize) -> Option<IdentifierRole> {
    let rest = source.get(start..)?;
    let length = rest
        .find(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '.')
        })
        .unwrap_or(rest.len());
    let word = &rest[..length];
    if matches!(
        word,
        "function"
            | "if"
            | "else"
            | "for"
            | "while"
            | "repeat"
            | "in"
            | "next"
            | "break"
            | "TRUE"
            | "FALSE"
            | "NULL"
            | "NA"
            | "Inf"
            | "NaN"
            | "NA_integer_"
            | "NA_real_"
            | "NA_character_"
            | "T"
            | "F"
    ) {
        return None;
    }
    let before = source[..start].trim_end_matches([' ', '\t']);
    let after = rest[length..].trim_start_matches([' ', '\t']);
    if before.ends_with("::") {
        return Some(IdentifierRole::Namespaced);
    }
    if before.ends_with('$') || before.ends_with('@') {
        return Some(IdentifierRole::Field);
    }
    if after.starts_with('(') {
        return Some(IdentifierRole::CallHead);
    }
    // Arguments often sit one per line, so the `(` or `,` may end an earlier line.
    let before_line = source[..start].trim_end();
    if after.starts_with('=')
        && !after.starts_with("==")
        && (before_line.ends_with('(') || before_line.ends_with(','))
    {
        return Some(IdentifierRole::ArgumentName);
    }
    Some(IdentifierRole::Variable)
}

const REQUEST_SAMPLES: usize = 200;
const REQUEST_FILE_STRIDE: usize = 7;
const REQUEST_POSITION_STRIDE: usize = 37;

#[derive(Clone, Copy)]
enum RequestKind {
    Parse,
    Hover,
    Completion,
    Definition,
    References,
    Rename,
}

impl RequestKind {
    const ALL: [RequestKind; 6] = [
        RequestKind::Parse,
        RequestKind::Hover,
        RequestKind::Completion,
        RequestKind::Definition,
        RequestKind::References,
        RequestKind::Rename,
    ];

    fn label(self) -> &'static str {
        match self {
            RequestKind::Parse => "parse (floor)",
            RequestKind::Hover => "hover",
            RequestKind::Completion => "completion",
            RequestKind::Definition => "definition",
            RequestKind::References => "references",
            RequestKind::Rename => "rename",
        }
    }
}

fn time_request(
    db: &RootDatabase,
    files: ProjectFiles,
    kind: RequestKind,
    file: SourceFile,
    offset: TextSize,
) -> Duration {
    let start = Instant::now();
    match kind {
        RequestKind::Parse => drop(semantics::parse(db, file)),
        RequestKind::Hover => drop(ide::hover(db, files, file, offset)),
        // Mid-identifier, as while typing.
        RequestKind::Completion => {
            drop(ide::completion(db, files, file, offset + TextSize::from(1)))
        }
        RequestKind::Definition => drop(ide::definition(db, files, file, offset)),
        RequestKind::References => drop(ide::references(db, files, file, offset, true)),
        RequestKind::Rename => drop(ide::rename(db, files, file, offset)),
    }
    start.elapsed()
}

fn percentile(sorted: &[Duration], percent: usize) -> f64 {
    let index = (sorted.len() * percent / 100).min(sorted.len().saturating_sub(1));
    sorted
        .get(index)
        .map_or(0.0, |duration| duration.as_secs_f64() * 1e3)
}

/// Byte offsets of identifiers outside comments and strings, line by line.
fn identifier_offsets(source: &str) -> Vec<TextSize> {
    let mut offsets = Vec::new();
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        let code = line.split('#').next().unwrap_or("");
        if !code.contains('"') && !code.contains('\'') {
            let bytes = code.as_bytes();
            for (index, &byte) in bytes.iter().enumerate() {
                let previous = index.checked_sub(1).and_then(|before| bytes.get(before));
                let starts_word = byte.is_ascii_alphabetic()
                    && previous.is_none_or(|&before| {
                        !(before.is_ascii_alphanumeric() || before == b'_' || before == b'.')
                    });
                if starts_word {
                    offsets.push(TextSize::from((line_start + index) as u32));
                }
            }
        }
        line_start += line.len();
    }
    offsets
}

/// Salsa's own census: how many memos each query holds and their inline size plus
/// bookkeeping (dependency edges are the bulk of the metadata). Heap behind a value is
/// not counted, so read it beside the phase growth above.
fn print_memo_census(db: &mut RootDatabase) {
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
    let mut structs: Vec<_> = info
        .structs
        .iter()
        .filter(|entry| entry.count() > 0)
        .collect();
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
    let record = &records[index];
    let mut text = record.source.clone();
    text.push('\n');
    let checks_before = CHECK_EXECUTIONS.load(Ordering::Relaxed);
    let resolves_before = resolve_calls();
    let mut keystroke_times = Vec::with_capacity(TYPING_BURST);
    for _ in 0..TYPING_BURST {
        text.push('#');
        let start = Instant::now();
        record
            .file
            .set_text(db)
            .with_durability(Durability::LOW)
            .to(text.clone());
        let _ = document_diagnostics(db, record.file, config);
        keystroke_times.push(start.elapsed());
    }
    let checks_after_burst = CHECK_EXECUTIONS.load(Ordering::Relaxed);
    let resolves_after_burst = resolve_calls();
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
    record
        .file
        .set_text(db)
        .with_durability(Durability::LOW)
        .to(record.source.clone());
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
