//! Mutations of real R programs for differential fuzzing.
//!
//! [`mutate`] applies one to four edits to a program, each chosen by the seed and spliced into
//! the text at the range of a syntax node: a literal becomes a boundary value, an operator its
//! neighbor, a statement gets wrapped in a closure or `local`, a call loses, repeats or swaps an
//! argument, and so on. Every edit keeps the program parseable, but a mutant may well fail when
//! it runs: GNU R and ry then have to agree on the error.

use syntax::{
    NodeOrToken, SyntaxKind, SyntaxNode, TextRange,
    kind::SyntaxKind::{
        AMP, AMP2, ARGUMENT, ARGUMENT_LIST, BANG_EQ, BINARY_EXPR, BRACE_EXPR, CALL_EXPR, CARET,
        COLON, DOUBLE, EQ, EQ2, FALSE_KW, FUNCTION_DEF, GREATER, GREATER_EQ, IDENT, INTEGER,
        L_BRACKET, L_BRACKET2, LESS, LESS_EQ, LESS_MINUS, LESS2_MINUS, LITERAL, MINUS, NA_KW, NAME,
        NULL_KW, PARAMETER, PARAMETER_LIST, PIPE, PIPE2, PLUS, SLASH, SOURCE_FILE, SPECIAL, STAR,
        STRING, SUBSET_EXPR, SUBSET2_EXPR, TRUE_KW,
    },
    parse,
    testing::SplitMix64,
};

/// The kinds of edit [`mutate`] draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MutationKind {
    /// A numeric literal becomes a boundary value (`0`, `NA`, `-0`, `1e300`, ...).
    Number,
    /// A string literal becomes an empty, missing, non-ASCII, latin1, long or escaped string.
    String,
    /// `a:b` becomes a materialized form of the compact sequence, such as `seq(a, b)`.
    Range,
    /// A `c(...)` call gains names or an `NA`, is reversed, or is emptied.
    Combine,
    /// A top-level statement (or the value an assignment stores) is wrapped in a closure call,
    /// `local`, `identity`, a serialization round trip, `eval(quote())`, or runs twice.
    Statement,
    /// The statement holding the first call of a top-level function runs three times.
    RepeatedCall,
    /// A call has two positional arguments swapped, or one argument dropped or duplicated.
    Arguments,
    /// A binary operator becomes another of its family, or `<-` becomes `<<-` in a function.
    Operator,
    /// `[[` becomes `[` and the other way around.
    Subset,
    /// `TRUE`, `FALSE` and `NA` trade places, and `NULL` becomes an empty vector or list.
    Constant,
    /// A function body gains a statement that never runs: a copy of another statement of the
    /// program behind `if (FALSE)`, in a loop that makes no pass, or in a closure nobody calls.
    DeadCode,
    /// A function body gains a statement that does not change its result: a constant, a loop
    /// over nothing much, a look at the frame (`environment()`, `sys.call()`), a new local.
    Neutral,
}

/// The kinds that leave what a program computes alone and change only what the compiler sees.
const EFFECT_FREE: [MutationKind; 2] = [MutationKind::DeadCode, MutationKind::Neutral];

/// `program` with one to four mutations, the same text for the same seed. A mutation whose
/// result would not parse is skipped, so the result always parses when `program` does; a
/// program that does not parse comes back unchanged.
pub fn mutate(program: &str, seed: u64) -> String {
    let mut rng = SplitMix64(MUTATION_STREAM ^ seed);
    let count = 1 + rng.below(4);
    let mut current = program.to_string();
    if !parse(&current).errors().is_empty() {
        return current;
    }
    for _ in 0..count {
        let root = parse(&current).syntax_node();
        let sites = sites(&root);
        let mut kinds: Vec<MutationKind> = sites.iter().map(|site| site.kind).collect();
        kinds.sort();
        kinds.dedup();
        if kinds.is_empty() {
            break;
        }
        let kind = kinds[rng.below(kinds.len())];
        let candidates: Vec<&Site> = sites.iter().filter(|site| site.kind == kind).collect();
        let site = candidates[rng.below(candidates.len())];
        if let Some(candidate) = apply(&current, site, &mut rng)
            && parse(&candidate).errors().is_empty()
        {
            current = candidate;
        }
    }
    current
}

