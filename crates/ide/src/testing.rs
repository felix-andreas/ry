//! The editor features' fuzz contract, shared by the in-tree harness and the coverage-guided
//! `fuzz/` target so both check exactly the same properties.

use {
    crate::{DefinitionTarget, DocumentSymbol, HoverDefinition, Occurrence, TextEdit},
    salsa::{Setter as _, plumbing::AsId as _},
    semantics::{
        AnalysisSettings, DocumentKind, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::file_diagnostics, stubs::install_shipped_stubs,
    },
    std::{
        collections::{BTreeMap, BTreeSet, HashSet},
        panic::{self, AssertUnwindSafe},
    },
    syntax::{
        SyntaxKind, TextRange, TextSize, lex, parse,
        testing::{FAILURES_PER_RUN, fuzz_seeds, replay_command},
    },
};

/// An editor's database that outlives a fuzz case, so a case pays for its analysis and not for
/// parsing the stub corpus again. It moves from project to project through the setters, as a
/// session does, so every case also runs on a database with an unrelated history.
pub struct Workbench {
    db: RootDatabase,
    inputs: Vec<SourceFile>,
}

impl Default for Workbench {
    fn default() -> Workbench {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        AnalysisSettings::new(&db, true, false, true);
        ProjectFiles::new(&db, Vec::new());
        Workbench {
            db,
            inputs: Vec::new(),
        }
    }
}

/// Runs the projects `session` draws for each seed of [`fuzz_seeds`], in order, on a warm
/// [`Workbench`]. A failure is retried on a fresh one to tell a failure of the session alone,
/// which its seed replays, from one that needs the sessions before it, which the window from
/// the workbench's first seed replays. The workbench is rebuilt every [`WORKBENCH_SESSIONS`]
/// seeds so that window stays short, and after a failure, since the run goes on to report up to
/// [`FAILURES_PER_RUN`] failing seeds.
pub fn for_each_session(default: usize, mut session: impl FnMut(u64) -> Vec<Project>) {
    let mut bench: Option<(u64, Workbench)> = None;
    let mut replays: Vec<String> = Vec::new();
    for seed in fuzz_seeds(default) {
        if seed % WORKBENCH_SESSIONS == 0 {
            bench = None;
        }
        let (first_seed, workbench) = bench.get_or_insert_with(|| (seed, Workbench::default()));
        let projects = session(seed);
        // After the first project, only the files a step changed are swept again.
        let check = |workbench: &mut Workbench| {
            panic::catch_unwind(AssertUnwindSafe(|| {
                let mut previous: &[(String, DocumentKind)] = &[];
                for project in &projects {
                    check_files(workbench, project, &|index| {
                        previous.get(index) != project.get(index)
                    });
                    previous = project;
                }
            }))
        };
        if check(workbench).is_ok() {
            continue;
        }
        replays.push(match check(&mut Workbench::default()) {
            Err(_) => format!("    {}", replay_command(seed)),
            Ok(()) => {
                eprintln!("seed {seed} fails only after the sessions before it");
                format!(
                    "    FUZZ_ITERS={} {}",
                    seed - *first_seed + 1,
                    replay_command(*first_seed)
                )
            }
        });
        bench = None;
        if replays.len() == FAILURES_PER_RUN {
            break;
        }
    }
    assert!(
        replays.is_empty(),
        "{} session(s) failed; replay with\n{}",
        replays.len(),
        replays.join("\n")
    );
}

/// A project's files in project order.
pub type Project = Vec<(String, DocumentKind)>;

const WORKBENCH_SESSIONS: u64 = 256;

/// Runs every feature over a single package file on a database built for this call. A
/// coverage-guided target's input is its whole reproducer, so it has no history.
pub fn check_ide_invariants(source: &str) {
    check_project(
        &mut Workbench::default(),
        &[(source.to_owned(), DocumentKind::Package)],
    );
}

/// Runs every feature in every file of the project `sources`, in project order, then renames
/// one name and checks that the project means the same. Nothing may panic, and a panic prints
/// the project, since a feature's own panic message does not name its input.
pub fn check_project(workbench: &mut Workbench, sources: &[(String, DocumentKind)]) {
    check_files(workbench, sources, &|_| true);
}

fn check_files(
    workbench: &mut Workbench,
    sources: &[(String, DocumentKind)],
    swept: &dyn Fn(usize) -> bool,
) {
    let checked = panic::catch_unwind(AssertUnwindSafe(|| {
        let files = set_project(workbench, sources);
        let project = ProjectFiles::get(&workbench.db);
        for (index, &file) in files.iter().enumerate() {
            if swept(index) {
                check_file(&workbench.db, project, file);
            }
        }
        check_rename_keeps_meaning(workbench, sources);
    }));
    if let Err(payload) = checked {
        for (index, (text, kind)) in sources.iter().enumerate() {
            eprintln!("---- file {index} ({kind:?})\n{text}");
        }
        panic::resume_unwind(payload);
    }
}

