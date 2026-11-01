//! A preserving formatter: single-line constructs stay single-line and
//! multiline ones keep their line structure. Syntax errors refuse the file,
//! except inside `#:` annotations, whose block is kept verbatim.

use {
    serde::Deserialize,
    std::{
        error::Error,
        fmt::{self, Display},
        iter::{once, repeat_n, successors},
    },
    syntax::{
        SyntaxElement,
        SyntaxKind::{self, *},
        SyntaxNode, SyntaxToken, TextRange, TextSize, lex, parse,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub indent_width: usize,
    pub line_ending: LineEnding,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            indent_width: 2,
            line_ending: LineEnding::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEnding {
    Auto,
    Lf,
    CrLf,
}

/// `line` and `column` are zero-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Syntax error: {} at line {}, column {}",
            self.message,
            self.line + 1,
            self.column + 1
        )
    }
}

impl Error for FormatError {}

pub fn format(source: &str, config: Config) -> Result<String, FormatError> {
    layout(source, config, &" ".repeat(config.indent_width)).map(|layout| layout.text)
}

/// A formatted text, and where it placed each statement it laid out itself.
struct Layout {
    text: String,
    /// Each statement and standalone comment the formatter placed (not one in
    /// a `# fmt: off` region or exempted by `# fmt: skip`), as its source
    /// offset and the indentation level it was placed at.
    statement_levels: Vec<(TextSize, usize)>,
}

fn layout(source: &str, config: Config, indent: &str) -> Result<Layout, FormatError> {
    let parse = parse(source);
    let root = parse.syntax_node();
    let line_starts = line_starts(source);
    let error_at = |message: String, offset: TextSize| {
        let offset = usize::from(offset);
        let line = line_of(&line_starts, offset);
        FormatError {
            message,
            line,
            column: offset - line_starts[line],
        }
    };

    if let Some(error) = parse.errors().iter().find(|error| !error.in_annotation) {
        return Err(error_at(error.message.clone(), error.range.start()));
    }

    // A mid-line `#:` region with its statement continuing below (`f <-#: T`)
    // has no stable line layout, so it refuses like a syntax error.
    for node in root.descendants().filter(|node| node.kind() == ANNOTATION) {
        let offset = node.text_range().start();
        if line_prefix(source, &line_starts, usize::from(offset))
            .chars()
            .all(char::is_whitespace)
        {
            continue;
        }
        // Blocks are expressions in R, so the statement boundary is the
        // nearest brace block or the root, not the nearest expression.
        let statement = |mut candidate: SyntaxNode| {
            while let Some(parent) = candidate.parent() {
                if parent.kind() == BRACE_EXPR || parent.parent().is_none() {
                    break;
                }
                candidate = parent;
            }
            candidate
        };
        let continues = successors(
            node.last_token().and_then(|token| token.next_token()),
            SyntaxToken::next_token,
        )
        .find(|token| !is_blank(token.kind()))
        .and_then(|token| token.parent())
        .is_some_and(|parent| statement(parent) == statement(node.clone()));
        if continues {
            return Err(error_at(
                "a `#:` annotation cannot interrupt an expression. Move it to its own line"
                    .to_owned(),
                offset,
            ));
        }
    }

    let line_ending = match config.line_ending {
        LineEnding::Auto => {
            // Leading blank lines and empty statements are deleted, so the
            // first line break after content decides: deciding from a deleted
            // one would let a second pass see a different first break. A file
            // with no break after its content falls back to its first.
            let content = source.trim_start_matches([' ', '\t', '\u{c}', '\r', '\n', ';']);
            let first_line = content
                .find('\n')
                .map(|at| &content[..at])
                .or_else(|| source.find('\n').map(|at| &source[..at]));
            match first_line.is_some_and(|line| line.ends_with('\r')) {
                true => "\r\n",
                false => "\n",
            }
        }
        LineEnding::Lf => "\n",
        LineEnding::CrLf => "\r\n",
    };

    let mut formatter = Formatter {
        source,
        line_starts,
        error_ranges: parse
            .errors()
            .iter()
            .filter(|error| error.in_annotation)
            .map(|error| error.range)
            .collect(),
        indent: indent.to_owned(),
        line_ending,
        out: String::with_capacity(source.len() * 3 / 2),
        comment_end: None,
        statement_levels: Vec::new(),
    };
    formatter.source_file(&root);
    Ok(Layout {
        text: formatter.out,
        statement_levels: formatter.statement_levels,
    })
}

/// write there. An empty range is an insertion, empty text a deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: TextRange,
    pub new_text: String,
}

/// The edits that carry `source` to its formatted form, each as small as the
/// statements it changes allow — what an editor's "Format Document" applies,
/// so that untouched lines keep their place rather than being rewritten.
pub fn format_edits(source: &str, config: Config) -> Result<Vec<TextEdit>, FormatError> {
    let formatted = format(source, config)?;
    Ok(changed_spans(source, &formatted)
        .into_iter()
        .map(|(in_source, in_formatted)| TextEdit {
            range: in_source,
            new_text: formatted[in_formatted].to_owned(),
        })
        .collect())
}

/// The edits that lay out the lines `selection` touches exactly as formatting
/// the whole file lays them out at the file's own indentation, leaving every
/// other line of the file byte for byte as it was.
///
/// The text put in is *cut out of* the whole file's formatted form rather than
/// produced by formatting the selected text on its own, and that is what makes
/// the two agree by construction. Formatting a slice would lose everything
/// about the slice's surroundings that decides its layout: the indentation
/// depth it sits at, a `# fmt: off` region opened above it, a
/// `# fmt: skip-file` header, and the line endings the rest of the file uses.
///
/// That whole-file form is indented by the step the file already uses
/// ([`format_in_place`]), not by `indent-width`. A selection is a request to
/// tidy some code where it sits: laid out at the configured width inside a
/// file indented by four spaces or tabs, the selected lines would come out at
/// a different depth from the unselected lines beside them. Re-indenting the
/// file is what formatting the whole file is for.
///
/// The selection widens to whole lines, and a statement is laid out as one
/// unit, so a line just outside the selection can still change when it belongs
/// to a statement the selection reaches into. That is the only way an
/// unselected line changes.
///
/// Edits are ordered, and never overlap or share an offset with an insertion,
/// so a client may apply them in any order. A file that does not parse refuses
/// exactly as whole-file formatting refuses: a slice of a file whose structure
/// is unknown has no trustworthy layout either.
pub fn format_range(
    source: &str,
    config: Config,
    selection: TextRange,
) -> Result<Vec<TextEdit>, FormatError> {
    let formatted = format_in_place(source, config)?;
    let selected = selected_lines(source, selection);
    Ok(changed_spans(source, &formatted)
        .into_iter()
        // A span that only inserts covers no source text, so it touches the
        // selection when it lands inside it or on either edge. Counting the
        // far edge is what lets an insertion at the very end of the file be
        // reached at all — no line starts there — so selecting the whole file
        // always carries every edit whole-file formatting makes.
        .filter(|(in_source, _)| match in_source.is_empty() {
            true => selected.start() <= in_source.start() && in_source.start() <= selected.end(),
            false => in_source.start() < selected.end() && in_source.end() > selected.start(),
        })
        .map(|(in_source, in_formatted)| TextEdit {
            range: in_source,
            new_text: formatted[in_formatted].to_owned(),
        })
        .collect())
}

/// `source` formatted as a whole, but with one level of indentation being
/// the step the file already uses rather than `indent-width` — what a
/// formatted selection is cut from.
///
/// The step is read off the statements the formatter places: a statement it
/// puts at level `n` and that the source indents by `n` copies of one string
/// is a vote for that string, and the commonest wins (the first seen on a
/// tie). Levels come from the file's structure, never from its indentation,
/// so formatting part of the file cannot move a statement to another level;
/// it can only re-indent statements by the winning step, which adds to its
/// lead. That is what keeps formatting a selection from changing the step
/// the rest of the file is then formatted with. A file with no indented
/// statement to read has no step of its own and gets `indent-width`.
fn format_in_place(source: &str, config: Config) -> Result<String, FormatError> {
    let configured = " ".repeat(config.indent_width);
    let configured_layout = layout(source, config, &configured)?;
    let mut votes: Vec<(&str, usize)> = Vec::new();
    for &(offset, level) in &configured_layout.statement_levels {
        let Some(line_start) = indented_line_start(source, offset) else {
            continue;
        };
        let indent = &source[usize::from(line_start)..usize::from(offset)];
        if level == 0 || indent.is_empty() || !indent.len().is_multiple_of(level) {
            continue;
        }
        let step = &indent[..indent.len() / level];
        if step.repeat(level) != indent {
            continue;
        }
        match votes.iter_mut().find(|(seen, _)| *seen == step) {
            Some((_, count)) => *count += 1,
            None => votes.push((step, 1)),
        }
    }
    match votes
        .into_iter()
        .min_by_key(|&(_, count)| std::cmp::Reverse(count))
    {
        Some((step, _)) if step != configured => {
            layout(source, config, step).map(|layout| layout.text)
        }
        _ => Ok(configured_layout.text),
    }
}

