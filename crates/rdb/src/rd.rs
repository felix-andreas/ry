//! Renders a parsed Rd help page, as installed packages store it, to compact Markdown for
//! editor hovers: the title, the description, the usage, the arguments and the value.

use crate::unserialize::Value;

/// The description is cut to this many characters, because a hover shows the summary and
/// links to the full page.
const DESCRIPTION_LIMIT: usize = 2000;

pub(crate) fn render(rd: &Value) -> String {
    let mut title = String::new();
    let mut description = String::new();
    let mut usage = String::new();
    let mut arguments = Vec::new();
    let mut value = String::new();
    for section in children(rd) {
        match tag(section) {
            "\\title" => title = prose(section),
            "\\description" => description = prose(section),
            "\\usage" => usage = code(section),
            "\\arguments" => arguments = argument_items(section),
            "\\value" => value = prose(section),
            _ => {}
        }
    }

    let mut output = String::new();
    if !title.is_empty() {
        output.push_str("**");
        output.push_str(&title);
        output.push_str("**\n\n");
    }
    if !description.is_empty() {
        output.push_str(&truncate(&description, DESCRIPTION_LIMIT));
        output.push_str("\n\n");
    }
    if !usage.is_empty() {
        output.push_str("```r\n");
        output.push_str(&usage);
        output.push_str("\n```\n\n");
    }
    if !arguments.is_empty() {
        output.push_str("**Arguments**\n\n");
        for (name, text) in arguments {
            output.push_str("- `");
            output.push_str(&name);
            output.push_str("`: ");
            output.push_str(&text);
            output.push('\n');
        }
        output.push('\n');
    }
    if !value.is_empty() {
        output.push_str("**Value**\n\n");
        output.push_str(&truncate(&value, DESCRIPTION_LIMIT));
        output.push('\n');
    }
    output.trim_end().to_string()
}

/// Markdown text with whitespace collapsed: a blank line in the source starts a paragraph,
/// and any other run of whitespace becomes one space.
struct Markdown {
    output: String,
    pending: Break,
    /// Inside `\usage` and `\preformatted`, where line breaks and indentation are kept.
    verbatim: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Break {
    None,
    Space,
    Line,
    Paragraph,
}

impl Markdown {
    fn new(verbatim: bool) -> Markdown {
        Markdown {
            output: String::new(),
            pending: Break::None,
            verbatim,
        }
    }

    fn text(&mut self, text: &str) {
        if self.verbatim {
            self.output.push_str(text);
            return;
        }
        let mut newlines = 0;
        let mut whitespace = false;
        for character in text.chars() {
            if character.is_whitespace() {
                whitespace = true;
                newlines += usize::from(character == '\n');
                continue;
            }
            if whitespace {
                self.request(if newlines >= 2 {
                    Break::Paragraph
                } else {
                    Break::Space
                });
                whitespace = false;
                newlines = 0;
            }
            self.flush();
            self.output.push(character);
        }
        if whitespace {
            self.request(if newlines >= 2 {
                Break::Paragraph
            } else {
                Break::Space
            });
        }
    }

    /// Text written as is, after any pending break.
    fn raw(&mut self, text: &str) {
        if !self.verbatim {
            self.flush();
        }
        self.output.push_str(text);
    }

    fn request(&mut self, kind: Break) {
        self.pending = self.pending.max(kind);
    }

    fn flush(&mut self) {
        if !self.output.is_empty() {
            match self.pending {
                Break::None => {}
                // Right after a list marker or a line break, a space would be indentation.
                Break::Space if self.output.ends_with([' ', '\n']) => {}
                Break::Space => self.output.push(' '),
                Break::Line => self.output.push_str("  \n"),
                Break::Paragraph => self.output.push_str("\n\n"),
            }
        }
        self.pending = Break::None;
    }

    fn node(&mut self, node: &Value) {
        match node {
            Value::Character(vector) => {
                if matches!(tag(node), "COMMENT") {
                    return;
                }
                for string in vector.elements.iter().flatten() {
                    self.text(string);
                }
            }
            Value::List(_) => self.macro_node(node),
            _ => {}
        }
    }