/// Moves the database to the project as a host would, setting only what differs.
fn set_project(workbench: &mut Workbench, sources: &[(String, DocumentKind)]) -> Vec<SourceFile> {
    let db = &mut workbench.db;
    let mut files = Vec::with_capacity(sources.len());
    for (index, (text, kind)) in sources.iter().enumerate() {
        let file = match workbench.inputs.get(index) {
            Some(&file) => {
                if file.text(db) != text {
                    file.set_text(db).to(text.clone());
                }
                if *file.kind(db) != *kind {
                    file.set_kind(db).to(*kind);
                }
                file
            }
            None => {
                let file = SourceFile::new(db, text.clone(), *kind);
                workbench.inputs.push(file);
                file
            }
        };
        files.push(file);
    }
    let project = ProjectFiles::get(db);
    if *project.files(db) != files {
        project.set_files(db).to(files.clone());
    }
    files
}

/// The most offsets one file is swept at. A long file is sampled evenly from a start its
/// length picks, so its cost stays in line with a short one's and files of different lengths
/// are asked at different places.
const OFFSETS_PER_FILE: usize = 64;

/// Runs every feature at sampled offsets of `file`. Every range must lie inside the text it
/// indexes, on character boundaries, and wherever the cursor sits on a name, every range a
/// feature returns must spell that name. The features must also agree with each other: a
/// name's references are one set whichever occurrence the cursor is on, a definition is its own
/// definition, and a hover or a selection range covers the cursor.
fn check_file(db: &RootDatabase, files: ProjectFiles, file: SourceFile) {
    let source: &str = file.text(db);
    let completion_offsets = one_offset_per_context(source);
    let mut offsets = sample_offsets(source);
    if offsets.len() > OFFSETS_PER_FILE {
        let step = offsets.len().div_ceil(OFFSETS_PER_FILE);
        offsets = offsets
            .into_iter()
            .skip(source.len() % step)
            .step_by(step)
            .collect();
    }
    // A name's occurrences are compared from each of them once, not once per offset inside.
    let mut compared: HashSet<Vec<(SourceFile, TextRange)>> = HashSet::new();
    let mut completed: HashSet<&str> = HashSet::new();
    for (index, offset) in offsets.into_iter().enumerate() {
        let offset = TextSize::from(offset as u32);
        let at = |what: &str| format!("{what} in {source:?} at {offset:?}");

        let hover = crate::hover(db, files, file, offset);
        if index % 4 == 0 {
            assert_eq!(
                hover,
                crate::hover(db, files, file, offset),
                "{}",
                at("non-deterministic hover")
            );
        }
        if let Some(hover) = &hover {
            check_range(db, file, hover.range, &at("hover"));
            // The range leaves out trailing blanks, which an unterminated string keeps in its
            // token, so the cursor may sit in them. A vertical tab is a blank to the lexer.
            let covered = hover.range.start() <= offset
                && source
                    .as_bytes()
                    .get(usize::from(hover.range.end())..usize::from(offset))
                    .is_none_or(|rest| {
                        rest.iter()
                            .all(|byte| byte.is_ascii_whitespace() || *byte == 0x0B)
                    });
            assert!(
                covered,
                "{}: the hover range {:?} does not cover the cursor",
                at("hover"),
                hover.range
            );
        }

        crate::hover_debug(db, file, offset);

        let cursor_name = name_at(source, offset);

        let definitions = crate::definition(db, files, file, offset);
        for definition in &definitions {
            check_target(db, Some(definition), &at("definition"));
        }
        // The link a hover shows is where "go to definition" leads.
        if let Some(link) = hover.as_ref().and_then(|hover| hover.definition.as_ref()) {
            let agrees = match link {
                HoverDefinition::Local { target, .. } | HoverDefinition::Global { target } => {
                    definitions.contains(&DefinitionTarget::Project(*target))
                }
                HoverDefinition::Stub {
                    declaration: Some(declaration),
                    ..
                } => definitions.contains(&DefinitionTarget::Stub(*declaration)),
                HoverDefinition::Stub {
                    declaration: None, ..
                }
                | HoverDefinition::Package(_) => definitions
                    .iter()
                    .any(|definition| matches!(definition, DefinitionTarget::Package(_))),
            };
            assert!(
                agrees,
                "{}: the hover links to {link:?} but the definition is {definitions:?}",
                at("hover")
            );
        }
        let target = crate::target_at(db, files, file, offset);
        // A by-name guess at a field's declaration is not reciprocal: the declaration's references
        // keep only the reads that provably reach it.
        let guessed_field = matches!(
            &target,
            Some(crate::Target::Field(field)) if !field.precise
        );
        let definition = match definitions.as_slice() {
            [single] if !guessed_field => Some(single.clone()),
            _ => None,
        };
        // A field no construction in the project declares leads to the call that produces its
        // receiver, which spells the function's name.
        let produced_field = matches!(
            &target,
            Some(crate::Target::Field(field)) if field.sites.is_empty()
        );
        if !produced_field {
            check_names(
                db,
                definitions
                    .iter()
                    .filter_map(|target| definition_range(Some(target))),
                cursor_name,
                &at("definition"),
            );
        }

        let references = crate::references(db, files, file, offset, true);
        for occurrence in &references {
            check_range(db, occurrence.file, occurrence.range, &at("reference"));
        }
        check_names(
            db,
            references
                .iter()
                .map(|occurrence| (occurrence.file, occurrence.range)),
            cursor_name,
            &at("reference"),
        );
        assert_eq!(
            crate::references(db, files, file, offset, false),
            references
                .iter()
                .copied()
                .filter(|occurrence| !occurrence.is_declaration)
                .collect::<Vec<_>>(),
            "{}",
            at("references without declarations are not the references minus the declarations")
        );

        let rename = crate::rename(db, files, file, offset);
        for occurrence in rename.iter().flatten() {
            check_range(db, occurrence.file, occurrence.range, &at("rename edit"));
        }
        check_names(
            db,
            rename
                .iter()
                .flatten()
                .map(|occurrence| (occurrence.file, occurrence.range)),
            cursor_name,
            &at("rename edit"),
        );
        if let Some(rename) = &rename {
            check_disjoint(rename, &at("rename edit"));
        }

        // A consistently shifted set of ranges passes every containment check yet corrupts the
        // file; the round trip catches it.
        if let Some((target_file, target_range)) = definition_range(definition.as_ref())
            && cursor_name.is_some()
        {
            if target_file == file {
                let back = crate::references(db, files, file, target_range.start(), true);
                // A data.table `..name` read is a use of `name`, so its reference covers the name
                // and leaves out the dots the cursor may sit on.
                let dots = TextSize::from(match cursor_name {
                    Some(name) if name.starts_with("..") => 2,
                    _ => 0,
                });
                assert!(
                    back.iter().any(|occurrence| {
                        occurrence.file == file
                            && occurrence.range.start() <= offset + dots
                            && offset < occurrence.range.end()
                    }),
                    "{}: definition led to {target_range:?} but its references do not come back to \
                     the cursor",
                    at("round trip")
                );
            }
            // Where a name is defined, the hover is about that name too. A data.table `..name`
            // read spells the variable `name` with a prefix the definition lacks.
            if let Some(here) = &hover
                && here.definition.is_some()
            {
                let there = crate::hover(db, files, target_file, target_range.start());
                let label = |hover: &crate::Hover| {
                    hover
                        .lines
                        .first()
                        .and_then(|line| line.split(':').next())
                        .map(|label| unquote(label.trim()).trim_start_matches("..").to_owned())
                };
                assert!(
                    there
                        .as_ref()
                        .is_some_and(|there| label(there) == label(here)),
                    "{}: the hover says {:?} and the hover at its definition {target_range:?} \
                     says {:?}",
                    at("hover"),
                    here.lines,
                    there.map(|there| there.lines)
                );
            }
            // Where a name is defined, it is its own definition.
            let again = crate::definition(db, files, target_file, target_range.start());
            assert!(
                again
                    .iter()
                    .any(|target| definition_range(Some(target))
                        == Some((target_file, target_range))),
                "{}: the definition at {target_range:?} does not lead to itself but to {again:?}",
                at("definition")
            );
        }

        // The occurrences of a name are one set, whichever of them the cursor is on.
        if cursor_name.is_some() && !guessed_field && !references.is_empty() {
            let set = occurrence_set(&references);
            if compared.insert(set.clone()) {
                for occurrence in [references.first(), references.last()]
                    .into_iter()
                    .flatten()
                {
                    // A smaller expression that ends where the occurrence starts takes the
                    // cursor there, so the occurrence is asked from its end as well.
                    let ends = [occurrence.range.start(), occurrence.range.end()];
                    let from = ends
                        .into_iter()
                        .map(|offset| crate::references(db, files, occurrence.file, offset, true))
                        .find(|from| occurrence_set(from) == set)
                        .unwrap_or_default();
                    // A read that a local write reaches on some paths only reads the global on
                    // the others, and is an occurrence of both (`backlog.md`, "A read that may
                    // be a local or a global").
                    let is_local = |file, offset| match crate::target_at(db, files, file, offset) {
                        Some(crate::Target::Slot { .. }) => Some(true),
                        Some(crate::Target::Global(_)) => Some(false),
                        _ => None,
                    };
                    let cursor = is_local(file, offset);
                    let shared = cursor.is_some()
                        && ends.into_iter().any(|end| {
                            is_local(occurrence.file, end).is_some_and(|end| Some(end) != cursor)
                        });
                    assert!(
                        occurrence_set(&from) == set || shared,
                        "{}: the references from the occurrence at {:?} are {from:?}, not \
                         {references:?}",
                        at("reference"),
                        occurrence.range
                    );
                }
            }
        }

        // Parameter spans index the rendered label, not the file.
        for signature in crate::signature_help(db, file, offset)
            .iter()
            .flat_map(|help| &help.signatures)
        {
            for parameter in &signature.parameters {
                assert!(
                    usize::from(parameter.end()) <= signature.label.len(),
                    "{}: parameter span {parameter:?} escapes label {:?}",
                    at("signature help"),
                    signature.label
                );
            }
        }

        // Completion dominates this harness's cost and depends on the syntactic context, not the
        // exact byte, so one offset per context suffices.
        if completion_offsets.contains(&offset) {
            for item in crate::completion(db, files, file, offset)
                .iter()
                .flat_map(|result| &result.items)
            {
                assert!(
                    !item.label.is_empty(),
                    "{}: empty completion label",
                    at("completion")
                );
            }
        }

        // A name the editor resolves is one it offers to whoever has typed it in full.
        // Where the name is being declared, it is the one name not worth offering, and the
        // `..` of a data.table `..name` read is no part of the name completion matches.
        let start = cursor_name.map_or(0, |name| name.as_ptr() as usize - source.as_ptr() as usize);
        // A variable written twice declares at both writes, and only the first is its definition.
        let declares = references.iter().any(|occurrence| {
            occurrence.is_declaration
                && occurrence.file == file
                && occurrence
                    .range
                    .contains_inclusive(TextSize::from(start as u32))
        });
        // The hover of a `package::name` is about `name` wherever the cursor is in it.
        let hovers_name = |hover: &crate::Hover| {
            hover.lines.first().is_some_and(|line| {
                cursor_name.is_some_and(|name| {
                    unquote(line.split(':').next().unwrap_or(line).trim()) == name
                })
            })
        };
        // A member completes from its receiver's type. Where that is unknown, the hover links to
        // the stub of the call that made the receiver, which says nothing about its members.
        let member = start
            .checked_sub(1)
            .and_then(|before| source.as_bytes().get(before))
            .is_some_and(|byte| matches!(byte, b'$' | b'@'));
        if let Some(name) = cursor_name
            && hover.as_ref().is_some_and(|hover| {
                hovers_name(hover)
                    && match hover.definition {
                        Some(HoverDefinition::Local { .. } | HoverDefinition::Global { .. }) => {
                            true
                        }
                        Some(HoverDefinition::Stub { .. }) => !member,
                        _ => false,
                    }
            })
            && !declares
            && !name.starts_with('`')
            && !name.starts_with("..")
            && completed.insert(name)
        {
            let end = start + name.len();
            let offered = crate::completion(db, files, file, TextSize::from(end as u32));
            assert!(
                offered
                    .iter()
                    .flat_map(|result| &result.items)
                    .any(|item| item.label == name),
                "{}: completion after {name:?} does not offer it but {:?}",
                at("completion"),
                offered.map(|result| {
                    result
                        .items
                        .into_iter()
                        .map(|item| item.label)
                        .take(8)
                        .collect::<Vec<_>>()
                })
            );
        }

        let type_definition = crate::type_definition(db, files, file, offset);
        check_target(db, type_definition.as_ref(), &at("type definition"));

        let selection = TextRange::new(offset, offset);
        let actions = crate::code_actions(db, file, selection);
        for action in &actions {
            for edit in &action.edits {
                check_range(db, file, edit.range, &at("code action edit"));
            }
        }

        // Each step of an expanding selection covers the cursor and the step before it.
        let selections = crate::selection_ranges(db, file, offset);
        for range in &selections {
            check_range(db, file, *range, &at("selection range"));
            assert!(
                range.start() <= offset && offset <= range.end(),
                "{}: the selection range {range:?} does not cover the cursor",
                at("selection range")
            );
        }
        for pair in selections.windows(2) {
            if let [inner, outer] = pair {
                assert!(
                    outer.contains_range(*inner) && outer != inner,
                    "{}: {outer:?} does not grow {inner:?}",
                    at("selection range")
                );
            }
        }

        if let Some(item) = crate::call_hierarchy_item(db, files, file, offset) {
            check_range(db, item.file, item.range, &at("call hierarchy item"));
            check_range(
                db,
                item.file,
                item.selection,
                &at("call hierarchy selection"),
            );
            for call in crate::incoming_calls(db, files, &item) {
                for range in call.ranges {
                    check_range(db, call.item.file, range, &at("incoming call"));
                }
            }
            for call in crate::outgoing_calls(db, files, &item) {
                for range in call.ranges {
                    check_range(db, item.file, range, &at("outgoing call"));
                }
            }
        }
    }

    // The rewrites never overlap, stay inside the text, and never turn code that parses into
    // code that does not.
    let clean = parse(source).errors().is_empty();
    let rewrites = crate::data_table_rewrites(db, file);
    for pair in rewrites.windows(2) {
        if let [first, second] = pair {
            assert!(
                first.range.end() <= second.range.start(),
                "overlapping data.table rewrites in {source:?}: {:?} and {:?}",
                first.range,
                second.range
            );
        }
    }
    for rewrite in &rewrites {
        check_range(db, file, rewrite.range, "data.table rewrite");
    }
    if !rewrites.is_empty() && clean {
        let migrated = crate::apply_data_table_rewrites(source, &rewrites);
        assert!(
            parse(&migrated).errors().is_empty(),
            "the data.table rewrites of {source:?} do not parse: {migrated:?}"
        );
    }

    let hints = crate::inlay_hints(db, file, None);
    for hint in &hints {
        assert!(
            source.is_char_boundary(usize::from(hint.offset)),
            "inlay hint offset {:?} is not a position in {source:?}",
            hint.offset
        );
    }
    for symbol in crate::document_symbols(db, file) {
        check_symbol(db, file, &symbol);
    }
    for symbol in crate::workspace_symbols(db, files, "a") {
        check_range(
            db,
            symbol.target.file,
            symbol.target.range,
            "workspace symbol",
        );
    }

    // Highlighting covers each byte at most once, in source order.
    let tokens = crate::semantic_tokens(db, file);
    for (range, _) in &tokens {
        check_range(db, file, *range, "semantic token");
    }
    for pair in tokens.windows(2) {
        if let [(first, _), (second, _)] = pair {
            assert!(
                first.end() <= second.start(),
                "semantic tokens {first:?} and {second:?} overlap or are out of order in \
                 {source:?}"
            );
        }
    }

    let line_count = source.split('\n').count() as u32;
    for fold in crate::folding_ranges(db, file) {
        assert!(
            fold.start_line < fold.end_line && fold.end_line < line_count,
            "the fold {fold:?} is not a run of lines of {source:?}"
        );
    }
    for lens in crate::code_lenses(db, file) {
        check_range(db, file, lens.range, "code lens");
    }

    // An annotation the editor offers to write must leave the file parsing.
    for annotation in crate::inferred_annotations(db, file) {
        check_range(db, file, annotation.range, "inferred annotation");
        let annotated = apply_edits(db, file, &annotation.edits, "inferred annotation");
        if clean {
            let errors = parse(&annotated).errors().to_vec();
            assert!(
                errors.is_empty(),
                "the inferred annotation of `{}` leaves {errors:?} in\n{annotated}",
                annotation.name
            );
        }
    }

    // A quick fix leaves the file parsing, alone and with the other safe ones.
    let diagnostics = file_diagnostics(db, file);
    let everywhere = TextRange::new(TextSize::from(0), TextSize::from(source.len() as u32));
    for action in crate::diagnostic_fixes(&diagnostics, everywhere) {
        let fixed = apply_edits(db, file, &action.edits, &action.title);
        if clean {
            let errors = parse(&fixed).errors().to_vec();
            assert!(
                errors.is_empty(),
                "the fix {:?} leaves {errors:?} in\n{fixed}",
                action.title
            );
        }
    }
    if let Some(action) = crate::fix_all(&diagnostics) {
        let fixed = apply_edits(db, file, &action.edits, &action.title);
        if clean {
            let errors = parse(&fixed).errors().to_vec();
            assert!(
                errors.is_empty(),
                "applying all safe fixes leaves {errors:?} in\n{fixed}"
            );
        }
    }
}