/// `source` with `edits` applied — the document the editor ends up with.
/// Edits are ordered and disjoint, so applying them back to front leaves every
/// range still valid when its turn comes.
pub fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut text = source.to_owned();
    for edit in edits.iter().rev() {
        text.replace_range(
            usize::from(edit.range.start())..usize::from(edit.range.end()),
            &edit.new_text,
        );
    }
    text
}

/// The whole lines `selection` touches, from the first one's start to the last
/// one's end.
///
/// A selection ending at the first column of a line covers none of that line —
/// exactly what an editor sends for "these whole lines" — so the last line is
/// the one holding the last selected character. An empty selection is a bare
/// caret and takes the line it sits on; a caret after the final line break
/// takes the last line, since there is no line after it to take. Offsets are
/// read as bytes, so a selection need not land on a character boundary.
fn selected_lines(text: &str, selection: TextRange) -> TextRange {
    let bytes = text.as_bytes();
    let on_a_line = |offset: usize| match offset == bytes.len() && bytes.ends_with(b"\n") {
        true => offset - 1,
        false => offset,
    };
    let start = usize::from(selection.start()).min(bytes.len());
    let end = usize::from(selection.end()).min(bytes.len());
    let first = on_a_line(start);
    let last = on_a_line(if end > start { end - 1 } else { start });
    let first_line = bytes[..first]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |at| at + 1);
    let past_last_line = bytes[last..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |at| last + at + 1);
    TextRange::new(
        TextSize::new(first_line as u32),
        TextSize::new(past_last_line as u32),
    )
}

/// Where the source and its formatted form differ, as pairs of whole-line
/// ranges that hold the same code — the source text to replace, and the
/// formatted text to put there.
///
/// The ranges come from splitting both texts at *corresponding* points rather
/// than from diffing their lines. A line diff answers "which lines look
/// different", which is not the question: it is free to pair up two lines that
/// merely read alike, and a range cut that way can be text the formatter would
/// never produce for any input. Pairing the two parse trees instead — walking
/// them together and stopping wherever their shapes diverge — means a range
/// always holds whole statements on both sides, so swapping one in yields a
/// document that is still exactly this file, formatted a bit further.
fn changed_spans(source: &str, formatted: &str) -> Vec<(TextRange, TextRange)> {
    let mut statements = Vec::new();
    paired_statements(
        &parse(source).syntax_node(),
        &parse(formatted).syntax_node(),
        &mut statements,
    );

    // Only a statement starting a line's content can begin a span: the text
    // before it on its line is its own indentation, which the formatter
    // rewrites along with it. A statement starting mid-line (`x; y`) shares its
    // layout with what precedes it, so it is not a place the two texts can be
    // cut apart.
    let mut splits = vec![(TextSize::new(0), TextSize::new(0))];
    for (in_source, in_formatted) in statements {
        let (Some(source_line), Some(formatted_line)) = (
            indented_line_start(source, in_source),
            indented_line_start(formatted, in_formatted),
        ) else {
            continue;
        };
        let &(last_source, last_formatted) = splits.last().expect("seeded with the file's start");
        if source_line > last_source && formatted_line > last_formatted {
            splits.push((source_line, formatted_line));
        }
    }
    splits.push((TextSize::of(source), TextSize::of(formatted)));

    let mut spans: Vec<(TextRange, TextRange)> = Vec::new();
    for window in splits.windows(2) {
        let [(source_start, formatted_start), (source_end, formatted_end)] = *window else {
            continue;
        };
        let (in_source, in_formatted) = (
            TextRange::new(source_start, source_end),
            TextRange::new(formatted_start, formatted_end),
        );
        // Lines already identical at either end of the span are not part of the
        // change, and dropping them is exact: the same swap, just narrower. It
        // is what keeps a one-space fix inside a long call from rewriting the
        // whole call.
        let leading: TextSize = source[in_source]
            .split_inclusive('\n')
            .zip(formatted[in_formatted].split_inclusive('\n'))
            .take_while(|(left, right)| left == right)
            .map(|(line, _)| TextSize::of(line))
            .sum();
        let (in_source, in_formatted) = (
            TextRange::new(in_source.start() + leading, in_source.end()),
            TextRange::new(in_formatted.start() + leading, in_formatted.end()),
        );
        let trailing: TextSize = source[in_source]
            .split_inclusive('\n')
            .rev()
            .zip(formatted[in_formatted].split_inclusive('\n').rev())
            .take_while(|(left, right)| left == right)
            .map(|(line, _)| TextSize::of(line))
            .sum();
        let (in_source, in_formatted) = (
            TextRange::new(in_source.start(), in_source.end() - trailing),
            TextRange::new(in_formatted.start(), in_formatted.end() - trailing),
        );
        if in_source.is_empty() && in_formatted.is_empty() {
            continue;
        }
        // A span that replaces nothing is an insertion, and an insertion at the
        // offset where another edit starts or ends has no defined order against
        // it. Merging the two into one replacement is what keeps every edit
        // unambiguous; spans that both replace text may abut safely, since
        // their ranges still say which text is whose.
        match spans.last_mut() {
            Some((last_source, last_formatted))
                if last_source.end() == in_source.start()
                    && last_formatted.end() == in_formatted.start()
                    && (last_source.is_empty() || in_source.is_empty()) =>
            {
                *last_source = last_source.cover(in_source);
                *last_formatted = last_formatted.cover(in_formatted);
            }
            _ => spans.push((in_source, in_formatted)),
        }
    }
    spans
}

/// Where each statement of `source` starts, paired with where the statement
/// holding the same code starts in `formatted`, in source order.
///
/// Statements are the unit because they are the only thing this formatter lays
/// out independently: it decides how to break a call or a block from that
/// construct's *own* line structure, so swapping a formatted argument into a
/// half-formatted call can flip the decision for the whole call and produce
/// text neither form contains. Whole statements have no such coupling — every
/// one starts its own line at a fixed indent, and nothing above it decides how
/// it breaks — which is exactly the property that lets a selection be formatted
/// at all. They nest, so a statement inside a function body is a unit of its
/// own and selecting one line of a long function still rewrites only that
/// line's statement.
///
/// The walk descends only while the two nodes have the same children in the
/// same order, so it cannot pair up constructs that are not each other. That is
/// what rules out the formatter's structural rewrites: bracing a bare `if` body
/// wraps it in a node the source has no counterpart for, and the walk stops at
/// the `if` rather than pairing its body with the new block — the difference
/// then belongs to the `if` as a whole, which is the only way to describe it.
fn paired_statements(
    source: &SyntaxNode,
    formatted: &SyntaxNode,
    pairs: &mut Vec<(TextSize, TextSize)>,
) {
    // A braced block is an expression in R, so the node kind is what marks a
    // sequence of statements — not the position in the tree.
    let sequence = matches!(
        source.kind(),
        SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
    );
    // Both lists are stepped together so that one running out while the other
    // has more counts as a difference — pairing up what is left would pair
    // constructs that are not each other.
    let mut source_children = members(source, sequence);
    let mut formatted_children = members(formatted, sequence);
    let same_shape = loop {
        match (source_children.next(), formatted_children.next()) {
            (None, None) => break true,
            (Some(left), Some(right)) if left.kind() == right.kind() => {}
            _ => break false,
        }
    };
    if !same_shape {
        return;
    }
    for (source_child, formatted_child) in
        members(source, sequence).zip(members(formatted, sequence))
    {
        if sequence {
            pairs.push((
                source_child.text_range().start(),
                formatted_child.text_range().start(),
            ));
        }
        if let (Some(source_child), Some(formatted_child)) =
            (source_child.as_node(), formatted_child.as_node())
        {
            paired_statements(source_child, formatted_child, pairs);
        }
    }
}

/// The children [`paired_statements`] pairs up: every child node, and — in a
/// statement sequence — every comment standing on its own between statements.
///
/// Such a comment is a unit like a statement: it holds its own line and nothing
/// around it decides how it is laid out, so selecting it need not drag in the
/// statement above. A comment *trailing* a statement belongs to that
/// statement's node instead and is never one of these.
fn members(node: &SyntaxNode, sequence: bool) -> impl Iterator<Item = SyntaxElement> {
    node.children_with_tokens().filter(move |element| {
        element.as_node().is_some() || (sequence && element.kind() == SyntaxKind::COMMENT)
    })
}

/// The start of the line `offset` is on, when nothing but indentation precedes
/// `offset` there.
///
/// Only spaces and tabs count. A bare carriage return is a line break to the
/// lexer but not to a text editor, so a line split on `\n` can hold one: it
/// looks like whitespace before the statement and is in fact a break the
/// formatter counts, and a span that swallowed it would silently join two lines
/// the formatter had kept apart.
fn indented_line_start(text: &str, offset: TextSize) -> Option<TextSize> {
    let before = text.as_bytes().get(..usize::from(offset))?;
    let line_start = before
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |at| at + 1);
    before[line_start..]
        .iter()
        .all(|&byte| byte == b' ' || byte == b'\t')
        .then(|| TextSize::new(line_start as u32))
}

