//! The parser fuzz program: source fragments handed to `parse(text =)`, with the tree each
//! parses to, or the error it is rejected with, printed in full.
//!
//! A program is a prelude and one `p_("<fragment>")` statement per fragment, so the reducer
//! shrinks a finding to the one line that shows it. `p_` prints the number of expressions and a
//! structural dump of each: a call as `(head argument ...)` with its argument names, a symbol by
//! name, a string by its bytes and encoding mark, and a number by its type and bits. Deparsed
//! text would hide exactly what a parser gets wrong (an integer read as a double, the grouping of
//! `-a^b`, an escape decoded to another byte), and it would add the deparser's own differences.
//!
//! A fragment comes from one [`Family`], and a seed enables a random subset of them. Half of the
//! families write text that should parse (operator chains without parentheses, nested constructs,
//! literals, names, the same constructs across line breaks), and the others damage such text by
//! one token or draw tokens blindly, because a parser's two answers, the tree and the refusal,
//! are checked by different inputs.
//!
//! ry's own syntax is text GNU R rejects by design. A fragment with an inline annotation is not
//! generated, and one where ry's parser finds a record, a tuple or another ry construct is passed
//! in the mode `"extension"`: `p_` then prints only that R rejects it, on ry always and on GNU R
//! when the parse fails. Text GNU R accepts and ry reads as its own syntax still differs, which
//! is the one difference such a fragment can show.
//!
//! A rejected fragment prints its whole error message (the line, the column, the token the
//! parser stopped at and the lines it quotes) only in the mode `"message"`, which the string and
//! number families use, because there the lexer is what refuses. Every other family prints the
//! fact that the fragment was rejected, because the token a parser names for arbitrary damaged
//! text is not what those families ask about. A rejected fragment prints no warnings.

use {
    std::iter::successors,
    syntax::{
        Direction,
        SyntaxKind::{
            self, ARGUMENT, ARGUMENT_LIST, ARRAY_EXPR, BANG_EQ, BINARY_EXPR, BRACE_EXPR,
            BREAK_EXPR, CALL_EXPR, COMMA, ELSE_KW, EQ, EQ2, FOR_EXPR, FSTRING, FUNCTION_DEF,
            GREATER, GREATER_EQ, IF_EXPR, L_BRACKET, L_BRACKET2, L_PAREN, LESS, LESS_EQ, LITERAL,
            MATCH_EXPR, NAME, NAMESPACE_EXPR, NEW_EXPR, NEWLINE, NEXT_EXPR, PARAMETER_LIST,
            PAREN_EXPR, PURE_MARKER, QUESTION, R_BRACE, R_BRACKET, R_PAREN, RAW_STRING,
            RECORD_EXPR, REPEAT_EXPR, SOURCE_FILE, STRING, SUFFIXED_NUMBER, TAG_EXPR, TRY_EXPR,
            TUPLE_EXPR, TYPE_DEFINITION, UNARY_EXPR, UNDERSCORE, WHILE_EXPR,
        },
        SyntaxNode, SyntaxToken, erase_inline_annotations, parse,
        testing::SplitMix64,
    },
};

/// One way of writing a fragment. A seed enables each with probability one half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// Atoms joined by unary and binary operators with no parentheses, so the tree shows every
    /// precedence and associativity decision.
    Chain,
    /// A nested expression over every construct of the grammar.
    Construct,
    /// A [`Family::Construct`] fragment whose tokens are separated by line breaks, semicolons,
    /// comments and nothing at all.
    Layout,
    /// One numeric literal, well formed or cut short.
    Number,
    /// One string literal: escapes of every form, raw strings, and both quotes.
    Text,
    /// A name of an unusual spelling in each position that takes a name.
    Name,
    /// A fragment of another family with one token deleted, repeated, swapped or replaced.
    Mutant,
    /// Tokens drawn blindly.
    Soup,
}

pub const FAMILIES: [Family; 8] = [
    Family::Chain,
    Family::Construct,
    Family::Layout,
    Family::Number,
    Family::Text,
    Family::Name,
    Family::Mutant,
    Family::Soup,
];

const FRAGMENTS_PER_PROGRAM: usize = 60;

