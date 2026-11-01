//! Docs examples as tests: every docs code block marked as `ry` output runs through the real
//! `ry` binary, and the output must match what the page shows, so a stale example fails here.
//!
//! A marker is the rest of a fence's info string after the language, starting with the word
//! `ry`, such as ```` ```text ry check typing ````. Expressive Code ignores the extra words, so
//! the rendered site does not show them. `contributing/testing.md` documents the format;
//! the short version:
//!
//! - A marked `text` block is the output of the nearest `r` block above it, or of every tab of the
//!   nearest `<Tabs>` group above it, each tab run on its own.
//! - A marked `r` block holds its own expected output as `#>` lines after the code that prints it.
//! - The words after `ry` name the command (`check` or `run`) and its options: `typing`,
//!   `strict`, any other `[check]` key as `key=value`, `runtime=ry` or `runtime=gnu-r`, `exact`,
//!   `continue`, `dput`, and `tab=LABEL`.
//! - A `text` block may open with a `$ ry run sales.R` line, which names the file and is not output.
//!
//! The landing page's samples in `docs/src/pages/index.astro` are checked too, without markers.
//!
//! `DOCS_PAGE=<substring>` checks only the pages whose path contains it, and `RY_BLESS=1`
//! rewrites the expected output of exact blocks (excerpts are left to their author).

use {
    similar::TextDiff,
    std::{
        collections::BTreeMap,
        fs,
        io::Read,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Instant,
    },
};

#[test]
fn docs_examples_hold() {
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves");
    let only = std::env::var("DOCS_PAGE").ok();
    let bless = std::env::var_os("RY_BLESS").is_some();

    let mut pages = Vec::new();
    collect_pages(&repository.join("docs/src/content/docs"), &mut pages);
    pages.push(repository.join("docs/src/pages/index.astro"));
    pages.sort();

    let mut examples = Vec::new();
    let mut problems = Vec::new();
    for path in pages {
        let relative = path
            .strip_prefix(&repository)
            .unwrap_or(&path)
            .display()
            .to_string();
        if only
            .as_ref()
            .is_some_and(|only| !relative.contains(only.as_str()))
        {
            continue;
        }
        let text = fs::read_to_string(&path).expect("a docs page reads");
        let parsed = if relative.ends_with(".astro") {
            parse_landing(&relative, &text)
        } else {
            parse_page(&relative, &text)
        };
        match parsed {
            Ok(found) => examples.extend(found),
            Err(problem) => problems.push(problem),
        }
    }

    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    examples.retain(|example| match missing_requirement(&example.config) {
        Some(reason) => {
            *skipped.entry(reason).or_default() += 1;
            false
        }
        None => true,
    });

    let plan = plan_jobs(&examples);
    let outputs = run_jobs(&plan.jobs);
    let mut blessings: BTreeMap<String, Vec<Blessing>> = BTreeMap::new();
    for (example_index, example) in examples.iter().enumerate() {
        for verdict in judge(example, example_index, &plan, &outputs) {
            match verdict {
                Verdict::Pass => {}
                Verdict::Fail { message, blessing } => match blessing {
                    Some(blessing) if bless => blessings
                        .entry(example.page.clone())
                        .or_default()
                        .push(blessing),
                    _ => problems.push(message),
                },
            }
        }
    }
    for (page, page_blessings) in blessings {
        let path = repository.join(&page);
        let text = fs::read_to_string(&path).expect("a docs page reads");
        fs::write(&path, apply_blessings(&text, page_blessings)).expect("a blessed page writes");
        eprintln!("blessed {page}");
    }

    let checked: usize = examples.iter().map(|example| example.variants.len()).sum();
    eprintln!(
        "docs examples: {} blocks, {checked} runs in {} processes, {:.1}s",
        examples.len(),
        plan.jobs.len(),
        started.elapsed().as_secs_f64()
    );
    for (reason, count) in &skipped {
        eprintln!("docs examples: skipped {count} blocks, {reason}");
    }
    assert!(
        problems.is_empty(),
        "{} docs example(s) do not match the tool:\n\n{}",
        problems.len(),
        problems.join("\n\n")
    );
}

/// One marked output, with the code that produces it.
struct Example {
    page: String,
    /// The 1-based line of the marked fence.
    line: usize,
    config: Config,
    /// The code to run: one per tab, or one for a plain block.
    variants: Vec<Variant>,
    expected: Expected,
}

#[derive(Clone)]
struct Variant {
    label: Option<String>,
    code: String,
    /// With `continue`, the code of the earlier `r` blocks, which runs first: every one for a
    /// `check`, and those whose output the page checks for a `run`.
    prelude: Vec<String>,
}

