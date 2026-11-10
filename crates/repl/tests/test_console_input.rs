//! Seeded harnesses for the console's input layer: the bytes typed ahead of a prompt, the lines
//! they split into, the chunks R reads, and the rule that decides whether a command is finished.
//! Each case draws everything from one seed, so a failure replays with the command it prints.

use {
    repl::console::{KeyDecoder, Typeahead, TypedKey, input_is_complete, take_chunk},
    std::{collections::VecDeque, fmt::Write as _, process::Command},
    syntax::testing::{SplitMix64, for_each_seed},
};

/// Keys a user can press ahead of the prompt, as the model of what they type.
#[derive(Debug, Clone)]
enum Key {
    Text(String),
    Backspace,
    Enter,
    /// A raw-mode Enter sends `\r`; a paste can carry `\r\n`.
    CarriageReturnLineFeed,
    Arrow(u8),
    /// A function key such as F5, sent as an escape sequence.
    Function,
    /// Alt plus a printable key: ESC and the key, both dropped.
    Alt(u8),
    /// An ESC on its own and then Enter (or Alt+Enter), which must still end the line.
    EscapeEnter,
    ControlC,
    /// A control byte the decoder ignores.
    Ignored(u8),
}

/// The decoder keeps printable text, deletes one character per backspace, drops escape sequences
/// whole, and ends a line at `\n`, `\r` or `\r\n`, wherever the reads split the bytes.
#[test]
fn typed_keys_decode_to_the_lines_they_spell() {
    for_each_seed(4000, |seed| {
        let mut rng = SplitMix64(seed);
        let keys: Vec<Key> = (0..rng.below(40)).map(|_| random_key(&mut rng)).collect();
        let bytes = encode_keys(&keys);
        let expected = spell_keys(&keys);

        // The console takes a line at each line end, and the unfinished rest only once the
        // terminal holds nothing more, so the decoder sees the bytes as one stream.
        let mut decoder = KeyDecoder::default();
        let mut decoded = Vec::new();
        for &byte in &bytes {
            match decoder.push(byte) {
                TypedKey::Pending => {}
                TypedKey::LineEnd => decoded.push(Decoded::Line(decoder.take_line())),
                TypedKey::Interrupt => decoded.push(Decoded::Interrupt),
            }
        }
        let line = decoder.take_line();
        decoded.push(Decoded::Line(line));
        assert_eq!(decoded, expected, "keys {keys:?}, bytes {bytes:?}");
    });
}

#[derive(Debug, PartialEq, Eq)]
enum Decoded {
    Line(String),
    Interrupt,
}

/// ESC then Enter once dropped the line end, and a CRLF split between two reads once ended an
/// extra, empty line.
#[test]
fn pinned_typed_keys() {
    let mut decoder = KeyDecoder::default();
    let pushes: Vec<TypedKey> = b"1 + 1\x1b\r"
        .iter()
        .map(|&byte| decoder.push(byte))
        .collect();
    assert_eq!(pushes.last(), Some(&TypedKey::LineEnd));
    assert_eq!(decoder.take_line(), "1 + 1\n");
    assert_eq!(decoder.push(b'\n'), TypedKey::Pending);
    assert_eq!(decoder.take_line(), "");

    let mut typeahead = Typeahead::default();
    typeahead.push("a\r");
    typeahead.push("\nb\n");
    assert_eq!(typeahead.pop_line().as_deref(), Some("a"));
    assert_eq!(typeahead.pop_line().as_deref(), Some("b"));
    assert_eq!(typeahead.pop_line(), None);
}