/// The text of `file` after `edits`, which must lie in it on character boundaries without
/// overlapping.
fn apply_edits(db: &RootDatabase, file: SourceFile, edits: &[TextEdit], what: &str) -> String {
    let mut edits: Vec<&TextEdit> = edits.iter().collect();
    edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
    for pair in edits.windows(2) {
        if let [first, second] = pair {
            assert!(
                first.range.end() <= second.range.start(),
                "{what}: overlapping edits {first:?} and {second:?}"
            );
        }
    }
    let mut text = file.text(db).to_owned();
    for edit in edits.iter().rev() {
        check_range(db, file, edit.range, what);
        text.replace_range(
            usize::from(edit.range.start())..usize::from(edit.range.end()),
            &edit.replacement,
        );
    }
    text
}

fn occurrence_set(occurrences: &[Occurrence]) -> Vec<(SourceFile, TextRange)> {
    let mut set: Vec<(SourceFile, TextRange)> = occurrences
        .iter()
        .map(|occurrence| (occurrence.file, occurrence.range))
        .collect();
    // The file's id only groups a file's ranges together; nothing prints in this order.
    set.sort_by_key(|(file, range)| (file.as_id().index(), range.start(), range.end()));
    set.dedup();
    set
}

fn check_disjoint(occurrences: &[Occurrence], what: &str) {
    let set = occurrence_set(occurrences);
    for pair in set.windows(2) {
        if let [(first_file, first), (second_file, second)] = pair {
            assert!(
                first_file != second_file || first.end() <= second.start(),
                "{what}: {first:?} and {second:?} overlap"
            );
        }
    }
}

