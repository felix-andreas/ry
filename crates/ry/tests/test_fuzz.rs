//! The config loader, the position conversions and the reader of an executable's payload under
//! their exported contracts (`ry::config::check_config_invariants`,
//! `ry::position::check_position_invariants`, `ry::executable::testing`), over seeds, mutations
//! of them, and random bytes, and the editor's two diagnostic waves over generated sessions.
//! Each generated case is one seed of [`for_each_seed_on_named_threads`], so `FUZZ_SEED`
//! replays a failure and `FUZZ_ITERS` sets the budget.

use {
    ry::{
        config::{Config, check_config_invariants},
        diagnostics::{apply_suppressions, document_diagnostics, first_wave_diagnostics},
        executable::{self, testing as executable_testing},
        position::check_position_invariants,
    },
    salsa::Setter as _,
    semantics::{
        AnalysisSettings, ProjectFiles, RootDatabase, SourceFile,
        diagnostics::Diagnostic,
        lints::{LintLevel, NameStyle},
        stubs::install_shipped_stubs,
        testing::{ProjectState, generate_session},
    },
    syntax::testing::{
        SplitMix64, fixture_case_sources, for_each_seed_on_named_threads, mutate_seed,
        program_corpus_sources, random_bytes, split_files, token_soup,
    },
};

const CONFIG_SEEDS: &[&str] = &[
    "",
    "[format]\nindent-width = 4\nline-width = 100\n",
    "[check]\ntyping = true\nstrict = true\nexclude = [\"scripts/\", \"!scripts/keep.R\"]\n",
    "[lint]\nnaming-style = \"snake_case\"\n",
    "case = \"camelCase\"\nspaces = 2\n",
    "typing = true\n",
    "[check]\ninline-annotations = false\nobject-systems = false\n",
    "[format]\nindent-width = \"four\"\n",
    "[check\nexclude = [\"[\"]\n",
    "a.b.c = 1\n[x.y]\nz = { w = [1, 2] }\n",
];

const CONFIG_ALPHABET: &[&str] = &[
    "[",
    "]",
    "[[",
    "]]",
    "=",
    "\"",
    "'",
    "\n",
    "\r\n",
    "#",
    ".",
    ",",
    "{",
    "}",
    "format",
    "lint",
    "check",
    "exclude",
    "typing",
    "strict",
    "indent-width",
    "line-width",
    "true",
    "false",
    "1",
    "-1",
    "1e400",
    "\"**\"",
    "\"[\"",
    "\"!\"",
    "é",
    "😀",
];

const POSITION_SEEDS: &[&str] = &[
    "",
    "\n",
    "abc\ndef\n",
    "x <- \"café\" ; y = 2L\n",
    "é😀x\n",
    "a\r\nb\r\n",
    "\r\r\n\n",
    "😀\n😀😀\r\n\u{feff}z",
];

/// Inputs the coverage-guided `config` and `position` targets have broken.
const CONFIG_REGRESSIONS: &[&str] = &[];
const POSITION_REGRESSIONS: &[&str] = &[];

#[test]
fn config_seeds_hold_invariants() {
    for input in CONFIG_SEEDS.iter().chain(CONFIG_REGRESSIONS) {
        check_config_invariants(input);
    }
}

#[test]
fn fuzz_config() {
    for_each_seed_on_named_threads(2000, |seed| {
        let mut rng = SplitMix64(0xc0f1_9000 ^ seed);
        let input = match rng.below(3) {
            0 => random_bytes(&mut rng),
            1 => token_soup(&mut rng, CONFIG_ALPHABET),
            _ => mutate_seed(&mut rng, CONFIG_SEEDS, 24),
        };
        check_config_invariants(&input);
    });
}

#[test]
fn position_seeds_hold_invariants() {
    for input in POSITION_SEEDS.iter().chain(POSITION_REGRESSIONS) {
        check_position_invariants(input);
    }
}

#[test]
fn fuzz_positions() {
    for_each_seed_on_named_threads(2000, |seed| {
        let mut rng = SplitMix64(0x000f_f5e7 ^ seed);
        let input = match rng.below(2) {
            0 => random_bytes(&mut rng),
            _ => mutate_seed(&mut rng, POSITION_SEEDS, 8),
        };
        check_position_invariants(&input);
    });
}

#[test]
fn fuzz_executable_payloads() {
    for_each_seed_on_named_threads(2000, |seed| {
        let mut rng = SplitMix64(0x0e8e_c0de ^ seed);
        let bytes = random_bytes(&mut rng).into_bytes();
        executable_testing::check_round_trip(&bytes);
        // A payload with some of its bytes changed: the reader must find it damaged, find none,
        // or find one that lies inside the file, and never panic.
        let mut file = bytes.clone();
        let metadata = executable::Metadata {
            flags: 64,
            script: String::from("main.R"),
            source: bytes.clone(),
            runtime: None,
            base: None,
        };
        executable::append(&mut file, bytes.len() as u64, &bytes, &metadata);
        let tail = file.len().saturating_sub(96);
        for _ in 0..=rng.below(4) {
            let at = tail + rng.below(file.len() - tail);
            if let Some(byte) = file.get_mut(at) {
                *byte = rng.next_u64() as u8;
            }
        }
        executable_testing::check_parse_input(&file);
        executable_testing::check_parse_input(&bytes);
    });
}