/// The parser fuzz program for `seed`.
pub fn program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x7061_7273_6572_5f66);
    // One seed in eight uses every family, so that the whole space is also sampled densely.
    let mut families: Vec<Family> = if rng.chance(1, 8) {
        FAMILIES.to_vec()
    } else {
        FAMILIES.into_iter().filter(|_| rng.chance(1, 2)).collect()
    };
    if families.is_empty() {
        families.push(FAMILIES[rng.below(FAMILIES.len())]);
    }
    let mut out = format!("# parser fuzz program, seed {seed}, families {families:?}\n{PRELUDE}");
    let mut written = 0;
    // A fragment ry reads as annotated code has no R program to compare, so the draw repeats;
    // the bound keeps a seed whose families write mostly such text from looping.
    for _ in 0..FRAGMENTS_PER_PROGRAM * 4 {
        if written == FRAGMENTS_PER_PROGRAM {
            break;
        }
        let family = families[rng.below(families.len())];
        let text = fragment(&mut rng, family);
        let Some(extension) = ry_extension(&text) else {
            continue;
        };
        if known_difference(&text) {
            continue;
        }
        let mode = match family {
            _ if extension => ", \"extension\"",
            Family::Text | Family::Number => "",
            _ => ", \"answer\"",
        };
        out.push_str(&format!("p_(\"{}\"{mode})\n", r_string_body(&text)));
        written += 1;
    }
    out
}

/// The text of one fragment of `family`.
pub fn fragment(rng: &mut SplitMix64, family: Family) -> String {
    match family {
        Family::Chain => {
            let mut tokens = Vec::new();
            let links = 2 + rng.below(5);
            chain(rng, &mut tokens, links);
            tokens.join(" ")
        }
        Family::Construct => {
            let mut tokens = Vec::new();
            expression(rng, &mut tokens, 3);
            tokens.join(" ")
        }
        Family::Layout => {
            let mut tokens = Vec::new();
            expression(rng, &mut tokens, 3);
            let mut text = String::new();
            for token in &tokens {
                text.push_str(token);
                text.push_str(SEPARATORS[rng.below(SEPARATORS.len())]);
            }
            text
        }
        Family::Number => number(rng),
        Family::Text => string(rng),
        Family::Name => {
            let template = NAME_TEMPLATES[rng.below(NAME_TEMPLATES.len())];
            let mut text = String::new();
            for character in template.chars() {
                match character {
                    '•' => text.push_str(NAMES[rng.below(NAMES.len())]),
                    other => text.push(other),
                }
            }
            text
        }
        Family::Mutant => {
            let mut tokens = Vec::new();
            match rng.below(3) {
                0 => {
                    let links = 2 + rng.below(4);
                    chain(rng, &mut tokens, links);
                }
                _ => expression(rng, &mut tokens, 2),
            }
            for _ in 0..1 + rng.below(2) {
                let at = rng.below(tokens.len().max(1));
                match rng.below(5) {
                    0 if !tokens.is_empty() => {
                        tokens.remove(at);
                    }
                    1 if !tokens.is_empty() => {
                        let token = tokens[at].clone();
                        tokens.insert(at, token);
                    }
                    2 if tokens.len() > 1 => {
                        let other = rng.below(tokens.len());
                        tokens.swap(at, other);
                    }
                    3 if !tokens.is_empty() => {
                        tokens[at] = ALPHABET[rng.below(ALPHABET.len())].to_owned();
                    }
                    _ => tokens.insert(at, ALPHABET[rng.below(ALPHABET.len())].to_owned()),
                }
            }
            tokens.join(" ")
        }
        Family::Soup => {
            let mut text = String::new();
            for _ in 0..1 + rng.below(8) {
                text.push_str(ALPHABET[rng.below(ALPHABET.len())]);
                if !rng.chance(1, 4) {
                    text.push(' ');
                }
            }
            text
        }
    }
}

/// Whether ry's parser reads `text` as ry's own syntax, which GNU R rejects by design, or `None`
/// for text that has no R program to compare: an inline annotation erases to another program, a
/// carriage return is a line break only to ry, and a `#:` comment is ry's to parse.
fn ry_extension(text: &str) -> Option<bool> {
    if text.contains('\r') || text.contains("#:") {
        return None;
    }
    let parsed = parse(text);
    let mut extension = false;
    for element in parsed.syntax_node().descendants_with_tokens() {
        match element.kind() {
            TYPE_DEFINITION | NEW_EXPR | PURE_MARKER => return None,
            TUPLE_EXPR | RECORD_EXPR | ARRAY_EXPR | TAG_EXPR | MATCH_EXPR | FSTRING | TRY_EXPR
            | SUFFIXED_NUMBER => extension = true,
            _ => {}
        }
    }
    (erase_inline_annotations(&parsed) == text).then_some(extension)
}