/// The names a rename is tried on in one project.
const RENAMES_PER_PROJECT: usize = 2;

/// Renaming a name to one the project does not use changes no meaning: every file still
/// parses, no file gains a finding, and the renamed name has as many occurrences as before.
/// Only a project that parses is asked, since broken code has no meaning to keep.
fn check_rename_keeps_meaning(workbench: &mut Workbench, sources: &[(String, DocumentKind)]) {
    if sources
        .iter()
        .any(|(text, _)| !parse(text).errors().is_empty())
    {
        return;
    }
    let files: Vec<SourceFile> = workbench
        .inputs
        .iter()
        .copied()
        .take(sources.len())
        .collect();
    let project = ProjectFiles::get(&workbench.db);
    let findings_before = project_findings(&workbench.db, &files);

    // One candidate per distinct name, spread over the project by a stride.
    let mut candidates: Vec<(usize, TextSize, &str)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (index, (text, _)) in sources.iter().enumerate() {
        let mut start = 0usize;
        for token in lex(text).0 {
            let end = start + usize::from(token.len);
            if token.kind == SyntaxKind::IDENT
                && let Some(name) = text.get(start..end)
                && seen.insert(name)
            {
                candidates.push((index, TextSize::from(start as u32), name));
            }
            start = end;
        }
    }
    let stride = candidates.len().div_ceil(RENAMES_PER_PROJECT).max(1);
    for (index, offset, name) in candidates.into_iter().step_by(stride) {
        let Some(occurrences) = crate::rename(&workbench.db, project, files[index], offset) else {
            continue;
        };
        let name = unquote(name);
        let bare = name.trim_start_matches(['.', '_']);
        // A dot inside a name may make it an S3 method, whose name is what dispatch finds it by,
        // and a generic's methods keep their names when it is renamed (`backlog.md`, "Rename of
        // an S3 generic leaves its methods behind").
        // A name the library binds too reads as the library's value ahead of a script's first
        // write to it and in every file that does not see the script, and rename does not tell
        // those reads apart (`backlog.md`, "Rename takes a script's reads of a library name it
        // later shadows" and "Editor targets ignore which files see a global").
        // An operator is spelled between percent signs wherever it is used, and rename does not
        // reach those uses (`backlog.md`, "Rename leaves the uses of a `%op%` behind").
        // The checker reads rlang's `is_*` type predicates as type tests by their names, whoever
        // defines them, so a renamed one stops narrowing its argument.
        let method_prefix = format!("{name}.");
        if bare.contains('.')
            || bare.starts_with('%')
            || bare.starts_with("is_")
            || crate::stub_target(&workbench.db, name).is_some()
            || !crate::candidate_packages(&workbench.db, name).is_empty()
            || sources
                .iter()
                .any(|(text, _)| text.contains(&method_prefix))
        {
            continue;
        }
        // A record or column name is renamed where its value is built and where reads trace back
        // to that value. A second value of the same shape keeps its spelling, so the checker may
        // find the two apart afterwards (`backlog.md`, "Rename of a field follows one value").
        let field = matches!(
            crate::target_at(&workbench.db, project, files[index], offset),
            Some(crate::Target::Field(_))
        );
        // The new name keeps what the old one's spelling means: the case that tells a tag or a
        // type from a value, and the leading dot or underscore that marks a name as private or
        // deliberately unused.
        let prefix = &name[..name.len() - bare.len()];
        let stem = match bare.chars().next() {
            Some(first) if first.is_uppercase() => "Zq",
            _ => "zq",
        };
        let fresh = (0..)
            .map(|number| format!("{prefix}{stem}{number}"))
            .find(|fresh| {
                sources
                    .iter()
                    .all(|(text, _)| !text.contains(fresh.as_str()))
            })
            .unwrap_or_default();

        let mut edits: BTreeMap<usize, Vec<TextRange>> = BTreeMap::new();
        for (file, range) in occurrence_set(&occurrences) {
            if let Some(position) = files.iter().position(|known| *known == file) {
                edits.entry(position).or_default().push(range);
            }
        }
        let mut renamed: Vec<(String, DocumentKind)> = sources.to_vec();
        let mut cursor = offset;
        for (position, ranges) in &edits {
            // The cursor's occurrence moves by what the occurrences before it grow.
            let mut grown = 0i64;
            for range in ranges.iter().rev() {
                let span = usize::from(range.start())..usize::from(range.end());
                // An occurrence written in backticks or quotes keeps them.
                let quote = renamed[*position].0[span.clone()]
                    .chars()
                    .next()
                    .filter(|first| matches!(first, '`' | '"' | '\''));
                let replacement = match quote {
                    Some(quote) => format!("{quote}{fresh}{quote}"),
                    None => fresh.clone(),
                };
                if range.end() <= offset {
                    grown += replacement.len() as i64 - span.len() as i64;
                }
                renamed[*position].0.replace_range(span, &replacement);
            }
            if *position == index {
                cursor = TextSize::from((i64::from(u32::from(offset)) + grown) as u32);
            }
        }
        let what = format!("renaming `{name}` at {offset:?} of file {index} to `{fresh}`");
        for (text, _) in &renamed {
            let errors = parse(text).errors().to_vec();
            assert!(errors.is_empty(), "{what} leaves {errors:?} in\n{text}");
        }
        set_project(workbench, &renamed);
        let findings_after = project_findings(&workbench.db, &files);
        // A finding that hinges on the old spelling, such as a suggested correction, may go.
        let added = findings_after
            .iter()
            .zip(&findings_before)
            .any(|(after, before)| {
                let mut before = before.clone();
                after
                    .iter()
                    .any(|code| match before.iter().position(|known| known == code) {
                        Some(position) => {
                            before.swap_remove(position);
                            false
                        }
                        None => true,
                    })
            });
        assert!(
            field || !added,
            "{what} adds findings: {findings_before:?} became {findings_after:?}\n{}",
            renamed
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n----\n")
        );
        let again = crate::references(&workbench.db, project, files[index], cursor, true);
        assert!(
            occurrence_set(&again).len() == occurrence_set(&occurrences).len(),
            "{what}: the new name has {} occurrences, the old one had {}",
            occurrence_set(&again).len(),
            occurrence_set(&occurrences).len()
        );
        set_project(workbench, sources);
    }
}

