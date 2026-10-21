#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Converts corpus/tree-sitter-r/*.txt into crates/syntax/tests/tsr/ fixtures. R source may hold
//! `-`/`=` lines too: a header needs a closing `=` fence, and the *last* `-` divider splits source
//! from the S-expression, which never holds an all-`-` line.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const CORPUS_SUBDIR: &str = "corpus/tree-sitter-r";
const FIXTURE_SUBDIR: &str = "crates/syntax/tests/tsr";
const EXPECTED_SUBDIR: &str = "corpus/tree-sitter-r-expected";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let repo_root = repo_root()?;
    let corpus_dir = repo_root.join(CORPUS_SUBDIR);
    let fixture_dir = repo_root.join(FIXTURE_SUBDIR);
    let expected_dir = repo_root.join(EXPECTED_SUBDIR);

    if !corpus_dir.is_dir() {
        eprintln!(
            "corpus not found at {} (run scripts/fetch-corpus.rs first)",
            corpus_dir.display()
        );
        return Ok(ExitCode::FAILURE);
    }

    let mut corpus_files: Vec<PathBuf> = fs::read_dir(&corpus_dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(".txt")))
        .collect();
    corpus_files.sort();
    if corpus_files.is_empty() {
        eprintln!("no *.txt files under {}", corpus_dir.display());
        return Ok(ExitCode::FAILURE);
    }

    let mut total_emitted = 0;
    let mut total_skipped = 0;
    for path in &corpus_files {
        let (emitted, skipped) = convert_file(path, &fixture_dir, &expected_dir)?;
        total_emitted += emitted;
        total_skipped += skipped;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        println!("{name}: {emitted} emitted, {skipped} skipped");
    }

    println!(
        "\nconverted {} file(s): {} sections total, {} emitted, {} skipped",
        corpus_files.len(),
        total_emitted + total_skipped,
        total_emitted,
        total_skipped
    );
    println!("fixtures -> {FIXTURE_SUBDIR}");
    println!("expected sexps -> {EXPECTED_SUBDIR}");
    Ok(ExitCode::SUCCESS)
}

/// Convert one corpus file. Returns `(emitted, skipped)` section counts.
fn convert_file(
    path: &Path,
    fixture_dir: &Path,
    expected_dir: &Path,
) -> Result<(usize, usize), Box<dyn Error>> {
    let stem = path
        .file_stem()
        .ok_or_else(|| format!("corpus file has no stem: {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
    let sections = parse_sections(&text);

    let mut fixture_lines: Vec<String> = vec![format!("#==== {stem}"), String::new()];
    let expected_stem_dir = expected_dir.join(&stem);
    let mut used_slugs: HashMap<String, usize> = HashMap::new();
    let mut emitted = 0;
    let mut skipped = 0;
    let mut collisions: Vec<String> = Vec::new();

    for section in &sections {
        if section.attributes.iter().any(|attribute| attribute.starts_with(":skip")) {
            skipped += 1;
            continue;
        }

        let expected_text = section.expected.join("\n");
        let is_error = expected_text.contains("(ERROR")
            || expected_text.contains("(MISSING")
            || section.attributes.iter().any(|attribute| attribute.starts_with(":error"));

        let mut slug = slugify(&section.name);
        if is_error {
            slug = format!("{slug}_ts_error");
        }
        let count = used_slugs.get(&slug).copied().unwrap_or(0) + 1;
        used_slugs.insert(slug.clone(), count);
        if count > 1 {
            slug = format!("{slug}_{count}");
        }

        for source_line in &section.source {
            if source_line.starts_with("#====")
                || source_line.starts_with("#----")
                || source_line.starts_with("#++++")
            {
                collisions.push(format!("{stem}/{slug}: {}", python_repr(source_line)));
            }
        }

        fixture_lines.push(format!("#---- {slug}"));
        fixture_lines.extend(section.source.iter().cloned());
        fixture_lines.push("#++++".to_string());
        fixture_lines.push(String::new());

        fs::create_dir_all(&expected_stem_dir)?;
        fs::write(
            expected_stem_dir.join(format!("{slug}.sexp")),
            expected_text + "\n",
        )?;
        emitted += 1;
    }

    fs::create_dir_all(fixture_dir)?;
    fs::write(
        fixture_dir.join(format!("{stem}.R.test")),
        fixture_lines.join("\n") + "\n",
    )?;

    if !collisions.is_empty() {
        println!(
            "  WARNING: {} source line(s) collide with fixture directives:",
            collisions.len()
        );
        for collision in &collisions {
            println!("    {collision}");
        }
    }

    Ok((emitted, skipped))
}

struct Section {
    name: String,
    attributes: Vec<String>,
    source: Vec<String>,
    expected: Vec<String>,
}

fn parse_sections(text: &str) -> Vec<Section> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut sections = Vec::new();

    let mut index = 0;
    while index < lines.len() {
        let Some((close_index, content)) = try_header(&lines, index) else {
            index += 1;
            continue;
        };
        let name_parts: Vec<&str> = content
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.starts_with(':'))
            .collect();
        let name = name_parts.join(" ").trim().to_string();
        let attributes: Vec<String> = content
            .iter()
            .map(|line| line.trim())
            .filter(|line| line.starts_with(':'))
            .map(str::to_string)
            .collect();

        let body_start = close_index + 1;
        let mut next_header = lines.len();
        let mut scan = body_start;
        while scan < lines.len() {
            if try_header(&lines, scan).is_some() {
                next_header = scan;
                break;
            }
            scan += 1;
        }
        let body = &lines[body_start..next_header];

        let last_dash = body
            .iter()
            .enumerate()
            .filter(|(_, line)| is_dash(line))
            .map(|(position, _)| position)
            .next_back();
        let (source, expected): (&[&str], &[&str]) = match last_dash {
            Some(divider) => (&body[..divider], &body[divider + 1..]),
            None => (body, &[]),
        };

        sections.push(Section {
            name,
            attributes,
            source: strip_blank_edges(source),
            expected: strip_blank_edges(expected),
        });
        index = next_header;
    }

    sections
}