#[derive(Clone)]
struct Config {
    command: CommandKind,
    /// `[check]` keys for the project's `ry.toml`, as written.
    check_keys: Vec<(String, String)>,
    exact: bool,
    continues: bool,
    dput: bool,
    /// The tab whose code produces this output, when only one does.
    tab: Option<String>,
    file: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CommandKind {
    Check,
    Run(Runtime),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Runtime {
    GnuR,
    Ry,
}

enum Expected {
    /// A `text` block's body, and the page lines it occupies, for blessing. The landing page's
    /// samples have no such lines.
    Block {
        text: String,
        body: Option<PageLines>,
    },
    /// The `#>` groups of a marked `r` block.
    Chunks(Vec<Chunk>),
    /// A tab group that no output block follows: its tabs are two spellings of one program, so
    /// they must give the same findings.
    SameAcrossTabs,
}

struct Chunk {
    code: String,
    expected: Vec<String>,
    lines: PageLines,
}

/// A 0-based, end-exclusive range of page lines, and the indentation of the fence they sit in.
#[derive(Clone, Copy)]
struct PageLines {
    start: usize,
    end: usize,
    indent: usize,
}

struct Blessing {
    lines: PageLines,
    replacement: Vec<String>,
}

fn collect_pages(directory: &Path, pages: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the docs directory reads") {
        let path = entry.expect("a docs directory entry").path();
        if path.is_dir() {
            collect_pages(&path, pages);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "md" || extension == "mdx")
        {
            pages.push(path);
        }
    }
}

/// Finds the marked blocks of a Markdown or MDX page and pairs each output block with its code.
fn parse_page(page: &str, text: &str) -> Result<Vec<Example>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut examples = Vec::new();
    let mut tabs: Option<Vec<(String, Option<String>)>> = None;
    let mut source: Option<Vec<Variant>> = None;
    // Every `r` block or tab group so far, and whether the page checks its output. A `run` with
    // `continue` replays only the checked ones, so an illustration the test cannot run (one that
    // needs a package the machine lacks) does not stop the blocks after it. A `check` replays
    // them all, because checking runs nothing.
    let mut history: Vec<(Vec<Variant>, bool)> = Vec::new();
    let mut tabs_line = 0;
    // The latest tab group, until an output block pairs with it or other code replaces it.
    let mut unpaired: Option<(usize, Vec<Variant>)> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if trimmed.starts_with("<Tabs") {
            tabs = Some(Vec::new());
            tabs_line = index + 1;
        } else if trimmed.starts_with("</Tabs>") {
            let variants: Vec<Variant> = tabs
                .take()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(label, code)| {
                    code.map(|code| Variant {
                        label: Some(label),
                        code,
                        prelude: Vec::new(),
                    })
                })
                .collect();
            if !variants.is_empty() {
                flush_unpaired(page, &mut unpaired, &mut examples);
                if variants.len() > 1 {
                    unpaired = Some((tabs_line, variants.clone()));
                }
                history.push((variants.clone(), false));
                source = Some(variants);
            }
        } else if let Some(rest) = trimmed.strip_prefix("<TabItem label=\"") {
            let label = rest.split('"').next().unwrap_or_default().to_owned();
            if let Some(items) = tabs.as_mut() {
                items.push((label, None));
            }
        }
        let Some(fence) = open_fence(line) else {
            index += 1;
            continue;
        };
        let body_start = index + 1;
        let mut body_end = body_start;
        while body_end < lines.len() && !closes_fence(lines[body_end], &fence) {
            body_end += 1;
        }
        if body_end == lines.len() {
            return Err(format!("{page}:{}: an unclosed code fence", index + 1));
        }
        let body: Vec<&str> = lines[body_start..body_end]
            .iter()
            .map(|line| strip_indent(line, fence.indent))
            .collect();
        let is_r = fence.language.eq_ignore_ascii_case("r");
        let location = format!("{page}:{}", index + 1);
        let marker = fence
            .marker
            .as_deref()
            .map(|marker| parse_marker(marker).map_err(|problem| format!("{location}: {problem}")))
            .transpose()?;
        let range = PageLines {
            start: body_start,
            end: body_end,
            indent: fence.indent,
        };
        match (marker, is_r, tabs.as_mut()) {
            (Some(_), _, Some(_)) => {
                return Err(format!(
                    "{location}: a marked block inside a tab group; put the output after `</Tabs>`"
                ));
            }
            (None, true, Some(items)) => {
                let Some(item) = items.last_mut() else {
                    return Err(format!("{location}: an `r` block outside any `<TabItem>`"));
                };
                if item.1.is_some() {
                    return Err(format!(
                        "{location}: the tab `{}` holds a second `r` block",
                        item.0
                    ));
                }
                item.1 = Some(join_lines(&body));
            }
            (None, true, None) => {
                flush_unpaired(page, &mut unpaired, &mut examples);
                let variants = vec![Variant {
                    label: None,
                    code: join_lines(&body),
                    prelude: Vec::new(),
                }];
                history.push((variants.clone(), false));
                source = Some(variants);
            }
            (Some(config), true, None) => {
                let (code, chunks) = split_chunks(&body, range);
                if chunks.is_empty() {
                    return Err(format!(
                        "{location}: a marked `r` block needs `#>` lines with the output"
                    ));
                }
                if config.command == CommandKind::Check {
                    return Err(format!(
                        "{location}: `#>` output comes from `run`, not `check`"
                    ));
                }
                if config.exact {
                    return Err(format!("{location}: `#>` output is always exact"));
                }
                if config.continues {
                    return Err(format!(
                        "{location}: `continue` applies to output blocks only"
                    ));
                }
                flush_unpaired(page, &mut unpaired, &mut examples);
                let variant = Variant {
                    label: None,
                    code,
                    prelude: Vec::new(),
                };
                history.push((vec![variant.clone()], true));
                source = Some(vec![variant.clone()]);
                examples.push(Example {
                    page: page.to_owned(),
                    line: index + 1,
                    config,
                    variants: vec![variant],
                    expected: Expected::Chunks(chunks),
                });
            }
            (Some(mut config), false, None) => {
                unpaired = None;
                let Some(mut variants) = source.clone() else {
                    return Err(format!(
                        "{location}: no `r` block or tab group above this output block"
                    ));
                };
                if config.continues
                    && matches!(config.command, CommandKind::Run(_))
                    && variants.len() > 1
                {
                    return Err(format!(
                        "{location}: a `run` with `continue` follows one `r` block, not a tab group"
                    ));
                }
                if let Some(latest) = history.last_mut() {
                    latest.1 = true;
                }
                if config.continues {
                    let replays_all = config.command == CommandKind::Check;
                    let earlier = &history[..history.len().saturating_sub(1)];
                    for variant in &mut variants {
                        variant.prelude = earlier
                            .iter()
                            .filter(|(_, checked)| replays_all || *checked)
                            .filter_map(|(group, _)| {
                                group
                                    .iter()
                                    .find(|candidate| candidate.label == variant.label)
                                    .or_else(|| group.first())
                            })
                            .map(|candidate| candidate.code.clone())
                            .collect();
                    }
                }
                if config.dput {
                    return Err(format!("{location}: `dput` applies to `#>` output only"));
                }
                // A leading `$ ry run sales.R` line shows the reader the command; it names the
                // file the code is written to, and it is not part of the output.
                let (body, range) = match body.first().and_then(|line| line.strip_prefix("$ ")) {
                    Some(command) => {
                        if let Some(file) = command
                            .split_whitespace()
                            .last()
                            .filter(|word| word.ends_with(".R"))
                        {
                            config.file = Some(file.to_owned());
                        }
                        let range = PageLines {
                            start: range.start + 1,
                            ..range
                        };
                        (&body[1..], range)
                    }
                    None => (&body[..], range),
                };
                let expected = join_lines(body);
                if config.file.is_none() {
                    config.file = snippet_path(&expected);
                }
                let variants = match config.tab.clone() {
                    Some(tab) => {
                        let chosen: Vec<Variant> = variants
                            .into_iter()
                            .filter(|variant| variant.label.as_deref() == Some(tab.as_str()))
                            .collect();
                        if chosen.is_empty() {
                            return Err(format!("{location}: no tab named `{tab}` above"));
                        }
                        chosen
                    }
                    None => variants,
                };
                examples.push(Example {
                    page: page.to_owned(),
                    line: index + 1,
                    config,
                    variants,
                    expected: Expected::Block {
                        text: expected,
                        body: Some(range),
                    },
                });
            }
            (None, false, _) => {}
        }
        index = body_end + 1;
    }
    flush_unpaired(page, &mut unpaired, &mut examples);
    Ok(examples)
}