/// Each file's findings by code, without ranges and messages, which a rename moves and respells.
fn project_findings(db: &RootDatabase, files: &[SourceFile]) -> Vec<Vec<&'static str>> {
    files
        .iter()
        .map(|file| {
            let mut codes: Vec<&'static str> = file_diagnostics(db, *file)
                .into_iter()
                .map(|diagnostic| diagnostic.code)
                .collect();
            codes.sort_unstable();
            codes
        })
        .collect()
}

/// Token boundaries are where position decisions happen; the stride keeps off-boundary and
/// mid-character offsets without paying for every byte.
fn sample_offsets(source: &str) -> Vec<usize> {
    const STRIDE: usize = 7;
    let mut offsets: Vec<usize> = token_boundaries(source)
        .iter()
        .map(|&offset| usize::from(offset))
        .collect();
    offsets.extend((0..=source.len()).step_by(STRIDE));
    offsets.push(source.len());
    offsets.sort_unstable();
    offsets.dedup();
    offsets.retain(|&offset| offset <= source.len());
    offsets
}

fn token_boundaries(source: &str) -> BTreeSet<TextSize> {
    let mut offsets = BTreeSet::from([TextSize::from(0)]);
    let mut at = 0usize;
    for token in lex(source).0 {
        at += usize::from(token.len);
        offsets.insert(TextSize::from(at.min(source.len()) as u32));
    }
    offsets
}