/// Keeps the mutator's stream apart from the stream that picks the snippet for the same seed.
const MUTATION_STREAM: u64 = 0x6d75_7461_6e74_0001;

/// `program` with four to twelve effect-free mutations in its function bodies, the same text for
/// the same seed. A mutation that changes a value usually ends the program in an early error.
/// These keep the whole run and still move the slot numbers, constant indices, branch offsets
/// and frame states the compiled body has, so a difference from GNU R on the result is a bug in
/// how the body was compiled or run. A program without a function comes back unchanged.
pub fn perturb(program: &str, seed: u64) -> String {
    let mut rng = SplitMix64(PERTURBATION_STREAM ^ seed);
    let count = 4 + rng.below(9);
    let mut current = program.to_string();
    if !parse(&current).errors().is_empty() {
        return current;
    }
    for _ in 0..count {
        let root = parse(&current).syntax_node();
        let kind = EFFECT_FREE[rng.below(EFFECT_FREE.len())];
        let candidates: Vec<Site> = sites(&root)
            .into_iter()
            .filter(|site| site.kind == kind)
            .collect();
        if candidates.is_empty() {
            break;
        }
        let site = &candidates[rng.below(candidates.len())];
        if let Some(candidate) = apply(&current, site, &mut rng)
            && parse(&candidate).errors().is_empty()
        {
            current = candidate;
        }
    }
    current
}

const PERTURBATION_STREAM: u64 = 0x7065_7274_7572_6201;

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "2",
    "0L",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NaN",
    "Inf",
    "-Inf",
    // Not `-0`, which GNU R's compiler merges with a `0` in the same function.
    "-numeric(1)",
    "0.5",
    "1e-300",
    "1e300",
    "63",
    "64",
    "65",
    "255",
    "256",
    "1000",
    "2147483647L",
    ".Machine$integer.max",
    "1e5",
];

const STRINGS: &[&str] = &[
    "\"\"",
    "NA_character_",
    "\"aä\"",
    "\"ß\"",
    "\"日本\"",
    "\"\\U0001F600\"",
    "iconv(\"é\", \"UTF-8\", \"latin1\")",
    "strrep(\"ab\", 200)",
    "\"a\\nb\"",
    "\"a\\\"b\"",
    "\"a\\\\b\"",
];

/// Names given to the unnamed elements of a `c()` call, in order, so the result mixes plain,
/// non-ASCII, non-syntactic and duplicated names.
const ELEMENT_NAMES: &[&str] = &["a", "\"ä\"", "b", "`c d`", "a"];

/// Statements that never run, with `{}` for the statement they hold.
const DEAD_FORMS: &[&str] = &[
    "if (FALSE) { {} }",
    "while (FALSE) { {} }",
    "if (TRUE) NULL else { {} }",
    "dead__ <- function() { {} }",
    "repeat { break; {} }",
    "for (dead__ in NULL) { {} }",
    "switch(\"none\", other = { {} })",
];

/// Statements for a dead form to hold when the program offers none of its own.
const DEAD_STATEMENTS: &[&str] = &[
    "stop(\"unreachable\")",
    "dead__ <- numeric(64); dead__[2] <- 1; dead__[3] <- 2",
    "return(NULL)",
    "dead__ <<- NULL",
];

/// Statements that leave a body's result alone. Several ask for the frame, which a call that
/// keeps its variables in registers must then build partway through the body, and the loops
/// give a body without one a backward jump to compile at.
const NEUTRAL_STATEMENTS: &[&str] = &[
    "invisible(NULL)",
    "NULL",
    "environment()",
    "sys.function()",
    "sys.call()",
    "parent.frame()",
    "nargs()",
    "gc(FALSE)",
    "neutral__ <- 0L",
    "(function() NULL)()",
    "on.exit(NULL, add = TRUE)",
    "tryCatch(NULL, error = function(e) NULL)",
    "local(NULL)",
    "eval(quote(NULL))",
    "suppressWarnings(NULL)",
    "for (neutral__ in 1:3) NULL",
    "repeat break",
    "if (TRUE) NULL",
];

