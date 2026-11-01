//! Scenario tests: scripted user journeys through the editor and the CLI.
//!
//! Each `tests/scenarios/*.scenario` file is one journey, written in the small line-based
//! language that `docs/src/content/docs/contributing/robustness.md` specifies. Editor steps
//! drive the real language server in-process through the deterministic simulator
//! (`ry::server::simulation`), settling after every step; CLI steps run the real `ry` binary in
//! the scenario's workspace. When a journey ends, its editor session is held against a server
//! started from scratch on the same disk and buffers.
//!
//! `SCENARIO=<name>` runs one scenario.

use {
    async_lsp::lsp_types::{self, Url},
    ry::server::simulation::{
        ClientOptions, Notification, RequestKind, Simulation, position_of,
        verify_against_fresh_start,
    },
    serde_json::Value,
    std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::OnceLock,
    },
};

#[test]
fn scenarios_hold() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
    let mut paths: Vec<PathBuf> = fs::read_dir(&directory)
        .expect("the scenario directory exists")
        .map(|entry| entry.expect("a scenario directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "scenario")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no scenarios found in {}",
        directory.display()
    );
    let only = std::env::var("SCENARIO").ok();
    let mut failures = Vec::new();
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("scenario names are UTF-8")
            .to_owned();
        if only.as_ref().is_some_and(|only| only != &name) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("a scenario reads");
        match run_scenario(&name, &text) {
            Ok(Outcome::Passed(steps)) => eprintln!("scenario {name}: {steps} steps passed"),
            Ok(Outcome::Skipped(reason)) => eprintln!("scenario {name}: skipped, {reason}"),
            Err(failure) => failures.push(format!("scenario {name}: {failure}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

enum Outcome {
    Passed(usize),
    Skipped(String),
}

struct Step {
    line: usize,
    words: Vec<String>,
    body: Option<String>,
}

fn run_scenario(name: &str, text: &str) -> Result<Outcome, String> {
    let steps = parse(text)?;
    for step in &steps {
        if step.words[0] == "requires"
            && let Some(reason) = missing_requirement(&step.words[1..])
        {
            return Ok(Outcome::Skipped(reason));
        }
    }
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("scenarios")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root)
            .map_err(|error| format!("clearing {}: {error}", root.display()))?;
    }
    fs::create_dir_all(&root).map_err(|error| format!("creating {}: {error}", root.display()))?;
    let mut journey = Journey {
        root,
        options: ClientOptions {
            pull_diagnostics: false,
            utf8: false,
        },
        editor: None,
        open: BTreeMap::new(),
        last_run: None,
    };
    for step in &steps {
        journey.run(step).map_err(|problem| {
            format!("line {}: `{}`: {problem}", step.line, step.words.join(" "))
        })?;
    }
    if let Some(editor) = journey.editor.as_mut() {
        verify_against_fresh_start(editor, &journey.root, journey.options, &journey.open)
            .map_err(|problem| format!("at the end of the journey: {problem}"))?;
    }
    Ok(Outcome::Passed(steps.len()))
}

/// Splits the scenario into steps: one command per line, `#` comments, and `|` lines that
/// form the body of the command above them.
fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps: Vec<Step> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(content) = line.strip_prefix('|') {
            let content = content.strip_prefix(' ').unwrap_or(content);
            let Some(step) = steps.last_mut() else {
                return Err(format!(
                    "line {}: a `|` body line without a command",
                    index + 1
                ));
            };
            let body = step.body.get_or_insert_with(String::new);
            body.push_str(content);
            body.push('\n');
            continue;
        }
        steps.push(Step {
            line: index + 1,
            words: split_words(line).map_err(|problem| format!("line {}: {problem}", index + 1))?,
            body: None,
        });
    }
    Ok(steps)
}

