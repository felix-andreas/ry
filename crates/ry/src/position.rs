//! Reported columns count characters, not bytes, so they match what an editor
//! shows; LSP encodings are converted at the protocol edge.

use syntax::TextSize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    line_starts: Vec<u32>,
    text_len: u32,
}

/// Zero-based; the column unit depends on the producing function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineColumn {
    pub line: u32,
    pub column: u32,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset as u32 + 1);
            }
        }
        LineIndex {
            line_starts,
            text_len: text.len() as u32,
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Clamps `line` to the last line.
    pub fn line_start(&self, line: u32) -> u32 {
        let line = (line as usize).min(self.line_starts.len() - 1);
        self.line_starts[line]
    }

    pub fn line_length(&self, line: u32, text: &str) -> u32 {
        let start = self.line_start(line);
        let end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text_len);
        let line_text = &text[start as usize..end as usize];
        line_text.trim_end_matches(['\n', '\r']).len() as u32
    }

    /// Byte column; `offset` is clamped to the text.
    pub fn line_column(&self, offset: TextSize) -> LineColumn {
        let offset = u32::from(offset).min(self.text_len);
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        LineColumn {
            line: line as u32,
            column: offset - self.line_starts[line],
        }
    }

    /// An offset inside a character counts the characters before it.
    pub fn line_column_chars(&self, offset: TextSize, text: &str) -> LineColumn {
        let (line, before) = self.line_prefix(offset, text);
        LineColumn {
            line,
            column: before.chars().count() as u32,
        }
    }

    /// An offset inside a character counts the code units before it.
    pub fn line_column_utf16(&self, offset: TextSize, text: &str) -> LineColumn {
        let (line, before) = self.line_prefix(offset, text);
        LineColumn {
            line,
            column: before
                .chars()
                .map(|character| character.len_utf16() as u32)
                .sum(),
        }
    }

    /// The line holding `offset` and its text before `offset`, cut at a character boundary.
    fn line_prefix<'a>(&self, offset: TextSize, text: &'a str) -> (u32, &'a str) {
        let position = self.line_column(offset);
        let start = self.line_starts[position.line as usize] as usize;
        let end = text.floor_char_boundary(start + position.column as usize);
        (position.line, text.get(start..end).unwrap_or_default())
    }

    /// Line and byte column are both clamped.
    pub fn offset(&self, position: LineColumn, text: &str) -> TextSize {
        let line = position.line.min(self.line_count() - 1);
        let start = self.line_start(line);
        let column = position.column.min(self.line_length(line, text));
        let mut offset = (start + column) as usize;
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        TextSize::from(offset as u32)
    }

    /// Line and UTF-16 column are both clamped.
    pub fn offset_utf16(&self, position: LineColumn, text: &str) -> TextSize {
        let line = position.line.min(self.line_count() - 1);
        let start = self.line_start(line) as usize;
        let line_end = start + self.line_length(line, text) as usize;
        let mut units = 0u32;
        for (index, character) in text[start..line_end].char_indices() {
            if units >= position.column {
                return TextSize::from((start + index) as u32);
            }
            units += character.len_utf16() as u32;
        }
        TextSize::from(line_end as u32)
    }
}

/// Every conversion accepts any offset and any position without panicking, and lands inside the
/// text on a character boundary. An offset on a character boundary, outside a line's terminator,
/// survives the round trip through both a byte and a UTF-16 position. Shared with the
/// coverage-guided `fuzz/` target.
pub fn check_position_invariants(text: &str) {
    let index = LineIndex::new(text);
    let in_text = |offset: TextSize, what: &str| {
        let offset = usize::from(offset);
        assert!(
            offset <= text.len() && text.is_char_boundary(offset),
            "{what} gave offset {offset}, not a character boundary of {text:?}"
        );
    };
    for offset in 0..=text.len() + 1 {
        let size = TextSize::from(offset as u32);
        let position = index.line_column(size);
        assert!(
            position.line < index.line_count(),
            "offset {offset} maps to line {} of {text:?}",
            position.line
        );
        let chars = index.line_column_chars(size, text);
        let utf16 = index.line_column_utf16(size, text);
        assert!(
            chars.column <= position.column && chars.column <= utf16.column,
            "columns disagree at offset {offset} of {text:?}"
        );
        let round = index.offset(position, text);
        in_text(round, "a byte position");
        let round_utf16 = index.offset_utf16(utf16, text);
        in_text(round_utf16, "a UTF-16 position");
        let in_line = position.column <= index.line_length(position.line, text);
        if offset <= text.len() && text.is_char_boundary(offset) && in_line {
            assert_eq!(usize::from(round), offset, "byte round trip in {text:?}");
            assert_eq!(
                usize::from(round_utf16),
                offset,
                "UTF-16 round trip in {text:?}"
            );
        }
    }
    for line in 0..=index.line_count() {
        let length = index.line_length(line.min(index.line_count() - 1), text);
        for column in [0, 1, length / 2, length, length + 1, u32::MAX] {
            let position = LineColumn { line, column };
            in_text(index.offset(position, text), "a byte position");
            in_text(index.offset_utf16(position, text), "a UTF-16 position");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_columns_round_trip() {
        let text = "abc\ndef\n";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 3);
        let position = index.line_column(TextSize::from(5));
        assert_eq!((position.line, position.column), (1, 1));
        assert_eq!(index.offset(position, text), TextSize::from(5));
    }

    #[test]
    fn character_columns_ignore_utf8_width() {
        let text = "x <- \"café\" ; y = 2L\n";
        let index = LineIndex::new(text);
        let equals = TextSize::from(text.find("= 2L").expect("the assignment is there") as u32);
        assert_eq!(index.line_column(equals).column, 17, "byte column");
        assert_eq!(
            index.line_column_chars(equals, text).column,
            16,
            "character column"
        );
    }

    #[test]
    fn utf16_columns_count_code_units() {
        // 'é' is 2 UTF-8 bytes / 1 UTF-16 unit; '😀' is 4 bytes / 2 units.
        let text = "é😀x\n";
        let index = LineIndex::new(text);
        let x_offset = TextSize::from(6);
        let utf16 = index.line_column_utf16(x_offset, text);
        assert_eq!((utf16.line, utf16.column), (0, 3));
        assert_eq!(index.offset_utf16(utf16, text), x_offset);
    }

    #[test]
    fn out_of_bounds_positions_clamp() {
        let text = "ab\n";
        let index = LineIndex::new(text);
        assert_eq!(
            index.offset(LineColumn { line: 9, column: 9 }, text),
            TextSize::from(3)
        );
        let position = index.line_column(TextSize::from(99));
        assert_eq!((position.line, position.column), (1, 0));
    }
}
