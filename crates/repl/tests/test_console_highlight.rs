use {
    nu_ansi_term::{Color, Style},
    repl::highlight::{Palette, parse_style, styled_spans},
};

#[test]
fn style_words_parse() {
    let plain = Style::new();
    assert_eq!(parse_style("none"), Ok(plain));
    assert_eq!(
        parse_style("bright-blue bold"),
        Ok(plain.fg(Color::LightBlue).bold())
    );
    assert_eq!(parse_style("italic red"), Ok(plain.fg(Color::Red).italic()));
    assert!(parse_style("red blue").is_err_and(|error| error.contains("more than one color")));
    assert!(parse_style("purple").is_err_and(|error| error.contains("unknown style word")));
}

#[test]
fn palette_rejects_unknown_keys() {
    let mut palette = Palette::default();
    assert_eq!(palette.set("keyword", Style::new().fg(Color::Blue)), Ok(()));
    assert_ne!(palette, Palette::default());
    assert!(
        palette
            .set("keywords", Style::new())
            .is_err_and(|error| error.contains("expected one of keyword"))
    );
}

/// The spans tile the whole line, so every byte is drawn exactly once.
#[test]
fn spans_cover_the_line() {
    let line = "f(x = \"a\") # note";
    let spans = styled_spans(line, 0, &Palette::default());
    let mut offset = 0;
    for (range, _) in &spans {
        assert_eq!(range.start, offset);
        offset = range.end;
    }
    assert_eq!(offset, line.len());
}

#[test]
fn the_bracket_pair_at_the_cursor_is_marked() {
    let reverse = Style::new().reverse();
    let marked = |line: &str, cursor: usize| -> Vec<usize> {
        styled_spans(line, cursor, &Palette::default())
            .into_iter()
            .filter(|(_, style)| *style == reverse)
            .map(|(range, _)| range.start)
            .collect()
    };
    // On the opener, and just after a closer that was typed.
    assert_eq!(marked("f(g(x))", 1), vec![1, 6]);
    assert_eq!(marked("f(g(x))", 5), vec![3, 5]);
    assert_eq!(marked("f(g(x))", 7), vec![1, 6]);
    assert_eq!(marked("x[[1]]", 1), vec![1, 4]);
    assert_eq!(marked("x[[1]]", 5), vec![1, 4]);
    assert_eq!(marked("f(x", 1), Vec::<usize>::new());
}