fn flush_unpaired(
    page: &str,
    unpaired: &mut Option<(usize, Vec<Variant>)>,
    examples: &mut Vec<Example>,
) {
    if let Some((line, variants)) = unpaired.take() {
        examples.push(Example {
            page: page.to_owned(),
            line,
            config: Config {
                command: CommandKind::Check,
                check_keys: vec![("typing".to_owned(), "true".to_owned())],
                exact: false,
                continues: false,
                dput: false,
                tab: None,
                file: None,
            },
            variants,
            expected: Expected::SameAcrossTabs,
        });
    }
}

struct Fence {
    indent: usize,
    ticks: usize,
    language: String,
    /// The words after `ry`, when the info string has the marker.
    marker: Option<String>,
}

fn open_fence(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
    if ticks < 3 {
        return None;
    }
    let mut words = trimmed[ticks..].split_whitespace();
    let language = words.next().unwrap_or_default().to_owned();
    let marker = match words.next() {
        Some("ry") => Some(words.collect::<Vec<_>>().join(" ")),
        _ => None,
    };
    Some(Fence {
        indent: line.len() - trimmed.len(),
        ticks,
        language,
        marker,
    })
}

fn closes_fence(line: &str, fence: &Fence) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= fence.ticks && trimmed.chars().all(|character| character == '`')
}

fn strip_indent(line: &str, indent: usize) -> &str {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    &line[spaces.min(indent)..]
}

