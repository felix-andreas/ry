use std::fs;
use std::path::Path;
use std::process::Command;

/// One finished `ry` invocation, its streams decoded.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ry(directory: &Path, arguments: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_ry"))
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("failed to run the ry binary");
    Run {
        code: output.status.code().expect("terminated by signal"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("failed to create a temporary directory");
    for (name, content) in files {
        let path = directory.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create test directories");
        }
        fs::write(path, content).expect("failed to write a test file");
    }
    directory
}

// The unclosed `(` is reported at 1-based line 2, column 6.
const SYNTAX_ERROR_SOURCE: &str = "print(1)\ny <- (\n";

/// Stderr is a pipe under test, so the ASCII theme is in force: `x` for an
/// error, `!` for a warning.
fn heading(code: &str, marker: char, message: &str) -> String {
    format!("{code}\n\n  {marker} {message}")
}

#[test]
fn check_clean_file_exits_zero() {
    let directory = project(&[("clean.R", "x <- 1\nprint(x)\n")]);
    let output = ry(directory.path(), &["check", "clean.R"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
}

#[test]
fn check_renders_a_syntax_error_as_a_one_based_snippet() {
    let directory = project(&[("bad.R", SYNTAX_ERROR_SOURCE)]);
    let output = ry(directory.path(), &["check", "bad.R"]);
    let stderr = &output.stderr;

    assert_eq!(output.code, 1, "stderr: {stderr}");
    assert!(
        stderr.starts_with("syntax-error\n"),
        "expected the diagnostic code to head the report, got: {stderr}"
    );
    assert!(
        stderr.contains("   --[bad.R:2:6]\n 1 | print(1)\n 2 | y <- (\n   |      ^\n"),
        "expected a 1-based snippet window with a caret underline, got: {stderr}"
    );
    assert!(
        !stderr.contains("`----"),
        "expected no closing rule under the snippet, got: {stderr}"
    );
    let message = stderr
        .lines()
        .nth(2)
        .and_then(|line| line.strip_prefix("  x "))
        .unwrap_or_else(|| panic!("expected an error message under its code first, got: {stderr}"));
    assert_eq!(
        stderr.matches(message).count(),
        1,
        "expected the message not to repeat under the caret, got: {stderr}"
    );
}

#[test]
fn check_snippet_shows_only_the_lines_around_the_finding() {
    let directory = project(&[("bad.R", "one <- 1\nprint(one)\nprint(one)\ny <- (\n")]);
    let output = ry(directory.path(), &["check", "bad.R"]);
    let stderr = &output.stderr;

    assert!(
        stderr.contains("4 | y <- (") && stderr.contains("3 | print(one)"),
        "expected the finding's line and the one above it, got: {stderr}"
    );
    assert!(
        !stderr.contains("one <- 1"),
        "expected no line further away than that, got: {stderr}"
    );
}

#[test]
fn check_clamps_a_long_multi_line_range() {
    let directory = project(&[(
        "long.R",
        "# typing: on\nx <- 1L + \"alpha\nbeta\ngamma\ndelta\"\nprint(x)\n",
    )]);
    let output = ry(directory.path(), &["check", "long.R"]);
    let rendered = &output.stderr;

    assert!(
        rendered.contains("the range continues for 3 more lines"),
        "expected the range's reach to be stated, got: {rendered}"
    );
    assert!(
        !rendered.contains("gamma"),
        "expected the snippet to stop after the range's first line, got: {rendered}"
    );
}

#[test]
fn check_counts_warnings_as_findings() {
    let directory = project(&[("warn.R", "x = 1\n")]);
    let output = ry(directory.path(), &["check", "warn.R"]);
    let stderr = &output.stderr;

    assert!(
        stderr.contains(&heading("unused", '!', "`x` is assigned but never used.")),
        "expected a code-carrying warning diagnostic, got: {stderr}"
    );
    assert_eq!(output.code, 1, "warnings must exit 1: {stderr}");
}

#[test]
fn check_json_output_is_one_based_and_parses() {
    let directory = project(&[("bad.R", SYNTAX_ERROR_SOURCE)]);
    let output = ry(directory.path(), &["check", "--output", "json", "bad.R"]);
    let stdout = &output.stdout;

    assert_eq!(output.code, 1, "stderr: {}", output.stderr);
    assert!(
        !output.stderr.contains("[bad.R:"),
        "expected no human rendering in json mode, got: {}",
        output.stderr
    );

    let records: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("diagnostic line is not valid JSON"))
        .collect();
    assert!(!records.is_empty(), "expected JSON lines, got: {stdout}");
    let record = &records[0];

    assert!(
        record["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("bad.R")),
        "unexpected path: {record}"
    );
    assert_eq!(record["line"], 2, "line must be 1-based: {record}");
    assert_eq!(record["column"], 6, "column must be 1-based: {record}");
    assert_eq!(record["endLine"], 2, "endLine must be 1-based: {record}");
    assert_eq!(
        record["endColumn"], 7,
        "endColumn must be 1-based: {record}"
    );
    assert_eq!(record["severity"], "error", "unexpected severity: {record}");
    assert!(
        record["code"].as_str().is_some_and(|code| !code.is_empty()),
        "expected a diagnostic code: {record}"
    );
    assert!(
        record["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "expected a message: {record}"
    );
}

#[test]
fn check_json_output_maps_warning_severity() {
    let directory = project(&[("warn.R", "x = 1\nprint(x)\n")]);
    let output = ry(directory.path(), &["check", "--output", "json", "warn.R"]);

    assert_eq!(output.code, 1);
    let record: serde_json::Value =
        serde_json::from_str(output.stdout.trim()).expect("diagnostic line is not valid JSON");
    assert_eq!(record["severity"], "warning", "got: {record}");
}

#[test]
fn check_related_notes_render_on_both_surfaces() {
    let files = &[("R/a.R", "shared <- 1\n"), ("R/b.R", "shared <- 2\n")];

    let human = ry(project(files).path(), &["check", "."]);
    assert_eq!(human.code, 1, "stderr: {}", human.stderr);
    let rendered = &human.stderr;
    assert!(
        rendered.contains("> the later binding is here.")
            && rendered.contains("> the earlier binding is here."),
        "expected one note per overwrite warning, got: {rendered}"
    );
    // Each note is drawn from the file it points into.
    assert!(
        rendered.contains("[R/a.R:1:1]") && rendered.contains("[R/b.R:1:1]"),
        "expected each note to carry its own snippet, got: {rendered}"
    );

    let json = ry(project(files).path(), &["check", "--output", "json", "."]);
    assert_eq!(json.code, 1, "stderr: {}", json.stderr);
    let records: Vec<serde_json::Value> = json
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("diagnostic line is not valid JSON"))
        .collect();
    let noted = records
        .iter()
        .filter_map(|record| record["related"].as_array())
        .filter(|related| !related.is_empty())
        .count();
    assert_eq!(
        noted, 2,
        "expected both overwrite warnings to carry related info: {records:?}"
    );
    let entry = records
        .iter()
        .find_map(|record| record["related"].as_array().and_then(|array| array.first()))
        .expect("a related entry exists");
    assert!(
        entry["path"]
            .as_str()
            .is_some_and(|path| path.ends_with(".R")),
        "related path points at the sibling file: {entry}"
    );
    assert_eq!(entry["line"], 1, "related line must be 1-based: {entry}");
    assert_eq!(
        entry["column"], 1,
        "related column must be 1-based: {entry}"
    );
    assert!(
        entry["message"]
            .as_str()
            .is_some_and(|message| message.contains("binding is here")),
        "related message names the sibling binding: {entry}"
    );
}

#[test]
fn check_validates_namespace_imports_against_stubs() {
    let directory = project(&[
        ("R/main.R", "x <- 1\n"),
        (
            "NAMESPACE",
            "import(stats)\nimportFrom(stats, sd, medain)\nimportFrom(dplyr, mutate)\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains(&heading(
            "unresolved",
            'x',
            "`medain` is not exported by `stats`, so this package will not load.",
        )),
        "an import R refuses to load is an error, not advice: {rendered}"
    );
    // Backticked names are a finding's subject; the plain spelling may appear in context lines.
    assert!(
        !rendered.contains("`mutate`") && !rendered.contains("`sd`"),
        "unknown namespaces and real exports must stay quiet: {rendered}"
    );
    assert!(
        rendered.contains("NAMESPACE:2:23"),
        "expected a precise range on the NAMESPACE file, got: {rendered}"
    );
}

#[test]
fn check_flags_unused_imports_when_opted_in() {
    let files: &[(&str, &str)] = &[
        ("R/main.R", "run <- function() stats::sd(c(1, 2))\n"),
        ("NAMESPACE", "importFrom(stats, sd, median)\n"),
    ];

    let default_run = ry(project(files).path(), &["check", "."]);
    assert_eq!(default_run.code, 0, "stderr: {}", default_run.stderr);

    let opted_in = project(&[
        files[0],
        files[1],
        ("ry.toml", "[lint]\nunused-import = \"warn\"\n"),
    ]);
    let output = ry(opted_in.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("imported name `median` from `stats` is never used."),
        "expected the unused-import warning, got: {rendered}"
    );
    assert!(
        !rendered.contains("`sd`"),
        "a name used via pkg::name must not be flagged: {rendered}"
    );
}

#[test]
fn check_flags_builtin_shadows_when_opted_in() {
    let files: &[(&str, &str)] = &[("R/main.R", "mean <- function(x) x\nsd <- 1\n")];

    let default_run = ry(project(files).path(), &["check", "."]);
    assert_eq!(default_run.code, 0, "stderr: {}", default_run.stderr);

    let opted_in = project(&[
        files[0],
        (
            "ry.toml",
            "[lint]\nshadows-builtin = \"warn\"\nshadows-namespace = \"warn\"\n",
        ),
    ]);
    let output = ry(opted_in.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("Top-level binding `mean` shadows a builtin."),
        "expected the shadows-builtin warning, got: {rendered}"
    );
    assert!(
        rendered.contains("Top-level binding `sd` shadows `stats::sd`."),
        "expected the shadows-namespace warning, got: {rendered}"
    );
}

#[test]
fn check_min_severity_error_ignores_warnings() {
    let directory = project(&[("warn.R", "x = 1\n")]);
    let filtered = ry(
        directory.path(),
        &["check", "--min-severity", "error", "warn.R"],
    );
    assert_eq!(filtered.code, 0, "stderr: {}", filtered.stderr);
    assert!(
        !filtered.stderr.contains("assignment-operator"),
        "warnings must not render under --min-severity error: {}",
        filtered.stderr
    );
    assert!(
        filtered.stdout.contains("no problems"),
        "the summary counts only what passed the severity floor: {}",
        filtered.stdout
    );
}

#[test]
fn check_unknown_config_key_warns_but_starts() {
    let directory = project(&[
        ("clean.R", "x <- 1\nprint(x)\n"),
        ("ry.toml", "[check]\nstric = true\n"),
    ]);
    let output = ry(directory.path(), &["check", "clean.R"]);
    assert_eq!(
        output.code, 0,
        "an unknown key must not block the run: {}",
        output.stderr
    );
    assert!(
        output
            .stderr
            .contains("ignoring unknown config key `check.stric`"),
        "the unknown key warns visibly: {}",
        output.stderr
    );
}

#[test]
fn check_invalid_config_exits_two() {
    let directory = project(&[
        ("clean.R", "x <- 1\n"),
        ("ry.toml", "[check]\ntyping = 1\n"),
    ]);

    let check = ry(directory.path(), &["check", "clean.R"]);
    assert_eq!(check.code, 2, "stderr: {}", check.stderr);
    let rendered = &check.stderr;
    assert!(
        rendered.contains("invalid config for `check.typing`"),
        "expected the offending setting to be named, got: {rendered}"
    );
    assert!(
        rendered.contains("ry.toml:2:10") && rendered.contains("2 | typing = 1"),
        "expected the config error to be shown in place, got: {rendered}"
    );

    let fmt = ry(directory.path(), &["fmt", "--check", "clean.R"]);
    assert_eq!(fmt.code, 2, "stderr: {}", fmt.stderr);
}

#[test]
fn check_missing_target_exits_two() {
    let directory = project(&[]);
    let output = ry(directory.path(), &["check", "does-not-exist.R"]);
    assert_eq!(output.code, 2, "stderr: {}", output.stderr);
}

#[test]
fn check_reports_dropped_override_stub_declarations() {
    let directory = project(&[
        ("clean.R", "x <- 1\n"),
        ("stubs/project.Rtypes", "size : Frobnicate\n"),
    ]);
    let output = ry(directory.path(), &["check", "clean.R"]);
    let stderr = &output.stderr;
    assert_eq!(
        output.code, 1,
        "a dropped override declaration is a finding: {stderr}"
    );
    assert!(
        stderr.contains("does not load"),
        "expected the dropped declaration to be reported, got: {stderr}"
    );
    assert!(
        stderr.contains("project.Rtypes:1:1"),
        "expected a 1-based stub-file position header, got: {stderr}"
    );
}

#[test]
fn check_loads_valid_override_stubs_silently() {
    let directory = project(&[
        ("clean.R", "x <- 1\nprint(x)\n"),
        (
            "stubs/project.Rtypes",
            "my_helper : fn(x: double) -> double\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "clean.R"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
}

// Every shipped overload set ends in an `Any` catch-all, so only an override reaches this.
#[test]
fn check_reports_no_matching_overload() {
    let directory = project(&[
        ("R/main.R", "answer <- pick(\"word\")\n"),
        (
            "stubs/project.Rtypes",
            "pick : fn(x: integer) -> integer\npick : fn(x: double) -> double\n",
        ),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains(
            "no overload of `pick` matches these arguments. I tried all 2 declared signatures"
        ),
        "expected the no-overload finding, got: {rendered}"
    );
}

#[test]
fn check_override_stub_types_apply() {
    let directory = project(&[
        ("R/main.R", "answer <- my_helper(\"not a double\")\n"),
        (
            "stubs/project.Rtypes",
            "my_helper : fn(x: double) -> double\n",
        ),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("R/main.R") || rendered.contains("main.R"),
        "expected a type finding against the override stub, got: {rendered}"
    );
}

#[test]
fn check_namespace_imports_resolve_bare_reads() {
    let unimported = project(&[("R/main.R", "f <- function(x) rbindlist(x)\n")]);
    let output = ry(unimported.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("could not resolve `rbindlist`"),
        "expected the unresolved warning without an import, got: {rendered}"
    );

    let imported = project(&[
        ("R/main.R", "f <- function(x) rbindlist(x)\n"),
        ("NAMESPACE", "importFrom(data.table, rbindlist)\n"),
    ]);
    let output = ry(imported.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 0, "stderr: {rendered}");
}

#[test]
fn check_description_dependencies_quiet_namespace_reads() {
    let declared = project(&[
        ("R/main.R", "f <- function(d) dplyr::mutate(d)\n"),
        ("DESCRIPTION", "Package: demo\nImports: dplyr\n"),
    ]);
    let output = ry(declared.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 0, "stderr: {rendered}");
}

#[test]
fn check_collate_order_decides_the_package_winner() {
    // The reader errors only when the character binding wins: path order makes `b.R`
    // the later writer and Collate reverses that. The error floor hides the
    // duplicate-binding warnings that fire either way.
    let base: &[(&str, &str)] = &[
        ("R/a.R", "value <- 1L\n"),
        ("R/b.R", "value <- \"word\"\n"),
        ("R/use.R", "f <- function() value + 1L\n"),
        ("ry.toml", "[check]\ntyping = true\n"),
    ];
    let path_ordered = project(base);
    let output = ry(
        path_ordered.path(),
        &["check", ".", "--min-severity", "error"],
    );
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("expected a numeric value"),
        "expected the reader to see the character winner, got: {rendered}"
    );

    let mut with_collate = base.to_vec();
    with_collate.push(("DESCRIPTION", "Package: demo\nCollate: 'b.R' 'a.R'\n"));
    let collated = project(&with_collate);
    let output = ry(collated.path(), &["check", ".", "--min-severity", "error"]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 0, "stderr: {rendered}");
}

#[test]
fn analysis_stats_notes_when_it_forces_typing_on() {
    let directory = project(&[("R/a.R", "f <- function(x) x + 1\n")]);
    let output = ry(directory.path(), &["debug", "analysis-stats", "."]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
    assert!(
        output
            .stdout
            .contains("note: [check] typing is off in the configuration"),
        "report:\n{}",
        output.stdout
    );
}

#[test]
fn analysis_stats_rejects_a_workspace_without_r_files() {
    let directory = project(&[("README.md", "no code here\n")]);
    let output = ry(directory.path(), &["debug", "analysis-stats", "."]);
    assert_eq!(output.code, 2);
    assert!(
        output.stderr.contains("no R files"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn analysis_stats_reports_phases_and_probe() {
    let directory = project(&[
        ("R/a.R", "value <- 1L\nf <- function() value + 1L\n"),
        ("R/b.R", "g <- function(x) f() + x\n"),
    ]);
    let output = ry(directory.path(), &["debug", "analysis-stats", "."]);
    let rendered = &output.stdout;
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
    for section in [
        "cold analysis",
        "typecheck (+interfaces)",
        "slowest files (typecheck):",
        "incremental (typing burst",
    ] {
        assert!(
            rendered.contains(section),
            "expected `{section}` in the report, got: {rendered}"
        );
    }
}

#[test]
fn check_without_r_files_reports_nothing_and_exits_clean() {
    let directory = project(&[]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
    assert!(
        output.stdout.contains("0 files checked, no problems"),
        "expected the empty-target summary, got: {}",
        output.stdout
    );
}

#[test]
fn testthat_files_share_one_namespace_with_their_helpers() {
    // testthat sources `tests/testthat/` into one environment, `helper-*.R` first.
    let directory = project(&[
        (
            "DESCRIPTION",
            "Package: tth\nVersion: 0.1\nSuggests: testthat\n",
        ),
        ("R/add.R", "add_one <- function(n) n + 1L\n"),
        (
            "tests/testthat/helper-input.R",
            "make_input <- function() list(n = 1L)\n",
        ),
        (
            "tests/testthat/test-add.R",
            "test_that(\"adds\", {\n  expect_equal(add_one(make_input()$n), 2L)\n})\n",
        ),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert!(
        output.stdout.contains("no problems"),
        "a helper shared with a test file is not a finding: {}",
        output.stdout
    );

    // A real typo must still be reported, with the helper suggested.
    let directory = project(&[
        (
            "DESCRIPTION",
            "Package: tth\nVersion: 0.1\nSuggests: testthat\n",
        ),
        (
            "tests/testthat/helper-input.R",
            "make_input <- function() list(n = 1L)\n",
        ),
        (
            "tests/testthat/test-add.R",
            "test_that(\"adds\", {\n  expect_equal(make_inpt(), 1L)\n})\n",
        ),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert!(
        output.stderr.contains("Did you mean `make_input`?"),
        "expected the typo and its suggestion: {}",
        output.stderr
    );
}

#[test]
fn the_analysis_instrument_measures_the_same_program_as_check() {
    let directory = project(&[
        (
            "DESCRIPTION",
            "Package: tth\nVersion: 0.1\nSuggests: testthat\n",
        ),
        ("ry.toml", "[check]\ntyping = true\n"),
        ("R/add.R", "add_one <- function(n) n + 1L\n"),
        (
            "tests/testthat/helper-input.R",
            "make_input <- function() list(n = 1L)\n",
        ),
        (
            "tests/testthat/test-add.R",
            "test_that(\"adds\", {\n  expect_equal(add_one(make_input()$n), 2L)\n})\n",
        ),
    ]);
    let check = ry(directory.path(), &["check"]);
    assert!(
        check.stdout.contains("no problems"),
        "expected a clean project: {}",
        check.stdout
    );
    let stats = ry(directory.path(), &["debug", "analysis-stats", "."]);
    assert!(
        stats.stdout.contains("(0 diagnostics)"),
        "the instrument disagrees with `check` about this project: {}",
        stats.stdout
    );
}

#[test]
fn a_configured_exclude_does_not_lose_the_vendored_directory_skip() {
    let vendored = &[
        ("a.R", "x = 1\n"),
        ("renv/library/pkg/R/vendored.R", "y = T\n"),
        ("packrat/lib/vendored.R", "z = T\n"),
    ];
    for exclude in [
        "[check]\n",
        "[check]\nexclude = []\n",
        "[check]\nexclude = [\"nothing-here/\"]\n",
    ] {
        let mut files = vendored.to_vec();
        files.push(("ry.toml", exclude));
        let directory = project(&files);
        let output = ry(directory.path(), &["check"]);
        assert!(
            output.stdout.contains("in 1 file"),
            "exclude {exclude:?} walked a vendored directory: {}",
            output.stdout
        );
    }
}

#[test]
fn check_answer_is_independent_of_how_the_paths_are_named() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        (
            "R/types.R",
            "#: @alias Config {list{id: character}}\nNULL\n",
        ),
        (
            "R/build.R",
            "#: fn(id: character) -> Config\nbuild <- function(id) list(id = id)\n",
        ),
    ]);
    for arguments in [
        vec!["check", "."],
        vec!["check", "R"],
        vec!["check", "R/build.R"],
        vec!["check", "R/build.R", "R/types.R"],
    ] {
        let output = ry(directory.path(), &arguments);
        assert_eq!(
            output.code,
            0,
            "`{}` must agree with every other spelling: {}",
            arguments.join(" "),
            output.stderr
        );
    }
}

#[test]
fn check_reports_an_export_the_package_does_not_define() {
    let directory = project(&[
        ("DESCRIPTION", "Package: expkg\n"),
        (
            "NAMESPACE",
            "export(add_one)\nexport(missing_fn)\nexportPattern(\"^get_\")\n",
        ),
        (
            "R/a.R",
            "add_one <- function(x) x + 1L\nget_thing <- function() 1L\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert!(
        rendered.contains(
            "`missing_fn` is exported but this package defines no such top-level object."
        ) && !rendered.contains("add_one` is exported")
            && !rendered.contains("get_thing` is exported"),
        "only the undefined export is reported: {rendered}"
    );
}

#[test]
fn check_accepts_exports_of_imports_and_of_chained_definitions() {
    let directory = project(&[
        ("DESCRIPTION", "Package: expkg\nImports: magrittr\n"),
        (
            "NAMESPACE",
            "export(\"%>%\")\nexport(memoize)\nexport(memoise)\nexport(not_anywhere)\n\
             importFrom(magrittr,\"%>%\")\n",
        ),
        ("R/a.R", "memoise <- memoize <- function(f) f\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    for name in ["%>%", "memoize", "memoise"] {
        assert!(
            !rendered.contains(&format!("`{name}` is exported")),
            "`{name}` is in the namespace: {rendered}"
        );
    }
    assert!(
        rendered.contains("`not_anywhere` is exported"),
        "an export nothing defines or imports is still reported: {rendered}"
    );

    // A whole-namespace import of a package without stubs may supply any name.
    let directory = project(&[
        ("DESCRIPTION", "Package: expkg\nImports: someunknownpkg\n"),
        ("NAMESPACE", "export(reexported)\nimport(someunknownpkg)\n"),
        ("R/a.R", "f <- function() 1L\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    assert!(
        !output.stderr.contains("`reexported` is exported"),
        "the unknown namespace may export it: {}",
        output.stderr
    );
}

// testthat runs a test file in a child of the package namespace, so the file's own `path`
// value neither hides the package's `path` from `pkg::path` nor from a call of `path`.
#[test]
fn check_reads_the_package_namespace_past_a_test_file_s_bindings() {
    let directory = project(&[
        ("DESCRIPTION", "Package: pathpkg\n"),
        ("ry.toml", "[check]\ntyping = true\n"),
        ("R/a.R", "path <- function(x) x\n"),
        (
            "tests/testthat/test-a.R",
            "path <- \"a\"\nqualified <- pathpkg::path(1L)\ncalled <- path(2L)\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    assert!(
        !output.stderr.contains("not a function"),
        "both calls reach the package's function: {}",
        output.stderr
    );
}

#[test]
fn check_accepts_exports_made_at_run_time_or_found_on_the_search_path() {
    let directory = project(&[
        ("DESCRIPTION", "Package: expkg\n"),
        (
            "NAMESPACE",
            "export(bgRed)\nexport(diff)\nS3method(diff,thing)\n",
        ),
        (
            "R/a.R",
            "styles <- list(bgRed = 41L)\nfor (style in names(styles)) assign(style, styles[[style]])\ndiff.thing <- function(x, ...) x\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    assert!(
        !output.stderr.contains("is exported but"),
        "every export is bound: {}",
        output.stderr
    );
}

// The caret pads by terminal cells, not characters, so it sits beneath the glyph.
#[test]
fn check_reports_character_columns_and_aligns_the_caret() {
    let directory = project(&[(
        "accents.R",
        "x <- \"résumé — café\" ; y = 2L\nprint(x)\nprint(y)\n",
    )]);
    let output = ry(directory.path(), &["check", "accents.R"]);
    let rendered = &output.stderr;
    assert!(
        rendered.contains("accents.R:1:26"),
        "the `=` is at character column 26: {rendered}"
    );
    let caret_line = rendered
        .lines()
        .find(|line| line.contains('^'))
        .expect("a caret line");
    let source_line = rendered
        .lines()
        .find(|line| line.contains("résumé"))
        .expect("the snippet line");
    // Measured in terminal cells: byte offsets would pass on a broken renderer, since
    // only the snippet line carries multibyte text.
    let cells_before = |line: &str, index: usize| {
        unicode_width::UnicodeWidthStr::width(line.get(..index).unwrap_or_default())
    };
    let caret_at = cells_before(caret_line, caret_line.find('^').expect("a caret"));
    let equals_at = cells_before(
        source_line,
        source_line.find("= 2L").expect("the assignment"),
    );
    assert_eq!(
        caret_at, equals_at,
        "the caret must sit under the `=`: {rendered}"
    );

    let json = ry(
        directory.path(),
        &["check", "--output", "json", "accents.R"],
    );
    let record: serde_json::Value =
        serde_json::from_str(json.stdout.trim()).expect("diagnostic line is not valid JSON");
    assert_eq!(
        record["column"], 26,
        "JSON columns agree with the rendered ones: {record}"
    );
}

// Dispatch is not a read, so the S3 carve-out must see the whole namespace.
#[test]
fn check_sees_a_generic_declared_in_another_package_file() {
    let directory = project(&[
        ("DESCRIPTION", "Package: speaker\n"),
        (
            "R/generic.R",
            "speak <- function(x, ...) UseMethod(\"speak\")\n",
        ),
        ("R/dog.R", "speak.dog <- function(x, ...) \"woof\"\n"),
        ("R/cat.R", "meow.cat <- function(count) \"meow\"\n"),
        ("ry.toml", "[lint]\nunused-parameter = \"warn\"\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert!(
        !rendered.contains("speak.dog"),
        "a method of a generic in a sibling file is exempt: {rendered}"
    );
    assert!(
        rendered.contains("parameter `count` is never used"),
        "a dotted name that dispatches nowhere is still reported: {rendered}"
    );
}

// `usethis` writes `library(yourpkg)` into `tests/testthat.R`; tolerating unresolved
// reads for the project's own package would silence the whole package.
#[test]
fn check_still_reports_unresolved_names_when_a_test_file_attaches_the_package() {
    let directory = project(&[
        ("DESCRIPTION", "Package: tallyr\n"),
        ("tests/testthat.R", "library(testthat)\nlibrary(tallyr)\n"),
        ("R/a.R", "tally_up <- function(x) zzframboz(x)\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("I could not resolve `zzframboz`"),
        "the project's own package must buy no tolerance: {rendered}"
    );
}

#[test]
fn check_roots_a_nested_package_at_its_own_description() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        ("pkg/DESCRIPTION", "Package: inner\n"),
        ("pkg/R/a.R", "helper <- function() 1L\n"),
    ]);
    let output = ry(directory.path(), &["check", "pkg"]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 0, "stderr: {rendered}");
    assert!(
        !rendered.contains("unused"),
        "a package-visible binding is not unused: {rendered}"
    );
}

#[test]
fn check_reports_only_the_paths_it_was_given() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        ("R/inside.R", "package_bad <- 1L + \"x\"\n"),
        ("driver.R", "script_bad <- 2L + \"y\"\n"),
    ]);
    let output = ry(directory.path(), &["check", "R"]);
    let rendered = &output.stderr;
    assert!(
        rendered.contains("R/inside.R") && !rendered.contains("driver.R"),
        "analysis covers the project, reporting covers the request: {rendered}"
    );
}

#[test]
fn check_analyses_r_chunks_in_a_literate_document() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        (
            "report.Rmd",
            "# Title\n\nProse about it.\n\n```{r}\ncount <- 10L\n```\n\n```{r}\nprint(count + \"text\")\n```\n\n```{python}\nx = 1\n```\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("report.Rmd:10:") && rendered.contains("found `character`"),
        "the chunk error is located in the original document: {rendered}"
    );
    let formatted = ry(directory.path(), &["fmt", "--check", "."]);
    assert!(
        !formatted.stdout.contains("1 file"),
        "fmt must not pick up literate documents: {}",
        formatted.stdout
    );
}

#[test]
fn check_exclude_scopes_the_directory_walk() {
    let directory = project(&[
        ("ry.toml", "[check]\nexclude = [\"scripts/\"]\n"),
        ("top.R", "top_unused <- 1\n"),
        ("scripts/skipped.R", "script_unused <- 2\n"),
        ("scripts/deep/also.R", "deep_unused <- 3\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("top_unused"),
        "the non-excluded script is checked: {rendered}"
    );
    assert!(
        !rendered.contains("script_unused") && !rendered.contains("deep_unused"),
        "the excluded tree must not be walked: {rendered}"
    );
}

#[test]
fn check_exclude_negation_reincludes() {
    let directory = project(&[
        (
            "ry.toml",
            "[check]\nexclude = [\"scripts/*\", \"!scripts/keep\"]\n",
        ),
        ("scripts/skipped.R", "script_unused <- 2\n"),
        ("scripts/keep/kept.R", "kept_unused <- 3\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("kept_unused") && !rendered.contains("script_unused"),
        "negation re-includes the kept subtree: {rendered}"
    );
}

#[test]
fn check_explicit_file_bypasses_exclude() {
    let directory = project(&[
        ("ry.toml", "[check]\nexclude = [\"scripts/\"]\n"),
        ("scripts/named.R", "named_unused <- 2\n"),
    ]);
    let output = ry(directory.path(), &["check", "scripts/named.R"]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("named_unused"),
        "a file named on the command line is always checked: {rendered}"
    );
}

#[test]
fn check_invalid_exclude_pattern_exits_two() {
    let directory = project(&[
        ("ry.toml", "[check]\nexclude = [\"scripts/[\"]\n"),
        ("top.R", "x <- 1\n"),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    assert_eq!(output.code, 2, "stderr: {}", output.stderr);
    assert!(
        output.stderr.contains("invalid `[check] exclude` pattern"),
        "the bad pattern is named: {}",
        output.stderr
    );
}

// A suppression also covers the line below it, hence the blank line.
#[test]
fn check_suppression_comments_drop_findings() {
    let directory = project(&[(
        "warn.R",
        "x = 1 # ry: allow(assignment-operator, unused)\n\ny = 2\n",
    )]);
    let output = ry(directory.path(), &["check", "warn.R"]);
    let rendered = &output.stderr;
    assert_eq!(output.code, 1, "stderr: {rendered}");
    assert!(
        rendered.contains("warn.R:3:1"),
        "the unsuppressed finding remains: {rendered}"
    );
    assert!(
        !rendered.contains("warn.R:1:1"),
        "the suppressed finding must not render: {rendered}"
    );
}

#[test]
fn fmt_check_lists_dirty_files_and_writes_nothing() {
    let directory = project(&[("dirty.R", "x<-1\n"), ("clean.R", "y <- 2\n")]);
    let output = ry(directory.path(), &["fmt", "--check"]);
    let stderr = &output.stderr;

    assert_eq!(output.code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("Would reformat") && stderr.contains("dirty.R"),
        "expected the dirty file to be listed, got: {stderr}"
    );
    assert!(
        stderr.contains("1 file would be reformatted, 1 file already formatted"),
        "expected a summary count, got: {stderr}"
    );
    let content = fs::read_to_string(directory.path().join("dirty.R")).expect("read dirty.R");
    assert_eq!(content, "x<-1\n", "--check must not modify files");
}

#[test]
fn fmt_check_clean_exits_zero_with_summary() {
    let directory = project(&[("clean.R", "x <- 1\n")]);
    let output = ry(directory.path(), &["fmt", "--check", "clean.R"]);
    let stderr = &output.stderr;

    assert_eq!(output.code, 0, "stderr: {stderr}");
    assert!(
        stderr.contains("0 files would be reformatted, 1 file already formatted"),
        "expected a summary line, got: {stderr}"
    );
}

#[test]
fn fmt_rewrites_in_place_and_exits_zero() {
    let directory = project(&[("dirty.R", "x<-1\n")]);
    let output = ry(directory.path(), &["fmt", "dirty.R"]);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
    let content = fs::read_to_string(directory.path().join("dirty.R")).expect("read dirty.R");
    assert_eq!(content, "x <- 1\n");
}

#[test]
fn fmt_on_a_deliberately_skipped_file_exits_clean() {
    let directory = project(&[("report.Rmd", "# Title\n\n```{r}\nx<-1\n```\n")]);
    for arguments in [
        vec!["fmt", "--diff", "report.Rmd"],
        vec!["fmt", "--check", "report.Rmd"],
        vec!["fmt", "report.Rmd"],
    ] {
        let output = ry(directory.path(), &arguments);
        assert_eq!(
            output.code, 0,
            "{arguments:?} should be clean: {}",
            output.stderr
        );
    }
    let text = fs::read_to_string(directory.path().join("report.Rmd"))
        .expect("failed to read the document back");
    assert!(
        text.contains("x<-1"),
        "the document must not be rewritten: {text}"
    );
}

#[test]
fn fmt_syntax_error_exits_two() {
    let directory = project(&[("bad.R", SYNTAX_ERROR_SOURCE)]);
    let output = ry(directory.path(), &["fmt", "bad.R"]);

    assert_eq!(output.code, 2, "stderr: {}", output.stderr);
    assert!(
        output.stderr.contains("failed to format"),
        "expected the format failure to be reported, got: {}",
        output.stderr
    );
}

#[test]
fn usage_error_exits_two() {
    let directory = project(&[]);
    let output = ry(directory.path(), &["bogus-subcommand"]);
    assert_eq!(output.code, 2, "stderr: {}", output.stderr);
}

#[test]
fn typing_off_directive_silences_one_file() {
    let directory = project(&[
        ("R/opted_out.R", "# typing: off\nbad <- 1L + \"a\"\n"),
        ("R/plain.R", "also_bad <- 1L + \"a\"\n"),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(report.contains("plain.R"), "report:\n{report}");
    assert!(!report.contains("opted_out.R"), "report:\n{report}");
}

#[test]
fn typing_on_and_strict_directives_opt_single_files_in() {
    let directory = project(&[
        ("R/opted_in.R", "# typing: on\nbad <- 1L + \"a\"\n"),
        ("R/strict_file.R", "# typing: strict\nx <- not_defined()\n"),
        ("R/plain.R", "quiet <- 1L + \"a\"\n"),
        ("ry.toml", "[check]\ntyping = false\n"),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(report.contains("opted_in.R"), "report:\n{report}");
    assert!(
        report.contains(&heading(
            "unresolved",
            'x',
            "I could not resolve `not_defined`",
        )),
        "strict escalates the unresolved reference to an error:\n{report}"
    );
    assert!(!report.contains("plain.R"), "report:\n{report}");
}

// Strict reports reads tolerated by an attached package with unknown exports. Not a
// fixture: the tolerance keys on `PackageMetadata`, which only a real project sets.
#[test]
fn strict_reports_a_read_the_attached_package_tolerance_silenced() {
    let sources = [
        (
            "R/a.R",
            "library(someunknownpkg)

bare <- mystery_thing
print(bare)
",
        ),
        (
            "ry.toml",
            "[check]
typing = true
strict = true
",
        ),
    ];
    let directory = project(&sources);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(
        report.contains("nothing this project can see defines `mystery_thing`"),
        "strict surfaces the tolerated read:\n{report}"
    );

    // Without strict the tolerance still holds.
    let lenient = project(&[
        sources[0],
        (
            "ry.toml",
            "[check]
typing = true
",
        ),
    ]);
    let output = ry(lenient.path(), &["check"]);
    assert_eq!(output.code, 0, "{}", output.stderr);

    // And the remedy the message names actually closes it.
    let declared = project(&[
        sources[0],
        sources[1],
        ("stubs/someunknownpkg.Rtypes", "mystery_thing : integer\n"),
    ]);
    let output = ry(declared.path(), &["check"]);
    assert_eq!(output.code, 0, "{}", output.stderr);
}

#[test]
fn unknown_typing_directive_value_is_reported() {
    let directory = project(&[("R/a.R", "# typing: strcit\nx <- 1L\n")]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    assert!(
        output.stderr.contains("Unknown typing directive `strcit`"),
        "report:\n{}",
        output.stderr
    );
}

// Inside data.table brackets and `with`/`subset`; base indexing still warns.
#[test]
fn data_masked_column_references_do_not_warn() {
    let directory = project(&[
        (
            "R/dt.R",
            "summarize_sales <- function(dt) {\n  dt[region == \"west\", .(total = sum(amount)), by = product]\n  dt[, revenue := price * quantity]\n  with(dt, mean(score_col))\n}\n",
        ),
        ("R/plain.R", "pick <- function(m) m[not_a_column]\n"),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(
        report.contains("not_a_column"),
        "base indexing keeps its unresolved warning:\n{report}"
    );
    for column in ["region", "amount", "product", "revenue", "score_col"] {
        assert!(
            !report.contains(&format!("`{column}`")),
            "masked column `{column}` must not warn:\n{report}"
        );
    }
}

#[test]
fn global_variables_declarations_suppress_unresolved_warnings() {
    let directory = project(&[
        (
            "R/globals.R",
            "utils::globalVariables(c(\"generated_col\", \"another_col\"))\n",
        ),
        (
            "R/use.R",
            "f <- function() generated_col + another_col + genuinely_undefined\n",
        ),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(report.contains("genuinely_undefined"), "report:\n{report}");
    for declared in ["generated_col", "another_col"] {
        assert!(
            !report.contains(&format!("`{declared}`")),
            "declared name `{declared}` must not warn:\n{report}"
        );
    }
}

// Both bare and `pkg::name` call forms; a locally shadowed name masks nothing.
#[test]
fn masked_stub_verbs_mask_rest_absorbed_arguments() {
    let directory = project(&[
        (
            "stubs/dplyr.Rtypes",
            "filter : @masked fn(.data: Any, ...: Any) -> Any\nmutate : @masked fn(.data: Any, ...: Any) -> Any\n",
        ),
        (
            "R/pipeline.R",
            "report <- function(df) {\n  filtered <- dplyr::filter(df, amount > 100)\n  mutate(filtered, doubled = amount * 2)\n}\ncontrol <- function() {\n  filter <- function(x) x\n  filter(shadowed_undefined)\n}\n",
        ),
        ("ry.toml", "[check]\ntyping = true\n"),
    ]);
    let output = ry(directory.path(), &["check"]);
    assert_eq!(output.code, 1);
    let report = &output.stderr;
    assert!(report.contains("shadowed_undefined"), "report:\n{report}");
    for column in ["amount", "doubled"] {
        assert!(
            !report.contains(&format!("`{column}`")),
            "masked column `{column}` must not warn:\n{report}"
        );
    }
}

#[test]
fn a_type_name_two_scripts_both_declare_is_not_a_duplicate() {
    // Script type declarations reach only their own file, so two scripts may each
    // declare `Thing`. Fixture cases are single-file, so they cannot cover this.
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        (
            "one.R",
            "#: @alias Thing {double}\n\nuse_a <- 1.0\nprint(use_a)\n",
        ),
        (
            "two.R",
            "#: @alias Thing {character}\n\nuse_b <- \"x\"\nprint(use_b)\n",
        ),
    ]);
    let output = ry(directory.path(), &["check", "."]);
    assert!(
        !output.stderr.contains("declared more than once"),
        "two scripts each declaring one name must not conflict: {}",
        output.stderr
    );
    assert_eq!(output.code, 0, "{}", output.stderr);
}

const INLINE_SOURCE: &str = "count: integer <- \"one\"\n";

#[test]
fn check_types_inline_annotations_by_default() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\n"),
        ("R/a.R", INLINE_SOURCE),
    ]);
    let run = ry(directory.path(), &["check"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains("expected `integer`, found `character`"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("inline-annotation"), "{}", run.stderr);
}

#[test]
fn check_warns_while_inline_annotations_are_off_and_ignores_them() {
    let directory = project(&[
        (
            "ry.toml",
            "[check]\ntyping = true\ninline-annotations = false\n",
        ),
        ("R/a.R", INLINE_SOURCE),
    ]);
    let run = ry(directory.path(), &["check"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains(
            "inline type annotations are turned off (`[check] inline-annotations = false`)"
        ),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains(&heading(
            "inline-annotation",
            '!',
            "inline type annotations are turned off (`[check] inline-annotations = false`), so this one is not checked"
        )),
        "{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("expected `integer`"),
        "an annotation that is turned off must not type anything: {}",
        run.stderr
    );
}

#[test]
fn turning_inline_annotations_off_warns_even_without_any() {
    let directory = project(&[
        ("ry.toml", "[check]\ninline-annotations = false\n"),
        ("R/a.R", "x <- 1L\n"),
    ]);
    let run = ry(directory.path(), &["check"]);
    assert!(
        run.stderr
            .contains("inline type annotations are turned off"),
        "{}",
        run.stderr
    );
}

#[test]
fn check_turns_object_system_definitions_into_errors_when_disabled() {
    let source = (
        "R/main.R",
        "setClass(\"Person\", representation(name = \"character\"))\n",
    );

    let default_run = ry(project(&[source]).path(), &["check", "."]);
    assert!(
        default_run.stderr.contains(
            "`setClass` defines an S4 class. ry recommends a record or a tagged union instead"
        ),
        "expected the object-system warning, got: {}",
        default_run.stderr
    );

    let disabled = project(&[source, ("ry.toml", "[check]\nobject-systems = false\n")]);
    let output = ry(disabled.path(), &["check", "."]);
    assert_eq!(output.code, 1, "stderr: {}", output.stderr);
    assert!(
        output.stderr.contains(
            "S4 classes are turned off in this project (`[check] object-systems = false`)"
        ),
        "expected the object-system error, got: {}",
        output.stderr
    );
}

#[test]
fn root_help_lists_run_and_r_but_not_the_console_s_earlier_name() {
    let directory = project(&[]);
    let output = ry(directory.path(), &["--help"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr);
    let commands: Vec<&str> = output
        .stdout
        .lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        commands,
        [
            "check", "fmt", "run", "R", "load", "server", "debug", "help"
        ],
        "{}",
        output.stdout
    );
}

#[test]
fn run_and_r_help_hide_the_runtime_flag() {
    let directory = project(&[]);
    for command in ["run", "R", "load"] {
        let output = ry(directory.path(), &[command, "--help"]);
        assert_eq!(output.code, 0, "{command}: {}", output.stderr);
        assert!(
            !output.stdout.contains("--runtime"),
            "{command}: {}",
            output.stdout
        );
    }
    let run = ry(directory.path(), &["run", "--help"]);
    assert!(
        run.stdout
            .contains("Usage: ry run [OPTIONS] [FILE] [ARGS]..."),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("--interactive"), "{}", run.stdout);
    assert!(run.stdout.contains("--no-load"), "{}", run.stdout);
    let r = ry(directory.path(), &["R", "--help"]);
    assert!(
        r.stdout.contains("Usage: ry R [OPTIONS] [FILE] [ARGS]..."),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("--no-load"), "{}", r.stdout);
}

#[test]
fn a_missing_script_exits_two_before_any_r_starts() {
    let directory = project(&[]);
    for arguments in [
        &["run", "missing.R"][..],
        &["R", "missing.R"],
        &["run", "-i", "missing.R"],
        &["R", "--interactive", "missing.R"],
        &["repl", "-f", "missing.R"],
    ] {
        let output = ry(directory.path(), arguments);
        assert_eq!(output.code, 2, "{arguments:?}: {}", output.stderr);
        assert!(
            output.stderr.contains("cannot read missing.R"),
            "{arguments:?}: {}",
            output.stderr
        );
        assert_eq!(output.stdout, "", "{arguments:?}");
    }
}

#[test]
fn session_usage_errors_exit_two() {
    let directory = project(&[("script.R", "1\n")]);
    for arguments in [
        &["run", "--interactive"][..],
        &["run", "-i", "script.R", "extra"],
        &["R", "--runtime", "ry", "script.R"],
        &["run", "--keybindings", "nano"],
        &["run", "--runtime", "jvm", "script.R"],
        &["R", "--bogus"],
    ] {
        let output = ry(directory.path(), arguments);
        assert_eq!(output.code, 2, "{arguments:?}: {}", output.stderr);
        assert!(
            output.stderr.starts_with("error:"),
            "{arguments:?}: {}",
            output.stderr
        );
    }
}

#[test]
fn a_strict_mode_refusal_exits_one_before_any_r_starts() {
    let directory = project(&[
        ("ry.toml", "[check]\ntyping = true\nstrict = true\n"),
        (
            "script.R",
            "describe <- function(x) cat(\"a value\\n\")\ndescribe(1)\n",
        ),
    ]);
    for command in ["run", "R"] {
        let output = ry(directory.path(), &[command, "script.R"]);
        assert_eq!(output.code, 1, "{command}: {}", output.stderr);
        assert!(
            output.stderr.contains("strict mode found 1 error"),
            "{}",
            output.stderr
        );
        assert_eq!(output.stdout, "", "{command}: nothing ran");
    }
}