/// The kind of token the cursor sits after decides what completion may offer.
fn one_offset_per_context(source: &str) -> BTreeSet<TextSize> {
    let mut seen = HashSet::new();
    let mut offsets = BTreeSet::from([TextSize::from(0), TextSize::from(source.len() as u32)]);
    let mut at = 0usize;
    for token in lex(source).0 {
        at += usize::from(token.len);
        if seen.insert(token.kind) {
            offsets.insert(TextSize::from(at.min(source.len()) as u32));
        }
    }
    offsets
}

fn name_at(source: &str, offset: TextSize) -> Option<&str> {
    let offset = usize::from(offset);
    let mut start = 0usize;
    for token in lex(source).0 {
        let end = start + usize::from(token.len);
        // A backtick-quoted name lexes as IDENT, backticks included. One whose closing backtick
        // is missing runs to the end of its line and names nothing.
        if (start..end).contains(&offset) && token.kind == SyntaxKind::IDENT {
            return source
                .get(start..end)
                .filter(|name| !name.starts_with('`') || (name.len() > 1 && name.ends_with('`')));
        }
        start = end;
    }
    None
}

/// A range shifted by one byte is in bounds and self-consistent yet destroys code, so each range
/// must spell the cursor's name. Quotes are normalized: `` `x` `` and `x` name one binding, and
/// so does the string in `"x" <- 1`.
fn check_names(
    db: &RootDatabase,
    ranges: impl IntoIterator<Item = (SourceFile, TextRange)>,
    cursor_name: Option<&str>,
    what: &str,
) {
    let Some(cursor_name) = cursor_name.map(unquote) else {
        return;
    };
    for (file, range) in ranges {
        let text = file.text(db);
        let Some(spelled) = text.get(usize::from(range.start())..usize::from(range.end())) else {
            continue;
        };
        // A rest pattern (`..rest`) lexes as one name and binds the part after the dots.
        let spelled = unquote(spelled);
        assert!(
            spelled == cursor_name || cursor_name.strip_prefix("..") == Some(spelled),
            "{what}: range {range:?} covers {spelled:?}, not the name under the cursor \
             ({cursor_name:?})"
        );
    }
}