fn join_lines(lines: &[&str]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Reads the marker words after `ry`.
fn parse_marker(marker: &str) -> Result<Config, String> {
    let mut words = marker.split_whitespace();
    let mut command = match words.next() {
        Some("check") => CommandKind::Check,
        Some("run") => CommandKind::Run(Runtime::GnuR),
        Some(other) => return Err(format!("unknown command `{other}` (expected check or run)")),
        None => return Err("the marker `ry` needs a command, check or run".to_owned()),
    };
    let mut config = Config {
        command,
        check_keys: Vec::new(),
        exact: false,
        continues: false,
        dput: false,
        tab: None,
        file: None,
    };
    for word in words {
        match word.split_once('=') {
            None => match word {
                "typing" | "strict" => config.check_keys.push((word.to_owned(), "true".to_owned())),
                "exact" => config.exact = true,
                "continue" => config.continues = true,
                "dput" => config.dput = true,
                other => return Err(format!("unknown marker word `{other}`")),
            },
            Some(("runtime", runtime)) => {
                let runtime = match runtime {
                    "ry" => Runtime::Ry,
                    "gnu-r" => Runtime::GnuR,
                    other => return Err(format!("unknown runtime `{other}`")),
                };
                match command {
                    CommandKind::Run(_) => command = CommandKind::Run(runtime),
                    CommandKind::Check => return Err("`runtime=` needs `run`".to_owned()),
                }
            }
            Some(("tab", tab)) => config.tab = Some(tab.to_owned()),
            Some((key, value)) => config.check_keys.push((key.to_owned(), value.to_owned())),
        }
    }
    config.command = command;
    Ok(config)
}

/// Splits a marked `r` block into the code before each `#>` group and the group's lines. Code
/// after the last group runs too, and is returned as the whole block's code.
fn split_chunks(body: &[&str], range: PageLines) -> (String, Vec<Chunk>) {
    let mut chunks = Vec::new();
    let mut code: Vec<&str> = Vec::new();
    let mut all_code: Vec<&str> = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        let line = body[offset];
        if !line.trim_start().starts_with("#>") {
            code.push(line);
            all_code.push(line);
            offset += 1;
            continue;
        }
        let start = offset;
        let mut expected = Vec::new();
        while offset < body.len() && body[offset].trim_start().starts_with("#>") {
            let content = body[offset].trim_start().trim_start_matches("#>");
            expected.push(content.strip_prefix(' ').unwrap_or(content).to_owned());
            offset += 1;
        }
        chunks.push(Chunk {
            code: join_lines(&code),
            expected,
            lines: PageLines {
                start: range.start + start,
                end: range.start + offset,
                indent: range.indent,
            },
        });
        code.clear();
    }
    (join_lines(&all_code), chunks)
}

/// The file a diagnostic snippet in the expected output names, as in `--[R/both.R:2:31]`, so
/// the example's line and column numbers mean what the page shows.
fn snippet_path(expected: &str) -> Option<String> {
    let start = expected.find("--[")? + 3;
    let rest = &expected[start..];
    let end = rest.find(':')?;
    Some(rest[..end].to_owned())
}

/// The landing page's samples are TypeScript objects with a `code` template literal (or the name
/// of a `const` holding one), an optional `command` (`ry check` when absent) and an `output`
/// template literal. Each is checked as an excerpt with typing on.
fn parse_landing(page: &str, text: &str) -> Result<Vec<Example>, String> {
    let mut examples = Vec::new();
    let mut rest = text;
    while let Some(position) = rest.find("code: ") {
        let consumed = text.len() - rest.len() + position;
        let line = text[..consumed].lines().count();
        let after_key = &rest[position + "code: ".len()..];
        let (code, after) = match after_key.strip_prefix('`') {
            Some(literal) => template_literal(literal)
                .ok_or_else(|| format!("{page}:{line}: an unterminated `code` literal"))?,
            None => {
                let name_end = after_key
                    .find(|character: char| !(character.is_alphanumeric() || character == '_'))
                    .unwrap_or(after_key.len());
                let name = &after_key[..name_end];
                // `code: string;` is the samples' type declaration, not a sample.
                if name == "string" {
                    rest = &after_key[name_end..];
                    continue;
                }
                let value = landing_constant(text, name)
                    .ok_or_else(|| format!("{page}:{line}: no `const {name}` literal"))?;
                (value, &after_key[name_end..])
            }
        };
        let code = interpolate(text, &code)
            .ok_or_else(|| format!("{page}:{line}: an interpolation the test cannot evaluate"))?;
        let output_start = after
            .find("output: `")
            .ok_or_else(|| format!("{page}:{line}: a sample without `output`"))?;
        let command = match after[..output_start].find("command: \"") {
            Some(command_start) => {
                let command_text = &after[command_start + "command: \"".len()..];
                let command_end = command_text
                    .find('"')
                    .ok_or_else(|| format!("{page}:{line}: an unterminated `command`"))?;
                &command_text[..command_end]
            }
            None => "ry check",
        };
        let (output, after_output) =
            template_literal(&after[output_start + "output: `".len()..])
                .ok_or_else(|| format!("{page}:{line}: an unterminated `output` literal"))?;
        let words: Vec<&str> = command.split_whitespace().collect();
        let (kind, file) = match words.as_slice() {
            ["ry", "check"] => (CommandKind::Check, snippet_path(&output)),
            ["ry", "run", "--runtime", "ry", file] => {
                (CommandKind::Run(Runtime::Ry), Some((*file).to_owned()))
            }
            ["ry", "run", file] => (CommandKind::Run(Runtime::GnuR), Some((*file).to_owned())),
            _ => {
                return Err(format!(
                    "{page}:{line}: an unknown sample command `{command}`"
                ));
            }
        };
        examples.push(Example {
            page: page.to_owned(),
            line,
            config: Config {
                command: kind,
                check_keys: vec![("typing".to_owned(), "true".to_owned())],
                exact: false,
                continues: false,
                dput: false,
                tab: None,
                file,
            },
            variants: vec![Variant {
                label: None,
                code: format!("{code}\n"),
                prelude: Vec::new(),
            }],
            expected: Expected::Block {
                text: output,
                body: None,
            },
        });
        rest = after_output;
    }
    Ok(examples)
}