fn try_header<'lines>(lines: &[&'lines str], index: usize) -> Option<(usize, Vec<&'lines str>)> {
    if index >= lines.len() || !is_equals(lines[index]) {
        return None;
    }
    let mut cursor = index + 1;
    let mut content: Vec<&str> = Vec::new();
    while cursor < lines.len() && !is_equals(lines[cursor]) {
        content.push(lines[cursor]);
        cursor += 1;
    }
    if cursor >= lines.len() {
        return None; // no closing fence: not a header
    }
    if content.iter().all(|line| line.trim().is_empty()) {
        return None; // empty header block: not a header
    }
    Some((cursor, content))
}

fn is_fence(line: &str, fence: char) -> bool {
    let stripped = line.trim_end();
    stripped.len() >= 3 && stripped.chars().all(|character| character == fence)
}

fn is_equals(line: &str) -> bool {
    is_fence(line, '=')
}

fn is_dash(line: &str) -> bool {
    is_fence(line, '-')
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else {
            slug.push('_');
        }
    }
    let mut slug = slug.trim_matches('_').to_string();
    while slug.contains("__") {
        slug = slug.replace("__", "_");
    }
    if slug.is_empty() { "case".to_string() } else { slug }
}

/// Python's `repr()` for the collision warning: single quotes (double when the
/// text contains a single quote but no double quote), ASCII control escapes.
fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut repr = String::with_capacity(text.len() + 2);
    repr.push(quote);
    for character in text.chars() {
        match character {
            '\\' => repr.push_str("\\\\"),
            '\t' => repr.push_str("\\t"),
            '\n' => repr.push_str("\\n"),
            '\r' => repr.push_str("\\r"),
            character if character == quote => {
                repr.push('\\');
                repr.push(character);
            }
            character if character.is_ascii_control() => {
                repr.push_str(&format!("\\x{:02x}", character as u32));
            }
            character => repr.push(character),
        }
    }
    repr.push(quote);
    repr
}

fn strip_blank_edges(lines: &[&str]) -> Vec<String> {
    let mut start = 0;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].iter().map(|line| line.to_string()).collect()
}

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    let script_path = std::env::args().next().ok_or("cannot determine the script path")?;
    let script_dir = Path::new(&script_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok(fs::canonicalize(script_dir.join(".."))?)
}