/// Shared with the coverage-guided `fuzz/` targets so the contract has one home.
pub fn check_format_invariants(input: &str) {
    let first = format(input, Config::default());
    let again = format(input, Config::default());
    assert_eq!(first, again, "non-deterministic format for input {input:?}");

    if let Ok(output) = first {
        assert_eq!(
            significant_tokens(input),
            significant_tokens(&output),
            "formatting changed the code for input {input:?} (output {output:?})"
        );
        match format(&output, Config::default()) {
            Ok(second) => assert_eq!(second, output, "format not idempotent for input {input:?}"),
            Err(error) => panic!(
                "formatted output no longer formats for input {input:?}: {error} (output {output:?})"
            ),
        }
    }
}

/// The fuzz invariant battery for range formatting over one input, swept
/// across a bounded, deterministic set of selections. "In place" below is the
/// whole file formatted at the indentation it already uses, which is what a
/// selection is cut from. The contract every selection must hold:
///
///  1. determinism, and refusal exactly when whole-file formatting refuses;
///  2. geometry — edits are ordered, disjoint, whole lines, and in bounds;
///  3. preservation — applying them keeps every token, so no code is lost;
///  4. convergence — the applied document still formats to what the original
///     formats to, both as a whole file and in place, which is the property
///     that makes formatting a selection safe to do repeatedly and in any
///     order;
///  5. restriction — every edit is one formatting the whole file in place
///     would have made, so a selection can only ever do less, never something
///     else;
///  6. completeness — selecting the whole file reproduces formatting it in
///     place byte for byte, and the whole-document edits reproduce whole-file
///     formatting byte for byte;
///  7. stability — the region the edits produced is already laid out, so
///     formatting the same selection again is a no-op;
///  8. additivity — a selection of whole lines makes exactly the edits its
///     lines make one at a time, so the result never depends on how a region
///     was selected;
///  9. composition — formatting one line at a time, top to bottom and swept
///     until a sweep changes nothing, reaches formatting in place, and every
///     step on the way still formats to it.
///
/// Additivity and composition re-run range formatting per line, so they are
/// checked on small inputs only, which is all a fuzzer generates.
pub fn check_range_format_invariants(input: &str) {
    let config = Config::default();
    let whole_file = format(input, config);
    let in_place = format_in_place(input, config);
    if let Ok(formatted) = &whole_file {
        let document = format_edits(input, config).expect("the file formats");
        assert_eq!(
            &apply_edits(input, &document),
            formatted,
            "the whole-document edits do not reproduce whole-file formatting for {input:?}"
        );
    }
    let length = TextSize::of(input);
    let everything = format_range(input, config, TextRange::up_to(length));
    for selection in probe_selections(input) {
        let first = format_range(input, config, selection);
        let again = format_range(input, config, selection);
        assert_eq!(
            first, again,
            "non-deterministic range format for {selection:?} of {input:?}"
        );
        let edits = match (first, &whole_file) {
            (Err(error), Err(expected)) => {
                assert_eq!(
                    &error, expected,
                    "range format refused differently than whole-file formatting for {input:?}"
                );
                continue;
            }
            (Err(error), Ok(_)) => {
                panic!("range format refused {selection:?} of a file that formats: {error}")
            }
            (Ok(edits), Err(error)) => {
                panic!("range format produced {edits:?} for a file that refuses: {error}")
            }
            (Ok(edits), Ok(_)) => edits,
        };
        let formatted = whole_file.as_ref().expect("checked above");
        let in_place = in_place
            .as_ref()
            .expect("refuses exactly when formatting does");
        let everything = everything.as_ref().expect("checked above");

        for edit in &edits {
            assert!(
                edit.range.end() <= length
                    && starts_line(input, edit.range.start())
                    && starts_line(input, edit.range.end()),
                "edit {edit:?} is out of bounds or does not cover whole lines in {input:?}"
            );
            assert!(
                everything.contains(edit),
                "edit {edit:?} is not one formatting {input:?} in place would make"
            );
        }
        for pair in edits.windows(2) {
            let [before, after] = pair else { continue };
            // An insertion sharing an offset with the edit beside it has no
            // defined application order, so it is as much a defect as an
            // overlap. Two replacements may abut: their ranges still say which
            // text belongs to which.
            let abut = before.range.end() == after.range.start()
                && !before.range.is_empty()
                && !after.range.is_empty();
            assert!(
                before.range.end() < after.range.start() || abut,
                "edits {before:?} and {after:?} are out of order, overlap, or share an insertion's offset in {input:?}"
            );
        }

        let result = apply_edits(input, &edits);
        assert_eq!(
            significant_tokens(input),
            significant_tokens(&result),
            "range formatting {selection:?} changed the code in {input:?} (result {result:?})"
        );
        assert_eq!(
            format(&result, config).as_ref(),
            Ok(formatted),
            "range formatting {selection:?} changed what {input:?} formats to (result {result:?})"
        );
        assert_eq!(
            format_in_place(&result, config).as_ref(),
            Ok(in_place),
            "range formatting {selection:?} changed what {input:?} formats to in place (result {result:?})"
        );
        if selection.start() == TextSize::new(0) && selection.end() >= length {
            assert_eq!(
                &result, in_place,
                "selecting the whole file did not reproduce formatting {input:?} in place"
            );
        }

        let (Some(first_edit), Some(last_edit)) = (edits.first(), edits.last()) else {
            continue;
        };
        // Where the edits' text ends up in the result: everything before the
        // first edit stays put, and the last one ends shifted by what all of
        // them put in against what they took out.
        let inserted: TextSize = edits
            .iter()
            .map(|edit| TextSize::of(edit.new_text.as_str()))
            .sum();
        let removed: TextSize = edits.iter().map(|edit| edit.range.len()).sum();
        let touched = TextRange::new(
            first_edit.range.start(),
            last_edit.range.end() + inserted - removed,
        );
        // Edits that only delete leave no lines behind to re-check, and the
        // empty range they collapse to would pick up the untouched line that
        // moved into its place.
        if touched.is_empty() {
            continue;
        }
        assert_eq!(
            format_range(&result, config, touched),
            Ok(Vec::new()),
            "range formatting {selection:?} of {input:?} left {touched:?} of {result:?} unformatted"
        );
    }

    let Ok(in_place) = &in_place else {
        return;
    };
    let starts = line_starts_of(input);
    if starts.len() > 24 {
        return;
    }
    let caret_edits = |text: &str, start: TextSize| {
        format_range(text, config, TextRange::empty(start))
            .expect("a file that formats never refuses a selection")
    };
    let per_line: Vec<Vec<TextEdit>> = starts
        .iter()
        .map(|&start| caret_edits(input, start))
        .collect();
    for first in 0..starts.len() {
        for last in first..starts.len() {
            let end = starts.get(last + 1).copied().unwrap_or(length);
            let selection = TextRange::new(starts[first], end);
            let mut union: Vec<TextEdit> = per_line[first..=last].concat();
            union.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
            union.dedup();
            assert_eq!(
                format_range(input, config, selection),
                Ok(union),
                "selecting lines {first}..={last} of {input:?} is not the union of formatting each"
            );
        }
    }

    let mut document = input.to_owned();
    for _ in 0..=starts.len() {
        let before = document.clone();
        let mut line = 0;
        while let Some(&start) = line_starts_of(&document).get(line) {
            document = apply_edits(&document, &caret_edits(&document, start));
            assert_eq!(
                format_in_place(&document, config).as_ref(),
                Ok(in_place),
                "formatting {input:?} line by line changed what it formats to in place at {document:?}"
            );
            line += 1;
        }
        if document == before {
            break;
        }
    }
    assert_eq!(
        &document, in_place,
        "formatting {input:?} line by line did not reach formatting it in place"
    );
}

/// Where each line of `text` starts; a final line break opens no line.
fn line_starts_of(text: &str) -> Vec<TextSize> {
    std::iter::once(TextSize::new(0))
        .chain(
            text.match_indices('\n')
                .map(|(at, _)| TextSize::new(at as u32 + 1))
                .filter(|&start| start < TextSize::of(text)),
        )
        .collect()
}

/// Whether `offset` is the first byte of a line (or the end of the text).
fn starts_line(text: &str, offset: TextSize) -> bool {
    let offset = usize::from(offset);
    offset == 0 || offset == text.len() || text.as_bytes().get(offset - 1) == Some(&b'\n')
}

/// A bounded, deterministic spread of selections over one input: the whole
/// file, bare carets, whole lines, spans of several lines, part-line spans,
/// and a range past the end. Line count is sampled rather than enumerated so
/// the battery stays linear in the fuzzer's budget rather than quadratic in
/// file size.
fn probe_selections(text: &str) -> Vec<TextRange> {
    let length = TextSize::of(text);
    let mut selections = vec![
        TextRange::up_to(length),
        TextRange::empty(TextSize::new(0)),
        TextRange::empty(length),
        TextRange::new(length, length + TextSize::new(7)),
    ];
    let starts = line_starts_of(text);
    let step = (starts.len() / 8).max(1);
    for (index, &start) in starts.iter().enumerate().step_by(step).take(12) {
        let end = starts.get(index + 1).copied().unwrap_or(length);
        selections.push(TextRange::new(start, end));
        selections.push(TextRange::empty(start));
        // A caret in the middle of the line, and a span that starts and ends
        // inside one — neither lands on a line boundary, and neither is
        // required to land on a character boundary either.
        let middle = start + TextSize::new(u32::from(end - start) / 2);
        selections.push(TextRange::empty(middle));
        selections.push(TextRange::new(middle, end));
        let next = starts.get(index + 2).copied().unwrap_or(length);
        selections.push(TextRange::new(middle, next));
    }
    selections
}