/// Statements that read the parameter `{}`. They leave a body's result alone unless the body
/// depends on when the argument is forced: `force` and the self-assignment evaluate it at once,
/// which raises the error of a missing argument early and moves a side effect of its promise.
const NEUTRAL_PARAMETER_STATEMENTS: &[&str] = &[
    "missing({})",
    "exists(\"{}\", inherits = FALSE)",
    "{} <- {}",
    "force({})",
    "substitute({})",
];

const ARITHMETIC: &[&str] = &["+", "-", "*", "/", "%%", "%/%", "^"];
const COMPARISON: &[&str] = &["<", "<=", ">", ">=", "==", "!="];

/// A node a mutation of `kind` can rewrite.
struct Site {
    kind: MutationKind,
    node: SyntaxNode,
}

fn sites(root: &SyntaxNode) -> Vec<Site> {
    let mut sites = Vec::new();
    for statement in root.children() {
        sites.push(Site {
            kind: MutationKind::Statement,
            node: statement.clone(),
        });
        if defined_function(&statement).is_some() {
            sites.push(Site {
                kind: MutationKind::RepeatedCall,
                node: statement,
            });
        }
    }
    for node in root.descendants() {
        let kind = match node.kind() {
            LITERAL if is_argument_tag(&node) => None,
            LITERAL => match literal_token(&node) {
                Some(INTEGER | DOUBLE) => Some(MutationKind::Number),
                Some(STRING) => Some(MutationKind::String),
                Some(TRUE_KW | FALSE_KW | NA_KW | NULL_KW) => Some(MutationKind::Constant),
                _ => None,
            },
            BINARY_EXPR => match operator(&node).map(|token| token.kind()) {
                Some(COLON) => Some(MutationKind::Range),
                Some(_) if !operator_replacements(&node).is_empty() => Some(MutationKind::Operator),
                _ => None,
            },
            SUBSET_EXPR | SUBSET2_EXPR => Some(MutationKind::Subset),
            CALL_EXPR => {
                if callee_name(&node).as_deref() == Some("c") {
                    sites.push(Site {
                        kind: MutationKind::Combine,
                        node: node.clone(),
                    });
                }
                if arguments(&node).is_empty() {
                    None
                } else {
                    Some(MutationKind::Arguments)
                }
            }
            _ => None,
        };
        if let Some(kind) = kind {
            sites.push(Site { kind, node });
        }
    }
    for node in root.descendants() {
        // A statement of a braced block inside a function, or a function's whole unbraced
        // body: the places a statement can go in front of without changing the body's value.
        let places: Vec<SyntaxNode> = match node.kind() {
            BRACE_EXPR if enclosing_function(&node).is_some() => node.children().collect(),
            FUNCTION_DEF => node
                .children()
                .last()
                .filter(|body| !matches!(body.kind(), BRACE_EXPR | PARAMETER_LIST))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        for place in places {
            for kind in EFFECT_FREE {
                sites.push(Site {
                    kind,
                    node: place.clone(),
                });
            }
        }
    }
    sites
}

/// The mutated program, or `None` when the drawn variant does not apply to the site.
fn apply(text: &str, site: &Site, rng: &mut SplitMix64) -> Option<String> {
    let node = &site.node;
    match site.kind {
        MutationKind::Number => Some(replace(
            text,
            significant_range(node),
            NUMBERS[rng.below(NUMBERS.len())],
        )),
        MutationKind::String => Some(replace(
            text,
            significant_range(node),
            STRINGS[rng.below(STRINGS.len())],
        )),
        MutationKind::Constant => {
            let replacement = match literal_token(node)? {
                NULL_KW => ["list()", "character(0)", "integer(0)"][rng.below(3)],
                kind => {
                    let others: Vec<&str> = [(TRUE_KW, "TRUE"), (FALSE_KW, "FALSE"), (NA_KW, "NA")]
                        .into_iter()
                        .filter(|(other, _)| *other != kind)
                        .map(|(_, spelling)| spelling)
                        .collect();
                    others[rng.below(others.len())]
                }
            };
            Some(replace(text, significant_range(node), replacement))
        }
        MutationKind::Range => {
            let operands: Vec<String> = node
                .children()
                .map(|child| significant_text(text, &child))
                .collect();
            let [from, to] = operands.as_slice() else {
                return None;
            };
            let replacement = match rng.below(5) {
                0 => format!("seq({from}, {to})"),
                1 => format!("c({from}:{to})"),
                2 => format!("rev(({to}):({from}))"),
                3 => format!("as.numeric({from}:{to})"),
                _ => format!("({from}:{to})[TRUE]"),
            };
            Some(replace(text, significant_range(node), &replacement))
        }
        MutationKind::Combine => {
            let mut elements: Vec<String> = arguments(node)
                .iter()
                .map(|argument| significant_text(text, argument))
                .collect();
            match rng.below(4) {
                0 => {
                    let mut names = ELEMENT_NAMES.iter().cycle();
                    for (argument, element) in arguments(node).iter().zip(elements.iter_mut()) {
                        if !is_tagged(argument) && !element.is_empty() && element != "..." {
                            let name = names.next()?;
                            *element = format!("{name} = {element}");
                        }
                    }
                }
                1 => elements.push("NA".to_string()),
                2 => elements.reverse(),
                _ => elements.clear(),
            }
            Some(replace(
                text,
                significant_range(node),
                &format!("c({})", elements.join(", ")),
            ))
        }
        MutationKind::Statement => {
            let range = significant_range(node);
            let statement = &text[span(range)];
            if rng.below(8) == 0 {
                return Some(replace(text, range, &format!("{statement}\n{statement}")));
            }
            // Wrapping a whole assignment would bind its name in the wrapper's scope instead of
            // the global one, so the stored value is wrapped instead.
            let target = match assignment_value(node) {
                Some(value) => value,
                None => node.clone(),
            };
            let target_range = significant_range(&target);
            let expression = &text[span(target_range)];
            let wrapped = match rng.below(7) {
                0 => format!("(function() {expression})()"),
                1 => format!("local({expression})"),
                2 => format!("local({{ x__ <- {expression}; x__ }})"),
                3 => format!("identity({expression})"),
                4 => format!("unserialize(serialize({expression}, NULL))"),
                5 => format!("eval(quote({expression}))"),
                _ => format!("withVisible({expression})$value"),
            };
            Some(replace(text, target_range, &wrapped))
        }
        MutationKind::RepeatedCall => {
            let name = defined_function(node)?;
            let call = node
                .siblings(syntax::Direction::Next)
                .skip(1)
                .find(|statement| calls(statement, &name))?;
            let range = significant_range(&call);
            let statement = &text[span(range)];
            let end = TextRange::empty(range.end());
            Some(replace(text, end, &format!("\n{statement}\n{statement}")))
        }
        MutationKind::Arguments => {
            let list = arguments(node);
            let mut elements: Vec<String> = list
                .iter()
                .map(|argument| significant_text(text, argument))
                .collect();
            let positional: Vec<usize> = list
                .iter()
                .enumerate()
                .filter(|(index, argument)| !is_tagged(argument) && !elements[*index].is_empty())
                .map(|(index, _)| index)
                .collect();
            match rng.below(3) {
                0 if positional.len() >= 2 => {
                    let first = rng.below(positional.len());
                    let second = (first + 1 + rng.below(positional.len() - 1)) % positional.len();
                    elements.swap(positional[first], positional[second]);
                }
                1 => {
                    elements.remove(rng.below(elements.len()));
                }
                _ => {
                    let index = rng.below(elements.len());
                    let copy = elements[index].clone();
                    elements.insert(index + 1, copy);
                }
            }
            let callee = node.first_child()?;
            Some(replace(
                text,
                significant_range(node),
                &format!(
                    "{}({})",
                    significant_text(text, &callee),
                    elements.join(", ")
                ),
            ))
        }
        MutationKind::Operator => {
            let token = operator(node)?;
            let replacements = operator_replacements(node);
            let replacement = replacements[rng.below(replacements.len())];
            Some(replace(text, token.text_range(), replacement))
        }
        MutationKind::Subset => {
            let list = node
                .children()
                .find(|child| child.kind() == ARGUMENT_LIST)?;
            let range = significant_range(&list);
            let brackets = &text[span(range)];
            let replacement = match list.first_token()?.kind() {
                L_BRACKET2 => format!("[{}]", brackets.get(2..brackets.len().checked_sub(2)?)?),
                L_BRACKET => format!("[[{}]]", brackets.get(1..brackets.len().checked_sub(1)?)?),
                _ => return None,
            };
            Some(replace(text, range, &replacement))
        }
        MutationKind::DeadCode => {
            let root = node.ancestors().last()?;
            // A `break` or `next` copied out of its loop is an error where GNU R's compiler
            // sees it, dead or not.
            let donors: Vec<String> = root
                .descendants()
                .filter(|donor| {
                    donor
                        .parent()
                        .is_some_and(|parent| matches!(parent.kind(), BRACE_EXPR | SOURCE_FILE))
                })
                .map(|donor| significant_text(text, &donor))
                .filter(|donor| {
                    !donor.is_empty()
                        && donor.len() <= 400
                        && !donor.contains("break")
                        && !donor.contains("next")
                })
                .collect();
            let statement = if donors.is_empty() || rng.below(4) == 0 {
                DEAD_STATEMENTS[rng.below(DEAD_STATEMENTS.len())].to_string()
            } else {
                donors[rng.below(donors.len())].clone()
            };
            let form = DEAD_FORMS[rng.below(DEAD_FORMS.len())];
            Some(insert_before(text, node, &form.replace("{}", &statement)))
        }
        MutationKind::Neutral => {
            let parameters = enclosing_function(node)
                .map(|function| parameter_names(&function))
                .unwrap_or_default();
            let statement = if parameters.is_empty() || rng.below(3) != 0 {
                NEUTRAL_STATEMENTS[rng.below(NEUTRAL_STATEMENTS.len())].to_string()
            } else {
                NEUTRAL_PARAMETER_STATEMENTS[rng.below(NEUTRAL_PARAMETER_STATEMENTS.len())]
                    .replace("{}", &parameters[rng.below(parameters.len())])
            };
            Some(insert_before(text, node, &statement))
        }
    }
}

/// `text` with `statement` run just before `place`, which is a statement of a braced block or
/// a function's unbraced body.
fn insert_before(text: &str, place: &SyntaxNode, statement: &str) -> String {
    let range = significant_range(place);
    if place
        .parent()
        .is_some_and(|parent| parent.kind() == BRACE_EXPR)
    {
        let start = TextRange::empty(range.start());
        return replace(text, start, &format!("{statement}\n"));
    }
    let body = &text[span(range)];
    replace(text, range, &format!("{{ {statement}\n{body} }}"))
}

fn enclosing_function(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors()
        .skip(1)
        .find(|ancestor| ancestor.kind() == FUNCTION_DEF)
}

/// The plain names among a function's parameters, without `...`.
fn parameter_names(function: &SyntaxNode) -> Vec<String> {
    function
        .children()
        .filter(|child| child.kind() == PARAMETER_LIST)
        .flat_map(|list| list.children())
        .filter(|child| child.kind() == PARAMETER)
        .filter_map(|parameter| parameter.first_token())
        .filter(|token| token.kind() == IDENT)
        .map(|token| token.text().to_string())
        .collect()
}

fn replace(text: &str, range: TextRange, replacement: &str) -> String {
    let range = span(range);
    format!(
        "{}{}{}",
        &text[..range.start],
        replacement,
        &text[range.end..]
    )
}

fn span(range: TextRange) -> std::ops::Range<usize> {
    usize::from(range.start())..usize::from(range.end())
}

/// The node's range without leading or trailing trivia: a statement node ends with the comment
/// on its line, which would swallow whatever text a mutation puts after it.
fn significant_range(node: &SyntaxNode) -> TextRange {
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|token| !token.kind().is_trivia());
    let Some(first) = tokens.next() else {
        return TextRange::empty(node.text_range().start());
    };
    let last = tokens.last().unwrap_or_else(|| first.clone());
    TextRange::new(first.text_range().start(), last.text_range().end())
}

