//! The parser fuzz program: source fragments handed to `parse(text =)`, with the tree each
//! parses to, or the error it is rejected with, printed in full.
//!
//! A program is a prelude and one `p_("<fragment>")` statement per fragment, so the reducer
//! shrinks a finding to one line. `p_` prints a structural dump of each expression: a call as
//! `(head argument ...)` with argument names, a symbol by name, a string by its bytes and encoding
//! mark, a number by its type and bits. Deparsed text would hide what a parser gets wrong (an
//! integer read as a double, the grouping of `-a^b`, an escape decoded to another byte).
//!
//! A seed enables a random subset of [`FragmentFamily`]s. Most write text that should parse, and
//! two damage it by one token or draw tokens blindly, so both the tree and the refusal are tested.
//!
//! A fragment with an inline annotation is not generated, and one where ry's parser finds a
//! record, a tuple or another ry construct is passed in the mode `"extension"`, where `p_` prints
//! only that R rejects it. Text GNU R accepts and ry reads as its own syntax still differs.
//!
//! A rejected fragment prints its whole error message only in the mode `"message"`, which the
//! string and number families use, because there the lexer refuses; GNU R prints an
//! uninitialized line number after "invalid \u{xxxx} sequence", so that message loses its
//! location. Other families print only that the fragment was rejected, and no warnings.

use {
    std::iter::successors,
    syntax::{
        Direction,
        SyntaxKind::{
            self, ARGUMENT_LIST, ARRAY_EXPR, BANG_EQ, BINARY_EXPR, BRACE_EXPR, BREAK_EXPR, COMMA,
            ELSE_KW, EQ, EQ2, FOR_EXPR, FSTRING, FUNCTION_DEF, GREATER, GREATER_EQ, IF_EXPR,
            L_BRACKET, L_BRACKET2, L_PAREN, LESS, LESS_EQ, LITERAL, MATCH_EXPR, NAME,
            NAMESPACE_EXPR, NEW_EXPR, NEWLINE, NEXT_EXPR, PARAMETER_LIST, PAREN_EXPR, PURE_MARKER,
            QUESTION, R_BRACE, R_BRACKET, R_PAREN, RAW_STRING, RECORD_EXPR, REPEAT_EXPR,
            SOURCE_FILE, STRING, SUFFIXED_NUMBER, TAG_EXPR, TRY_EXPR, TUPLE_EXPR, TYPE_DEFINITION,
            UNARY_EXPR, WHILE_EXPR,
        },
        SyntaxNode, SyntaxToken, erase_inline_annotations, parse,
        testing::{FRAGMENT_FAMILIES, FragmentFamily, SplitMix64, source_fragment},
    },
};

const FRAGMENTS_PER_PROGRAM: usize = 60;

/// The parser fuzz program for `seed`.
pub fn program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x7061_7273_6572_5f66);
    // One seed in eight uses every family, so that the whole space is also sampled densely.
    let mut families: Vec<FragmentFamily> = if rng.chance(1, 8) {
        FRAGMENT_FAMILIES.to_vec()
    } else {
        FRAGMENT_FAMILIES
            .into_iter()
            .filter(|_| rng.chance(1, 2))
            .collect()
    };
    if families.is_empty() {
        families.push(FRAGMENT_FAMILIES[rng.below(FRAGMENT_FAMILIES.len())]);
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
        let text = source_fragment(&mut rng, family);
        let Some(extension) = ry_extension(&text) else {
            continue;
        };
        if known_difference(&text) {
            continue;
        }
        let mode = match family {
            _ if extension => ", \"extension\"",
            FragmentFamily::Text | FragmentFamily::Number => "",
            _ => ", \"answer\"",
        };
        out.push_str(&format!("p_(\"{}\"{mode})\n", r_string_body(&text)));
        written += 1;
    }
    out
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
        for family in FRAGMENT_FAMILIES {
            let mut rng = SplitMix64(7);
            let (mut accepted, mut rejected) = (0, 0);
            for _ in 0..400 {
                if parse(&source_fragment(&mut rng, family))
                    .errors()
                    .is_empty()
                {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
            if family != FragmentFamily::Soup {
                assert!(accepted > 20, "{family:?} parses {accepted} of 400");
            }
            if family != FragmentFamily::Chain {
                assert!(rejected > 4, "{family:?} is rejected {rejected} of 400");
            }
        }
    }
}