/// Whitespace-separated words, where a double-quoted word may hold spaces and the escapes
/// `\n`, `\t`, `\"` and `\\`.
fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut characters = line.chars().peekable();
    while let Some(&character) = characters.peek() {
        if character.is_whitespace() {
            characters.next();
            continue;
        }
        let mut word = String::new();
        if character == '"' {
            characters.next();
            loop {
                match characters.next() {
                    None => return Err("an unterminated string".to_owned()),
                    Some('"') => break,
                    Some('\\') => match characters.next() {
                        Some('n') => word.push('\n'),
                        Some('t') => word.push('\t'),
                        Some(other) => word.push(other),
                        None => return Err("a dangling `\\`".to_owned()),
                    },
                    Some(other) => word.push(other),
                }
            }
        } else {
            while let Some(&next) = characters.peek() {
                if next.is_whitespace() {
                    break;
                }
                word.push(next);
                characters.next();
            }
        }
        words.push(word);
    }
    Ok(words)
}

fn missing_requirement(requirements: &[String]) -> Option<String> {
    for requirement in requirements {
        match requirement.as_str() {
            "gnu-r" => {
                if which("Rscript").is_none() {
                    return Some("GNU R is not installed".to_owned());
                }
            }
            "ry-runtime" => {
                if which("Rscript").is_none() {
                    return Some(
                        "ry's runtime loads base R from GNU R, which is not installed".to_owned(),
                    );
                }
                if let Err(error) = libr_path() {
                    return Some(format!("ry's runtime is missing: {error}"));
                }
            }
            other => return Some(format!("unknown requirement `{other}`")),
        }
    }
    None
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
}

/// ry's `libR.so`, built first: `cargo test` of this package does not build libr, and `ry run`
/// loads the copy beside the binary, which only `cargo build` refreshes.
fn libr_path() -> Result<&'static PathBuf, String> {
    static LIBR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    LIBR.get_or_init(|| {
        let profile_directory = Path::new(env!("CARGO_BIN_EXE_ry"))
            .parent()
            .ok_or("the ry binary has no directory")?;
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "-q", "-p", "libr"]);
        if profile_directory
            .file_name()
            .is_some_and(|name| name == "release")
        {
            build.arg("--release");
        }
        let status = build
            .status()
            .map_err(|error| format!("building libr: {error}"))?;
        if !status.success() {
            return Err("building libr failed".to_owned());
        }
        Ok(profile_directory.join("libR.so"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// ry's R home, assembled once per test run from this build's `libR` and headers, whose
/// `bin/R CMD INSTALL` is the front end a user installs packages through.
fn r_home() -> Result<&'static PathBuf, String> {
    static HOME: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scenario-r-home");
        let profile_directory = Path::new(env!("CARGO_BIN_EXE_ry"))
            .parent()
            .ok_or("the ry binary has no directory")?;
        // `ry-rcmd` is another package's binary, so cargo test does not build it.
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "-q", "-p", "rcmd", "--bin", "ry-rcmd"]);
        if profile_directory
            .file_name()
            .is_some_and(|name| name == "release")
        {
            build.arg("--release");
        }
        let status = build
            .status()
            .map_err(|error| format!("building ry-rcmd: {error}"))?;
        if !status.success() {
            return Err("building ry-rcmd failed".to_owned());
        }
        // The home links GNU R's base packages, from `R RHOME`.
        let gnu_r_home = Command::new("R")
            .arg("RHOME")
            .output()
            .map_err(|error| format!("running `R RHOME`: {error}"))?;
        let gnu_r_home = PathBuf::from(String::from_utf8_lossy(&gnu_r_home.stdout).trim());
        rcmd::assemble(
            &root,
            &rcmd::HomeSources {
                include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("../libr/include"),
                libr: libr_path()?,
                rcmd: &profile_directory.join("ry-rcmd"),
                gnu_r_home: &gnu_r_home,
                toolchain: rcmd::Toolchain::default(),
            },
        )
        .map_err(|error| format!("assembling ry's R home: {error}"))?;
        Ok(root)
    })
    .as_ref()
    .map_err(Clone::clone)
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Journey {
    root: PathBuf,
    options: ClientOptions,
    editor: Option<Simulation>,
    /// The editor's open buffers by URI: version and text.
    open: BTreeMap<String, (i32, String)>,
    last_run: Option<Run>,
}

