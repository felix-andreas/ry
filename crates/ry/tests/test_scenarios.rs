//! Scenario tests: scripted user journeys through the editor and the CLI.
//!
//! Each `tests/scenarios/*.scenario` file is one journey, written in the small line-based
//! language that `docs/src/content/docs/development/robustness.md` specifies. Editor steps
//! drive the real language server in-process through the deterministic simulator
//! (`ry::server::simulation`), settling after every step; CLI steps run the real `ry` binary in
//! the scenario's workspace. When a journey ends, its editor session is held against a server
//! started from scratch on the same disk and buffers.
//!
//! `SCENARIO=<name>` runs one scenario.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use {
    async_lsp::lsp_types::{self, Url},
    ry::server::simulation::{
        ClientOptions, Notification, Outgoing, RequestKind, Simulation, position_of,
        verify_against_fresh_start,
    },
    serde_json::{Value, json},
    std::{
        collections::{BTreeMap, VecDeque},
        fs,
        io::{BufRead, BufReader, Read},
        path::{Path, PathBuf},
        process::{Child, ChildStdin, Command, Stdio},
        sync::{
            OnceLock,
            mpsc::{self, Receiver},
        },
        time::{Duration, Instant},
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
            cold: false,
        },
        editor: None,
        open: BTreeMap::new(),
        last_run: None,
        debugger: None,
        spawned: None,
        hide_gnu_r: false,
        environment: BTreeMap::new(),
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
                if ry_base().is_none() {
                    return Some(
                        "ry's base R is not built; build it with `just base-r`".to_owned(),
                    );
                }
                if let Err(error) = libr_path() {
                    return Some(format!("ry's runtime is missing: {error}"));
                }
            }
            other => {
                let Some(package) = other.strip_prefix("r-package:") else {
                    return Some(format!("unknown requirement `{other}`"));
                };
                let installed = which("Rscript").is_some_and(|rscript| {
                    Command::new(rscript)
                        .args([
                            "-e",
                            &format!(
                                "quit(status = !requireNamespace('{package}', quietly = TRUE))"
                            ),
                        ])
                        .output()
                        .is_ok_and(|output| output.status.success())
                });
                if !installed {
                    return Some(format!("the R package {package} is not installed"));
                }
            }
        }
    }
    None
}

/// The base R tree ry's runtime loads in scenarios: the one `R_HOME` names, which emulates that
/// GNU R, else ry's own ([`ry_base`]).
fn base_r() -> Option<PathBuf> {
    std::env::var_os("R_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(ry_base)
}

/// ry's own base R tree (`just base-r`) where the `ry` under test finds it, which scenarios that
/// hide GNU R or embed the tree need even when `R_HOME` names a GNU R. Cargo puts test binaries
/// in a different directory from `ry`, so the lookup starts from `ry`'s.
fn ry_base() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("RY_BASE_R").filter(|base| !base.is_empty()) {
        return Some(PathBuf::from(base)).filter(|base| rcmd::base::holds_base_r(base));
    }
    let directory = Path::new(env!("CARGO_BIN_EXE_ry")).parent()?;
    [directory.join("base-r"), directory.join("../base-r")]
        .into_iter()
        .find(|candidate| rcmd::base::is_complete(candidate))
}

/// `PATH` without the directories that hold GNU R's `R` or `Rscript`, for `hide gnu-r`.
fn path_without_gnu_r() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|directory| !directory.join("R").is_file() && !directory.join("Rscript").is_file())
        .collect();
    std::env::join_paths(kept).unwrap_or_default()
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
        let base = base_r().ok_or("ry's base R is not built; build it with `just base-r`")?;
        rcmd::assemble(
            &root,
            &rcmd::HomeSources {
                include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("../libr/include"),
                libr: libr_path()?,
                rcmd: &profile_directory.join("ry-rcmd"),
                base: &base,
                toolchain: rcmd::Toolchain::for_base(&base).map_err(|error| {
                    format!("reading the toolchain of {}: {error}", base.display())
                })?,
            },
        )
        .map_err(|error| format!("assembling ry's R home: {error}"))?;
        Ok(root)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// A `ry` with the runtime and base R embedded, as a release ships it, alone in a directory of
