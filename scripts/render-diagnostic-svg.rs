#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---
//! Renders `ry check` on the README's snippet (as `setup.R` beside a `ry.toml`) to
//! `.github/diagnostic.svg`: GitHub strips ANSI from code blocks but honors
//! `prefers-color-scheme` inside an SVG. Re-run after any change to diagnostic rendering.

use std::fmt::Write as _;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let (Some(fixture), Some(output)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: render-diagnostic-svg.rs <fixture.R> <out.svg>");
        std::process::exit(2);
    };

    // `script` gives the child a pty; piped output is plain text.
    let rendered = Command::new("script")
        .args(["-qec", &format!("ry check {fixture}"), "/dev/null"])
        .output()?;
    let text = String::from_utf8_lossy(&rendered.stdout).replace('\r', "");
    if text.trim().is_empty() {
        return Err(format!("`ry check {fixture}` printed nothing — is `ry` on PATH?").into());
    }

    std::fs::write(&output, svg(&text))?;
    println!("wrote {output}");
    Ok(())
}

fn svg(text: &str) -> String {
    let lines: Vec<Vec<Span>> = text
        .lines()
        .map(parse_ansi)
        .filter(|spans| !spans.is_empty())
        .filter(|spans| {
            // Drops the "N problems in M files" summary; the locus line stays because
            // it opens the snippet frame the gutter hangs from.
            let plain: String = spans.iter().map(|span| span.text.as_str()).collect();
            let plain = plain.trim();
            !(plain.starts_with(|c: char| c.is_ascii_digit()) && plain.contains(" problem"))
        })
        .collect();

    // A 0.6em monospace advance only sizes the padded viewport.
    let columns = lines
        .iter()
        .map(|spans| spans.iter().map(|span| span.text.chars().count()).sum())
        .max()
        .unwrap_or(0);
    let width = (columns as f32 * 8.4).max(360.0) + 40.0;
    let height = lines.len() as f32 * 22.0 + 30.0;
    // Screen readers and search read the description, so it carries the plain text.
    let plain: String = lines
        .iter()
        .map(|spans| {
            spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");

    let mut out = String::new();
    let _ = write!(
        out,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0} {height:.0}" width="{width:.0}" height="{height:.0}" role="img" aria-labelledby="title desc">
  <title id="title">A ry diagnostic</title>
  <desc id="desc">{}</desc>
  <style>
    .bg {{ fill: #ffffff; stroke: #d0d7de; }}
    .fg {{ fill: #1f2328; }}
    .red {{ fill: #cf222e; }}
    .blue {{ fill: #0969da; }}
    .cyan {{ fill: #1b7c83; }}
    .magenta {{ fill: #8250df; }}
    .green {{ fill: #1a7f37; }}
    .yellow {{ fill: #9a6700; }}
    .dim {{ fill: #59636e; }}
    @media (prefers-color-scheme: dark) {{
      .bg {{ fill: #0d1117; stroke: #30363d; }}
      .fg {{ fill: #e6edf3; }}
      .red {{ fill: #ff7b72; }}
      .blue {{ fill: #79c0ff; }}
      .cyan {{ fill: #76e3ea; }}
      .magenta {{ fill: #d2a8ff; }}
      .green {{ fill: #7ee787; }}
      .yellow {{ fill: #e3b341; }}
      .dim {{ fill: #9198a1; }}
    }}
    text {{ font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace; font-size: 14px; white-space: pre; }}
  </style>
  <rect class="bg" x="0.5" y="0.5" width="{:.0}" height="{:.0}" rx="6"/>
"##,
        escape(&plain),
        width - 1.0,
        height - 1.0,
    );

    for (row, spans) in lines.iter().enumerate() {
        let y = 30.0 + row as f32 * 22.0;
        let _ = write!(out, r#"  <text x="16" y="{y:.0}">"#);
        let mut column = 0usize;
        for span in spans {
            let _ = write!(
                out,
                r#"<tspan x="{:.1}" class="{}"{}{}>{}</tspan>"#,
                16.0 + column as f32 * 8.4,
                span.class,
                if span.bold { r#" font-weight="600""# } else { "" },
                if span.underline { r#" text-decoration="underline""# } else { "" },
                escape(&span.text)
            );
            column += span.text.chars().count();
        }
        out.push_str("</text>\n");
    }
    out.push_str("</svg>\n");
    out
}

struct Span {
    text: String,
    class: &'static str,
    bold: bool,
    underline: bool,
}

/// One escape carries several parameters (`\e[36;1;4m` is cyan, bold and underlined),
/// so each applies in turn; missing one renders a plausible-looking monochrome picture.
fn parse_ansi(line: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let (mut class, mut bold, mut underline) = ("fg", false, false);
    let mut current = String::new();
    let mut rest = line;

    while let Some(start) = rest.find('\u{1b}') {
        if !rest[..start].is_empty() {
            current.push_str(&rest[..start]);
        }
        let Some(end) = rest[start..].find('m') else { break };
        let code = &rest[start + 2..start + end];
        if !current.is_empty() {
            spans.push(Span { text: std::mem::take(&mut current), class, bold, underline });
        }
        for parameter in code.split(';') {
            match parameter {
                "0" | "" => (class, bold, underline) = ("fg", false, false),
                "1" => bold = true,
                "4" => underline = true,
                "2" | "90" => class = "dim",
                "31" => class = "red",
                "32" => class = "green",
                "33" => class = "yellow",
                "34" => class = "blue",
                "35" => class = "magenta",
                "36" => class = "cyan",
                _ => (),
            }
        }
        rest = &rest[start + end + 1..];
    }
    current.push_str(rest);
    if !current.trim().is_empty() {
        spans.push(Span { text: current, class, bold, underline });
    }
    spans
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