/// Whether `text` holds a construct ry and GNU R are known to read differently, each listed in
/// the backlog under "Differences the fuzz kinds report". Such a fragment is not generated, so
/// the kind reports only differences nobody has seen; an entry leaves this function with its fix.
fn known_difference(text: &str) -> bool {
    let parsed = parse(text);
    let root = parsed.syntax_node();
    line_break_ends_if(&root)
        || root.descendants().any(|node| {
            let operator = |node: &SyntaxNode| {
                node.children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .map(|token| token.kind())
                    .find(|kind| !kind.is_trivia())
            };
            match node.kind() {
                // GNU R's comparison operators do not associate: `a < b > c` is a syntax error.
                BINARY_EXPR if operator(&node).is_some_and(is_comparison) => {
                    node.first_child().is_some_and(|left| {
                        left.kind() == BINARY_EXPR && operator(&left).is_some_and(is_comparison)
                    })
                }
                // `=` assigns only where GNU R's grammar has a statement: at the top level, in
                // braces and parentheses, as a body, and as an operand of `=` or `?` there. ry takes
                // it for an assignment in an argument, a condition, an index and a default too.
                BINARY_EXPR if operator(&node) == Some(EQ) => {
                    let mut position = node.clone();
                    loop {
                        let Some(parent) = position.parent() else {
                            break false;
                        };
                        match parent.kind() {
                            SOURCE_FILE | BRACE_EXPR | PAREN_EXPR | REPEAT_EXPR => break false,
                            BINARY_EXPR | UNARY_EXPR
                                if matches!(operator(&parent), Some(EQ | QUESTION)) =>
                            {
                                position = parent;
                            }
                            // A body follows the head's closing parenthesis.
                            FUNCTION_DEF | IF_EXPR | WHILE_EXPR | FOR_EXPR => {
                                break !position.siblings_with_tokens(Direction::Prev).any(
                                    |sibling| {
                                        matches!(sibling.kind(), R_PAREN | PARAMETER_LIST | ELSE_KW)
                                    },
                                );
                            }
                            _ => break true,
                        }
                    }
                }
                // GNU R refuses a call of an empty name as it builds the call, so before a syntax
                // error later in the text, which is the one ry reports.
                CALL_EXPR if !parsed.errors().is_empty() => node
                    .first_child()
                    .filter(|callee| callee.kind() == LITERAL)
                    .and_then(|callee| callee.first_token())
                    .is_some_and(|token| {
                        matches!(token.kind(), STRING | RAW_STRING)
                            && token
                                .text()
                                .chars()
                                .all(|character| "rR\"'-()[]{}".contains(character))
                    }),
                // GNU R takes `::` and `:::` only between names and strings.
                NAMESPACE_EXPR => node.first_child().is_none_or(|package| {
                    !(package.kind() == NAME
                        || package.kind() == LITERAL
                            && package
                                .first_token()
                                .is_some_and(|token| matches!(token.kind(), STRING | RAW_STRING)))
                }),
                // GNU R rejects a comma that ends a parameter list.
                PARAMETER_LIST => node
                    .children_with_tokens()
                    .filter(|child| !child.kind().is_trivia() && child.kind() != R_PAREN)
                    .last()
                    .is_some_and(|last| last.kind() == COMMA),
                // GNU R parses `_` wherever a name can stand and rejects it later, with rules of its
                // own; ry rejects every `_` that is not a named argument, and holds a backquoted `_`
                // to the placeholder's rules, where GNU R reads a name.
                NAME => node.first_token().is_some_and(|token| {
                    token.text() == "`_`"
                        || token.kind() == UNDERSCORE
                            && !node.parent().is_some_and(|parent| {
                                parent.kind() == ARGUMENT && operator(&parent) == Some(EQ)
                            })
                }),
                _ => false,
            }
        })
}