struct Formatter<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    /// Annotation-grammar errors: the blocks containing them render verbatim.
    error_ranges: Vec<TextRange>,
    indent: String,
    line_ending: &'static str,
    out: String,
    /// Output length right after the last comment: anything written on the
    /// same line after it would become comment text.
    comment_end: Option<usize>,
    /// See [`Layout::statement_levels`].
    statement_levels: Vec<(TextSize, usize)>,
}

impl Formatter<'_> {
    fn source_file(&mut self, root: &SyntaxNode) {
        let elements = child_elements(root);
        if elements.first().and_then(directive) == Some(Directive::SkipFile) {
            self.out.push_str(self.source);
            return;
        }
        self.item_sequence(&elements, 0);
        if !elements.is_empty() || !self.source.is_empty() {
            self.newline(0);
        }
    }

    fn item_sequence(&mut self, elements: &[SyntaxElement], level: usize) {
        let mut enabled = true;
        let mut pending_directive = None;
        let mut previous: Option<&SyntaxElement> = None;
        for element in elements {
            match pending_directive {
                Some(Directive::On) => enabled = true,
                Some(Directive::Off) => enabled = false,
                _ => {}
            }
            // A trailing comment attaches inside the code it follows once the
            // formatter drops the `;` between them, so only a comment that
            // starts its line is stable as a directive.
            pending_directive = directive(element).filter(|_| !self.trails(previous, element));
            if element.kind() == SEMICOLON {
                continue;
            }
            if enabled {
                if is_comment(element) && self.trails(previous, element) {
                    self.space();
                } else {
                    self.newlines_between(previous, element, level);
                    if element.as_node().is_none_or(|node| !self.skip_exempt(node)) {
                        self.statement_levels
                            .push((element.text_range().start(), level));
                    }
                }
                self.element(element, level, false);
            } else {
                // Only the directive comments inside an off region are formatted.
                let range = element.text_range();
                let start = previous.map_or(range.start(), |previous| previous.text_range().end());
                let end = if pending_directive.is_some() {
                    range.start()
                } else {
                    range.end()
                };
                self.out.push_str(&self.source[TextRange::new(start, end)]);
                if pending_directive.is_some() {
                    self.element(element, level, false);
                }
            }
            previous = Some(element);
        }
    }

    fn element(&mut self, element: &SyntaxElement, level: usize, make_multiline: bool) {
        // Catches joins that do not know a comment interposed (an operand
        // continuing an operator across a commented line break).
        if let Some(comment_end) = self.comment_end.take()
            && element.kind() != COMMENT
            && !self.out[comment_end..].contains('\n')
        {
            self.newline(level);
        }
        match element {
            SyntaxElement::Node(node) => self.node(node, level, make_multiline),
            SyntaxElement::Token(token) => match token.kind() {
                COMMENT => self.comment(token),
                STRING => self.string(token),
                _ => self.out.push_str(token.text()),
            },
        }
    }

    fn node(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        match node.kind() {
            BINARY_EXPR => self.binary(node, level),
            UNARY_EXPR => self.unary(node, level),
            PAREN_EXPR => self.paren(node, level),
            BRACE_EXPR => self.braces(node, level, make_multiline),
            CALL_EXPR | SUBSET_EXPR | SUBSET2_EXPR => self.call_like(node, level),
            ARGUMENT_LIST | PARAMETER_LIST => self.argument_list(node, level),
            DOLLAR_EXPR | AT_EXPR | NAMESPACE_EXPR => self.field_access(node, level),
            FUNCTION_DEF => self.function_definition(node, level),
            IF_EXPR => self.conditional(node, level, make_multiline),
            WHILE_EXPR => self.conditional(node, level, true),
            FOR_EXPR => self.for_expression(node, level),
            REPEAT_EXPR => self.repeat_expression(node, level),
            MATCH_EXPR => self.match_expression(node, level),
            MATCH_ARM => self.match_arm(node, level),
            MATCH_GUARD => self.match_guard(node, level),
            PATTERN_WILDCARD | PATTERN_BIND | PATTERN_LITERAL | PATTERN_TAG | PATTERN_LIST
            | PATTERN_REST | PATTERN_OR | PATTERN_PAREN => self.pattern(node),
            ANNOTATION => {
                self.annotation(node, level);
                // Nothing may follow an annotation on its line either.
                self.comment_end = Some(self.out.len());
            }
            TYPE_ASCRIPTION | RETURN_TYPE | TYPE_BINDER_LIST => self.inline_annotation(node),
            TYPE_DEFINITION => self.type_definition(node, level),
            NEW_TYPE => {
                self.out.push('@');
                for child in node.children() {
                    self.inline_annotation(&child);
                }
            }
            _ => {
                for element in child_elements(node) {
                    self.element(&element, level, false);
                }
            }
        }
    }

    /// A skip comment alone on the line above, or trailing on the node's last
    /// line, where it may attach inside the node or as its next sibling.
    fn skip_exempt(&self, node: &SyntaxNode) -> bool {
        let is_skip = |token: &SyntaxToken| {
            token.kind() == COMMENT && parse_directive(token.text()) == Some(Directive::Skip)
        };
        let previous_significant = |element: Option<SyntaxElement>| {
            successors(element, SyntaxElement::prev_sibling_or_token)
                .find(|element| !is_blank(element.kind()))
        };
        if let Some(SyntaxElement::Token(comment)) =
            previous_significant(node.prev_sibling_or_token())
            && is_skip(&comment)
            && previous_significant(comment.prev_sibling_or_token())
                .is_none_or(|before| !self.same_line(before.text_range(), comment.text_range()))
        {
            return true;
        }
        if let Some(SyntaxElement::Token(comment)) = successors(
            node.next_sibling_or_token(),
            SyntaxElement::next_sibling_or_token,
        )
        .find(|element| element.kind() != WHITESPACE)
            && is_skip(&comment)
        {
            return true;
        }
        if let Some(token) = last_token_before(node, node.text_range().end())
            && is_skip(&token)
        {
            return last_token_before(node, token.text_range().start())
                .is_some_and(|code| self.same_line(code.text_range(), token.text_range()));
        }
        false
    }

    /// When only whitespace precedes the node on its line, the original
    /// indentation replaces the emitted one, so column alignment survives.
    fn emit_skipped(&mut self, node: &SyntaxNode) {
        let range = significant_range(&SyntaxElement::Node(node.clone()));
        let leading = line_prefix(self.source, &self.line_starts, usize::from(range.start()));
        if leading.chars().all(char::is_whitespace) {
            let content_length = self.out.trim_end_matches(' ').len();
            if content_length == 0 || self.out[..content_length].ends_with(self.line_ending) {
                self.out.truncate(content_length);
                self.out.push_str(leading);
            }
        }
        self.out.push_str(&self.source[range]);
    }

    fn comment(&mut self, token: &SyntaxToken) {
        self.comment_end = Some(self.out.len());
        let text = token.text().trim_end();
        let mut chars = text.chars();
        match chars.nth(1) {
            Some(char @ ('\'' | '*')) => match chars.next() {
                Some(' ') | None => self.out.push_str(text),
                Some(_) if char == '\'' && chars.clone().any(|c| c == '\'') => {
                    self.out.push_str(text)
                }
                Some(other) => {
                    self.out.push('#');
                    self.out.push(char);
                    self.out.push(' ');
                    self.out.push(other);
                    self.out.extend(chars);
                }
            },
            Some('#' | '!' | '|' | ' ') | None => self.out.push_str(text),
            Some(other) => {
                self.out.push_str("# ");
                self.out.push(other);
                self.out.extend(chars);
            }
        }
    }

    fn string(&mut self, token: &SyntaxToken) {
        let text = token.text();
        if matches!(text.chars().next(), Some('r' | 'R')) {
            self.out.push_str(text);
            return;
        }
        let content = &text[1..text.len().saturating_sub(1)];
        let mut all_quotes_escaped = true;
        let mut previous_was_escape = false;
        for char in content.chars() {
            if char == '"' {
                all_quotes_escaped &= previous_was_escape;
            }
            previous_was_escape = char == '\\' && !previous_was_escape;
        }
        let quote = if all_quotes_escaped { '"' } else { '\'' };
        self.out.push(quote);
        self.out.push_str(content);
        self.out.push(quote);
    }

    fn binary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let Some(operator_index) = elements
            .iter()
            .position(|element| is_operator(element.kind()))
        else {
            for element in &elements {
                self.element(element, level, false);
            }
            return;
        };
        let operator = &elements[operator_index];
        // `x : :None` keeps its spaces: `x::None` would lex as `::`. The tag can
        // also start a larger operand, as in `x : :None$field`.
        let has_spacing = !matches!(operator.kind(), COLON | CARET)
            || (operator.kind() == COLON
                && elements[operator_index + 1..].iter().any(|element| {
                    element
                        .as_node()
                        .and_then(SyntaxNode::first_token)
                        .is_some_and(|token| token.kind() == COLON)
                }));
        let break_after_operator = elements[operator_index + 1..]
            .iter()
            .find(|element| element.as_node().is_some())
            .is_some_and(|rhs| !self.same_line(operator.text_range(), rhs.text_range()));

        let mut seen_operator = false;
        for (index, (previous, element)) in with_previous(&elements).enumerate() {
            match element.kind() {
                COMMENT | ANNOTATION => {
                    // An own-line comment after the last operand belongs to the
                    // enclosing construct's level.
                    if self.trails(previous, element)
                        || elements[index + 1..].iter().all(is_comment)
                    {
                        self.place_comment(previous, element, level, level);
                    } else {
                        self.continuation(element, level);
                    }
                }
                kind => {
                    let is_operator = is_operator(kind);
                    if !seen_operator && !is_operator {
                        self.element(element, level, false);
                    } else if comment_before(node, element.text_range().start())
                        || (!is_operator && break_after_operator)
                    {
                        // The comment may be the previous operand's own last
                        // token; gluing after it would comment it out.
                        self.continuation(element, level);
                    } else {
                        if has_spacing {
                            self.space();
                        }
                        self.element(element, level, false);
                    }
                    seen_operator |= is_operator;
                }
            }
        }
    }

    fn unary(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        // `...` also parses as NAME, but the tilde hugs identifiers only
        // (`~x` but `~ ...`).
        let operand_is_name = elements
            .iter()
            .rev()
            .find_map(SyntaxElement::as_node)
            .is_some_and(|operand| {
                operand.kind() == NAME
                    && operand
                        .children_with_tokens()
                        .any(|child| child.kind() == IDENT)
            });
        let has_space = elements
            .first()
            .is_some_and(|operator| operator.kind() == TILDE)
            && !operand_is_name;

        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => {
                    self.newlines_between(previous, element, level + 1);
                    self.element(element, level + 1, false);
                }
                _ if element.as_node().is_none() => self.element(element, level, false),
                _ if previous.is_some_and(is_comment) => self.continuation(element, level),
                _ => {
                    if previous.is_some() && has_space {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
    }

    fn field_access(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let operator = elements
            .iter()
            .position(|element| matches!(element.kind(), DOLLAR | AT | COLON2 | COLON3));
        // Skip comments trailing the operator: the field name's line decides.
        let is_multiline = operator.is_some_and(|operator| {
            let lhs = &elements[operator.saturating_sub(1)];
            elements[operator + 1..]
                .iter()
                .find(|element| element.kind() != COMMENT)
                .is_some_and(|rhs| !self.same_line(lhs.text_range(), rhs.text_range()))
        });

        let mut seen_operator = false;
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                DOLLAR | AT | COLON2 | COLON3 => {
                    self.element(element, level, false);
                    seen_operator = true;
                }
                _ if seen_operator && is_multiline => self.continuation(element, level),
                _ => self.element(element, level, false),
            }
        }
    }

    fn paren(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let hug = self.hugs(node, &elements, R_PAREN, false);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                L_PAREN => self.element(element, level, false),
                R_PAREN => {
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                _ if hug => self.element(element, level, false),
                _ => self.continuation(element, level),
            }
        }
    }

    fn braces(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = child_elements(node);
        let is_multiline =
            !self.hugs(node, &elements, R_BRACE, true) || make_multiline || forces_multiline(node);
        let mut body: Vec<SyntaxElement> = elements
            .into_iter()
            .filter(|element| !matches!(element.kind(), L_BRACE | R_BRACE))
            .collect();
        // A body of only semicolons is empty: emitting its lines would leave a
        // blank interior line that a second pass collapses.
        if body.iter().all(|element| element.kind() == SEMICOLON) {
            body.clear();
        }

        self.out.push('{');
        if is_multiline {
            let open_line = self.line(node.text_range().start());
            let leading = body
                .iter()
                .take_while(|element| {
                    element.kind() == COMMENT
                        && self.line(element.text_range().start()) == open_line
                })
                .count();
            for element in &body[..leading] {
                self.space();
                self.element(element, level, false);
            }
            let rest = &body[leading..];
            if !rest.is_empty() {
                self.out.push_str(self.line_ending);
                self.item_sequence(rest, level + 1);
            }
            if !body.is_empty() {
                self.newline(level);
            }
        } else {
            let mut first = true;
            for element in &body {
                match element.kind() {
                    SEMICOLON => {}
                    COMMENT => {
                        self.space();
                        self.element(element, level, false);
                    }
                    _ => {
                        self.out.push_str(if first { " " } else { "; " });
                        self.element(element, level, false);
                        first = false;
                    }
                }
            }
            if !body.is_empty() {
                self.space();
            }
        }
        self.out.push('}');
    }

    fn call_like(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let additional_indent = elements
            .first()
            .and_then(SyntaxElement::as_node)
            .filter(|target| matches!(target.kind(), DOLLAR_EXPR | AT_EXPR))
            .is_some_and(|target| {
                let parts = child_elements(target);
                parts
                    .iter()
                    .position(|part| matches!(part.kind(), DOLLAR | AT))
                    .and_then(|operator| parts.get(operator + 1))
                    .zip(parts.first())
                    .is_some_and(|(rhs, lhs)| !self.same_line(lhs.text_range(), rhs.text_range()))
            });

        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.continuation(element, level),
                ARGUMENT_LIST => {
                    // `:None (x)` calls a tag; hugged, `:None(x)` would be its payload.
                    if let Some(tag) = previous.and_then(SyntaxElement::as_node)
                        && tag.kind() == TAG_EXPR
                        && tag.children().all(|child| child.kind() != ARGUMENT_LIST)
                    {
                        self.space();
                    }
                    if let Some(list) = element.as_node() {
                        self.argument_list(list, level + usize::from(additional_indent));
                    }
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn argument_list(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let list_range = node.text_range();
        let is_multiline = self.line(list_range.start()) != self.line(list_range.end());
        let arguments: Vec<&SyntaxElement> = elements
            .iter()
            .filter(|element| matches!(element.kind(), ARGUMENT | PARAMETER))
            .collect();
        let is_empty = arguments
            .iter()
            .all(|argument| argument.text_range().is_empty());

        // Tokens, not elements: comments attach deep inside arguments, and hole
        // arguments are zero-width. For `]]` the first `]` is the placement
        // anchor; the second glues on.
        let close_previous_token = elements
            .iter()
            .find(|element| matches!(element.kind(), R_PAREN | R_BRACKET))
            .and_then(|close| last_token_before(node, close.text_range().start()));
        // A one-item literal keeps its comma hugged, `(x = 1,)`: it is a
        // marker, not a hole argument.
        let is_literal = node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), RECORD_EXPR | TUPLE_EXPR | ARRAY_EXPR));
        let trailing_space = !is_literal
            && close_previous_token
                .as_ref()
                .is_none_or(|token| token.kind() == COMMA);
        // Hug: some argument starts on the open line and ends on the closer's
        // line. A multiline signature always expands one parameter per line.
        let hug = node.kind() != PARAMETER_LIST
            && close_previous_token.as_ref().is_none_or(|close_previous| {
                close_previous.kind() != COMMENT
                    && arguments.iter().any(|argument| {
                        let range = significant_range(argument);
                        !range.is_empty()
                            && self.line(range.start()) == self.line(list_range.start())
                            && self.line(range.end())
                                == self.line(close_previous.text_range().end())
                    })
            });
        let expand = is_multiline && !hug;

        let mut seen_argument = false;
        let mut seen_close = false;
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                R_PAREN | R_BRACKET => {
                    if !seen_close {
                        if expand && !is_empty {
                            self.newline(level);
                        } else if trailing_space {
                            // Also for all-empty lists (`alist(, )`) and
                            // collapsed multiline lists, which must match what
                            // their single-line output reformats to.
                            self.space();
                        }
                        seen_close = true;
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION if self.trails(previous, element) => {
                    self.space();
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => {
                    self.newlines_between(previous, element, level + 1);
                    self.element(element, level + 1, false);
                }
                COMMA => {
                    let start = element.text_range().start();
                    if expand
                        && (comment_before(node, start)
                            || previous.is_none_or(|previous| {
                                previous.kind() == COMMA || previous.text_range().is_empty()
                            }))
                    {
                        self.newline(level + 1);
                    } else if last_token_before(node, start).is_some_and(|token| token.kind() == EQ)
                    {
                        // A fall-through argument (`z = ,`) keeps its space.
                        self.space();
                    }
                    self.out.push(',');
                }
                ARGUMENT | PARAMETER => {
                    if let Some(argument) = element
                        .as_node()
                        .filter(|argument| !argument.text_range().is_empty())
                    {
                        if expand {
                            if seen_argument {
                                self.newlines_between(previous, element, level + 1);
                            } else {
                                self.newline(level + 1);
                            }
                        } else if seen_argument
                            || node
                                .parent()
                                .is_some_and(|parent| parent.kind() == SUBSET_EXPR)
                                && previous.is_some_and(|open| open.kind() == L_BRACKET)
                                && successors(argument.first_token(), SyntaxToken::next_token)
                                    .find(|token| !is_blank(token.kind()))
                                    .is_some_and(|token| {
                                        matches!(token.kind(), L_BRACKET | L_BRACKET2)
                                    })
                        {
                            // `x[ [1]]` subsets by an array; hugged, `x[[1]]`
                            // would be a different operator.
                            self.space();
                        }
                        self.argument(argument, level + usize::from(expand));
                    }
                    seen_argument = true;
                }
                _ => self.element(element, level, false),
            }
        }
    }

    /// Arms go one per line, whatever separated them. A `;` separator is
    /// dropped like a statement's, and a `,` stays glued to its arm.
    fn match_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let Some(open) = elements
            .iter()
            .position(|element| element.kind() == L_BRACE)
        else {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        };
        for (previous, element) in with_previous(&elements[..open]) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level),
                _ => {
                    if previous.is_some() {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
        self.out.push_str(" {");
        let body: Vec<&SyntaxElement> = elements[open + 1..]
            .iter()
            .filter(|element| !matches!(element.kind(), R_BRACE | SEMICOLON))
            .collect();
        let open_line = self.line(elements[open].text_range().start());
        let leading = body
            .iter()
            .take_while(|element| {
                element.kind() == COMMENT && self.line(element.text_range().start()) == open_line
            })
            .count();
        for element in &body[..leading] {
            self.space();
            self.element(element, level, false);
        }
        let rest = &body[leading..];
        if !rest.is_empty() {
            self.out.push_str(self.line_ending);
        }
        let mut previous: Option<&SyntaxElement> = None;
        for element in rest.iter().copied() {
            if element.kind() == COMMA {
                if previous.is_some_and(is_comment) {
                    self.newline(level + 1);
                }
                self.out.push(',');
                continue;
            }
            if is_comment(element) && self.trails(previous, element) {
                self.space();
            } else {
                self.newlines_between(previous, element, level + 1);
            }
            self.element(element, level + 1, false);
            previous = Some(element);
        }
        if !body.is_empty() {
            self.newline(level);
        }
        self.out.push('}');
    }

    fn match_arm(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level + 1),
                FAT_ARROW => self.out.push_str(" =>"),
                _ if previous.is_some() => {
                    self.space();
                    self.element(element, level, false);
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn match_guard(&mut self, node: &SyntaxNode, level: usize) {
        for (previous, element) in with_previous(&child_elements(node)) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level + 1, level + 1),
                IF_KW => self.out.push_str("if "),
                _ => self.element(element, level, false),
            }
        }
    }

    /// A pattern prints on one line: `, ` between items, ` = ` in a field and
    /// ` | ` between alternatives. One holding a comment is copied as written,
    /// because joining its lines would comment out the rest of the pattern.
    fn pattern(&mut self, node: &SyntaxNode) {
        if node
            .descendants_with_tokens()
            .any(|element| matches!(element.kind(), COMMENT | ANNOTATION))
        {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        }
        let mut pending_space = false;
        for token in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
        {
            match token.kind() {
                R_PAREN => pending_space = false,
                EQ | PIPE => pending_space = true,
                _ => {}
            }
            if pending_space {
                self.space();
            }
            match token.kind() {
                STRING => self.string(&token),
                _ => self.out.push_str(token.text()),
            }
            pending_space = matches!(token.kind(), COMMA | EQ | PIPE);
        }
    }

    fn argument(&mut self, node: &SyntaxNode, level: usize) {
        if self.skip_exempt(node) {
            self.emit_skipped(node);
            return;
        }
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level, level),
                _ if previous.is_some_and(is_comment) => self.continuation(element, level),
                EQ => self.out.push_str(" ="),
                _ => {
                    if previous.is_some_and(|previous| previous.kind() == EQ) {
                        self.space();
                    }
                    self.element(element, level, false);
                }
            }
        }
    }

    /// An inline annotation renders on one line with the spacing a `#:` type gets, so a
    /// type reads the same written either way.
    fn inline_annotation(&mut self, node: &SyntaxNode) {
        if self.keeps_verbatim(node) {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        }
        let mut previous: Option<AnnotationToken> = None;
        for token in node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !token.kind().is_trivia())
        {
            let Some(kind) = annotation_token_kind(&token) else {
                self.out.push_str(token.text());
                previous = None;
                continue;
            };
            // A directive name lexes as `@` plus adjacent tokens (`@if-unknown`); glue it back.
            if kind == AnnotationTokenKind::Word
                && let Some(previous) = previous.as_mut()
                && previous.kind == AnnotationTokenKind::Directive
                && previous.end == token.text_range().start()
            {
                self.out.push_str(token.text());
                previous.end = token.text_range().end();
                continue;
            }
            if let Some(previous) = &previous
                && annotation_space_between(previous, kind)
            {
                self.out.push(' ');
            }
            self.out.push_str(token.text());
            previous = Some(AnnotationToken {
                kind,
                text: token.text().to_owned(),
                end: token.text_range().end(),
            });
        }
    }

    /// A type definition lays out like the `#:` block it stands for, without the markers:
    /// author line breaks stay, a line inside brackets indents one level per bracket, and
    /// any other continuation line, such as a union member, hangs one level in.
    fn type_definition(&mut self, node: &SyntaxNode, level: usize) {
        let lines = if self.keeps_verbatim(node) {
            None
        } else {
            self.annotation_line_tokens(node)
        };
        let Some(lines) = lines else {
            self.out.push_str(&self.source[node.text_range()]);
            return;
        };
        let rendered = render_annotation_lines(&lines, &self.indent, "", 1);
        for (index, line) in rendered.iter().enumerate() {
            if index > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// An inline annotation holding a comment is copied as written, because joining it onto
    /// one line would comment out the code after it. So is one with a syntax error, whose
    /// tokens may glue into different ones.
    fn keeps_verbatim(&self, node: &SyntaxNode) -> bool {
        let range = node.text_range();
        node.descendants_with_tokens()
            .any(|element| element.kind() == COMMENT)
            || self
                .error_ranges
                .iter()
                .any(|error| range.contains_inclusive(error.start()))
    }

    fn function_definition(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let is_multiline = self.spans_lines(node);
        let body = elements.iter().rev().find(|element| {
            element.as_node().is_some()
                && !matches!(
                    element.kind(),
                    PARAMETER_LIST | RETURN_TYPE | TYPE_BINDER_LIST
                )
        });
        let is_same_line = elements.first().zip(body).is_none_or(|(head, body)| {
            self.line(head.text_range().start()) == self.line(body.text_range().start())
        });

        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                COMMENT | ANNOTATION => self.place_comment(previous, element, level, level),
                PURE_MARKER => {
                    self.element(element, level, false);
                    self.out.push(' ');
                }
                PARAMETER_LIST => {
                    if after_comment {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                }
                RETURN_TYPE => {
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                _ if Some(element) == body => {
                    self.space_or_newline(after_comment, level);
                    if !is_multiline {
                        self.element(element, level, false);
                    } else if is_same_line {
                        self.element(element, level, true);
                    } else {
                        self.braced_body(element, level);
                    }
                }
                _ => self.element(element, level, false),
            }
        }
    }

    /// `if` and `while` share one layout. A loop body always renders braced,
    /// so `while` passes `make_multiline`.
    fn conditional(&mut self, node: &SyntaxNode, level: usize, make_multiline: bool) {
        let elements = child_elements(node);
        let is_multiline = make_multiline || self.spans_lines(node) || forces_multiline(node);
        let hug = self.condition_hugs(&elements);

        let mut seen_close = false;
        let mut indent_comments = false;
        let mut is_alternative = false;
        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                ELSE_KW => {
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                    is_alternative = true;
                }
                COMMENT | ANNOTATION => self.place_comment(
                    previous,
                    element,
                    level + usize::from(indent_comments),
                    level,
                ),
                L_PAREN => {
                    indent_comments = true;
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                R_PAREN => {
                    indent_comments = false;
                    if !hug {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if element.as_node().is_none() => self.element(element, level, false),
                _ if !seen_close => {
                    if hug {
                        self.element(element, level, false);
                    } else {
                        self.continuation(element, level);
                    }
                }
                _ => {
                    self.space_or_newline(after_comment, level);
                    if !is_multiline {
                        self.element(element, level, false);
                    } else if element.kind() == IF_EXPR && is_alternative {
                        self.element(element, level, true);
                    } else {
                        self.braced_body(element, level);
                    }
                }
            }
        }
    }

    fn for_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        let open = elements.iter().find(|element| element.kind() == L_PAREN);
        let variable = elements.iter().find(|element| element.kind() == NAME);
        let sequence = elements
            .iter()
            .skip_while(|element| element.kind() != IN_KW)
            .skip(1)
            .find(|element| element.as_node().is_some());
        let condition_is_multiline = open.zip(sequence).is_some_and(|(open, sequence)| {
            !self.same_line(open.text_range(), sequence.text_range())
        });
        let header_is_multiline = variable.zip(sequence).is_some_and(|(variable, sequence)| {
            !self.same_line(variable.text_range(), sequence.text_range())
        });

        let mut seen_close = false;
        let mut indent_comments = false;
        for (previous, element) in with_previous(&elements) {
            let after_comment = previous.is_some_and(is_comment);
            match element.kind() {
                IN_KW => {
                    if after_comment || header_is_multiline {
                        self.newline(level + 1);
                    } else {
                        self.space();
                    }
                    self.element(element, level, false);
                }
                COMMENT | ANNOTATION => self.place_comment(
                    previous,
                    element,
                    level + usize::from(indent_comments),
                    level,
                ),
                L_PAREN => {
                    indent_comments = true;
                    self.space_or_newline(after_comment, level);
                    self.element(element, level, false);
                }
                R_PAREN => {
                    indent_comments = false;
                    if after_comment || condition_is_multiline {
                        self.newline(level);
                    }
                    self.element(element, level, false);
                    seen_close = true;
                }
                _ if Some(element) == variable && !seen_close => {
                    if after_comment || condition_is_multiline {
                        self.continuation(element, level);
                    } else {
                        self.element(element, level, false);
                    }
                }
                _ if Some(element) == sequence => {
                    if after_comment {
                        self.continuation(element, level);
                    } else {
                        self.space();
                        self.element(element, level + usize::from(condition_is_multiline), false);
                    }
                }
                _ if element.as_node().is_some() && seen_close => {
                    self.space_or_newline(after_comment, level);
                    self.braced_body(element, level);
                }
                _ => self.element(element, level, false),
            }
        }
    }

    fn repeat_expression(&mut self, node: &SyntaxNode, level: usize) {
        let elements = child_elements(node);
        for (previous, element) in with_previous(&elements) {
            if is_comment(element) {
                self.place_comment(previous, element, level, level);
            } else if element.as_node().is_some() {
                self.space_or_newline(previous.is_some_and(is_comment), level);
                self.braced_body(element, level);
            } else {
                self.element(element, level, false);
            }
        }
    }

    /// A clean block re-renders canonically, keeping author line breaks; a block
    /// with an error stays verbatim apart from one space after each `#:`.
    fn annotation(&mut self, node: &SyntaxNode, level: usize) {
        let range = node.text_range();
        // Inclusive end: an error at the block's end offset still belongs to it.
        let has_error = self
            .error_ranges
            .iter()
            .any(|error| range.contains_inclusive(error.start()));
        let groups = if has_error {
            None
        } else {
            self.annotation_line_tokens(node)
        };
        let lines: Vec<String> = match groups {
            Some(groups) => {
                // A leading `;` is dropped, so it does not make the head trail code.
                // A head trailing code renders apart from its continuation
                // lines, so they never indent relative to a column the
                // formatter does not control.
                let trailing_head =
                    !line_prefix(self.source, &self.line_starts, usize::from(range.start()))
                        .chars()
                        .all(|character| character.is_whitespace() || character == ';');
                let (head, rest) = groups.split_at(usize::from(trailing_head).min(groups.len()));
                let mut lines = render_annotation_lines(head, &self.indent, "#: ", 0);
                lines.extend(render_annotation_lines(rest, &self.indent, "#: ", 0));
                lines
            }
            None => self.source[range]
                .lines()
                .map(|line| {
                    let body = line
                        .trim_start()
                        .strip_prefix("#:")
                        .unwrap_or(line)
                        .trim_end();
                    match body.chars().next() {
                        None => "#:".to_owned(),
                        Some(character) if character.is_whitespace() => format!("#:{body}"),
                        Some(_) => format!("#: {}", body.trim_start()),
                    }
                })
                .collect(),
        };
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                self.newline(level);
            }
            self.out.push_str(line);
        }
    }

    /// `None` when a token has no display mapping; the block then goes verbatim.
    fn annotation_line_tokens(&self, node: &SyntaxNode) -> Option<Vec<Vec<AnnotationToken>>> {
        let mut line_tokens: Vec<Vec<AnnotationToken>> = Vec::new();
        let mut current_line = usize::MAX;
        for token in node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if is_blank(token.kind()) {
                continue;
            }
            let line = self.line(token.text_range().start());
            if line != current_line {
                line_tokens.push(Vec::new());
                current_line = line;
            }
            if token.kind() == ANNOTATION_MARKER {
                continue;
            }
            let kind = annotation_token_kind(&token)?;
            let tokens = line_tokens.last_mut()?;
            // Directive names lex as `@` plus adjacent tokens; glue them back.
            if kind == AnnotationTokenKind::Word
                && let Some(previous) = tokens.last_mut()
                && previous.kind == AnnotationTokenKind::Directive
                && previous.end == token.text_range().start()
            {
                previous.text.push_str(token.text());
                previous.end = token.text_range().end();
                continue;
            }
            tokens.push(AnnotationToken {
                kind,
                text: token.text().to_owned(),
                end: token.text_range().end(),
            });
        }
        Some(line_tokens)
    }

    fn braced_body(&mut self, body: &SyntaxElement, level: usize) {
        if body.kind() == BRACE_EXPR {
            self.element(body, level, true);
        } else {
            self.out.push('{');
            self.newline(level + 1);
            self.element(body, level + 1, false);
            self.newline(level);
            self.out.push('}');
        }
    }

    fn continuation(&mut self, element: &SyntaxElement, level: usize) {
        self.newline(level + 1);
        self.element(element, level + 1, false);
    }

    fn place_comment(
        &mut self,
        previous: Option<&SyntaxElement>,
        comment: &SyntaxElement,
        line_level: usize,
        level: usize,
    ) {
        if self.trails(previous, comment) {
            self.space();
        } else {
            self.newline(line_level);
        }
        self.element(comment, level, false);
    }

    fn space_or_newline(&mut self, break_line: bool, level: usize) {
        if break_line {
            self.newline(level);
        } else {
            self.space();
        }
    }

    fn hugs(
        &self,
        node: &SyntaxNode,
        elements: &[SyntaxElement],
        closer: SyntaxKind,
        whole_body: bool,
    ) -> bool {
        let Some(close) = elements
            .iter()
            .rposition(|element| element.kind() == closer)
        else {
            return true;
        };
        let Some(body) = close.checked_sub(1).and_then(|index| elements.get(index)) else {
            return true;
        };
        let open_line = self.line(node.text_range().start());
        !comment_before(node, elements[close].text_range().start())
            && self.line(body.text_range().start()) == open_line
            && (!whole_body || self.line(body.text_range().end()) == open_line)
    }

    fn condition_hugs(&self, elements: &[SyntaxElement]) -> bool {
        let Some(open) = elements
            .iter()
            .position(|element| element.kind() == L_PAREN)
        else {
            return true;
        };
        let Some(condition) = elements[open + 1..]
            .iter()
            .position(|element| element.as_node().is_some())
        else {
            return true;
        };
        let condition = open + 1 + condition;
        self.same_line(
            elements[open].text_range(),
            elements[condition].text_range(),
        ) && !is_comment(&elements[condition - 1])
            && !elements.get(condition + 1).is_some_and(is_comment)
    }

    fn spans_lines(&self, node: &SyntaxNode) -> bool {
        let range = significant_range(&SyntaxElement::Node(node.clone()));
        self.line(range.start()) != self.line(range.end())
    }

    fn newlines_between(
        &mut self,
        previous: Option<&SyntaxElement>,
        element: &SyntaxElement,
        level: usize,
    ) {
        let count = previous.map_or(0, |previous| {
            self.line(significant_range(element).start())
                .saturating_sub(self.line(significant_range(previous).end()))
                .clamp(1, 2)
        });
        for _ in 0..count {
            self.out.push_str(self.line_ending);
        }
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    fn newline(&mut self, level: usize) {
        self.out.push_str(self.line_ending);
        for _ in 0..level {
            self.out.push_str(&self.indent);
        }
    }

    fn space(&mut self) {
        self.out.push(' ');
    }

    fn trails(&self, previous: Option<&SyntaxElement>, element: &SyntaxElement) -> bool {
        previous.is_some_and(|previous| self.same_line(previous.text_range(), element.text_range()))
    }

    fn same_line(&self, left: TextRange, right: TextRange) -> bool {
        self.line(left.end()) == self.line(right.start())
    }

    fn line(&self, offset: TextSize) -> usize {
        line_of(&self.line_starts, usize::from(offset))
    }
}