fn significant_text(text: &str, node: &SyntaxNode) -> String {
    text[span(significant_range(node))].to_string()
}

fn literal_token(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.first_token().map(|token| token.kind())
}

/// The first token directly under a binary expression is its operator, because both operands
/// are nodes.
fn operator(node: &SyntaxNode) -> Option<syntax::SyntaxToken> {
    node.children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|token| !token.kind().is_trivia())
}

fn operator_replacements(node: &SyntaxNode) -> Vec<&'static str> {
    let Some(token) = operator(node) else {
        return Vec::new();
    };
    let family: &[&str] = match token.kind() {
        PLUS | MINUS | STAR | SLASH | CARET => ARITHMETIC,
        SPECIAL if matches!(token.text(), "%%" | "%/%") => ARITHMETIC,
        LESS | LESS_EQ | GREATER | GREATER_EQ | EQ2 | BANG_EQ => COMPARISON,
        AMP2 => &["&"],
        AMP => &["&&"],
        PIPE2 => &["|"],
        PIPE => &["||"],
        LESS_MINUS
            if node
                .ancestors()
                .skip(1)
                .any(|ancestor| ancestor.kind() == FUNCTION_DEF) =>
        {
            &["<<-"]
        }
        _ => &[],
    };
    family
        .iter()
        .copied()
        .filter(|spelling| *spelling != token.text())
        .collect()
}