/// Whether GNU R ends an `if` at a line break that ry reads across. Inside parentheses and
/// brackets GNU R's lexer skips line breaks, except after an operand of an `if` branch: there it
/// returns the break unless `else`, a comma or a closing token follows, and the grammar has no
/// place for a break inside parentheses. `(if (a) b` and `+ c)` on two lines is a syntax error.
fn line_break_ends_if(root: &SyntaxNode) -> bool {
    let tokens = || {
        root.descendants_with_tokens()
            .filter_map(|element| element.into_token())
    };
    tokens()
        .filter(|token| token.kind() == NEWLINE)
        .any(|line_break| {
            let significant = |token: &SyntaxToken| !token.kind().is_trivia();
            let previous =
                successors(line_break.prev_token(), SyntaxToken::prev_token).find(significant);
            let next =
                successors(line_break.next_token(), SyntaxToken::next_token).find(significant);
            let (Some(previous), Some(next)) = (previous, next) else {
                return false;
            };
            if matches!(next.kind(), R_PAREN | R_BRACKET | R_BRACE | COMMA | ELSE_KW) {
                return false;
            }
            // After an operator, a keyword or the head of a loop or a function GNU R skips the break.
            let parent = previous.parent().map(|parent| parent.kind());
            let ends_operand = match previous.kind() {
                R_BRACE | R_BRACKET => true,
                R_PAREN => matches!(parent, Some(PAREN_EXPR | ARGUMENT_LIST)),
                _ => matches!(parent, Some(NAME | LITERAL | BREAK_EXPR | NEXT_EXPR)),
            };
            if !ends_operand {
                return false;
            }
            let offset = line_break.text_range().start();
            let mut in_branch = false;
            for ancestor in line_break.parent_ancestors() {
                let direct = |kinds: &[SyntaxKind]| {
                    ancestor
                        .children_with_tokens()
                        .filter_map(|child| child.into_token())
                        .find(|token| kinds.contains(&token.kind()))
                        .map(|token| token.text_range().start())
                };
                match ancestor.kind() {
                    BRACE_EXPR | SOURCE_FILE => return false,
                    IF_EXPR | WHILE_EXPR | FOR_EXPR => {
                        if direct(&[R_PAREN]).is_none_or(|close| offset < close) {
                            return in_branch;
                        }
                        if ancestor.kind() == IF_EXPR
                            && direct(&[ELSE_KW]).is_none_or(|otherwise| offset < otherwise)
                        {
                            in_branch = true;
                        }
                    }
                    _ => {
                        if direct(&[L_PAREN, L_BRACKET, L_BRACKET2])
                            .is_some_and(|open| open < offset)
                        {
                            return in_branch;
                        }
                    }
                }
            }
            false
        })
}

fn is_comparison(kind: SyntaxKind) -> bool {
    matches!(kind, EQ2 | BANG_EQ | LESS | GREATER | LESS_EQ | GREATER_EQ)
}

/// `text` as the body of a double-quoted R string literal.
fn r_string_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 || control == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out
}

const ATOMS: &[&str] = &[
    "a",
    "b",
    "x",
    "1",
    "2L",
    "\"s\"",
    "TRUE",
    "NULL",
    "NA",
    "Inf",
    ".5",
    "1i",
    "..1",
    "...",
    "f",
    "T",
    "NA_integer_",
    "0x10",
    "1e3",
    "'q'",
    "`a b`",
];

/// Binary operators, each precedence level of R's grammar present, the common ones twice.
const BINARY: &[&str] = &[
    "^", "**", "-", "+", "-", "+", ":", "%%", "%in%", "%o%", "%||%", "|>", "*", "/", "*", "<", ">",
    "<=", ">=", "==", "!=", "&", "&&", "|", "||", "~", "->", "->>", "<-", "<-", "<<-", "=", "?",
    ":=", "$", "@", "::", ":::",
];

const UNARY: &[&str] = &["-", "+", "!", "~", "?", "-", "!"];

const NAMES: &[&str] = &[
    "a", ".a", "..a", "a.b", "a_b", "a1", ".1a", "_a", "a_", "..1", "...", "..", ".", "é", "naïve",
    "日本", "α1", "`a b`", "`a\\`b`", "`a\\nb`", "`if`", "`1`", "``", "`é`", "\"s\"", "'s'", "if",
    "NULL", "TRUE", "function", "in", "T", "NA", "Inf", "break", "a\u{a0}b", "ａ", "x1L", "e1",
    "`\\x41`", "`a\\\\b`", "1a", ".2", "`...`", "`_`",
];