fn with_previous(
    elements: &[SyntaxElement],
) -> impl Iterator<Item = (Option<&SyntaxElement>, &SyntaxElement)> {
    once(None).chain(elements.iter().map(Some)).zip(elements)
}

fn child_elements(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children_with_tokens()
        .filter(|element| !is_blank(element.kind()))
        .collect()
}

fn line_starts(source: &str) -> Vec<usize> {
    // A bare `\r` ends a line, as in the lexer; diverging would make tokens
    // split by a lone `\r` look same-line and glue into comment text.
    let mut starts = vec![0];
    let bytes = source.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' || (byte == b'\r' && bytes.get(index + 1) != Some(&b'\n')) {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_of(line_starts: &[usize], offset: usize) -> usize {
    line_starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1)
}

fn line_prefix<'source>(
    source: &'source str,
    line_starts: &[usize],
    offset: usize,
) -> &'source str {
    &source[line_starts[line_of(line_starts, offset)]..offset]
}

fn is_blank(kind: SyntaxKind) -> bool {
    matches!(kind, WHITESPACE | NEWLINE)
}

fn is_comment(element: &SyntaxElement) -> bool {
    matches!(element.kind(), COMMENT | ANNOTATION)
}

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        PLUS | MINUS
            | STAR
            | SLASH
            | CARET
            | SPECIAL
            | PIPE_GREATER
            | LESS
            | GREATER
            | LESS_EQ
            | GREATER_EQ
            | EQ2
            | BANG_EQ
            | AMP
            | AMP2
            | PIPE
            | PIPE2
            | TILDE
            | QUESTION
            | LESS_MINUS
            | LESS2_MINUS
            | MINUS_GREATER
            | MINUS_GREATER2
            | EQ
            | COLON_EQ
            | COLON
    )
}