    fn macro_node(&mut self, node: &Value) {
        let parts = children(node);
        match tag(node) {
            "\\code" | "\\samp" | "\\verb" | "\\env" | "\\option" | "\\command" | "\\kbd"
            | "\\file" | "\\var" => {
                let inner = inline_code(node);
                self.raw(&inner);
            }
            "\\emph" | "\\dfn" | "\\cite" => self.wrapped(node, "*"),
            "\\bold" | "\\strong" => self.wrapped(node, "**"),
            "\\sQuote" => {
                self.raw("\u{2018}");
                self.nodes(parts);
                self.raw("\u{2019}");
            }
            "\\dQuote" => {
                self.raw("\u{201c}");
                self.nodes(parts);
                self.raw("\u{201d}");
            }
            "\\href" => {
                let url = plain(parts.first());
                let text = plain(parts.get(1));
                self.raw(&format!("[{text}]({url})"));
            }
            "\\url" | "\\email" => {
                let target = plain(Some(node));
                self.raw(&format!("<{target}>"));
            }
            "\\eqn" | "\\deqn" => {
                // The second argument, when given, is the plain-text form of the formula.
                let formula = plain(parts.get(1).or(parts.first()));
                self.raw(&format!("`{}`", formula.trim()));
            }
            "\\dots" | "\\ldots" => self.raw("..."),
            "\\R" => self.raw("R"),
            "\\cr" => self.request(Break::Line),
            "\\tab" => self.request(Break::Space),
            "\\enc" => {
                if let Some(first) = parts.first() {
                    self.node(first);
                }
            }
            "\\method" | "\\S3method" | "\\S4method" => {
                if let Some(generic) = parts.first() {
                    self.node(generic);
                }
            }
            "\\itemize" | "\\enumerate" => {
                for part in parts {
                    if tag(part) == "\\item" {
                        self.request(Break::Paragraph);
                        self.flush();
                        self.output.push_str("- ");
                    } else {
                        self.node(part);
                    }
                }
                self.request(Break::Paragraph);
            }
            // A labelled item, as in `\describe` and in a `\value` that lists components.
            "\\item" if parts.len() >= 2 => {
                self.request(Break::Paragraph);
                self.flush();
                self.output.push_str("- **");
                self.output.push_str(&plain(parts.first()));
                self.output.push_str("**: ");
                self.node(&parts[1]);
                self.request(Break::Paragraph);
            }
            "\\preformatted" => {
                self.request(Break::Paragraph);
                self.flush();
                self.output.push_str("```\n");
                self.output.push_str(code(node).trim_matches('\n'));
                self.output.push_str("\n```");
                self.request(Break::Paragraph);
            }
            "\\if" => {
                let format = plain(parts.first());
                if format.contains("text") {
                    self.nodes(parts.get(1..).unwrap_or(&[]));
                }
            }
            "\\ifelse" => {
                let format = plain(parts.first());
                let branch = if format.contains("text") { 1 } else { 2 };
                if let Some(chosen) = parts.get(branch) {
                    self.node(chosen);
                }
            }
            "\\tabular" => {
                self.request(Break::Paragraph);
                if let Some(content) = parts.get(1) {
                    self.node(content);
                }
                self.request(Break::Paragraph);
            }
            "\\Sexpr" | "\\figure" | "USERMACRO" | "\\newcommand" | "\\renewcommand" => {}
            _ => self.nodes(parts),
        }
    }

    fn nodes(&mut self, nodes: &[Value]) {
        for node in nodes {
            self.node(node);
        }
    }