/// Where a name can stand; `•` is the hole.
const NAME_TEMPLATES: &[&str] = &[
    "•",
    "• <- 1",
    "1 -> •",
    "• = 1",
    "x$•",
    "x@•",
    "x$•$•",
    "f(• = 1)",
    "f(• = )",
    "f(•)",
    "function(•) 1",
    "function(• = 2, •) •",
    "\\(•) •",
    "•::•",
    "•:::•",
    "•(1)",
    "x$•(1)",
    "x[• = 1]",
    "x[[•]]",
    "for (• in x) •",
    "• |> f()",
    "x |> •()",
    "x |> f(• = _)",
    "• + •",
    "-•",
    "• %in% •",
    "~•",
    "?•",
    "• ? •",
    "•$• <- •",
    "c(• = 1, • = 2)",
];

const SEPARATORS: &[&str] = &[
    " ", " ", " ", " ", "", "\n", "\n", " \n ", ";", " # c\n", "\n\n", "\t", "\n# c\n", "  ",
];

const ALPHABET: &[&str] = &[
    "a", "b", "x", "f", "1", "2L", "\"s\"", "TRUE", "NULL", "(", ")", "{", "}", "[", "]", "[[",
    "]]", ",", ";", "\n", "if", "else", "for", "in", "while", "repeat", "function", "\\", "break",
    "next", "+", "-", "*", "/", "^", "**", ":", "::", ":::", "$", "@", "~", "?", "!", "!=", "==",
    "<", ">", "<=", ">=", "&", "&&", "|", "||", "|>", "<-", "<<-", "->", "->>", "=", ":=", "%%",
    "%in%", "%", "_", "...", "..1", ".", "#", "'", "\"", "`", "\\(", "0x", "1e", "1i", ".5",
    "r\"(", ")\"", "=>", "<", "-", "=", ">", "|",
];

/// `links` operands joined by binary operators, each operand an atom with unary operators and
/// postfix forms, or an open-ended construct (`if`, a function) that takes everything after it.
fn chain(rng: &mut SplitMix64, tokens: &mut Vec<String>, links: usize) {
    for index in 0..links {
        if index > 0 {
            tokens.push(BINARY[rng.below(BINARY.len())].to_owned());
        }
        while rng.chance(1, 4) {
            tokens.push(UNARY[rng.below(UNARY.len())].to_owned());
        }
        match rng.below(14) {
            0 => {
                tokens.extend(["if", "(", "a", ")"].map(str::to_owned));
                if rng.chance(1, 2) {
                    let inner = 1 + rng.below((links - index).max(1));
                    chain(rng, tokens, inner);
                    tokens.push("else".to_owned());
                }
            }
            1 => tokens.extend(["function", "(", "x", ")"].map(str::to_owned)),
            2 => tokens.extend(["\\", "(", "x", ")"].map(str::to_owned)),
            3 => {
                tokens.extend(["for", "(", "i", "in", "x", ")"].map(str::to_owned));
            }
            4 => tokens.extend(["while", "(", "a", ")"].map(str::to_owned)),
            5 => tokens.push("repeat".to_owned()),
            _ => {
                tokens.push(ATOMS[rng.below(ATOMS.len())].to_owned());
                while rng.chance(1, 5) {
                    let postfix: &[&str] = match rng.below(5) {
                        0 => &["(", ")"],
                        1 => &["(", "y", ")"],
                        2 => &["[", "1", "]"],
                        3 => &["[[", "1", "]]"],
                        _ => &["$", "y"],
                    };
                    tokens.extend(postfix.iter().map(|token| (*token).to_owned()));
                }
                continue;
            }
        }
        // The open-ended construct's body is the next operand.
        tokens.push(ATOMS[rng.below(ATOMS.len())].to_owned());
    }
}