impl Journey {
    fn run(&mut self, step: &Step) -> Result<(), String> {
        let words: Vec<&str> = step.words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["requires", ..] => Ok(()),
            ["client", options @ ..] => {
                if self.editor.is_some() {
                    return Err(
                        "the client's options must come before the first editor step".to_owned(),
                    );
                }
                for option in options {
                    match *option {
                        "pull" => self.options.pull_diagnostics = true,
                        "utf8" => self.options.utf8 = true,
                        other => return Err(format!("unknown client option `{other}`")),
                    }
                }
                Ok(())
            }
            ["file", path] => {
                let body = step.body.clone().ok_or("`file` needs a `|` body")?;
                let existed = self.root.join(path).exists();
                self.write(path, &body)?;
                self.watched(
                    path,
                    if existed {
                        lsp_types::FileChangeType::CHANGED
                    } else {
                        lsp_types::FileChangeType::CREATED
                    },
                );
                Ok(())
            }
            ["remove", path] => {
                fs::remove_file(self.root.join(path)).map_err(|error| error.to_string())?;
                self.watched(path, lsp_types::FileChangeType::DELETED);
                Ok(())
            }
            ["open", path] => {
                let text = fs::read_to_string(self.root.join(path))
                    .map_err(|error| format!("reading {path}: {error}"))?;
                let uri = self.uri(path);
                self.open.insert(uri.to_string(), (0, text.clone()));
                self.editor().notify(Notification::DidOpen(
                    lsp_types::DidOpenTextDocumentParams {
                        text_document: lsp_types::TextDocumentItem::new(
                            uri,
                            "r".to_owned(),
                            0,
                            text,
                        ),
                    },
                ));
                self.settle();
                Ok(())
            }
            ["close", path] => {
                let uri = self.uri(path);
                self.open
                    .remove(uri.as_str())
                    .ok_or(format!("{path} is not open"))?;
                self.editor().notify(Notification::DidClose(
                    lsp_types::DidCloseTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier::new(uri),
                    },
                ));
                self.settle();
                Ok(())
            }
            ["save", path] => {
                let uri = self.uri(path);
                let text = self.buffer(path)?.to_owned();
                self.write(path, &text)?;
                self.editor().notify(Notification::DidSave(
                    lsp_types::DidSaveTextDocumentParams {
                        text_document: lsp_types::TextDocumentIdentifier::new(uri),
                        text: None,
                    },
                ));
                self.watched(path, lsp_types::FileChangeType::CHANGED);
                Ok(())
            }
            ["type", path, "after", anchor, text] => {
                let offset = self.find(path, anchor)? + anchor.len();
                self.type_text(path, offset, text)
            }
            ["type", path, "at", "end", text] => {
                let offset = self.buffer(path)?.len();
                self.type_text(path, offset, text)
            }
            ["replace", path, old, new] => {
                let start = self.find(path, old)?;
                self.change(path, start, start + old.len(), new);
                self.settle();
                Ok(())
            }
            ["erase", path, old] => {
                let start = self.find(path, old)?;
                self.change(path, start, start + old.len(), "");
                self.settle();
                Ok(())
            }
            ["rename", path, anchor, new_name] => {
                let response = self.request_at(RequestKind::Rename, path, anchor, new_name)?;
                self.apply_workspace_edit(&response)
            }
            ["format", path] => {
                let response = self.request_at(RequestKind::Formatting, path, "", "")?;
                let uri = self.uri(path).to_string();
                self.apply_text_edits(&uri, response.as_array().cloned().unwrap_or_default())
            }
            ["expect", "diagnostics", path, "none"] => {
                let diagnostics = self.diagnostics(path)?;
                if diagnostics.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "expected no diagnostics, found:\n{}",
                        diagnostics.join("\n")
                    ))
                }
            }
            ["expect", "diagnostics", path, needle] => {
                let diagnostics = self.diagnostics(path)?;
                if diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.contains(needle))
                {
                    Ok(())
                } else {
                    Err(format!(
                        "no diagnostic mentions `{needle}`; found:\n{}",
                        diagnostics.join("\n")
                    ))
                }
            }
            ["expect", "no", "diagnostic", path, needle] => {
                let diagnostics = self.diagnostics(path)?;
                match diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.contains(needle))
                {
                    None => Ok(()),
                    Some(found) => Err(format!("unexpected diagnostic: {found}")),
                }
            }
            ["expect", "hover", path, anchor, needle] => {
                let hover = self.request_at(RequestKind::Hover, path, anchor, "")?;
                let rendered = hover.to_string();
                if rendered.contains(needle) {
                    Ok(())
                } else {
                    Err(format!("the hover lacks `{needle}`: {rendered}"))
                }
            }
            ["expect", "definition", path, anchor, target_path, target] => {
                let response = self.request_at(RequestKind::Definition, path, anchor, "")?;
                let location = match &response {
                    Value::Array(locations) => locations.first().cloned(),
                    Value::Object(_) => Some(response.clone()),
                    _ => None,
                }
                .ok_or(format!("no definition: {response}"))?;
                let uri = location["uri"].as_str().unwrap_or_default();
                if uri != self.uri(target_path).as_str() {
                    return Err(format!("the definition is in {uri}, not {target_path}"));
                }
                let target_text = self.text_of(target_path)?;
                let start: lsp_types::Position =
                    serde_json::from_value(location["range"]["start"].clone())
                        .map_err(|error| error.to_string())?;
                let expected = target_text
                    .find(target)
                    .ok_or(format!("`{target}` is not in {target_path}"))?;
                if position_of(&target_text, expected, self.options.utf8) == start {
                    Ok(())
                } else {
                    Err(format!(
                        "the definition starts at {start:?}, not at `{target}`"
                    ))
                }
            }
            ["expect", "completion", path, "after", anchor, label] => {
                let offset = self.find(path, anchor)? + anchor.len();
                let response = self.request_offset(RequestKind::Completion, path, offset, "")?;
                let items = match &response {
                    Value::Array(items) => items.clone(),
                    Value::Object(list) => list
                        .get("items")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                if items
                    .iter()
                    .any(|item| item["label"].as_str() == Some(label))
                {
                    Ok(())
                } else {
                    Err(format!(
                        "no completion labelled `{label}` among {} items",
                        items.len()
                    ))
                }
            }
            ["expect", "signature", path, "after", anchor, needle] => {
                let offset = self.find(path, anchor)? + anchor.len();
                let help = self.request_offset(RequestKind::SignatureHelp, path, offset, "")?;
                let active = help["activeSignature"].as_u64().unwrap_or_default() as usize;
                let signature = &help["signatures"][active];
                let label = signature["label"].as_str().unwrap_or_default();
                // A parameter label is its text, or offsets into the signature's label when the
                // client supports them.
                let parameter = help["activeParameter"]
                    .as_u64()
                    .map(|index| &signature["parameters"][index as usize]["label"])
                    .and_then(|parameter| match parameter {
                        Value::String(text) => Some(text.as_str()),
                        Value::Array(span) => {
                            let start = span.first()?.as_u64()? as usize;
                            let end = span.get(1)?.as_u64()? as usize;
                            label.get(start..end)
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                // The active parameter renders in brackets, as an editor highlights it.
                let rendered = label.replacen(parameter, &format!("[{parameter}]"), 1);
                if !parameter.is_empty() && rendered.contains(needle) {
                    Ok(())
                } else {
                    Err(format!(
                        "the signature help reads `{rendered}`, not `{needle}`"
                    ))
                }
            }
            ["expect", "symbol", path, name] => {
                let symbols = self.request_at(RequestKind::DocumentSymbol, path, "", "")?;
                let mut outline = Vec::new();
                flatten_symbols(&symbols, "", &mut outline);
                if outline.iter().any(|entry| entry == name) {
                    Ok(())
                } else {
                    Err(format!("the outline lacks `{name}`: {outline:?}"))
                }
            }
            ["expect", "workspace-symbol", query, path, name] => {
                let opened = self.uri(path).to_string();
                let symbols = self.request_offset(RequestKind::WorkspaceSymbol, path, 0, query)?;
                let found = symbols.as_array().into_iter().flatten().any(|symbol| {
                    symbol["name"].as_str() == Some(name)
                        && symbol["location"]["uri"].as_str() == Some(opened.as_str())
                });
                if found {
                    Ok(())
                } else {
                    Err(format!(
                        "searching `{query}` finds no `{name}` in {path}: {symbols}"
                    ))
                }
            }
            ["fix", path, anchor, title] => {
                let response = self.request_at(RequestKind::CodeAction, path, anchor, "")?;
                let action = response
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|action| action["title"].as_str() == Some(title))
                    .ok_or(format!("no code action titled `{title}`: {response}"))?;
                self.apply_workspace_edit(&action["edit"])
            }
            ["expect", "fold", path, from, to] => {
                let folds = self.request_at(RequestKind::FoldingRange, path, "", "")?;
                let text = self.text_of(path)?;
                let line_of = |needle: &str| {
                    text.find(needle)
                        .map(|at| text[..at].matches('\n').count() as u64)
                        .ok_or(format!("`{needle}` is not in {path}"))
                };
                let (start, end) = (line_of(from)?, line_of(to)?);
                let found = folds.as_array().into_iter().flatten().any(|fold| {
                    fold["startLine"].as_u64() == Some(start)
                        && fold["endLine"].as_u64() == Some(end)
                });
                if found {
                    Ok(())
                } else {
                    Err(format!("no fold from `{from}` to `{to}`: {folds}"))
                }
            }
            ["expect", "selection", path, anchor, expected] => {
                let response = self.request_at(RequestKind::SelectionRange, path, anchor, "")?;
                let text = self.text_of(path)?;
                let mut steps = Vec::new();
                let mut selection = response.get(0);
                while let Some(current) = selection {
                    let range: lsp_types::Range = serde_json::from_value(current["range"].clone())
                        .map_err(|error| error.to_string())?;
                    let start = offset_of(&text, range.start, self.options.utf8);
                    let end = offset_of(&text, range.end, self.options.utf8);
                    steps.push(text.get(start..end).unwrap_or_default().to_owned());
                    selection = current.get("parent");
                }
                if steps.iter().any(|step| step == expected) {
                    Ok(())
                } else {
                    Err(format!("no selection step covers `{expected}`: {steps:?}"))
                }
            }
            ["expect", "buffer", path, "lacks", needle] => {
                let buffer = self.buffer(path)?;
                if buffer.contains(needle) {
                    Err(format!("the buffer still contains `{needle}`:\n{buffer}"))
                } else {
                    Ok(())
                }
            }
            ["expect", "buffer", path, needle] => {
                let buffer = self.buffer(path)?;
                if buffer.contains(needle) {
                    Ok(())
                } else {
                    Err(format!("the buffer lacks `{needle}`:\n{buffer}"))
                }
            }
            ["expect", "file", path, needle] => {
                let text =
                    fs::read_to_string(self.root.join(path)).map_err(|error| error.to_string())?;
                if text.contains(needle) {
                    Ok(())
                } else {
                    Err(format!("{path} lacks `{needle}`:\n{text}"))
                }
            }
            ["run", "ry", arguments @ ..] => {
                let output = Command::new(env!("CARGO_BIN_EXE_ry"))
                    .args(arguments)
                    .current_dir(&self.root)
                    .output()
                    .map_err(|error| format!("running ry: {error}"))?;
                self.last_run = Some(Run {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
                Ok(())
            }
            ["install", package] => {
                let front_end = r_home()?.join("bin/R");
                let output = Command::new(&front_end)
                    .args(["CMD", "INSTALL", "-l", "library", package])
                    .current_dir(&self.root)
                    .output()
                    .map_err(|error| format!("running {}: {error}", front_end.display()))?;
                self.last_run = Some(Run {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
                Ok(())
            }
            ["expect", "exit", code] => {
                let run = self.last_run()?;
                let expected: i32 = code
                    .parse()
                    .map_err(|_| format!("`{code}` is not an exit code"))?;
                if run.code == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "exit {} instead of {expected}\nstdout:\n{}\nstderr:\n{}",
                        run.code, run.stdout, run.stderr
                    ))
                }
            }
            ["expect", stream @ ("stdout" | "stderr"), "lacks", needle] => {
                let run = self.last_run()?;
                let output = if *stream == "stdout" {
                    &run.stdout
                } else {
                    &run.stderr
                };
                if output.contains(needle) {
                    Err(format!("{stream} contains `{needle}`:\n{output}"))
                } else {
                    Ok(())
                }
            }
            ["expect", stream @ ("stdout" | "stderr"), needle] => {
                let run = self.last_run()?;
                let output = if *stream == "stdout" {
                    &run.stdout
                } else {
                    &run.stderr
                };
                if output.contains(needle) {
                    Ok(())
                } else {
                    Err(format!("{stream} lacks `{needle}`:\n{output}"))
                }
            }
            _ => Err("unknown step".to_owned()),
        }
    }

    fn editor(&mut self) -> &mut Simulation {
        self.editor
            .get_or_insert_with(|| Simulation::start(&self.root, self.options))
    }

    fn settle(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.settle();
        }
    }

    fn uri(&self, path: &str) -> Url {
        Url::from_file_path(self.root.join(path)).expect("workspace paths are absolute")
    }

    fn write(&self, path: &str, text: &str) -> Result<(), String> {
        let full = self.root.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(full, text).map_err(|error| error.to_string())
    }

    /// Tells a running editor what the file watcher saw.
    fn watched(&mut self, path: &str, kind: lsp_types::FileChangeType) {
        let uri = self.uri(path);
        if let Some(editor) = self.editor.as_mut() {
            editor.notify(Notification::DidChangeWatchedFiles(
                lsp_types::DidChangeWatchedFilesParams {
                    changes: vec![lsp_types::FileEvent::new(uri, kind)],
                },
            ));
            editor.settle();
        }
    }

    fn buffer(&self, path: &str) -> Result<&str, String> {
        self.open
            .get(self.uri(path).as_str())
            .map(|(_, text)| text.as_str())
            .ok_or(format!("{path} is not open"))
    }

    /// The buffer when the file is open, its disk content otherwise.
    fn text_of(&self, path: &str) -> Result<String, String> {
        match self.buffer(path) {
            Ok(text) => Ok(text.to_owned()),
            Err(_) => fs::read_to_string(self.root.join(path)).map_err(|error| error.to_string()),
        }
    }

    fn find(&self, path: &str, anchor: &str) -> Result<usize, String> {
        let buffer = self.buffer(path)?;
        buffer.find(anchor).ok_or(format!(
            "`{anchor}` is not in the buffer of {path}:\n{buffer}"
        ))
    }

    /// Replaces the bytes `start..end` of an open buffer through one `didChange`.
    fn change(&mut self, path: &str, start: usize, end: usize, text: &str) {
        let uri = self.uri(path);
        let utf8 = self.options.utf8;
        let (version, buffer) = self
            .open
            .get_mut(uri.as_str())
            .expect("changes target open buffers");
        *version += 1;
        let range = lsp_types::Range::new(
            position_of(buffer, start, utf8),
            position_of(buffer, end, utf8),
        );
        buffer.replace_range(start..end, text);
        let version = *version;
        self.editor().notify(Notification::DidChange(
            lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier::new(uri, version),
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: Some(range),
                    range_length: None,
                    text: text.to_owned(),
                }],
            },
        ));
    }

    /// One `didChange` per character with no settling in between, the way typing arrives.
    fn type_text(&mut self, path: &str, mut offset: usize, text: &str) -> Result<(), String> {
        for character in text.chars() {
            let mut encoded = [0; 4];
            self.change(path, offset, offset, character.encode_utf8(&mut encoded));
            offset += character.len_utf8();
        }
        self.settle();
        Ok(())
    }

    fn request_offset(
        &mut self,
        kind: RequestKind,
        path: &str,
        offset: usize,
        new_name: &str,
    ) -> Result<Value, String> {
        let text = self.text_of(path)?;
        let position = position_of(&text, offset, self.options.utf8);
        let uri = self.uri(path);
        let editor = self.editor();
        let id = editor.request_with_name(kind, &uri, position, new_name);
        editor.settle();
        match editor.take_response(id) {
            Some(Ok(value)) => Ok(value),
            Some(Err(error)) => Err(format!("{kind:?} failed: {error:?}")),
            None => Err(format!("{kind:?} got no response from an idle server")),
        }
    }

    fn request_at(
        &mut self,
        kind: RequestKind,
        path: &str,
        anchor: &str,
        new_name: &str,
    ) -> Result<Value, String> {
        let offset = if anchor.is_empty() {
            0
        } else {
            self.find(path, anchor)?
        };
        self.request_offset(kind, path, offset, new_name)
    }

    fn diagnostics(&mut self, path: &str) -> Result<Vec<String>, String> {
        let report = self.request_offset(RequestKind::Diagnostic, path, 0, "")?;
        let items = report["items"].as_array().cloned().unwrap_or_default();
        Ok(items
            .iter()
            .map(|item| {
                format!(
                    "{} {} (line {})",
                    item["code"].as_str().unwrap_or_default(),
                    item["message"].as_str().unwrap_or_default(),
                    item["range"]["start"]["line"].as_u64().unwrap_or_default() + 1
                )
            })
            .collect())
    }

    fn last_run(&self) -> Result<&Run, String> {
        self.last_run
            .as_ref()
            .ok_or("no `run` step came before".to_owned())
    }

    /// Applies a rename's edits the way an editor does: to the buffer of an open document,
    /// and to a closed one by opening it first.
    fn apply_workspace_edit(&mut self, edit: &Value) -> Result<(), String> {
        let changes = edit["changes"].as_object().cloned().unwrap_or_default();
        if changes.is_empty() {
            return Err(format!("the edit changed nothing: {edit}"));
        }
        for (uri, edits) in changes {
            if !self.open.contains_key(&uri) {
                let path = Url::parse(&uri)
                    .map_err(|error| error.to_string())?
                    .to_file_path()
                    .map_err(|()| format!("{uri} is not a file"))?;
                let relative = path
                    .strip_prefix(&self.root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .into_owned();
                self.run(&Step {
                    line: 0,
                    words: vec!["open".to_owned(), relative],
                    body: None,
                })?;
            }
            self.apply_text_edits(&uri, edits.as_array().cloned().unwrap_or_default())?;
        }
        Ok(())
    }

    /// Applies LSP text edits to an open buffer, last edit first so earlier offsets hold.
    fn apply_text_edits(&mut self, uri: &str, edits: Vec<Value>) -> Result<(), String> {
        let parsed = Url::parse(uri).map_err(|error| error.to_string())?;
        let path = parsed
            .to_file_path()
            .map_err(|()| format!("{uri} is not a file"))?
            .strip_prefix(&self.root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .into_owned();
        let mut ranges = Vec::new();
        for edit in edits {
            let range: lsp_types::Range =
                serde_json::from_value(edit["range"].clone()).map_err(|error| error.to_string())?;
            let text = edit["newText"].as_str().unwrap_or_default().to_owned();
            let buffer = self.buffer(&path)?;
            ranges.push((
                offset_of(buffer, range.start, self.options.utf8),
                offset_of(buffer, range.end, self.options.utf8),
                text,
            ));
        }
        ranges.sort_by(|left, right| right.0.cmp(&left.0));
        for (start, end, text) in ranges {
            self.change(&path, start, end, &text);
        }
        self.settle();
        Ok(())
    }
}

/// The byte offset of a protocol position, clamped to the text.
/// Each document symbol as its path of names from the top, such as `Setup/limit`.
fn flatten_symbols(symbols: &Value, prefix: &str, outline: &mut Vec<String>) {
    for symbol in symbols.as_array().into_iter().flatten() {
        let name = format!("{prefix}{}", symbol["name"].as_str().unwrap_or_default());
        flatten_symbols(&symbol["children"], &format!("{name}/"), outline);
        outline.push(name);
    }
}

fn offset_of(text: &str, position: lsp_types::Position, utf8: bool) -> usize {
    let mut line_start = 0;
    for _ in 0..position.line {
        match text[line_start..].find('\n') {
            Some(index) => line_start += index + 1,
            None => return text.len(),
        }
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |index| line_start + index);
    let line = &text[line_start..line_end];
    let mut units = 0;
    for (index, character) in line.char_indices() {
        if units >= position.character as usize {
            return line_start + index;
        }
        units += if utf8 {
            character.len_utf8()
        } else {
            character.len_utf16()
        };
    }
    line_end
}