/// Typed text splits into the same lines however it arrived, and the unfinished rest is kept.
#[test]
fn typeahead_lines_do_not_depend_on_how_the_text_arrived() {
    for_each_seed(4000, |seed| {
        let mut rng = SplitMix64(seed);
        let pieces = ["a", "x <- 1", "é", "\t", " ", "\n", "\r", "\r\n", "f(", ")"];
        let text: String = (0..rng.below(30))
            .map(|_| pieces[rng.below(pieces.len())])
            .collect();
        let expected: Vec<String> = {
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            normalized.split('\n').map(str::to_owned).collect()
        };

        let mut typeahead = Typeahead::default();
        let characters: Vec<char> = text.chars().collect();
        let mut position = 0;
        while position < characters.len() {
            let end = (position + 1 + rng.below(6)).min(characters.len());
            let piece: String = characters[position..end].iter().collect();
            typeahead.push(&piece);
            position = end;
        }
        let mut lines = Vec::new();
        while let Some(line) = typeahead.pop_line() {
            lines.push(line);
        }
        lines.push(typeahead.take_partial());
        assert_eq!(lines, expected, "text {text:?}");
        assert!(!typeahead.has_line());
    });
}

/// R reads accepted input in chunks no larger than its buffer. The chunks rejoin into the input,
/// a hook's code always ends a chunk, so R never parses it together with the typed line, and
/// other input goes at most one line per chunk.
#[test]
fn input_chunks_rejoin_and_keep_hook_code_apart() {
    for_each_seed(4000, |seed| {
        let mut rng = SplitMix64(seed);
        let mut pending = VecDeque::new();
        let mut unit_bytes = 0;
        let mut expected: Vec<u8> = Vec::new();
        let mut read: Vec<u8> = Vec::new();
        for _ in 0..rng.below(12) {
            if rng.below(3) == 0 && unit_bytes == 0 && pending.is_empty() {
                let hook = vec![b'h'; 1 + rng.below(20)];
                unit_bytes = hook.len();
                pending.extend(&hook);
                expected.extend(&hook);
            }
            let mut line = vec![b'l'; rng.below(30)];
            if rng.below(2) == 0 {
                line.push(b'\n');
            }
            pending.extend(&line);
            expected.extend(&line);
            while !pending.is_empty() && rng.below(2) == 0 {
                let mut buffer = vec![0u8; 1 + rng.below(16)];
                let hook_before = unit_bytes;
                let written = take_chunk(&mut pending, &mut unit_bytes, &mut buffer);
                assert!(written > 0 && written <= buffer.len());
                if hook_before > 0 {
                    assert!(written <= hook_before, "a chunk ran past the hook's code");
                    assert_eq!(unit_bytes, hook_before - written);
                } else {
                    let chunk = &buffer[..written];
                    assert!(
                        !chunk[..written - 1].contains(&b'\n'),
                        "a chunk held more than one line: {chunk:?}"
                    );
                }
                read.extend(&buffer[..written]);
            }
        }
        while !pending.is_empty() {
            let mut buffer = vec![0u8; 1 + rng.below(16)];
            let written = take_chunk(&mut pending, &mut unit_bytes, &mut buffer);
            read.extend(&buffer[..written]);
        }
        assert_eq!(read, expected);
        assert_eq!(unit_bytes, 0);
    });
}

/// A command built from a grammar is finished exactly when every opener is closed and it does
/// not end in an operator that needs a right-hand side.
#[test]
fn completeness_follows_a_grammar_model() {
    for_each_seed(3000, |seed| {
        let mut rng = SplitMix64(seed);
        let expression = random_expression(&mut rng, 3);
        assert!(
            input_is_complete(&expression),
            "finished command read as unfinished: {expression:?}"
        );
        let unfinished = match rng.below(5) {
            0 => format!("{expression} +"),
            1 => format!("f({expression},"),
            2 => format!("{{\n  {expression}"),
            3 => format!("{expression}\nx <- \"unfinished \\\" still\n open"),
            _ => format!("{expression} |>\n"),
        };
        assert!(
            !input_is_complete(&unfinished),
            "unfinished command read as finished: {unfinished:?}"
        );
    });
}