/// The template literal a landing-page `const NAME = \`...\`` holds.
fn landing_constant(text: &str, name: &str) -> Option<String> {
    let start = text.find(&format!("const {name} = `"))? + format!("const {name} = `").len();
    template_literal(&text[start..]).map(|(value, _)| value)
}

/// Expands the `${NAME}` and `${NAME.replace('old', 'new')}` interpolations the samples use.
fn interpolate(text: &str, code: &str) -> Option<String> {
    let mut expanded = String::new();
    let mut rest = code;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let end = start + rest[start..].find('}')?;
        let expression = &rest[start + 2..end];
        let (name, replacement) = match expression.split_once(".replace(") {
            Some((name, arguments)) => {
                let arguments = arguments.strip_suffix(')')?;
                let (old, new) = arguments.split_once("', '")?;
                (
                    name,
                    Some((old.strip_prefix('\'')?, new.strip_suffix('\'')?)),
                )
            }
            None => (expression, None),
        };
        let value = landing_constant(text, name)?;
        match replacement {
            Some((old, new)) => expanded.push_str(&value.replacen(old, new, 1)),
            None => expanded.push_str(&value),
        }
        rest = &rest[end + 1..];
    }
    expanded.push_str(rest);
    Some(expanded)
}

/// Reads a template literal's text up to its closing backtick, undoing the `\`` and `\\`
/// escapes, and returns it with the text after it.
fn template_literal(text: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut characters = text.char_indices();
    while let Some((offset, character)) = characters.next() {
        match character {
            '`' => return Some((value, &text[offset + 1..])),
            '\\' => value.push(characters.next()?.1),
            other => value.push(other),
        }
    }
    None
}

fn missing_requirement(config: &Config) -> Option<String> {
    let CommandKind::Run(runtime) = config.command else {
        return None;
    };
    if which("R").is_none() {
        return Some("because GNU R is not installed".to_owned());
    }
    let libr = Path::new(env!("CARGO_BIN_EXE_ry"))
        .parent()
        .map(|directory| directory.join("libR.so"));
    if runtime == Runtime::Ry && !libr.as_ref().is_some_and(|path| path.is_file()) {
        return Some("because ry's runtime needs libR.so (cargo build -p libr)".to_owned());
    }
    None
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
}

/// One `ry` process: a throwaway project with one R file.
struct Job {
    directory: PathBuf,
    file: String,
    script: String,
    toml: String,
    command: CommandKind,
}

struct Plan {
    jobs: Vec<Job>,
    /// For each example and variant, the job whose output it is compared with.
    single: BTreeMap<(usize, usize), usize>,
    /// For each example with `#>` output, the batch job that runs it.
    batched: BTreeMap<usize, usize>,
    /// For each `run` example with `continue`, the page's session job and the example's segment.
    session: BTreeMap<usize, (usize, usize)>,
}

