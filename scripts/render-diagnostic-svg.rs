#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Renders real `ry check` runs on README examples to the diagnostic panels under `docs/public/`.
//! Run it from the repository root with `just readme`; `--check` only reports a stale image.
//!
//! GitHub strips ANSI escapes from fenced code blocks, so an image is the only way to show a
//! diagnostic in the colours a terminal shows. GitHub honours a `<style>` block inside an embedded
//! SVG, including `prefers-color-scheme`, so one file serves the light and the dark theme. The
//! diagnostic's text also goes in `<desc>`, so screen readers and search see it.
//!
//! Each panel's example is the fenced ```r block right above its image in README.md, so the README
//! is the only copy of it, and the image is generated rather than drawn, so it cannot drift from
//! what the tool prints. A README panel has to make its point at a glance, so it keeps only the
//! finding's headline and each source line with its underline, and drops the rule name, the
//! location, the context lines, the help and the summary.
//!
//! Usage: `cargo -Zscript scripts/render-diagnostic-svg.rs <ry binary> [--check]`

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

const README: &str = "README.md";
/// Pixels per column of a 14px monospace font, whose advance is about 0.6em.
const ADVANCE: f32 = 8.4;
const LINE_HEIGHT: f32 = 20.0;
const PADDING: f32 = 16.0;

struct Panel {
    image: &'static str,
    file_name: &'static str,
    ry_toml: &'static str,
    title: &'static str,
}

const PANELS: [Panel; 2] = [
    Panel {
        image: "docs/public/diagnostic-typing.svg",
        file_name: "discount.R",
        ry_toml: "[check]\ntyping = true\n",
        title: "ry check reports a type error that it inferred from plain R",
    },
    Panel {
        image: "docs/public/diagnostic.svg",
        file_name: "payment.R",
        ry_toml: "[check]\ntyping = true\n",
        title: "ry check reports a match that misses a case",
    },
];

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
    let readme = fs::read_to_string(README)?;
    let mut code = ExitCode::SUCCESS;
    for panel in &PANELS {
        let example = readme_example(&readme, panel.image)?;
        let svg = render(&finding(&ry, panel, &example)?, panel.title)?;
        if fs::read_to_string(panel.image).is_ok_and(|current| current == svg) {
            continue;
        }
        if check {
            eprintln!("error: {} differs from what `ry check` prints; run `just readme`", panel.image);
            code = ExitCode::from(1);
            continue;
        }
        fs::write(panel.image, svg)?;
        println!("wrote {}", panel.image);
    }
    Ok(code)
}

/// The body of the last ```r block before the line that shows `image`.
fn readme_example(readme: &str, image: &str) -> Result<String, Box<dyn Error>> {
    let lines: Vec<&str> = readme.lines().collect();
    let shown = lines
        .iter()
        .position(|line| line.contains(image))
        .ok_or(format!("{README} does not show {image}"))?;
    let open = lines[..shown]
        .iter()
        .rposition(|line| line.trim_end() == "```r")
        .ok_or(format!("{README} has no ```r block above {image}"))?;
    let close = lines[open + 1..shown]
        .iter()
        .position(|line| line.trim_end() == "```")
        .ok_or(format!("the ```r block above {image} in {README} is not closed"))?;
    let body = &lines[open + 1..open + 1 + close];
    Ok(body.iter().map(|line| format!("{line}\n")).collect())
}

/// The styled lines of the one finding `ry check` reports on `example`, cut down to its headline
/// and each source line that carries an underline.
fn finding(ry: &Path, panel: &Panel, example: &str) -> Result<Vec<Vec<Span>>, Box<dyn Error>> {
    // A fresh directory, so no `ry.toml` or check cache of the repository applies.
    let directory = std::env::temp_dir().join(format!("ry-readme-diagnostic-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("ry.toml"), panel.ry_toml)?;
    fs::write(directory.join(panel.file_name), example)?;
    // `CLICOLOR_FORCE` gives a pipe the colors and box drawing of a terminal, without wrapping
    // lines to a terminal's width.
    let output = Command::new(ry)
        .args(["check", panel.file_name])
        .current_dir(&directory)
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output();
    fs::remove_dir_all(&directory)?;
    let output = output?;
    // Findings go to stderr and the summary to stdout.
    let report = String::from_utf8(output.stderr)?;
    let summary = String::from_utf8(output.stdout)?;
    let headlines = report.lines().filter(|line| plain_text(line).trim_start().starts_with('×')).count();
    if output.status.code() != Some(1) || headlines != 1 {
        let file_name = panel.file_name;
        return Err(format!("`ry check {file_name}` should report one finding, but printed:\n{report}{summary}").into());
    }

    let plain: Vec<String> = report.lines().map(plain_text).collect();
    let underline = |index: usize| {
        plain.get(index).and_then(|text| text.split_once('│')).is_some_and(|(gutter, marks)| {
            gutter.trim().is_empty() && marks.contains('^')
        })
    };
    let source = |index: usize| {
        plain.get(index).and_then(|text| text.split_once('│')).is_some_and(|(gutter, _)| {
            gutter.trim().parse::<usize>().is_ok()
        })
    };
    let kept: Vec<Vec<Span>> = report
        .lines()
        .enumerate()
        .filter(|&(index, _)| {
            plain[index].trim_start().starts_with('×') || underline(index) || (source(index) && underline(index + 1))
        })
        .map(|(index, line)| {
            let mut spans = parse_sgr(line);
            if underline(index) && !(index + 1..plain.len()).any(underline) {
                blank_gutter_bar(&mut spans);
            }
            spans
        })
        .collect();
    if !(0..plain.len()).any(underline) {
        return Err(format!("`ry check {}` underlined no source line:\n{report}", panel.file_name).into());
    }
    Ok(kept)
}

/// Replaces the `│` of the last underline line with a space, so the panel does not end on a bar
/// that leads nowhere; the carets keep their columns.
fn blank_gutter_bar(spans: &mut [Span]) {
    if let Some(span) = spans.iter_mut().find(|span| span.text.contains('│')) {
        span.text = span.text.replacen('│', " ", 1);
    }
}

/// One SVG holding the colored lines, styled for both GitHub themes.
fn render(lines: &[Vec<Span>], title: &str) -> Result<String, std::fmt::Error> {
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
<title id="title">{}</title>
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
        escape(title),
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

/// A line's text without its SGR escapes.
fn plain_text(line: &str) -> String {
    parse_sgr(line).into_iter().map(|span| span.text).collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