/// A user types real R a line at a time, so at each Enter the console sees a prefix of a valid
/// program. It must hold the prefix back for another line exactly when GNU R's parser finds it
/// unfinished: holding back more keeps prompting where R would run the command or report its
/// error, and holding back less submits half a command. Skips without GNU R.
#[test]
fn the_console_holds_back_what_gnu_r_finds_unfinished() {
    let Some(rscript) = rscript() else {
        return;
    };
    let mut cases: Vec<(u64, String)> = Vec::new();
    for_each_seed(40, |seed| {
        let mut rng = SplitMix64(seed);
        for _ in 0..50 {
            cases.push((seed, random_prefix(&mut rng)));
        }
    });
    let verdicts = gnu_r_verdicts(&rscript, cases.iter().map(|(_, text)| text.as_str()));
    let mut failures = String::new();
    for ((seed, text), verdict) in cases.iter().zip(&verdicts) {
        let held_back = !input_is_complete(text);
        // The syntax crate's rule reads `pkg::` and a lone `\` at the end as unfinished, where
        // R's lexer reads the line end as a token and reports an error.
        let trimmed = text.trim_end();
        if held_back && (trimmed.ends_with("::") || trimmed.ends_with('\\')) {
            continue;
        }
        if held_back != (verdict == "incomplete") {
            writeln!(failures, "seed {seed}: GNU R says {verdict}: {text:?}").ok();
        }
    }
    assert!(
        failures.is_empty(),
        "the console and GNU R disagree:\n{failures}"
    );
}

fn random_key(rng: &mut SplitMix64) -> Key {
    let texts = ["x", "1 + 1", "é", "日本", "\t", " ", "f(a, b)", "\"s\""];
    match rng.below(12) {
        0..=3 => Key::Text(texts[rng.below(texts.len())].to_owned()),
        4 => Key::Backspace,
        5 => Key::Enter,
        6 => Key::CarriageReturnLineFeed,
        7 => Key::Arrow(b"ABCD"[rng.below(4)]),
        8 => Key::Function,
        9 => Key::Alt(b"abf."[rng.below(4)]),
        10 => match rng.below(3) {
            0 => Key::EscapeEnter,
            1 => Key::ControlC,
            _ => Key::Ignored([0x01, 0x02, 0x04, 0x0e][rng.below(4)]),
        },
        _ => Key::Text("y".to_owned()),
    }
}

fn encode_keys(keys: &[Key]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for key in keys {
        match key {
            Key::Text(text) => bytes.extend(text.as_bytes()),
            Key::Backspace => bytes.push(0x7f),
            Key::Enter => bytes.push(b'\r'),
            Key::CarriageReturnLineFeed => bytes.extend(b"\r\n"),
            Key::Arrow(letter) => bytes.extend([0x1b, b'[', *letter]),
            Key::Function => bytes.extend(b"\x1b[15~"),
            Key::Alt(letter) => bytes.extend([0x1b, *letter]),
            Key::EscapeEnter => bytes.extend([0x1b, b'\r']),
            Key::ControlC => bytes.push(0x03),
            Key::Ignored(byte) => bytes.push(*byte),
        }
    }
    bytes
}

/// What the keys type, by the model: the same output as the decoder, computed per key.
fn spell_keys(keys: &[Key]) -> Vec<Decoded> {
    let mut decoded = Vec::new();
    let mut line = String::new();
    for key in keys {
        match key {
            Key::Text(text) => line.push_str(text),
            Key::Backspace => {
                line.pop();
            }
            Key::Enter | Key::CarriageReturnLineFeed | Key::EscapeEnter => {
                line.push('\n');
                decoded.push(Decoded::Line(std::mem::take(&mut line)));
            }
            Key::ControlC => {
                line.clear();
                decoded.push(Decoded::Interrupt);
            }
            Key::Arrow(_) | Key::Function | Key::Alt(_) | Key::Ignored(_) => {}
        }
    }
    decoded.push(Decoded::Line(line));
    decoded
}

fn random_expression(rng: &mut SplitMix64, depth: usize) -> String {
    let leaves = [
        "x",
        "1L",
        "2.5",
        "\"text\"",
        "'quote'",
        "`odd name`",
        "TRUE",
        "NULL",
        "r\"(raw)\"",
    ];
    if depth == 0 || rng.below(3) == 0 {
        return leaves[rng.below(leaves.len())].to_owned();
    }
    let mut inner = || random_expression(rng, depth - 1);
    let left = inner();
    let right = inner();
    let operators = [
        "+", "-", "*", "/", "^", "%in%", "|>", "==", "&&", "||", "<-", "~", ":",
    ];
    match rng.below(9) {
        0 => format!("f({left}, {right})"),
        1 => format!("{left}[{right}]"),
        2 => format!("{left}[[{right}]]"),
        3 => format!("{{\n  {left}\n  {right}\n}}"),
        4 => format!("function(a, b = {left}) {right}"),
        5 => format!("if ({left}) {right} else {left}"),
        6 => format!("({left} # a comment ends in +\n)"),
        7 => format!("\\(a) {left}"),
        _ => {
            let operator = operators[rng.below(operators.len())];
            if operator == "|>" {
                format!("{left} |> g()")
            } else {
                format!("{left} {operator}\n  {right}")
            }
        }
    }
}