/// Gives each output block a process of its own, except where a runtime start, which costs
/// seconds, can be shared. The `#>` blocks of one page and configuration share one process, and
/// they cannot stop with an error, so a page like the standard library reference with a hundred
/// of them stays fast. The `continue` blocks of a page share one session too, which runs every
/// `r` block of the page in order once, instead of once per output block.
fn plan_jobs(examples: &[Example]) -> Plan {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("docs");
    let mut plan = Plan {
        jobs: Vec::new(),
        single: BTreeMap::new(),
        batched: BTreeMap::new(),
        session: BTreeMap::new(),
    };
    let mut batches: BTreeMap<(String, CommandKind, String, bool), Vec<usize>> = BTreeMap::new();
    let mut sessions: BTreeMap<(String, CommandKind, String), Vec<usize>> = BTreeMap::new();
    for (example_index, example) in examples.iter().enumerate() {
        let toml = project_toml(&example.config);
        if let Expected::Chunks(_) = example.expected {
            batches
                .entry((
                    example.page.clone(),
                    example.config.command,
                    toml,
                    example.config.dput,
                ))
                .or_default()
                .push(example_index);
            continue;
        }
        if example.config.continues && matches!(example.config.command, CommandKind::Run(_)) {
            sessions
                .entry((example.page.clone(), example.config.command, toml))
                .or_default()
                .push(example_index);
            continue;
        }
        for (variant_index, variant) in example.variants.iter().enumerate() {
            // The excerpt rule skips the findings in the earlier blocks' code.
            let prelude = variant.prelude.concat();
            plan.single
                .insert((example_index, variant_index), plan.jobs.len());
            plan.jobs.push(Job {
                directory: root.join(job_name(example, variant)),
                file: example
                    .config
                    .file
                    .clone()
                    .unwrap_or_else(|| "main.R".to_owned()),
                script: format!("{prelude}{}", variant.code),
                toml: toml.clone(),
                command: example.config.command,
            });
        }
    }
    for ((page, command, toml), members) in sessions {
        let mut blocks: Vec<&str> = Vec::new();
        for &example_index in &members {
            for variant in &examples[example_index].variants {
                let own: Vec<&str> = variant
                    .prelude
                    .iter()
                    .map(String::as_str)
                    .chain([variant.code.as_str()])
                    .collect();
                if own.len() > blocks.len() {
                    blocks = own;
                }
                plan.session
                    .insert(example_index, (plan.jobs.len(), variant.prelude.len()));
            }
        }
        let script: String = blocks
            .iter()
            .enumerate()
            .map(|(block_index, code)| {
                format!("cat(\"\\n<<ry-docs {block_index} 0>>\\n\")\n{code}")
            })
            .collect();
        let slug = page.replace(['/', '.'], "-");
        plan.jobs.push(Job {
            directory: root.join(format!("{slug}-session-{}", plan.jobs.len())),
            file: "main.R".to_owned(),
            script,
            toml,
            command,
        });
    }
    for ((page, command, toml, dput), members) in batches {
        let mut script = String::new();
        for &example_index in &members {
            let Expected::Chunks(chunks) = &examples[example_index].expected else {
                continue;
            };
            script.push_str("tryCatch(local({\n");
            for (chunk_index, chunk) in chunks.iter().enumerate() {
                script.push_str(&format!(
                    "cat(\"\\n<<ry-docs {example_index} {chunk_index}>>\\n\")\n"
                ));
                if dput {
                    script.push_str(&format!("dput({{\n{}}})\n", chunk.code));
                } else {
                    script.push_str(&format!(
                        ".ry_docs <- withVisible({{\n{}}})\nif (.ry_docs$visible) print(.ry_docs$value)\n",
                        chunk.code
                    ));
                }
            }
            script.push_str(
                "}), error = function(error) cat(\"Error:\", conditionMessage(error), \"\\n\"))\n",
            );
            plan.batched.insert(example_index, plan.jobs.len());
        }
        let slug = page.replace(['/', '.'], "-");
        plan.jobs.push(Job {
            directory: root.join(format!("{slug}-batch-{}", plan.jobs.len())),
            file: "main.R".to_owned(),
            script,
            toml,
            command,
        });
    }
    plan
}

fn job_name(example: &Example, variant: &Variant) -> String {
    let slug = example.page.replace(['/', '.'], "-");
    match &variant.label {
        Some(label) => format!("{slug}-{}-{}", example.line, label.to_lowercase()),
        None => format!("{slug}-{}", example.line),
    }
}

fn project_toml(config: &Config) -> String {
    let mut toml = String::from("[check]\n");
    for (key, value) in &config.check_keys {
        toml.push_str(&format!("{key} = {value}\n"));
    }
    toml
}

/// Runs the jobs on every core, since each is a separate process that mostly waits.
fn run_jobs(jobs: &[Job]) -> Vec<String> {
    let next = AtomicUsize::new(0);
    let outputs: Vec<Mutex<String>> = jobs.iter().map(|_| Mutex::new(String::new())).collect();
    let workers = thread::available_parallelism().map_or(4, |count| count.get());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else { break };
                    let output = run_job(job).unwrap_or_else(|problem| format!("<{problem}>"));
                    *outputs[index].lock().expect("an output slot locks") = output;
                }
            });
        }
    });
    outputs
        .into_iter()
        .map(|slot| slot.into_inner().expect("an output slot unlocks"))
        .collect()
}