/// A nested expression, as tokens. `depth` bounds the nesting.
fn expression(rng: &mut SplitMix64, tokens: &mut Vec<String>, depth: usize) {
    let push = |tokens: &mut Vec<String>, token: &str| tokens.push(token.to_owned());
    if depth == 0 {
        push(tokens, ATOMS[rng.below(ATOMS.len())]);
        return;
    }
    match rng.below(20) {
        0 | 1 => {
            let links = 2 + rng.below(3);
            chain(rng, tokens, links);
        }
        2 => {
            push(tokens, UNARY[rng.below(UNARY.len())]);
            expression(rng, tokens, depth - 1);
        }
        3 | 4 => {
            expression(rng, tokens, depth - 1);
            push(tokens, BINARY[rng.below(BINARY.len())]);
            expression(rng, tokens, depth - 1);
        }
        5 => {
            push(tokens, "(");
            expression(rng, tokens, depth - 1);
            push(tokens, ")");
        }
        6 => {
            push(tokens, "{");
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, [";", "\n", ";", "\n", ";;", "\n\n"][rng.below(6)]);
                }
                expression(rng, tokens, depth - 1);
            }
            push(tokens, "}");
        }
        7 | 8 => {
            push(tokens, "if");
            push(tokens, "(");
            expression(rng, tokens, depth - 1);
            push(tokens, ")");
            expression(rng, tokens, depth - 1);
            if rng.chance(2, 3) {
                push(tokens, "else");
                expression(rng, tokens, depth - 1);
            }
        }
        9 => {
            push(tokens, "for");
            push(tokens, "(");
            push(tokens, NAMES[rng.below(8)]);
            push(tokens, "in");
            expression(rng, tokens, depth - 1);
            push(tokens, ")");
            expression(rng, tokens, depth - 1);
        }
        10 => {
            if rng.chance(1, 2) {
                push(tokens, "while");
                push(tokens, "(");
                expression(rng, tokens, depth - 1);
                push(tokens, ")");
            } else {
                push(tokens, "repeat");
            }
            expression(rng, tokens, depth - 1);
        }
        11 | 12 => {
            push(tokens, ["function", "function", "\\"][rng.below(3)]);
            push(tokens, "(");
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, ",");
                }
                push(
                    tokens,
                    ["x", "y", "...", "a.b", "`a b`", ".z"][rng.below(6)],
                );
                if rng.chance(1, 3) {
                    push(tokens, "=");
                    expression(rng, tokens, depth - 1);
                }
            }
            push(tokens, ")");
            expression(rng, tokens, depth - 1);
        }
        13..=15 => {
            expression(rng, tokens, depth - 1);
            let (open, close) = [("(", ")"), ("(", ")"), ("[", "]"), ("[[", "]]")][rng.below(4)];
            push(tokens, open);
            for index in 0..rng.below(4) {
                if index > 0 {
                    push(tokens, ",");
                }
                match rng.below(8) {
                    0 => {}
                    1 => {
                        push(
                            tokens,
                            ["n", "\"n\"", "`n m`", "NULL", "if", "'n'"][rng.below(6)],
                        );
                        push(tokens, "=");
                        if rng.chance(3, 4) {
                            expression(rng, tokens, depth - 1);
                        }
                    }
                    _ => expression(rng, tokens, depth - 1),
                }
            }
            push(tokens, close);
        }
        16 => {
            expression(rng, tokens, depth - 1);
            push(tokens, ["$", "@", "$", "::", ":::"][rng.below(5)]);
            push(tokens, NAMES[rng.below(NAMES.len())]);
        }
        17 => {
            expression(rng, tokens, depth - 1);
            push(tokens, "|>");
            push(tokens, "f");
            push(tokens, "(");
            if rng.chance(1, 2) {
                push(tokens, "y");
                push(tokens, "=");
                push(tokens, "_");
            }
            push(tokens, ")");
        }
        18 => push(
            tokens,
            ["break", "next", "NULL", "function(x) x"][rng.below(4)],
        ),
        _ => push(tokens, ATOMS[rng.below(ATOMS.len())]),
    }
}