/// A literal before `=` in an argument is the argument's name (`switch(x, NULL = 1)`).
fn is_argument_tag(node: &SyntaxNode) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == ARGUMENT
            && parent.first_child().as_ref() == Some(node)
            && is_tagged(&parent)
    })
}

fn is_tagged(argument: &SyntaxNode) -> bool {
    argument
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .any(|token| token.kind() == EQ)
}

fn arguments(call: &SyntaxNode) -> Vec<SyntaxNode> {
    call.children()
        .find(|child| child.kind() == ARGUMENT_LIST)
        .map(|list| {
            list.children()
                .filter(|child| child.kind() == ARGUMENT)
                .collect()
        })
        .unwrap_or_default()
}

fn callee_name(call: &SyntaxNode) -> Option<String> {
    let callee = call.first_child()?;
    (callee.kind() == NAME).then(|| callee.text().trim().to_string())
}

/// The assigned value of `name <- value`, `name = value` or `name <<- value`.
fn assignment_value(statement: &SyntaxNode) -> Option<SyntaxNode> {
    if statement.kind() != BINARY_EXPR
        || !matches!(operator(statement)?.kind(), LESS_MINUS | LESS2_MINUS | EQ)
    {
        return None;
    }
    statement.children().nth(1)
}

/// The name a top-level `name <- function(...) ...` defines.
fn defined_function(statement: &SyntaxNode) -> Option<String> {
    let value = assignment_value(statement)?;
    let target = statement.first_child()?;
    (value.kind() == FUNCTION_DEF
        && target.kind() == NAME
        && statement.parent()?.kind() == SOURCE_FILE)
        .then(|| target.text().trim().to_string())
}