fn unquote(name: &str) -> &str {
    ['`', '"', '\'']
        .into_iter()
        .find_map(|quote| name.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(name)
}

fn definition_range(target: Option<&DefinitionTarget>) -> Option<(SourceFile, TextRange)> {
    match target {
        Some(DefinitionTarget::Project(navigation)) => Some((navigation.file, navigation.range)),
        _ => None,
    }
}

fn check_range(db: &RootDatabase, file: SourceFile, range: TextRange, what: &str) {
    let text: &str = file.text(db);
    assert!(
        text.is_char_boundary(usize::from(range.start()))
            && text.is_char_boundary(usize::from(range.end())),
        "{what}: range {range:?} does not lie between characters of its file (length {})",
        text.len()
    );
}

fn check_target(db: &RootDatabase, target: Option<&DefinitionTarget>, what: &str) {
    // A stub target's range indexes a stub source, not a project file.
    if let Some(DefinitionTarget::Project(navigation)) = target {
        check_range(db, navigation.file, navigation.range, what);
    }
}

fn check_symbol(db: &RootDatabase, file: SourceFile, symbol: &DocumentSymbol) {
    check_range(db, file, symbol.range, "document symbol");
    check_range(db, file, symbol.selection, "document symbol selection");
    for child in &symbol.children {
        check_symbol(db, file, child);
    }
}

/// Runs every refactoring at sampled selections of `source` and applies each one: the edits
/// must lie inside the text without overlapping, a file that parsed cleanly must still parse
/// cleanly and may report no unresolved name the original did not. Shared
/// with the coverage-guided `ide` target.
pub fn check_refactor_invariants(source: &str) {
    let (db, file) = refactor_database(source);
    let clean = parse(source).errors().is_empty();
    let unresolved_before = unresolved_names(&db, file, source);
    for selection in refactor_selections(source) {
        let actions = crate::refactors(&db, file, selection);
        assert_eq!(
            actions,
            crate::refactors(&db, file, selection),
            "refactors are not deterministic in {source:?} at {selection:?}"
        );
        for action in actions {
            let at = format!("`{}` in {source:?} at {selection:?}", action.title);
            let mut edits = action.edits.clone();
            edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
            for pair in edits.windows(2) {
                if let [first, second] = pair {
                    assert!(
                        first.range.end() <= second.range.start()
                            && first.range.start() != second.range.start(),
                        "{at}: overlapping edits {edits:?}"
                    );
                }
            }
            let mut edited = source.to_owned();
            for edit in edits.iter().rev() {
                let range = usize::from(edit.range.start())..usize::from(edit.range.end());
                assert!(
                    edited.get(range.clone()).is_some(),
                    "{at}: edit {edit:?} outside the text"
                );
                edited.replace_range(range, &edit.replacement);
            }
            // Broken input has no meaning to preserve, so only the edits' shape is checked.
            if !clean {
                continue;
            }
            let errors = parse(&edited).errors().to_vec();
            assert!(
                errors.is_empty(),
                "{at}: the result no longer parses: {errors:?}\n{edited}"
            );
            let (after, edited_file) = refactor_database(&edited);
            let unresolved_after = unresolved_names(&after, edited_file, &edited);
            let introduced: Vec<&String> =
                unresolved_after.difference(&unresolved_before).collect();
            assert!(
                introduced.is_empty(),
                "{at}: the result leaves {introduced:?} unresolved\n{edited}"
            );
        }
    }
}

/// Without stubs every library name is unresolved before and after alike, so the comparison
/// still holds and each check stays cheap.
fn refactor_database(source: &str) -> (RootDatabase, SourceFile) {
    let db = RootDatabase::default();
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);
    (db, file)
}