/// Non-empty loop bodies always render braced and multiline, so single-line
/// layout choices must consult this or a second pass formats differently.
fn forces_multiline(node: &SyntaxNode) -> bool {
    node.descendants()
        .filter(|descendant| matches!(descendant.kind(), FOR_EXPR | WHILE_EXPR | REPEAT_EXPR))
        .any(|descendant| {
            descendant.children().last().is_some_and(|body| {
                body.kind() != BRACE_EXPR
                    || body.children_with_tokens().any(|child| {
                        !matches!(
                            child.kind(),
                            L_BRACE | R_BRACE | WHITESPACE | NEWLINE | SEMICOLON
                        )
                    })
            })
        })
}

/// Some nodes swallow trailing trivia (an argument keeps its newline), and
/// line math must never see it.
fn significant_range(element: &SyntaxElement) -> TextRange {
    match element {
        SyntaxElement::Token(token) => token.text_range(),
        SyntaxElement::Node(node) => {
            let full = node.text_range();
            let start = node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| !is_blank(token.kind()))
                .map_or(full.start(), |token| token.text_range().start());
            let end = last_token_before(node, full.end())
                .map_or(full.end(), |token| token.text_range().end());
            TextRange::new(start, end)
        }
    }
}

/// Comments attach deep inside expression nodes, so closer placement must
/// look at tokens, not siblings.
fn last_token_before(node: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .take_while(|token| token.text_range().end() <= offset)
        .filter(|token| !is_blank(token.kind()))
        .last()
}