/// The editor publishes a first wave of diagnostics on every keystroke and the settled wave
/// once analysis catches up, so a finding of the first wave that the settled wave lacks shows
/// and then vanishes. For the same text and configuration, every finding of the first wave,
/// after suppressions, is one of the settled wave. Both waves lint with the same
/// configuration, so the relation checks the parse-stage findings and the suppressions; the
/// lints are one computation on both sides. Each seed is a generated session under a random
/// configuration, on one database that stays warm across seeds as an editor's does. A seed
/// that fails runs again on a fresh database, and the failure says which of the two failed.
#[test]
fn fuzz_first_wave_is_a_subset_of_the_settled_wave() {
    let pool = wave_pool();
    let mut db = wave_database();
    for_each_seed_on_named_threads(WAVE_SESSIONS, |seed| {
        let (states, config) = wave_session(seed, &pool);
        let Err(warm) = check_waves(&mut db, &states, &config) else {
            return;
        };
        match check_waves(&mut wave_database(), &states, &config) {
            Err(fresh) => panic!("{fresh}\n(on a fresh database as well as the warm one)"),
            Ok(()) => panic!(
                "{warm}\n(only on the database earlier seeds left warm; a fresh database agrees, so \
                 the incremental path keeps a stale finding)"
            ),
        }
    });
}

/// Feeds the relation a settled wave with one finding dropped, which a wave that loses a
/// finding would show, and asserts that the default seeds catch it.
#[test]
fn first_wave_oracle_catches_a_dropped_settled_finding() {
    let pool = wave_pool();
    let mut db = wave_database();
    let mut caught = 0;
    for_each_seed_on_named_threads(WAVE_SESSIONS, |seed| {
        let (states, mut config) = wave_session(seed, &pool);
        let state = &states[0];
        let files = set_wave_state(&mut db, state, &mut config);
        for (file, (_, text, _)) in files.iter().zip(&state.files) {
            let first = apply_suppressions(first_wave_diagnostics(&db, *file, &config), text);
            let mut settled = apply_suppressions(document_diagnostics(&db, *file, &config), text);
            if let Some(index) = settled
                .iter()
                .position(|diagnostic| first.contains(diagnostic))
            {
                settled.remove(index);
            }
            caught += usize::from(!first_wave_lost(&first, &settled).is_empty());
        }
    });
    assert!(
        caught > 0,
        "no default seed caught a dropped settled finding"
    );
}

/// The default budget of the two-wave relation and its canary.
const WAVE_SESSIONS: usize = 300;

/// The findings of the first wave that the settled wave lacks.
fn first_wave_lost<'a>(first: &'a [Diagnostic], settled: &[Diagnostic]) -> Vec<&'a Diagnostic> {
    first
        .iter()
        .filter(|diagnostic| !settled.contains(diagnostic))
        .collect()
}

/// Checks the two-wave relation on every state of a session, or says where it fails.
fn check_waves(
    db: &mut RootDatabase,
    states: &[ProjectState],
    config: &Config,
) -> Result<(), String> {
    let mut config = config.clone();
    for state in states {
        let files = set_wave_state(db, state, &mut config);
        for (file, (key, text, _)) in files.iter().zip(&state.files) {
            let first = apply_suppressions(first_wave_diagnostics(db, *file, &config), text);
            let settled = apply_suppressions(document_diagnostics(db, *file, &config), text);
            let lost = first_wave_lost(&first, &settled);
            if !lost.is_empty() {
                return Err(format!(
                    "the settled wave of file {key} lacks findings of the first wave\n{text}\n---- lost:\n{lost:#?}\n---- settled:\n{settled:#?}"
                ));
            }
        }
    }
    Ok(())
}

/// The sources sessions draw files from: the fixture cases and the program corpus.
fn wave_pool() -> Vec<String> {
    let mut pool: Vec<String> = Vec::new();
    for (_, source) in fixture_case_sources() {
        match split_files(&source) {
            Some(files) => pool.extend(files.into_iter().map(|(_, text)| text)),
            None => pool.push(source),
        }
    }
    pool.extend(
        program_corpus_sources()
            .into_iter()
            .map(|(_, source)| source),
    );
    pool
}

fn wave_database() -> RootDatabase {
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    AnalysisSettings::new(&db, true, false, true);
    ProjectFiles::new(&db, Vec::new());
    db
}

/// The session and the configuration a seed draws.
fn wave_session(seed: u64, pool: &[String]) -> (Vec<ProjectState>, Config) {
    let mut rng = SplitMix64(0x2_FA7E_5E75 ^ seed);
    let states = generate_session(&mut rng, pool);
    let mut config = Config::default();
    config.check.unused = rng.chance(1, 2);
    config.check.maybe_undefined = rng.chance(1, 2);
    config.check.typing = rng.chance(1, 2);
    config.check.inline_annotations = rng.below(8) != 0;
    config.lint.naming_style = [None, Some(NameStyle::Snake)][rng.below(2)];
    config.lint.unused_parameter = [LintLevel::Off, LintLevel::Warn][rng.below(2)];
    config.lint.shadows_builtin = [LintLevel::Off, LintLevel::Warn][rng.below(2)];
    (states, config)
}

/// Sets `state` as the database's inputs, with the settings `config` holds and the ones the
/// state decides, which it writes into `config`.
fn set_wave_state(
    db: &mut RootDatabase,
    state: &ProjectState,
    config: &mut Config,
) -> Vec<SourceFile> {
    config.check.strict = state.strict;
    config.check.prelude = state.prelude;
    let settings = AnalysisSettings::get(db);
    settings.set_strict(db).to(state.strict);
    settings.set_prelude(db).to(state.prelude);
    settings
        .set_inline_annotations(db)
        .to(config.check.inline_annotations);
    let files: Vec<SourceFile> = state
        .files
        .iter()
        .map(|(_, text, kind)| SourceFile::new(db, text.clone(), *kind))
        .collect();
    ProjectFiles::get(db).set_files(db).to(files.clone());
    files
}