/// A numeric literal built part by part, any part of which may be missing or doubled.
fn number(rng: &mut SplitMix64) -> String {
    const FIXED: &[&str] = &[
        "0x",
        "1e",
        "1e+",
        "1e-",
        "0x1p",
        "0x1p+",
        "1.5L",
        "1e3L",
        "1e-3L",
        "0x10L",
        "1i",
        "1e-2i",
        "100000000000L",
        "2147483647L",
        "2147483648L",
        "1e400",
        "1e-400",
        "0x1.8p1",
        "0x.8p1",
        "0x1.p1",
        "0x1.8",
        "1..2",
        "1.2.3",
        "1 .2",
        "5.",
        ".5",
        ".",
        "0x1g",
        "1e5e5",
        "1L2",
        "1iL",
        "1Li",
        "0x1i",
        "0x1L",
        "1l",
        "1E5",
        "1e+05L",
        "0X1F",
        "0x1P3",
        "1_000",
        "00012",
        "0.0L",
        "1e0L",
        "0xFFFFFFFFFFFFFFFFFFFF",
        "123456789012345678901234567890",
        "4.9e-324",
        "1.0e+00i",
        ".5e",
        "0.5e1L",
        "1.0L",
        "0x1p-1074",
        "1e1000L",
        "0xfp2L",
        "0b101",
        "1e 5",
        "1 e5",
        "1e5.5",
        "0x1.1",
        "9007199254740993",
        "0.1e-1i",
        ".e5",
        "0x.p1",
        "1L.5",
        "TRUE1",
        "1TRUE",
        "1x",
        "0xg",
    ];
    if rng.chance(1, 3) {
        return FIXED[rng.below(FIXED.len())].to_owned();
    }
    let mut text = String::new();
    let hex = rng.chance(1, 4);
    let digits = if hex {
        "0123456789abcdefABCDEF"
    } else {
        "0123456789"
    };
    let run = |rng: &mut SplitMix64, text: &mut String, most: usize| {
        for _ in 0..rng.below(most + 1) {
            text.push(char::from(digits.as_bytes()[rng.below(digits.len())]));
        }
    };
    if hex {
        text.push_str(["0x", "0X"][rng.below(2)]);
    }
    run(rng, &mut text, 4);
    if rng.chance(1, 2) {
        text.push('.');
        run(rng, &mut text, 4);
    }
    if rng.chance(1, 2) {
        let markers = if hex {
            ["p", "P", "p", "e"]
        } else {
            ["e", "E", "e", "p"]
        };
        text.push_str(markers[rng.below(4)]);
        text.push_str(["", "", "+", "-"][rng.below(4)]);
        for _ in 0..rng.below(4) {
            text.push(char::from(b'0' + rng.below(10) as u8));
        }
    }
    text.push_str(["", "", "", "L", "i", "l", "I"][rng.below(7)]);
    text
}

/// A string literal: a plain or a raw one, from pieces that each exercise one rule of the lexer.
fn string(rng: &mut SplitMix64) -> String {
    const PIECES: &[&str] = &[
        "a",
        " ",
        "é",
        "日",
        "😀",
        "\\n",
        "\\t",
        "\\\\",
        "\\\"",
        "\\'",
        "\\`",
        "\\a",
        "\\b",
        "\\f",
        "\\v",
        "\\r",
        "\\0",
        "\\x41",
        "\\x4",
        "\\xg",
        "\\x",
        "\\xe4",
        "\\101",
        "\\7",
        "\\18",
        "\\400",
        "\\377",
        "\\u00e4",
        "\\u{e4}",
        "\\u{1F600}",
        "\\U0001F600",
        "\\U{110000}",
        "\\u{}",
        "\\u",
        "\\u{e4",
        "\\ue4z",
        "\\U{0}",
        "\\u0000",
        "\\x00",
        "\\q",
        "\\ ",
        "\\\n",
        "\n",
        "\t",
        "'",
        "\"",
        "#",
        "\\U0010FFFF",
        "\\u{00e4}x",
        "\\xc3\\xa9",
        "\\8",
        "\\e",
        "%",
        "\\N",
    ];
    if rng.chance(1, 4) {
        let dashes = "-".repeat(rng.below(3));
        let (open, close) = [("(", ")"), ("[", "]"), ("{", "}")][rng.below(3)];
        let quote = ["\"", "'"][rng.below(2)];
        let mut text = format!("{}{quote}{dashes}{open}", ["r", "R"][rng.below(2)]);
        for _ in 0..rng.below(5) {
            text.push_str(
                [
                    "a", "\\", "\\n", "\"", "'", ")", "]", "}", "-", ")\"", "\n", "é", ")-", "(",
                ][rng.below(14)],
            );
        }
        // A closing delimiter that does not match leaves the string open.
        match rng.below(8) {
            0 => text.push_str(&format!("{close}{quote}")),
            1 => text.push_str(&format!(")-{quote}")),
            2 => text.push_str(&format!("{close}{dashes}")),
            _ => text.push_str(&format!("{close}{dashes}{quote}")),
        }
        return text;
    }
    let quote = ["\"", "\"", "'"][rng.below(3)];
    let mut text = quote.to_owned();
    for _ in 0..rng.below(6) {
        let piece = PIECES[rng.below(PIECES.len())];
        // A bare quote of the string's own kind ends it, which is a case of its own.
        if piece == quote && !rng.chance(1, 6) {
            continue;
        }
        text.push_str(piece);
    }
    if !rng.chance(1, 12) {
        text.push_str(quote);
    }
    text
}

