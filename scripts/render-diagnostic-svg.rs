#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Renders a real `ry check` run on the README's Payment example to `docs/public/diagnostic.svg`.
//! Run it from the repository root with `just readme`; `--check` only reports a stale image.
//!
//! GitHub strips ANSI escapes from fenced code blocks, so an image is the only way to show a
//! diagnostic in the colours a terminal shows. GitHub honours a `<style>` block inside an embedded
//! SVG, including `prefers-color-scheme`, so one file serves the light and the dark theme. The
//! diagnostic's text also goes in `<desc>`, so screen readers and search see it.
//!
//! The example is the fenced ```r block right above the image in README.md, so the README is the
//! only copy of it, and the image is generated rather than drawn, so it cannot drift from what the
//! tool prints.
//!
//! Usage: `cargo -Zscript scripts/render-diagnostic-svg.rs <ry binary> [--check]`

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::process::{Command, ExitCode};

const README: &str = "README.md";
const IMAGE: &str = "docs/public/diagnostic.svg";
const FILE_NAME: &str = "payment.R";
/// Pixels per column of a 14px monospace font, whose advance is about 0.6em.
const ADVANCE: f32 = 8.4;
const LINE_HEIGHT: f32 = 20.0;
const PADDING: f32 = 16.0;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (ry, check) = match arguments.as_slice() {
        [ry] => (ry, false),
        [ry, flag] if flag == "--check" => (ry, true),
        _ => return Err("usage: render-diagnostic-svg.rs <ry binary> [--check]".into()),
    };
    let ry = fs::canonicalize(ry).map_err(|error| format!("cannot find the ry binary `{ry}`: {error}"))?;
    let example = readme_example(&fs::read_to_string(README)?)?;

    // A fresh directory, so no `ry.toml` or check cache of the repository applies.
    let directory = std::env::temp_dir().join(format!("ry-readme-diagnostic-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("ry.toml"), "")?;
    fs::write(directory.join(FILE_NAME), example)?;
    // `CLICOLOR_FORCE` gives a pipe the colors and box drawing of a terminal, without wrapping
    // lines to a terminal's width.
    let output = Command::new(&ry)
        .args(["check", FILE_NAME])
        .current_dir(&directory)
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output();
    fs::remove_dir_all(&directory)?;
    let output = output?;
    // Findings go to stderr and the summary to stdout, which a terminal shows in that order.
    let transcript = format!(
        "\u{1b}[2m$\u{1b}[0m ry check {FILE_NAME}\n{}{}",
        String::from_utf8(output.stderr)?,
        String::from_utf8(output.stdout)?
    );
    if output.status.code() != Some(1) {
        return Err(format!("`ry check {FILE_NAME}` should report a finding, but printed:\n{transcript}").into());
    }

    let svg = render(&transcript)?;
    if fs::read_to_string(IMAGE).is_ok_and(|current| current == svg) {
        return Ok(ExitCode::SUCCESS);
    }
    if check {
        eprintln!("error: {IMAGE} differs from what `ry check` prints; run `just readme`");
        return Ok(ExitCode::from(1));
    }
    fs::write(IMAGE, svg)?;
    println!("wrote {IMAGE}");
    Ok(ExitCode::SUCCESS)
}

/// The body of the last ```r block before the line that shows the image.
fn readme_example(readme: &str) -> Result<String, Box<dyn Error>> {
    let lines: Vec<&str> = readme.lines().collect();
    let image = lines
        .iter()
        .position(|line| line.contains(IMAGE))
        .ok_or(format!("{README} does not show {IMAGE}"))?;
    let open = lines[..image]
        .iter()
        .rposition(|line| line.trim_end() == "```r")
        .ok_or(format!("{README} has no ```r block above {IMAGE}"))?;
    let close = lines[open + 1..image]
        .iter()
        .position(|line| line.trim_end() == "```")
        .ok_or(format!("the ```r block above {IMAGE} in {README} is not closed"))?;
    let body = &lines[open + 1..open + 1 + close];
    Ok(body.iter().map(|line| format!("{line}\n")).collect())
}

/// One SVG holding the colored lines, styled for both GitHub themes.
fn render(transcript: &str) -> Result<String, std::fmt::Error> {
    let lines: Vec<Vec<Span>> = transcript.trim_end().lines().map(parse_sgr).collect();
    let columns = lines
        .iter()
        .map(|spans| spans.iter().map(|span| span.text.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let width = (columns as f32 * ADVANCE + 2.0 * PADDING).ceil();
    let height = (lines.len() as f32 * LINE_HEIGHT + 2.0 * PADDING).ceil();
    let plain: Vec<String> = lines
        .iter()
        .map(|spans| spans.iter().map(|span| span.text.as_str()).collect::<String>().trim_end().to_owned())
        .collect();

    let mut out = String::new();
    write!(
        out,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" role="img" aria-labelledby="title desc">
<title id="title">ry check reports a match that misses a case</title>
<desc id="desc">{}</desc>
<style>
.bg {{ fill: #f6f8fa; stroke: #d0d7de; }}
text {{ fill: #1f2328; font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace; font-size: 14px; white-space: pre; }}
.red {{ fill: #cf222e; }} .green {{ fill: #116329; }} .yellow {{ fill: #9a6700; }} .blue {{ fill: #0969da; }}
.magenta {{ fill: #8250df; }} .cyan {{ fill: #1b7c83; }} .dim {{ fill: #6e7781; }}
@media (prefers-color-scheme: dark) {{
.bg {{ fill: #151b23; stroke: #3d444d; }}
text {{ fill: #e6edf3; }}
.red {{ fill: #ff7b72; }} .green {{ fill: #3fb950; }} .yellow {{ fill: #d29922; }} .blue {{ fill: #79c0ff; }}
.magenta {{ fill: #d2a8ff; }} .cyan {{ fill: #56d4dd; }} .dim {{ fill: #8b949e; }}
}}
</style>
<rect class="bg" x="0.5" y="0.5" width="{}" height="{}" rx="6"/>
"##,
        escape(&plain.join("\n")),
        width - 1.0,
        height - 1.0,
    )?;
    for (row, spans) in lines.iter().enumerate() {
        if spans.is_empty() {
            continue;
        }
        // Baseline of a 14px font centred in its line.
        let y = PADDING + row as f32 * LINE_HEIGHT + 14.5;
        write!(out, r#"<text y="{y}">"#)?;
        let mut column = 0;
        for span in spans {
            // Each span is placed by column, so a fallback font for `×` or `│` cannot shift the rest.
            write!(out, r#"<tspan x="{:.1}""#, PADDING + column as f32 * ADVANCE)?;
            if let Some(class) = span.style.class() {
                write!(out, r#" class="{class}""#)?;
            }
            if span.style.bold {
                out.push_str(r#" font-weight="bold""#);
            }
            if span.style.underline {
                out.push_str(r#" text-decoration="underline""#);
            }
            write!(out, ">{}</tspan>", escape(&span.text))?;
            column += span.text.chars().count();
        }
        out.push_str("</text>\n");
    }
    out.push_str("</svg>\n");
    Ok(out)
}

struct Span {
    text: String,
    style: Style,
}

#[derive(Clone, Copy, Default)]
struct Style {
    color: Option<&'static str>,
    bold: bool,
    dim: bool,
    underline: bool,
}

impl Style {
    fn class(self) -> Option<&'static str> {
        self.color.or(self.dim.then_some("dim"))
    }

    /// Applies one SGR sequence's parameters, such as `36;1;4`.
    fn apply(&mut self, parameters: &str) {
        const COLORS: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        for parameter in parameters.split(';') {
            match parameter.parse::<usize>().unwrap_or(0) {
                0 => *self = Style::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                4 => self.underline = true,
                22 => (self.bold, self.dim) = (false, false),
                24 => self.underline = false,
                39 => self.color = None,
                code @ 30..=37 => self.color = COLORS.get(code - 30).copied(),
                code @ 90..=97 => self.color = COLORS.get(code - 90).copied(),
                _ => (),
            }
        }
        // Black and white are the default ink in one theme and invisible in the other.
        if matches!(self.color, Some("black" | "white")) {
            self.color = None;
        }
    }
}

/// Splits one line at its SGR escapes (`ESC [ ... m`) into styled spans.
fn parse_sgr(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut rest = line;
    loop {
        let (text, escape) = match rest.find("\u{1b}[") {
            Some(start) => (&rest[..start], Some(&rest[start + 2..])),
            None => (rest, None),
        };
        if !text.is_empty() {
            spans.push(Span { text: text.to_owned(), style });
        }
        let Some(escape) = escape else { break };
        let Some(end) = escape.find('m') else { break };
        style.apply(&escape[..end]);
        rest = &escape[end + 1..];
    }
    spans
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