/// its own. It is built with `RY_EMBED_RUNTIME` and `RY_EMBED_BASE` (`crates/repl/build.rs`) and
/// copied away, and then `ry`
/// is built again without it, so the binary the other scenarios run is the usual one and the
/// next `cargo test` has nothing to rebuild. Scenarios run one after another, so none of them
/// sees the swap.
fn single_ry() -> Result<&'static PathBuf, String> {
    static SINGLE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    SINGLE
        .get_or_init(|| {
            let libr = libr_path()?;
            let profile_directory = Path::new(env!("CARGO_BIN_EXE_ry"))
                .parent()
                .ok_or("the ry binary has no directory")?;
            let release = profile_directory
                .file_name()
                .is_some_and(|name| name == "release");
            let base = ry_base().ok_or("ry's base R is not built; build it with `just base-r`")?;
            let build = |embed: Option<(&Path, &Path)>| -> Result<(), String> {
                let mut command = Command::new(env!("CARGO"));
                command.args(["build", "-q", "-p", "ry-lang", "--bin", "ry"]);
                if release {
                    command.arg("--release");
                }
                match embed {
                    Some((library, base)) => command
                        .env("RY_EMBED_RUNTIME", library)
                        .env("RY_EMBED_BASE", base),
                    None => command
                        .env_remove("RY_EMBED_RUNTIME")
                        .env_remove("RY_EMBED_BASE"),
                };
                let status = command
                    .status()
                    .map_err(|error| format!("building ry: {error}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err("building ry failed".to_owned())
                }
            };
            build(Some((libr, &base)))?;
            // Two levels below `single-ry`, so that no `base-r` of the checkout is where `ry`
            // looks beside itself (`rcmd::base::installed`).
            let directory =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("single-ry/bin/release");
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let single = directory.join("ry");
            let copied = fs::copy(env!("CARGO_BIN_EXE_ry"), &single)
                .map(|_| ())
                .map_err(|error| format!("copying ry: {error}"));
            build(None)?;
            copied?;
            Ok(single)
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
    debugger: Option<DebugSession>,
    spawned: Option<Spawned>,
    /// Set by `hide gnu-r`: later runs see no GNU R on `PATH` and no `R_HOME`.
    hide_gnu_r: bool,
    /// Variables `env` set for later `run` steps.
    environment: BTreeMap<String, String>,
}

/// A `ry` process the journey started with `spawn` and has not stopped yet, such as a watcher.
/// Threads forward its output as it arrives, and `await` reads it up to the line it waits for.
struct Spawned {
    child: Child,
    /// Whether `stop` already killed the group and waited for the child.
    reaped: bool,
    lines: Receiver<(Stream, String)>,
    /// What each stream printed so far, which `stop` keeps as the last run's output.
    stdout: String,
    stderr: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Stream {
    Stdout,
    Stderr,
}

/// How long `await` waits for a line before the journey fails. It only bounds a hang: a line
/// that arrives sooner is seen at once.
const AWAIT_LIMIT: Duration = Duration::from_secs(120);

impl Journey {
    fn run(&mut self, step: &Step) -> Result<(), String> {
        let words: Vec<&str> = step.words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["requires", ..] => Ok(()),
            ["hide", "gnu-r"] => {
                self.hide_gnu_r = true;
                Ok(())
            }
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
                        "cold" => self.options.cold = true,
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
            ["env", name, value] => {
                self.environment
                    .insert((*name).to_owned(), (*value).to_owned());
                Ok(())
            }
            ["damage", directory] => {
                // Flips one byte in the middle of every file, as a torn write or a bad disk would.
                let directory = self.root.join(directory);
                let entries = std::fs::read_dir(&directory)
                    .map_err(|error| format!("reading {}: {error}", directory.display()))?;
                let mut damaged = 0;
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    let mut bytes = std::fs::read(&path)
                        .map_err(|error| format!("reading {}: {error}", path.display()))?;
                    let middle = bytes.len() / 2;
                    if let Some(byte) = bytes.get_mut(middle) {
                        *byte ^= 0x5a;
                        damaged += 1;
                    }
                    std::fs::write(&path, bytes)
                        .map_err(|error| format!("writing {}: {error}", path.display()))?;
                }
                if damaged == 0 {
                    return Err(format!("{} holds no file to damage", directory.display()));
                }
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
            ["expect", "early", "diagnostics", path, needle] => {
                let diagnostics = self.early_diagnostics(path)?;
                if diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.contains(needle))
                {
                    Ok(())
                } else {
                    Err(format!(
                        "the first publish mentions no `{needle}`; it held:\n{}",
                        diagnostics.join("\n")
                    ))
                }
            }
            ["expect", "no", "early", "diagnostic", path, needle] => {
                let diagnostics = self.early_diagnostics(path)?;
                match diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.contains(needle))
                {
                    None => Ok(()),
                    Some(found) => Err(format!("unexpected early diagnostic: {found}")),
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
            ["refactor", path, selection, title] => {
                let actions = self.code_actions(path, selection, "all")?;
                let action = actions
                    .iter()
                    .find(|action| action["title"].as_str() == Some(title))
                    .ok_or(format!(
                        "no refactoring titled `{title}` over `{selection}`: {actions:?}"
                    ))?;
                self.apply_workspace_edit(&action["edit"])
            }
            ["expect", "action", path, selection, only, title] => {
                let actions = self.code_actions(path, selection, only)?;
                match actions
                    .iter()
                    .any(|action| action["title"].as_str() == Some(title))
                {
                    true => Ok(()),
                    false => Err(format!(
                        "asking for `{only}` over `{selection}` offers no `{title}`: {actions:?}"
                    )),
                }
            }
            ["expect", "no", "action", path, selection, only, title] => {
                let actions = self.code_actions(path, selection, only)?;
                match actions
                    .iter()
                    .find(|action| action["title"].as_str() == Some(title))
                {
                    None => Ok(()),
                    Some(action) => Err(format!(
                        "asking for `{only}` over `{selection}` still offers {action}"
                    )),
                }
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
            ["expect", "lens", path, anchor, title] => {
                let lenses = self.request_at(RequestKind::CodeLens, path, "", "")?;
                let text = self.text_of(path)?;
                let line = text[..self.find(path, anchor)?].matches('\n').count() as u32;
                let mut titles = Vec::new();
                for lens in lenses.as_array().into_iter().flatten() {
                    let start: lsp_types::Position =
                        serde_json::from_value(lens["range"]["start"].clone())
                            .map_err(|error| error.to_string())?;
                    if start.line != line {
                        continue;
                    }
                    let lens = match lens.get("command") {
                        Some(command) if !command.is_null() => lens.clone(),
                        _ => {
                            let offset = offset_of(&text, start, self.options.utf8);
                            self.request_offset(RequestKind::CodeLensResolve, path, offset, "")?
                        }
                    };
                    titles.push(
                        lens["command"]["title"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                }
                if titles.iter().any(|candidate| candidate == title) {
                    Ok(())
                } else {
                    Err(format!(
                        "no lens titled `{title}` on line {}: {titles:?}",
                        line + 1
                    ))
                }
            }
            // `class` is the token type, then its modifiers, as `function` or
            // `function+defaultLibrary`.
            ["expect", "token", path, anchor, class] => {
                const TYPES: [&str; 9] = [
                    "type",
                    "typeParameter",
                    "parameter",
                    "operator",
                    "decorator",
                    "keyword",
                    "enumMember",
                    "function",
                    "namespace",
                ];
                let response = self.request_at(RequestKind::SemanticTokens, path, "", "")?;
                let text = self.text_of(path)?;
                let target = self.find(path, anchor)?;
                let data: Vec<u64> = response["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect();
                let (mut line, mut column) = (0u64, 0u64);
                for token in data.as_chunks::<5>().0 {
                    if token[0] > 0 {
                        column = 0;
                    }
                    line += token[0];
                    column += token[1];
                    let position = lsp_types::Position::new(line as u32, column as u32);
                    if offset_of(&text, position, self.options.utf8) != target {
                        continue;
                    }
                    let mut found = TYPES
                        .get(token[3] as usize)
                        .copied()
                        .unwrap_or("unknown")
                        .to_owned();
                    if token[4] & 1 != 0 {
                        found.push_str("+defaultLibrary");
                    }
                    return match found == *class {
                        true => Ok(()),
                        false => Err(format!("`{anchor}` is a {found} token, not {class}")),
                    };
                }
                Err(format!("no semantic token starts at `{anchor}`"))
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
            ["run", program @ ("ry" | "single-ry"), arguments @ ..] => {
                let executable = if *program == "single-ry" {
                    single_ry()?.clone()
                } else {
                    // `ry build` and `ry install` find `ry-rcmd` beside libR, and cargo test
                    // only builds it for the scenario's R home.
                    if matches!(arguments.first(), Some(&("build" | "install"))) {
                        r_home()?;
                    }
                    PathBuf::from(env!("CARGO_BIN_EXE_ry"))
                };
                // A private cache keeps verdicts ry remembers between runs inside the scenario.
                let mut command = Command::new(executable);
                command
                    .args(arguments)
                    .current_dir(&self.root)
                    .env("XDG_CACHE_HOME", self.root.join(".cache"))
                    .env_remove("RY_LIBR")
                    .env_remove("RY_IMAGE")
                    .env_remove("RY_IMAGE_DIR")
                    .envs(&self.environment);
                if *program == "single-ry" {
                    // A release finds nothing beside it, so neither may the copy that stands in.
                    command.env_remove("RY_BASE_R");
                }
                if self.hide_gnu_r {
                    command
                        .env("PATH", path_without_gnu_r())
                        .env_remove("R_HOME");
                }
                let output = command
                    .output()
                    .map_err(|error| format!("running ry: {error}"))?;
                self.last_run = Some(Run {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
                Ok(())
            }
            ["spawn", "ry", arguments @ ..] => {
                if self.spawned.is_some() {
                    return Err("a spawned process is still running; `stop` it first".to_owned());
                }
                let mut command = Command::new(env!("CARGO_BIN_EXE_ry"));
                command
                    .args(arguments)
                    .current_dir(&self.root)
                    .env("XDG_CACHE_HOME", self.root.join(".cache"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                // Its own group, so `stop` can end the R session it starts along with it.
                #[cfg(unix)]
                command.process_group(0);
                let mut child = command
                    .spawn()
                    .map_err(|error| format!("spawning ry: {error}"))?;
                let (sender, lines) = mpsc::channel();
                let stdout = child.stdout.take().ok_or("ry has no stdout")?;
                let stderr = child.stderr.take().ok_or("ry has no stderr")?;
                for (stream, reader) in [
                    (Stream::Stdout, Box::new(stdout) as Box<dyn Read + Send>),
                    (Stream::Stderr, Box::new(stderr)),
                ] {
                    let sender = sender.clone();
                    std::thread::spawn(move || {
                        for line in BufReader::new(reader).lines() {
                            let Ok(line) = line else { break };
                            if sender.send((stream, line)).is_err() {
                                break;
                            }
                        }
                    });
                }
                self.spawned = Some(Spawned {
                    child,
                    reaped: false,
                    lines,
                    stdout: String::new(),
                    stderr: String::new(),
                });
                Ok(())
            }
            ["await", stream @ ("stdout" | "stderr"), needle, rest @ ..] => {
                let not_first = match rest {
                    [] => None,
                    ["before", other] => Some(*other),
                    _ => {
                        return Err(
                            "expected `await STREAM \"needle\" [before \"other\"]`".to_owned()
                        );
                    }
                };
                let spawned = self.spawned.as_mut().ok_or("no `spawn` step came before")?;
                let wanted = if *stream == "stdout" {
                    Stream::Stdout
                } else {
                    Stream::Stderr
                };
                let deadline = Instant::now() + AWAIT_LIMIT;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (from, line) = spawned.lines.recv_timeout(remaining).map_err(|_| {
                        format!(
                            "{stream} never printed `{needle}`\nstdout:\n{}\nstderr:\n{}",
                            spawned.stdout, spawned.stderr
                        )
                    })?;
                    let output = match from {
                        Stream::Stdout => &mut spawned.stdout,
                        Stream::Stderr => &mut spawned.stderr,
                    };
                    output.push_str(&line);
                    output.push('\n');
                    if from == wanted && line.contains(needle) {
                        return Ok(());
                    }
                    if from == wanted
                        && let Some(other) = not_first
                        && line.contains(other)
                    {
                        return Err(format!("{stream} printed `{line}` before `{needle}`"));
                    }
                }
            }
            ["stop"] => {
                let mut spawned = self.spawned.take().ok_or("no `spawn` step came before")?;
                kill_group(&mut spawned.child)?;
                spawned.reaped = true;
                // The readers end once every process of the group has closed the pipes, so this
                // drains everything they printed; a process that left the group would hold them
                // open, which the limit turns into a failure instead of a hang.
                let deadline = Instant::now() + AWAIT_LIMIT;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (from, line) = match spawned.lines.recv_timeout(remaining) {
                        Ok(received) => received,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            return Err(
                                "a process ry started kept its output open after `stop`".to_owned()
                            );
                        }
                    };
                    let output = match from {
                        Stream::Stdout => &mut spawned.stdout,
                        Stream::Stderr => &mut spawned.stderr,
                    };
                    output.push_str(&line);
                    output.push('\n');
                }
                self.last_run = Some(Run {
                    code: -1,
                    stdout: std::mem::take(&mut spawned.stdout),
                    stderr: std::mem::take(&mut spawned.stderr),
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
            ["debug", "start", path, options @ ..] => {
                let mut session = DebugSession::start(&self.root)?;
                let program = self.root.join(path);
                let mut launch = json!({ "program": program });
                let mut breakpoints: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
                breakpoints.insert(program.clone(), Vec::new());
                let mut options = options.iter();
                while let Some(option) = options.next() {
                    let mut value = |what: &str| {
                        options
                            .next()
                            .copied()
                            .ok_or(format!("`{option}` needs {what}"))
                    };
                    match *option {
                        "entry" => launch["stopOnEntry"] = json!(true),
                        "prelude" => launch["prelude"] = json!(value("R code")?),
                        "test" => launch["testName"] = json!(value("a test name")?),
                        // `break 3` is a line of the program, `break R/file.R:3` one of another file.
                        "break" => {
                            let place = value("a line")?;
                            let (file, line) = match place.rsplit_once(':') {
                                Some((file, line)) => (self.root.join(file), line),
                                None => (program.clone(), place),
                            };
                            let line: u64 = line
                                .parse()
                                .map_err(|_| format!("`{place}` names no line"))?;
                            breakpoints
                                .entry(file)
                                .or_default()
                                .push(json!({ "line": line }));
                        }
                        other => return Err(format!("unknown debug option `{other}`")),
                    }
                }
                session.request("initialize", json!({ "adapterID": "ry" }))?;
                session.request("launch", launch)?;
                for (file, lines) in breakpoints {
                    session.request(
                        "setBreakpoints",
                        json!({ "source": { "path": file }, "breakpoints": lines }),
                    )?;
                }
                session.request("setExceptionBreakpoints", json!({ "filters": ["error"] }))?;
                session.request("configurationDone", json!({}))?;
                self.debugger = Some(session);
                Ok(())
            }
            [
                "debug",
                action @ ("continue" | "next" | "step-in" | "step-out" | "disconnect"),
            ] => {
                let command = match *action {
                    "step-in" => "stepIn",
                    "step-out" => "stepOut",
                    other => other,
                };
                self.debugger()?
                    .request(command, json!({ "threadId": 1 }))?;
                Ok(())
            }
            ["debug", "evaluate", expression] => {
                let session = self.debugger()?;
                let response = session.request(
                    "evaluate",
                    json!({ "expression": expression, "frameId": 1, "context": "repl" }),
                )?;
                session.evaluated = match response["success"].as_bool() {
                    Some(true) => response["body"]["result"].as_str().map(str::to_owned),
                    _ => response["message"]
                        .as_str()
                        .map(|message| format!("error: {message}")),
                };
                Ok(())
            }
            ["expect", "stopped", reason, "at", line] => {
                let session = self.debugger()?;
                let stopped = session.event("stopped")?;
                let found = stopped["body"]["reason"].as_str().unwrap_or_default();
                if found != *reason {
                    return Err(format!("stopped for `{found}`: {stopped}"));
                }
                let stack = session.stack()?;
                match stack.first() {
                    Some(top) if top.ends_with(&format!("@{line}")) => Ok(()),
                    _ => Err(format!("stopped at {stack:?}")),
                }
            }
            ["expect", "stack", needle] => {
                let stack = self.debugger()?.stack()?.join(" ");
                match stack.contains(needle) {
                    true => Ok(()),
                    false => Err(format!("the stack is `{stack}`")),
                }
            }
            ["expect", "variable", name, value] => {
                let session = self.debugger()?;
                let scopes = session.request("scopes", json!({ "frameId": 1 }))?;
                let reference = scopes["body"]["scopes"][0]["variablesReference"].clone();
                let variables =
                    session.request("variables", json!({ "variablesReference": reference }))?;
                let variables = variables["body"]["variables"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                match variables.iter().find(|variable| variable["name"] == *name) {
                    Some(variable) if variable["value"] == *value => Ok(()),
                    Some(variable) => Err(format!("`{name}` is {}", variable["value"])),
                    None => Err(format!("no variable `{name}` among {variables:?}")),
                }
            }
            ["expect", "evaluated", needle] => {
                let session = self.debugger()?;
                match &session.evaluated {
                    Some(result) if result.contains(needle) => Ok(()),
                    other => Err(format!("evaluated to {other:?}")),
                }
            }
            ["expect", "debug-output", needle] => {
                let session = self.debugger()?;
                session.drain();
                match session.output.contains(needle) {
                    true => Ok(()),
                    false => Err(format!("the output lacks `{needle}`:\n{}", session.output)),
                }
            }
            ["expect", "exited", code] => {
                let session = self.debugger()?;
                let exited = session.event("exited")?;
                session.event("terminated")?;
                session.request("disconnect", json!({}))?;
                let status = session
                    .child
                    .wait()
                    .map_err(|error| format!("waiting for the adapter: {error}"))?;
                session.drain();
                match exited["body"]["exitCode"]
                    == serde_json::from_str::<serde_json::Value>(code)
                        .map_err(|error| format!("bad exit code {code}: {error}"))?
                    && status.code().is_some()
                {
                    true => Ok(()),
                    false => Err(format!(
                        "exited with {exited} and the adapter with {status}"
                    )),
                }
            }
            _ => Err("unknown step".to_owned()),
        }
    }

    fn debugger(&mut self) -> Result<&mut DebugSession, String> {
        self.debugger
            .as_mut()
            .ok_or_else(|| "no debug session is running".to_owned())
    }

    fn editor(&mut self) -> &mut Simulation {
        self.editor.get_or_insert_with(|| {
            Simulation::start_as(&self.root, self.options, env!("CARGO_BIN_EXE_ry").into())
        })
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

    /// The code actions over the first occurrence of `selection` in the buffer. `only` is a code
    /// action kind such as `refactor.extract`, which limits the answer as an editor's refactor
    /// menu does, or `all` for every kind.
    fn code_actions(
        &mut self,
        path: &str,
        selection: &str,
        only: &str,
    ) -> Result<Vec<Value>, String> {
        let start = self.find(path, selection)?;
        let text = self.text_of(path)?;
        let range = lsp_types::Range::new(
            position_of(&text, start, self.options.utf8),
            position_of(&text, start + selection.len(), self.options.utf8),
        );
        let only = match only {
            "all" => None,
            kind => Some(vec![lsp_types::CodeActionKind::from(kind.to_owned())]),
        };
        let uri = self.uri(path);
        let editor = self.editor();
        let id = editor.code_action(&uri, range, only);
        editor.settle();
        match editor.take_response(id) {
            Some(Ok(Value::Null)) => Ok(Vec::new()),
            Some(Ok(Value::Array(actions))) => Ok(actions),
            Some(Ok(value)) => Err(format!("the code action response is not a list: {value}")),
            Some(Err(error)) => Err(format!("the code action request failed: {error:?}")),
            None => Err("the code action request got no response from an idle server".to_owned()),
        }
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

    /// The findings of the first diagnostics the server pushed for `path`, which a push client
    /// shows on opening it, before the server's analysis settles.
    fn early_diagnostics(&mut self, path: &str) -> Result<Vec<String>, String> {
        let uri = self.uri(path);
        let outgoing = self.editor().outgoing();
        let first = outgoing
            .iter()
            .find_map(|message| match message {
                Outgoing::PublishDiagnostics(params) if params.uri == uri => Some(params),
                _ => None,
            })
            .ok_or(format!("the server pushed no diagnostics for {path}"))?;
        Ok(first
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let code = match &diagnostic.code {
                    Some(lsp_types::NumberOrString::String(code)) => code.clone(),
                    Some(lsp_types::NumberOrString::Number(code)) => code.to_string(),
                    None => String::new(),
                };
                format!(
                    "{code} {} (line {})",
                    diagnostic.message,
                    diagnostic.range.start.line + 1
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
        ranges.sort_by_key(|range| std::cmp::Reverse(range.0));
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

/// `ry debug-adapter` driven over its stdio, as an editor drives it.
struct DebugSession {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// Events that arrived and no step has waited for yet.
    events: VecDeque<Value>,
    /// Everything the program printed, from `output` events.
    output: String,
    evaluated: Option<String>,
    seq: u64,
}

/// A step that waits longer than this is a hang, not a slow machine.
const DEBUG_TIMEOUT: Duration = Duration::from_secs(120);

impl DebugSession {
    fn start(root: &Path) -> Result<DebugSession, String> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ry"))
            .arg("debug-adapter")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("starting the debug adapter: {error}"))?;
        let stdin = child.stdin.take().ok_or("the adapter has no stdin")?;
        let stdout = child.stdout.take().ok_or("the adapter has no stdout")?;
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(body)) = ry::debug_adapter::read_message(&mut reader) {
                let Ok(message) = serde_json::from_slice::<Value>(&body) else {
                    break;
                };
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        Ok(DebugSession {
            child,
            stdin,
            messages,
            events: VecDeque::new(),
            output: String::new(),
            evaluated: None,
            seq: 0,
        })
    }

    fn request(&mut self, command: &str, arguments: Value) -> Result<Value, String> {
        self.seq += 1;
        let seq = self.seq;
        let message =
            json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments });
        ry::debug_adapter::write_message(&mut self.stdin, message.to_string().as_bytes())
            .map_err(|error| format!("sending `{command}`: {error}"))?;
        loop {
            let message = self.receive(command)?;
            if message["type"] == "response" && message["request_seq"] == seq {
                return Ok(message);
            }
        }
    }

    /// The next event named `name`, skipping the other events before it.
    fn event(&mut self, name: &str) -> Result<Value, String> {
        while let Some(event) = self.events.pop_front() {
            if event["event"] == name {
                return Ok(event);
            }
        }
        loop {
            self.receive(name)?;
            while let Some(event) = self.events.pop_front() {
                if event["event"] == name {
                    return Ok(event);
                }
            }
        }
    }

    /// The stack of the stopped program as `name@line`, innermost first.
    fn stack(&mut self) -> Result<Vec<String>, String> {
        let response = self.request("stackTrace", json!({ "threadId": 1 }))?;
        Ok(response["body"]["stackFrames"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|frame| {
                format!(
                    "{}@{}",
                    frame["name"].as_str().unwrap_or_default(),
                    frame["line"]
                )
            })
            .collect())
    }

    /// Takes in the messages that already arrived.
    fn drain(&mut self) {
        while let Ok(message) = self.messages.try_recv() {
            self.take(message);
        }
    }

    fn receive(&mut self, waiting_for: &str) -> Result<Value, String> {
        let message = self.messages.recv_timeout(DEBUG_TIMEOUT).map_err(|_| {
            format!(
                "no answer while waiting for `{waiting_for}`; output:\n{}",
                self.output
            )
        })?;
        self.take(message.clone());
        Ok(message)
    }

    fn take(&mut self, message: Value) {
        if message["event"] == "output" {
            self.output
                .push_str(message["body"]["output"].as_str().unwrap_or_default());
        } else if message["type"] == "event" {
            self.events.push_back(message);
        }
    }
}

/// Kills `child` and every process it started, such as the R session a runtime runs in, which
/// would otherwise keep the output pipes open.
fn kill_group(child: &mut Child) -> Result<(), String> {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status()
            .map_err(|error| format!("running kill: {error}"))?;
        if !status.success() {
            return Err(format!("kill failed: {status}"));
        }
    }
    #[cfg(not(unix))]
    child
        .kill()
        .map_err(|error| format!("killing ry: {error}"))?;
    child
        .wait()
        .map_err(|error| format!("waiting for ry: {error}"))?;
    Ok(())
}

impl Drop for Spawned {
    fn drop(&mut self) {
        // A journey that failed while the process ran must not leave it, or the R session it
        // started, running; the group outlives a `ry` that already exited on its own.
        if !self.reaped
            && let Err(error) = kill_group(&mut self.child)
        {
            eprintln!("stopping the spawned ry: {error}");
        }
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        // A scenario that failed midway leaves the adapter waiting for its client.
        if let Err(error) = self.child.kill() {
            eprintln!("stopping the debug adapter: {error}");
        }
        if let Err(error) = self.child.wait() {
            eprintln!("waiting for the debug adapter: {error}");
        }
    }
}