/// Sibling checks miss a comment swallowed into the previous sibling's
/// subtree, and anything glued after it would become comment text.
fn comment_before(node: &SyntaxNode, offset: TextSize) -> bool {
    last_token_before(node, offset).is_some_and(|token| {
        token.kind() == COMMENT
            || token
                .parent_ancestors()
                .any(|ancestor| ancestor.kind() == ANNOTATION)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Directive {
    Skip,
    SkipFile,
    On,
    Off,
}

fn directive(element: &SyntaxElement) -> Option<Directive> {
    element
        .as_token()
        .filter(|token| token.kind() == COMMENT)
        .and_then(|token| parse_directive(token.text()))
}

fn parse_directive(text: &str) -> Option<Directive> {
    text.trim_start_matches(|c: char| c.is_whitespace() || c == '#')
        .strip_prefix("fmt:")
        .and_then(|rest| match rest.trim() {
            "skip" => Some(Directive::Skip),
            "skip-file" | "skip file" => Some(Directive::SkipFile),
            "on" => Some(Directive::On),
            "off" => Some(Directive::Off),
            _ => None,
        })
}

/// An opener ending its line expands (its closer gets its own line at the
/// opener's indent); anything else hugs. Each line starts with `prefix`, and a
/// continuation line outside every bracket indents `hanging` levels.
fn render_annotation_lines(
    line_tokens: &[Vec<AnnotationToken>],
    indent: &str,
    prefix: &str,
    hanging: usize,
) -> Vec<String> {
    let blank = prefix.trim_end();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_level = 0usize;
    let mut open_delimiters: Vec<OpenDelimiter> = Vec::new();
    let mut previous: Option<&AnnotationToken> = None;
    let mut pending_empty_lines = 0usize;
    let mut started = false;

    for tokens in line_tokens {
        if tokens.is_empty() {
            if started {
                pending_empty_lines += 1;
            } else {
                lines.push(blank.to_owned());
            }
            continue;
        }
        for (index, token) in tokens.iter().enumerate() {
            let closer = is_closer(token.kind);
            let closes_expanded = closer
                && open_delimiters
                    .last()
                    .is_none_or(|delimiter| delimiter.expanded);
            let break_before = started
                && if index == 0 {
                    pending_empty_lines > 0 || !closer || closes_expanded
                } else {
                    closes_expanded
                };
            if break_before {
                lines.push(annotation_line(prefix, current_level, &current, indent));
                current.clear();
                lines.extend(repeat_n(blank.to_owned(), pending_empty_lines));
                current_level = match open_delimiters.last() {
                    Some(delimiter) if closer => delimiter.opener_level,
                    Some(delimiter) => delimiter.opener_level + usize::from(delimiter.expanded),
                    None => hanging,
                };
                previous = None;
            }
            pending_empty_lines = 0;

            if let Some(previous) = previous
                && annotation_space_between(previous, token.kind)
            {
                current.push(' ');
            }
            current.push_str(&token.text);
            previous = Some(token);
            started = true;

            if is_opener(token.kind) {
                open_delimiters.push(OpenDelimiter {
                    expanded: index + 1 == tokens.len(),
                    opener_level: current_level,
                });
            } else if closer {
                open_delimiters.pop();
            }
        }
    }
    if started {
        lines.push(annotation_line(prefix, current_level, &current, indent));
    }
    lines
}

fn annotation_line(prefix: &str, level: usize, content: &str, indent: &str) -> String {
    format!("{prefix}{}{content}", indent.repeat(level))
}

struct OpenDelimiter {
    expanded: bool,
    opener_level: usize,
}

struct AnnotationToken {
    kind: AnnotationTokenKind,
    text: String,
    end: TextSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationTokenKind {
    Word,
    Directive,
    /// The `=` of an inline type definition.
    Equals,
    Dots,
    Arrow,
    Pipe,
    Comma,
    Colon,
    /// The `:` that starts a tag type, `:Some(integer)`.
    TagMarker,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    OpenAngle,
    CloseAngle,
}

fn annotation_token_kind(token: &SyntaxToken) -> Option<AnnotationTokenKind> {
    use AnnotationTokenKind::*;
    let kind = token.kind();
    Some(match kind {
        COLON
            if token
                .parent()
                .is_some_and(|parent| parent.kind() == TYPE_TAG) =>
        {
            TagMarker
        }
        L_PAREN => OpenParen,
        R_PAREN => CloseParen,
        L_BRACKET => OpenBracket,
        R_BRACKET => CloseBracket,
        L_BRACE => OpenBrace,
        R_BRACE => CloseBrace,
        LESS => OpenAngle,
        GREATER => CloseAngle,
        COMMA => Comma,
        COLON => Colon,
        PIPE => Pipe,
        EQ => Equals,
        MINUS_GREATER => Arrow,
        DOTS => Dots,
        AT => Directive,
        // `-` is interior to a joined directive name (`@if-unknown`).
        IDENT | DOTDOTI | MINUS => Word,
        _ if kind.is_keyword() => Word,
        _ => return None,
    })
}

fn annotation_space_between(previous: &AnnotationToken, current: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    match current {
        Comma | Colon | CloseParen | CloseBracket | CloseBrace | CloseAngle => return false,
        // `(` hugs only what it applies to (`fn(`); after `:`, `|` or `->` it
        // opens a grouped type: `x: (integer | character)`.
        OpenParen
            if matches!(
                previous.kind,
                Word | Dots | CloseParen | CloseBracket | CloseBrace | CloseAngle
            ) =>
        {
            return false;
        }
        OpenAngle if previous.kind == Word => return false,
        OpenBracket if matches!(previous.kind, Word | CloseAngle | CloseBracket) => return false,
        OpenBrace
            if previous.kind == Word
                && matches!(
                    previous.text.as_str(),
                    "list"
                        | "Record"
                        | "Tuple"
                        | "DataFrame"
                        | "LazyFrame"
                        | "GroupBy"
                        | "LazyGroupBy"
                        | "data.table"
                ) =>
        {
            return false;
        }
        Word if matches!(previous.kind, Dots | TagMarker) => return false,
        _ => {}
    }
    !is_opener(previous.kind)
}

fn is_opener(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, OpenParen | OpenBracket | OpenBrace | OpenAngle)
}

fn is_closer(kind: AnnotationTokenKind) -> bool {
    use AnnotationTokenKind::*;
    matches!(kind, CloseParen | CloseBracket | CloseBrace | CloseAngle)
}

/// Spellings, not just kinds: a wrong source range renames identifiers while
/// keeping every kind. Braces, `;` and `#:` markers may be added or moved, and
/// non-raw strings compare by content because the formatter picks the quote.
fn significant_tokens(text: &str) -> Vec<(SyntaxKind, &str)> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for token in lex(text).0 {
        let end = (start + usize::from(token.len)).min(text.len());
        let spelling = text.get(start..end).unwrap_or_default();
        start = end;
        if token.kind.is_trivia()
            || matches!(
                token.kind,
                L_BRACE | R_BRACE | SEMICOLON | ANNOTATION_MARKER
            )
        {
            continue;
        }
        tokens.push((token.kind, comparable_spelling(token.kind, spelling)));
    }
    tokens
}

fn comparable_spelling(kind: SyntaxKind, spelling: &str) -> &str {
    if kind != STRING || matches!(spelling.chars().next(), Some('r' | 'R')) {
        return spelling;
    }
    // An unterminated string has no closer to trim.
    let inner = spelling.strip_prefix(['"', '\'']).unwrap_or(spelling);
    if spelling.len() >= 2 {
        inner.strip_suffix(['"', '\'']).unwrap_or(inner)
    } else {
        inner
    }
}