fn unresolved_names(db: &RootDatabase, file: SourceFile, source: &str) -> BTreeSet<String> {
    file_diagnostics(db, file)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == "unresolved")
        .filter_map(|diagnostic| {
            source
                .get(usize::from(diagnostic.range.start())..usize::from(diagnostic.range.end()))
                .map(str::to_owned)
        })
        .collect()
}

/// Every node's range, runs of up to three neighboring statements, and the start of every name
/// and annotation, which is where the cursor-driven refactorings look.
fn refactor_selections(source: &str) -> Vec<TextRange> {
    let root = parse(source).syntax_node();
    let mut selections = Vec::new();
    for node in root.descendants() {
        let range = node.text_range();
        if !range.is_empty() {
            selections.push(range);
        }
        if node.kind() == SyntaxKind::ANNOTATION {
            selections.push(TextRange::empty(range.start()));
        }
        let statements: Vec<TextRange> = node
            .children()
            .filter(|_| {
                matches!(
                    node.kind(),
                    SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
                )
            })
            .map(|child| child.text_range())
            .collect();
        for width in 2..=3 {
            for run in statements.windows(width) {
                if let (Some(first), Some(last)) = (run.first(), run.last()) {
                    selections.push(TextRange::new(first.start(), last.end()));
                }
            }
        }
    }
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.kind() == SyntaxKind::IDENT {
            selections.push(TextRange::empty(token.text_range().start()));
        }
    }
    selections.sort_by_key(|range| (range.start(), range.end()));
    selections.dedup();
    selections
}