/// `dump_` prints a parsed expression's structure. `p_` parses one fragment and prints the
/// dump of every expression, or the error, then each warning the parse raised.
const PRELUDE: &str = r#"hex_ <- function(s) paste(as.character(charToRaw(s)), collapse = "")
name_ <- function(n) {
    if (grepl("^[A-Za-z.][A-Za-z0-9._]*$", n)) n else paste0("`", hex_(n), ":", Encoding(n), "`")
}
bits_ <- function(x) paste(as.character(writeBin(x, raw(), endian = "big")), collapse = "")
parts_ <- function(e) {
    n <- names(e)
    parts <- character(length(e))
    for (i in seq_along(e)) {
        part <- if (identical(e[[i]], quote(expr = ))) "<empty>" else dump_(e[[i]])
        parts[i] <- if (!is.null(n) && !is.na(n[i]) && nzchar(n[i])) paste0(name_(n[i]), "=", part) else part
    }
    paste(parts, collapse = " ")
}
dump_ <- function(e) {
    switch(typeof(e),
        symbol = name_(as.character(e)),
        language = paste0("(", parts_(e), ")"),
        pairlist = paste0("[", parts_(e), "]"),
        double = paste0("dbl:", bits_(e)),
        integer = paste0("int:", e),
        logical = paste0("lgl:", e),
        complex = paste0("cplx:", bits_(Re(e)), "+", bits_(Im(e))),
        character = if (is.na(e)) "str:NA" else paste0("str:", hex_(e), ":", Encoding(e)),
        "NULL" = "NULL",
        paste0("<", typeof(e), ">"))
}
n_ <- 0L
p_ <- function(s, mode = "message") {
    warnings <- character()
    result <- withCallingHandlers(
        tryCatch({
            es <- parse(text = s, keep.source = FALSE)
            paste0("ok ", length(es), " ", paste(vapply(as.list(es), dump_, ""), collapse = " ; "))
        }, error = function(e) paste0("error ", sub("^(invalid \\\\[uUx][{]x+[}] sequence) [(].*[)]$", "\\1", conditionMessage(e)))),
        warning = function(w) {
            warnings <<- c(warnings, conditionMessage(w))
            invokeRestart("muffleWarning")
        })
    rejected <- startsWith(result, "error ")
    ry <- function() tryCatch({ parse(text = "f\"s\""); TRUE }, error = function(e) FALSE)
    if (mode == "extension" && (rejected || ry())) {
        result <- "R rejects"
        warnings <- character()
    } else if (mode != "message" && rejected) {
        result <- "error"
    }
    if (rejected) warnings <- character()
    n_ <<- n_ + 1L
    cat(n_, " ", result, "\n", sep = "")
    for (w in warnings) cat("  warning: ", w, "\n", sep = "")
}
"#;

#[cfg(test)]
mod tests {
    use {super::*, std::collections::HashSet};

    /// The outer program is plain R whatever the fragments hold, a seed gives one program, and
    /// seeds differ.
    #[test]
    fn programs_parse_and_are_deterministic() {
        let mut distinct = HashSet::new();
        for seed in 0..200 {
            let text = program(seed);
            let errors = parse(&text).errors().to_vec();
            assert!(
                errors.is_empty(),
                "seed {seed} gave a program that does not parse: {errors:?}\n{text}"
            );
            assert_eq!(text, program(seed), "seed {seed}");
            assert!(text.lines().filter(|line| line.starts_with("p_(")).count() >= 30);
            distinct.insert(text);
        }
        assert_eq!(distinct.len(), 200);
    }

    /// Every family writes both text ry's parser accepts and text it rejects, except a chain,
    /// which is well formed, and soup, which almost never is.
    #[test]
    fn families_reach_both_answers() {
        for family in FAMILIES {
            let mut rng = SplitMix64(7);
            let (mut accepted, mut rejected) = (0, 0);
            for _ in 0..400 {
                if parse(&fragment(&mut rng, family)).errors().is_empty() {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
            if family != Family::Soup {
                assert!(accepted > 20, "{family:?} parses {accepted} of 400");
            }
            if family != Family::Chain {
                assert!(rejected > 4, "{family:?} is rejected {rejected} of 400");
            }
        }
    }
}