fn calls(statement: &SyntaxNode, name: &str) -> bool {
    statement
        .descendants()
        .any(|node| node.kind() == CALL_EXPR && callee_name(&node).as_deref() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program with a site for every kind of mutation.
    const PROGRAM: &str = r#"x <- c(1, 2L, "s") # a comment
square <- function(v) { total <- v^2; total }
square(3)
y <- list(a = TRUE, b = NULL)
y[["a"]] && x[1] < 2
for (i in 1:3) print(i %% 2)
paste("a", "b", sep = "-")
"#;

    fn mutate_kind(program: &str, kind: MutationKind, seed: u64) -> Option<String> {
        let mut rng = SplitMix64(seed);
        let root = parse(program).syntax_node();
        let sites: Vec<Site> = sites(&root)
            .into_iter()
            .filter(|site| site.kind == kind)
            .collect();
        if sites.is_empty() {
            return None;
        }
        let site = &sites[rng.below(sites.len())];
        apply(program, site, &mut rng)
    }

    #[test]
    fn every_kind_of_mutation_fires_and_reparses() {
        let kinds = [
            MutationKind::Number,
            MutationKind::String,
            MutationKind::Range,
            MutationKind::Combine,
            MutationKind::Statement,
            MutationKind::RepeatedCall,
            MutationKind::Arguments,
            MutationKind::Operator,
            MutationKind::Subset,
            MutationKind::Constant,
            MutationKind::DeadCode,
            MutationKind::Neutral,
        ];
        for kind in kinds {
            let mut changed = 0;
            for seed in 0..64 {
                let Some(mutant) = mutate_kind(PROGRAM, kind, seed) else {
                    continue;
                };
                let errors = parse(&mutant).errors().to_vec();
                if mutant != PROGRAM && errors.is_empty() {
                    changed += 1;
                }
            }
            assert!(changed > 0, "{kind:?} never produced a parseable mutant");
        }
    }

    #[test]
    fn perturbed_programs_parse_and_keep_every_statement() {
        const FUNCTIONS: &str = "f <- function(v, ...) { total <- v^2; if (total > 3) { total <- 3 }; total }\ng <- function(a) a + 1\nf(2); g(1)\n";
        let mut distinct = std::collections::BTreeSet::new();
        for seed in 0..300 {
            let perturbed = perturb(FUNCTIONS, seed);
            assert!(
                parse(&perturbed).errors().is_empty(),
                "seed {seed} gave a program that does not parse:\n{perturbed}"
            );
            assert_eq!(perturbed, perturb(FUNCTIONS, seed), "seed {seed}");
            for statement in ["total <- v^2", "total <- 3", "a + 1", "f(2); g(1)"] {
                assert!(
                    perturbed.contains(statement),
                    "seed {seed} lost `{statement}`:\n{perturbed}"
                );
            }
            assert!(
                !perturbed.contains("...)") || !perturbed.contains("missing(...)"),
                "seed {seed} reads `...` as a parameter:\n{perturbed}"
            );
            distinct.insert(perturbed);
        }
        assert!(distinct.len() > 200, "{} distinct programs", distinct.len());
        assert_eq!(perturb("x <- 1\n", 0), "x <- 1\n");
    }

    #[test]
    fn mutants_parse_and_are_deterministic() {
        let mut distinct = std::collections::BTreeSet::new();
        for seed in 0..500 {
            let mutant = mutate(PROGRAM, seed);
            assert!(
                parse(&mutant).errors().is_empty(),
                "seed {seed} gave a mutant that does not parse:\n{mutant}"
            );
            assert_eq!(
                mutant,
                mutate(PROGRAM, seed),
                "seed {seed} is not deterministic"
            );
            distinct.insert(mutant);
        }
        assert!(
            distinct.len() > 200,
            "only {} distinct mutants",
            distinct.len()
        );
    }

    #[test]
    fn specific_mutations_have_the_expected_shape() {
        let wrapped = (0..64)
            .filter_map(|seed| mutate_kind("x <- f(1) # note\n", MutationKind::Statement, seed))
            .collect::<Vec<_>>();
        assert!(
            wrapped
                .iter()
                .any(|mutant| mutant == "x <- local(f(1)) # note\n")
        );
        assert!(
            wrapped
                .iter()
                .any(|mutant| mutant == "x <- f(1)\nx <- f(1) # note\n")
        );
        let repeated = mutate_kind(
            "f <- function() 1\nz <- 2\nf() # call\n",
            MutationKind::RepeatedCall,
            0,
        );
        assert_eq!(
            repeated.as_deref(),
            Some("f <- function() 1\nz <- 2\nf()\nf()\nf() # call\n")
        );
        let subsets = (0..8)
            .filter_map(|seed| mutate_kind("x[[1]]\n", MutationKind::Subset, seed))
            .collect::<Vec<_>>();
        assert!(subsets.iter().all(|mutant| mutant == "x[1]\n"));
        let assigned = (0..8)
            .filter_map(|seed| {
                mutate_kind("g <- function() a <- 1\n", MutationKind::Operator, seed)
            })
            .collect::<Vec<_>>();
        assert!(
            assigned
                .iter()
                .all(|mutant| mutant == "g <- function() a <<- 1\n")
        );
        assert!(mutate_kind("switch(x, NULL = 1)\n", MutationKind::Constant, 0).is_none());
    }

    #[test]
    fn a_program_that_does_not_parse_comes_back_unchanged() {
        assert_eq!(mutate("f(1, \n", 3), "f(1, \n");
    }
}