/// A valid program cut at a line end or at any character, as typed up to an Enter.
fn random_prefix(rng: &mut SplitMix64) -> String {
    const PROGRAMS: &[&str] = &[
        "f <- function(x, y = 2) {\n  if (x > y) x else y\n}",
        "x[[\"a\"]][1] <- list(b = 'q', c = `odd name`)",
        "s <- \"line one\nline two\" ; t <- r\"-(a \" quote)-\"",
        "values |>\n  lapply(\\(v) v %in% c(1, 2)) |>\n  unlist()",
        "for (i in seq_len(3)) {\n  # a comment with a ' quote\n  print(i)\n}",
        "model <- lm(y ~ x + I(x^2), data = d)",
        "while (TRUE) {\n  break\n}\nrepeat {\n  next\n}",
        "a$b@c <- pkg::fun(1:10)[-1]",
        "g <- function() {\n  tryCatch(stop(\"e\"), error = function(e) NULL)\n}",
        "y = if (x) {\n  1\n} else {\n  2\n}",
        "m <- matrix(1:6,\n  nrow = 2\n)\nm[1, ,\n  drop = FALSE]",
        "`my var` <- -1 ->> z\nh <- function(...) ..1 && !is.null(NULL) || FALSE",
    ];
    let program = PROGRAMS[rng.below(PROGRAMS.len())];
    let cuts: Vec<usize> = if rng.below(2) == 0 {
        program
            .match_indices('\n')
            .map(|(index, _)| index)
            .chain([program.len()])
            .collect()
    } else {
        program
            .char_indices()
            .map(|(index, _)| index)
            .chain([program.len()])
            .collect()
    };
    program[..cuts[rng.below(cuts.len())]].to_owned()
}

/// GNU R's `Rscript`, or `None` (with a note) on a machine without one.
fn rscript() -> Option<String> {
    let available = Command::new("Rscript")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped: no GNU R installation on this machine");
    }
    available.then(|| "Rscript".to_owned())
}

/// Each text's verdict from R's parser, `complete`, `incomplete` or `error`, in one R session.
fn gnu_r_verdicts<'a>(rscript: &str, texts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let input = directory.join("console-completeness-cases.txt");
    let mut encoded = String::new();
    let mut count = 0;
    for text in texts {
        for byte in text.as_bytes() {
            write!(encoded, "{byte:02x}").ok();
        }
        encoded.push('\n');
        count += 1;
    }
    std::fs::write(&input, encoded).expect("write the cases for R");
    let program = r#"
        cases <- readLines(commandArgs(trailingOnly = TRUE)[1])
        verdict <- function(hex) {
            text <- if (nchar(hex) == 0) "" else rawToChar(as.raw(strtoi(
                substring(hex, seq(1, nchar(hex), 2), seq(2, nchar(hex), 2)), 16L)))
            tryCatch({ parse(text = text, keep.source = FALSE); "complete" },
                error = function(e) {
                    if (grepl("unexpected end of input|INCOMPLETE_STRING", conditionMessage(e)))
                        "incomplete" else "error"
                })
        }
        writeLines(vapply(cases, verdict, "", USE.NAMES = FALSE))
    "#;
    let output = Command::new(rscript)
        .args(["--vanilla", "-e", program])
        .arg(&input)
        .output()
        .expect("run Rscript");
    assert!(output.status.success(), "Rscript failed: {output:?}");
    let verdicts: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(verdicts.len(), count, "one verdict per case");
    verdicts
}