/// Runs one job and returns stdout and stderr as one stream, interleaved as a terminal shows
/// them, because a page shows what the user sees, not which stream it came from.
fn run_job(job: &Job) -> Result<String, String> {
    if job.directory.exists() {
        fs::remove_dir_all(&job.directory).map_err(|error| error.to_string())?;
    }
    let file = job.directory.join(&job.file);
    let parent = file.parent().ok_or("the R file has no directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(job.directory.join("ry.toml"), &job.toml).map_err(|error| error.to_string())?;
    fs::write(&file, &job.script).map_err(|error| error.to_string())?;

    let (mut reader, writer) = std::io::pipe().map_err(|error| error.to_string())?;
    let mut child = {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ry"));
        match job.command {
            CommandKind::Check => {
                command.arg("check");
            }
            CommandKind::Run(runtime) => {
                command.arg("run");
                if runtime == Runtime::Ry {
                    command.args(["--runtime", "ry"]);
                }
                command.arg(&job.file);
            }
        }
        command
            .current_dir(&job.directory)
            .env("NO_COLOR", "1")
            .env_remove("RY_RUNTIME")
            .stdin(Stdio::null())
            .stdout(writer.try_clone().map_err(|error| error.to_string())?)
            .stderr(writer)
            .spawn()
            .map_err(|error| format!("starting ry: {error}"))?
        // The command, and with it the pipe's write end, drops here, so the read below ends
        // when the child exits.
    };
    let mut output = String::new();
    reader
        .read_to_string(&mut output)
        .map_err(|error| error.to_string())?;
    child.wait().map_err(|error| error.to_string())?;
    fs::remove_dir_all(&job.directory).map_err(|error| error.to_string())?;
    let directory = format!("{}/", job.directory.display());
    Ok(strip_ansi(&output).replace(&directory, ""))
}

fn strip_ansi(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for next in characters.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(character);
        }
    }
    clean
}

enum Verdict {
    Pass,
    Fail {
        message: String,
        blessing: Option<Blessing>,
    },
}

fn judge(example: &Example, example_index: usize, plan: &Plan, outputs: &[String]) -> Vec<Verdict> {
    let location = format!("{}:{}", example.page, example.line);
    match &example.expected {
        Expected::Block { text, body } => example
            .variants
            .iter()
            .enumerate()
            .map(|(variant_index, variant)| {
                let actual = match plan.session.get(&example_index) {
                    Some(&(job, segment)) => {
                        match split_segments(&outputs[job]).remove(&(segment, 0)) {
                            Some(lines) => lines.join("\n"),
                            None => {
                                return Verdict::Fail {
                                    message: format!(
                                        "{location}{}: the page's earlier blocks stopped with an error:\n{}",
                                        tab_suffix(variant),
                                        indent(&outputs[job])
                                    ),
                                    blessing: None,
                                };
                            }
                        }
                    }
                    None => outputs[plan.single[&(example_index, variant_index)]].clone(),
                };
                compare_block(&location, variant, &example.config, text, &actual, *body)
            })
            .collect(),
        Expected::SameAcrossTabs => {
            let findings: Vec<String> = (0..example.variants.len())
                .map(|variant_index| {
                    findings_without_snippets(&outputs[plan.single[&(example_index, variant_index)]])
                })
                .collect();
            let first = &example.variants[0];
            example
                .variants
                .iter()
                .zip(&findings)
                .skip(1)
                .filter(|(_, other)| *other != &findings[0])
                .map(|(variant, other)| Verdict::Fail {
                    message: format!(
                        "{location}: the tabs `{}` and `{}` give different findings (- {}, + {}):\n{}",
                        first.label.as_deref().unwrap_or_default(),
                        variant.label.as_deref().unwrap_or_default(),
                        first.label.as_deref().unwrap_or_default(),
                        variant.label.as_deref().unwrap_or_default(),
                        diff(&findings[0], other)
                    ),
                    blessing: None,
                })
                .collect()
        }
        Expected::Chunks(chunks) => {
            let raw = &outputs[plan.batched[&example_index]];
            let segments = split_segments(raw);
            let mut verdicts = Vec::new();
            let mut blessing_failures = Vec::new();
            for (chunk_index, chunk) in chunks.iter().enumerate() {
                let actual = segments.get(&(example_index, chunk_index));
                let actual_lines: Vec<String> = match actual {
                    Some(lines) => lines.clone(),
                    None => {
                        verdicts.push(Verdict::Fail {
                            message: format!(
                                "{}:{}: the batch stopped before this example ran:\n{}",
                                example.page,
                                chunk.lines.start + 1,
                                indent(raw)
                            ),
                            blessing: None,
                        });
                        continue;
                    }
                };
                let expected: Vec<String> =
                    chunk.expected.iter().map(|line| line.trim_end().to_owned()).collect();
                if expected != actual_lines {
                    let message = format!(
                        "{}:{}: the `#>` output differs (- page, + ry):\n{}",
                        example.page,
                        chunk.lines.start + 1,
                        diff(&expected.join("\n"), &actual_lines.join("\n"))
                    );
                    let replacement = actual_lines
                        .iter()
                        .map(|line| {
                            if line.is_empty() {
                                "#>".to_owned()
                            } else {
                                format!("#> {line}")
                            }
                        })
                        .collect();
                    blessing_failures.push((message, chunk.lines, replacement));
                }
            }
            verdicts.extend(blessing_failures.into_iter().map(|(message, lines, replacement)| {
                Verdict::Fail {
                    message,
                    blessing: Some(Blessing { lines, replacement }),
                }
            }));
            verdicts
        }
    }
}