    fn wrapped(&mut self, node: &Value, marker: &str) {
        let inner = plain(Some(node));
        let inner = inner.trim();
        if !inner.is_empty() {
            self.raw(&format!("{marker}{inner}{marker}"));
        }
    }
}

/// A section as collapsed prose.
fn prose(section: &Value) -> String {
    let mut markdown = Markdown::new(false);
    markdown.nodes(children(section));
    markdown.output.trim().to_string()
}

/// A section as code, keeping its lines but dropping blank ones and common indentation.
fn code(section: &Value) -> String {
    let mut markdown = Markdown::new(true);
    markdown.nodes(children(section));
    let lines: Vec<&str> = markdown
        .output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let indent = lines
        .iter()
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| line.get(indent..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The plain text of a node, without Markdown markup.
fn plain(node: Option<&Value>) -> String {
    let mut text = String::new();
    if let Some(node) = node {
        collect_text(node, &mut text);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_text(node: &Value, text: &mut String) {
    match node {
        Value::Character(vector) if tag(node) != "COMMENT" => {
            for string in vector.elements.iter().flatten() {
                text.push_str(string);
            }
        }
        Value::List(_) => match tag(node) {
            "\\dots" | "\\ldots" => text.push_str("..."),
            "\\R" => text.push('R'),
            _ => {
                for child in children(node) {
                    collect_text(child, text);
                }
            }
        },
        _ => {}
    }
}

/// Inline code in backticks, with a longer fence when the code itself has a backtick.
fn inline_code(node: &Value) -> String {
    let text = plain(Some(node));
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn argument_items(section: &Value) -> Vec<(String, String)> {
    children(section)
        .iter()
        .filter(|item| tag(item) == "\\item")
        .filter_map(|item| {
            let parts = children(item);
            let name = plain(parts.first());
            if name.is_empty() {
                return None;
            }
            let mut markdown = Markdown::new(false);
            if let Some(text) = parts.get(1) {
                markdown.node(text);
            }
            // Indented continuation lines keep nested paragraphs and lists inside the
            // argument's own list item.
            let text = markdown.output.trim().replace('\n', "\n  ");
            Some((name, text.replace("\n  \n", "\n\n")))
        })
        .collect()
}

fn children(node: &Value) -> &[Value] {
    match node {
        Value::List(vector) => &vector.elements,
        _ => &[],
    }
}

fn tag(node: &Value) -> &str {
    node.attribute("Rd_tag")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}\u{2026}", text[..end].trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::unserialize::Vector, std::sync::Arc};

    fn tagged(tag: &str, mut value: Value) -> Value {
        let attribute = (
            Arc::from("Rd_tag"),
            Value::Character(Vector::new(vec![Some(tag.to_string())])),
        );
        match &mut value {
            Value::List(vector) => vector.attributes.push(attribute),
            Value::Character(vector) => vector.attributes.push(attribute),
            _ => {}
        }
        value
    }

    fn text(tag: &str, text: &str) -> Value {
        tagged(
            tag,
            Value::Character(Vector::new(vec![Some(text.to_string())])),
        )
    }

    fn list(tag: &str, elements: Vec<Value>) -> Value {
        tagged(tag, Value::List(Vector::new(elements)))
    }

    fn untagged(elements: Vec<Value>) -> Value {
        Value::List(Vector::new(elements))
    }

    #[test]
    fn renders_the_sections_a_hover_needs() {
        let rd = untagged(vec![
            list("\\title", vec![text("TEXT", "Apply a Function")]),
            list(
                "\\description",
                vec![
                    text("TEXT", "\n  Calls "),
                    list("\\code", vec![text("RCODE", "FUN")]),
                    text("TEXT", " on each\n   element.\n\n  Second "),
                    list("\\emph", vec![text("TEXT", "paragraph")]),
                    text("TEXT", " with "),
                    list("\\sQuote", vec![text("TEXT", "quotes")]),
                    text("COMMENT", "% hidden"),
                    text("TEXT", ".\n"),
                ],
            ),
            list(
                "\\usage",
                vec![
                    text("RCODE", "\n"),
                    text("RCODE", "lapply(X, FUN, "),
                    list("\\dots", Vec::new()),
                    text("RCODE", ")\n"),
                ],
            ),
            list(
                "\\arguments",
                vec![
                    text("TEXT", "\n"),
                    list(
                        "\\item",
                        vec![
                            untagged(vec![text("TEXT", "X")]),
                            untagged(vec![text("TEXT", "a vector\n    or list.")]),
                        ],
                    ),
                    list(
                        "\\item",
                        vec![
                            untagged(vec![list("\\dots", Vec::new())]),
                            untagged(vec![text("TEXT", "passed on.")]),
                        ],
                    ),
                ],
            ),
            list("\\examples", vec![text("RCODE", "lapply(1, print)")]),
        ]);
        assert_eq!(
            render(&rd),
            "**Apply a Function**\n\nCalls `FUN` on each element.\n\nSecond *paragraph* with \u{2018}quotes\u{2019}.\n\n```r\nlapply(X, FUN, ...)\n```\n\n**Arguments**\n\n- `X`: a vector or list.\n- `...`: passed on."
        );
    }
}
