//! The console's syntax highlighting: the classes of `syntax::highlight`, colored from a palette
//! of the 16 ANSI colors so the terminal's own theme decides the shades.

pub use nu_ansi_term::Style as ConsoleStyle;
use {
    nu_ansi_term::{Color, Style},
    reedline::{Highlighter, StyledText},
    syntax::{
        TextRange, TextSize,
        highlight::{Highlight, brackets, highlights},
        parse,
    },
};

/// The style of each highlight class, and of the bracket that pairs with the one at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    styles: [Style; KEYS.len()],
}

/// The names `[console.colors]` sets a style by, in [`Palette`] order. The last is the bracket
/// pair at the cursor; the rest follow [`Highlight`].
pub const KEYS: [&str; 15] = [
    "keyword",
    "constant",
    "number",
    "string",
    "comment",
    "annotation",
    "operator",
    "function",
    "namespace",
    "parameter",
    "property",
    "tag",
    "quoted-name",
    "error",
    "matching-bracket",
];

const MATCHING_BRACKET: usize = KEYS.len() - 1;

impl Default for Palette {
    /// Only hues that read on light and dark backgrounds alike: no yellow, white or black, and no
    /// bright variants, which some themes render near the background.
    fn default() -> Palette {
        let plain = Style::new();
        Palette {
            styles: [
                plain.fg(Color::Magenta).bold(),
                plain.fg(Color::Magenta),
                plain.fg(Color::Cyan),
                plain.fg(Color::Green),
                plain.dimmed().italic(),
                plain.fg(Color::Cyan).italic(),
                plain.bold(),
                plain.fg(Color::Blue),
                plain.fg(Color::Blue).italic(),
                plain.italic(),
                plain,
                plain.fg(Color::Magenta),
                plain.underline(),
                plain.fg(Color::Red).bold().underline(),
                plain.reverse(),
            ],
        }
    }
}

impl Palette {
    /// Sets the style named `key`, one of [`KEYS`].
    pub fn set(&mut self, key: &str, style: Style) -> Result<(), String> {
        let index = KEYS.iter().position(|known| *known == key).ok_or_else(|| {
            format!(
                "unknown console color `{key}`, expected one of {}",
                KEYS.join(", ")
            )
        })?;
        if let Some(slot) = self.styles.get_mut(index) {
            *slot = style;
        }
        Ok(())
    }

    fn style(&self, highlight: Highlight) -> Style {
        // `KEYS` lists the classes in `Highlight`'s order.
        self.styles
            .get(highlight as usize)
            .copied()
            .unwrap_or_default()
    }

    fn matching_bracket(&self) -> Style {
        self.styles
            .get(MATCHING_BRACKET)
            .copied()
            .unwrap_or_default()
    }
}

/// Reads a style written as words: at most one color (`red`, `green`, `yellow`, `blue`,
/// `magenta`, `cyan`, `black`, `white`, each also as `bright-red` and so on, or `default`) and
/// any of `bold`, `dim`, `italic`, `underline` and `reverse`. `none` is the plain style.
pub fn parse_style(spec: &str) -> Result<Style, String> {
    let mut style = Style::new();
    let mut colored = false;
    for word in spec.split_whitespace() {
        let color = match word {
            "none" => continue,
            "bold" => {
                style = style.bold();
                continue;
            }
            "dim" => {
                style = style.dimmed();
                continue;
            }
            "italic" => {
                style = style.italic();
                continue;
            }
            "underline" => {
                style = style.underline();
                continue;
            }
            "reverse" => {
                style = style.reverse();
                continue;
            }
            "default" => Color::Default,
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "bright-black" => Color::DarkGray,
            "bright-red" => Color::LightRed,
            "bright-green" => Color::LightGreen,
            "bright-yellow" => Color::LightYellow,
            "bright-blue" => Color::LightBlue,
            "bright-magenta" => Color::LightMagenta,
            "bright-cyan" => Color::LightCyan,
            "bright-white" => Color::LightGray,
            other => {
                return Err(format!(
                    "unknown style word `{other}` in `{spec}`: use a color such as `blue` or `bright-blue`, and `bold`, `dim`, `italic`, `underline` or `reverse`"
                ));
            }
        };
        if colored {
            return Err(format!("`{spec}` names more than one color"));
        }
        colored = true;
        style = style.fg(color);
    }
    Ok(style)
}

/// Colors the editor's buffer from its syntax tree. The whole buffer is parsed on each repaint,
/// which is linear in its length and so cheap per keystroke. Without a palette the buffer is
/// drawn plain, which reedline's own default highlighter does not do: it paints input white.
pub struct SyntaxHighlighter {
    pub palette: Option<Palette>,
}

impl Highlighter for SyntaxHighlighter {
    fn highlight(&self, line: &str, cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        match &self.palette {
            Some(palette) => {
                for (range, style) in styled_spans(line, cursor, palette) {
                    if let Some(text) = line.get(range) {
                        styled.push((style, text.to_owned()));
                    }
                }
            }
            None => styled.push((Style::new(), line.to_owned())),
        }
        if styled.buffer.is_empty() {
            styled.push((Style::new(), String::new()));
        }
        styled
    }
}

/// `line` cut into consecutive byte ranges, each with its style.
pub fn styled_spans(
    line: &str,
    cursor: usize,
    palette: &Palette,
) -> Vec<(std::ops::Range<usize>, Style)> {
    let root = parse(line).syntax_node();
    let mut spans: Vec<(TextRange, Style)> = highlights(&root)
        .into_iter()
        .map(|(range, highlight)| (range, palette.style(highlight)))
        .collect();
    if let Some(pair) = bracket_pair_at(&root, cursor) {
        for range in pair {
            spans.retain(|(span, _)| span.intersect(range).is_none_or(|common| common.is_empty()));
            let index = spans.partition_point(|(span, _)| span.start() < range.start());
            spans.insert(index, (range, palette.matching_bracket()));
        }
    }
    let mut result = Vec::with_capacity(spans.len() * 2 + 1);
    let mut offset = 0;
    for (range, style) in spans {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if start < offset {
            continue;
        }
        if start > offset {
            result.push((offset..start, Style::new()));
        }
        result.push((start..end, style));
        offset = end;
    }
    if offset < line.len() {
        result.push((offset..line.len(), Style::new()));
    }
    result
}

/// The bracket under the cursor and its partner, or else the bracket just before the cursor and
/// its partner, as a closer just typed.
fn bracket_pair_at(root: &syntax::SyntaxNode, cursor: usize) -> Option<[TextRange; 2]> {
    let cursor = TextSize::try_from(cursor).ok()?;
    let pairs = brackets(root).pairs;
    let under = |range: TextRange| range.start() == cursor;
    let before = |range: TextRange| range.end() == cursor;
    pairs
        .iter()
        .find(|(open, close)| under(*open) || under(*close))
        .or_else(|| {
            pairs
                .iter()
                .find(|(open, close)| before(*open) || before(*close))
        })
        .map(|(open, close)| [*open, *close])
}