fn compare_block(
    location: &str,
    variant: &Variant,
    config: &Config,
    expected: &str,
    actual: &str,
    body: Option<PageLines>,
) -> Verdict {
    let tab = tab_suffix(variant);
    if config.exact {
        let expected = normalize_exact(expected);
        let actual = normalize_exact(actual);
        if expected == actual {
            return Verdict::Pass;
        }
        return Verdict::Fail {
            message: format!(
                "{location}{tab}: the output differs (- page, + ry):\n{}",
                diff(&expected, &actual)
            ),
            blessing: body.map(|lines| Blessing {
                lines,
                replacement: actual.lines().map(str::to_owned).collect(),
            }),
        };
    }
    match find_excerpt(expected, actual) {
        None => Verdict::Pass,
        Some(missing) => Verdict::Fail {
            message: format!(
                "{location}{tab}: the output has no line matching `{missing}` (after the lines matched before it)\n  page excerpt:\n{}\n  ry output:\n{}",
                indent(expected),
                indent(actual)
            ),
            blessing: None,
        },
    }
}

/// The findings without their source snippets, whose line numbers differ between two spellings
/// of one program.
fn findings_without_snippets(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            let after_number =
                line.trim_start_matches(|character: char| character.is_ascii_digit());
            !line.is_empty()
                && !line.starts_with("--[")
                && !line.starts_with('|')
                && !(after_number.len() < line.len() && after_number.starts_with(" |"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tab_suffix(variant: &Variant) -> String {
    match &variant.label {
        Some(label) => format!(" (tab {label})"),
        None => String::new(),
    }
}

/// Whole-output comparison ignores trailing whitespace on each line and blank lines at the end,
/// which a page cannot show reliably.
fn normalize_exact(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_end_matches('\n').to_owned()
}

/// The excerpt rule: every non-blank line of the page's excerpt must equal a line of the real
/// output, both trimmed, and the matches must come in the page's order. Leading whitespace and
/// skipped lines are free because an excerpt deliberately drops the summary and the source
/// snippet, and indents freely. A message the page wraps by hand may span several excerpt lines,
/// which then match one output line when joined with single spaces. Returns the first excerpt
/// line that has no match.
fn find_excerpt(expected: &str, actual: &str) -> Option<String> {
    let wanted: Vec<&str> = expected
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let have: Vec<&str> = actual.lines().map(str::trim).collect();
    let mut position = 0;
    let mut wanted_index = 0;
    while wanted_index < wanted.len() {
        let mut matched = None;
        while position < have.len() && matched.is_none() {
            matched = match_wrapped(&wanted[wanted_index..], have[position]);
            position += 1;
        }
        match matched {
            Some(consumed) => wanted_index += consumed,
            None => return Some(wanted[wanted_index].to_owned()),
        }
    }
    None
}

/// How many of the `wanted` lines, joined with spaces, make up exactly `line`.
fn match_wrapped(wanted: &[&str], line: &str) -> Option<usize> {
    let mut joined = String::new();
    for (count, piece) in wanted.iter().enumerate() {
        if count > 0 {
            joined.push(' ');
        }
        joined.push_str(piece);
        if joined == line {
            return Some(count + 1);
        }
        if !line.starts_with(&format!("{joined} ")) {
            return None;
        }
    }
    None
}

/// Splits a batch's output at the `<<ry-docs EXAMPLE CHUNK>>` lines its script prints, dropping
/// the blank line each marker's leading newline leaves behind.
fn split_segments(raw: &str) -> BTreeMap<(usize, usize), Vec<String>> {
    let mut segments: BTreeMap<(usize, usize), Vec<String>> = BTreeMap::new();
    let mut current: Option<(usize, usize)> = None;
    for line in raw.lines() {
        if let Some(key) = line
            .strip_prefix("<<ry-docs ")
            .and_then(|rest| rest.strip_suffix(">>"))
            .and_then(|rest| rest.split_once(' '))
            .and_then(|(example, chunk)| Some((example.parse().ok()?, chunk.parse().ok()?)))
        {
            current = Some(key);
            segments.insert(key, Vec::new());
            continue;
        }
        if let Some(key) = current
            && let Some(lines) = segments.get_mut(&key)
        {
            lines.push(line.trim_end().to_owned());
        }
    }
    for lines in segments.values_mut() {
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
    }
    segments
}

fn diff(expected: &str, actual: &str) -> String {
    TextDiff::from_lines(&format!("{expected}\n"), &format!("{actual}\n"))
        .unified_diff()
        .context_radius(3)
        .to_string()
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replaces each blessed range with its new lines, from the bottom up so earlier ranges keep
/// their line numbers.
fn apply_blessings(text: &str, mut blessings: Vec<Blessing>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    blessings.sort_by_key(|blessing| std::cmp::Reverse(blessing.lines.start));
    for blessing in blessings {
        let prefix = " ".repeat(blessing.lines.indent);
        let replacement = blessing.replacement.iter().map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{prefix}{line}")
            }
        });
        lines.splice(blessing.lines.start..blessing.lines.end, replacement);
    }
    let mut result = lines.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    result
}
