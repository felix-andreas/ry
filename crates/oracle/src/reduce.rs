//! Shrinks a program that makes an engine diverge from GNU R to a small one that diverges the
//! same way, so a fuzz finding becomes a short oracle case.
//!
//! [`reduce`] is hierarchical delta debugging over the lossless syntax tree, against any
//! predicate. Every candidate is the current text with one node range replaced, and it reaches
//! the predicate only when it parses without a syntax error and is shorter, so the reduction
//! always terminates. The passes, repeated until none of them changes the program:
//!
//! - remove the comments and blank lines;
//! - delta debugging over the items of every list: the statements of the file and of every
//!   `{ }` block, the arguments of every call or subscript, the parameters of every function,
//!   outer lists before the lists inside them, removing chunks that halve in size;
//! - hoisting: replace a node by one of its parts, such as an `if` by a branch, a call by an
//!   argument, a loop by its body, `(function(x) body)(a)` by `body`;
//! - replacing a compound expression by `NULL`, `0`, `1L`, `""` or `TRUE`;
//! - shrinking string and number literals toward the empty string and toward 0, 1 and 2.
//!
//! [`reduce_divergence`] runs it with engines: a candidate is interesting when the engine still
//! differs from GNU R with the same [`Signature`]. The signature holds the channel that differs
//! and, where a reduction leaves it alone, what differs in it (the error messages, the way the
//! first observed expression differs), so the reduction seldom slides from one bug to another.
//! Printed output and the last value change with every candidate, so a difference in those
//! keeps only its channel.

use {
    crate::{
        Engine, EngineError, Outcome, differing_expressions,
        fuzz::{Difference, classify},
    },
    std::{fmt, path::Path, time::Instant},
    syntax::{
        NodeOrToken,
        SyntaxKind::{self, *},
        SyntaxNode, TextRange,
        ast::is_expression_kind,
        parse,
    },
};

/// Shrinks `program` while `interesting` holds, calling it at most `budget` times, and returns
/// the smallest interesting program found. `program` itself is assumed to be interesting.
pub fn reduce(program: &str, budget: usize, interesting: &mut dyn FnMut(&str) -> bool) -> String {
    let mut reducer = Reducer {
        current: program.to_string(),
        calls: 0,
        budget,
        interesting,
    };
    loop {
        let before = reducer.current.clone();
        strip_comments(&mut reducer);
        remove_list_items(&mut reducer);
        hoist(&mut reducer);
        replace_with_literals(&mut reducer);
        shrink_literals(&mut reducer);
        if reducer.current == before || reducer.spent() {
            return reducer.current;
        }
    }
}

/// The kind of difference between an engine and GNU R. A divergence is classified by the first
/// kind that applies, in the order of the variants: a run that crashed also has no output to
/// compare, and an error usually explains the output that differs after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// Killed by a signal, named as the exit status prints it (`signal: 11 (SIGSEGV)`).
    Crashed(String),
    TimedOut,
    /// Exited without a report, for another reason than a signal or the timeout.
    Failed,
    /// The error that ends the run differs: GNU R's message and the engine's, without their
    /// calls, which a reduction rewrites.
    ErrorsDiffer(Option<String>, Option<String>),
    /// The first warning that differs: GNU R's message and the engine's, without their calls.
    WarningsDiffer(Option<String>, Option<String>),
    MessagesDiffer,
    StdoutDiffers,
    ValueDiffers,
    /// Only the deep observer's dump differs ([`Outcome::observed`]): how the first differing
    /// expression differs, when the dump names one.
    ObservedDiffers(Option<Difference>),
}

impl fmt::Display for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn or_none(message: &Option<String>) -> &str {
            message.as_deref().unwrap_or("none")
        }
        match self {
            Signature::Crashed(signal) => write!(formatter, "crashed ({signal})"),
            Signature::TimedOut => formatter.write_str("timed out"),
            Signature::Failed => formatter.write_str("failed without a report"),
            Signature::ErrorsDiffer(reference, engine) => write!(
                formatter,
                "errors differ (GNU R: {}; the engine: {})",
                or_none(reference),
                or_none(engine)
            ),
            Signature::WarningsDiffer(reference, engine) => write!(
                formatter,
                "warnings differ (GNU R: {}; the engine: {})",
                or_none(reference),
                or_none(engine)
            ),
            Signature::MessagesDiffer => formatter.write_str("messages differ"),
            Signature::StdoutDiffers => formatter.write_str("stdout differs"),
            Signature::ValueDiffers => formatter.write_str("value differs"),
            Signature::ObservedDiffers(Some(difference)) => {
                write!(formatter, "observed differs ({})", difference.name())
            }
            Signature::ObservedDiffers(None) => formatter.write_str("observed differs"),
        }
    }
}

/// How `outcome` differs from GNU R's `reference`, or `None` when they agree. An engine that is
/// unavailable agrees, because it shows no bug.
pub fn signature(reference: &Outcome, outcome: &Result<Outcome, EngineError>) -> Option<Signature> {
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(EngineError::Unavailable(_)) => return None,
        Err(EngineError::Crashed(message)) => {
            return Some(Signature::Crashed(signal_name(message)));
        }
        // `ProcessEngine::run` reports a run it killed at the timeout this way.
        Err(EngineError::Failed(message)) if message.contains("was killed after") => {
            return Some(Signature::TimedOut);
        }
        Err(EngineError::Failed(_)) => return Some(Signature::Failed),
    };
    // The driver appends the call to a message as ` (in call)`.
    fn message(entry: Option<&String>) -> Option<String> {
        let entry = entry?;
        let text = match entry.rfind(" (in ") {
            Some(index) if entry.ends_with(')') => &entry[..index],
            _ => entry,
        };
        Some(text.to_string())
    }
    if reference.errors != outcome.errors {
        Some(Signature::ErrorsDiffer(
            message(reference.errors.first()),
            message(outcome.errors.first()),
        ))
    } else if reference.warnings != outcome.warnings {
        let index = reference
            .warnings
            .iter()
            .zip(&outcome.warnings)
            .take_while(|(first, second)| first == second)
            .count();
        Some(Signature::WarningsDiffer(
            message(reference.warnings.get(index)),
            message(outcome.warnings.get(index)),
        ))
    } else if reference.messages != outcome.messages {
        Some(Signature::MessagesDiffer)
    } else if without_addresses(&reference.stdout) != without_addresses(&outcome.stdout) {
        Some(Signature::StdoutDiffers)
    } else if reference.value != outcome.value {
        Some(Signature::ValueDiffers)
    } else if reference.observed != outcome.observed {
        Some(Signature::ObservedDiffers(
            differing_expressions(reference, outcome)
                .first()
                .map(classify),
        ))
    } else {
        None
    }
}

/// `text` with every address (`0x` and six or more hexadecimal digits) replaced by `0x`. An
/// address differs between two runs of one engine, so a reduction must not end at a program that
/// only prints one, such as `new.env()`.
fn without_addresses(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("0x") {
        let digits = rest[start + 2..]
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count();
        let kept = if digits >= 6 {
            start + 2
        } else {
            start + 2 + digits
        };
        result.push_str(&rest[..kept]);
        rest = &rest[start + 2 + digits..];
    }
    result.push_str(rest);
    result
}

/// A reduced program and what it took to find it.
#[derive(Debug, Clone)]
pub struct Reduction {
    pub program: String,
    pub signature: Signature,
    /// Candidates run under the engines, not counting the original.
    pub calls: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ReduceError {
    #[error("GNU R cannot run the program: {0}")]
    Reference(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0} agrees with GNU R on the program, so there is nothing to reduce")]
    Agrees(String),
    #[error("{0}")]
    Write(String),
}

/// Reduces `program`, on which `engine` diverges from GNU R (`reference`), to a smaller program on
/// which it diverges with the same [`Signature`], running at most `budget` candidates and none
/// after `deadline`. Candidates are written to `directory`.
///
/// GNU R must parse every candidate, and when GNU R ran the original to completion it must run
/// the candidate to completion too, so a crash does not reduce to a program that is wrong R.
pub fn reduce_divergence(
    program: &str,
    reference: &dyn Engine,
    engine: &dyn Engine,
    directory: &Path,
    budget: usize,
    deadline: Option<Instant>,
) -> Result<Reduction, ReduceError> {
    let path = directory.join("reduce.R");
    let write = |text: &str| {
        std::fs::write(&path, text)
            .map_err(|error| ReduceError::Write(format!("{}: {error}", path.display())))
    };
    write(program)?;
    let reference_outcome = reference
        .run(&path)
        .map_err(|error| ReduceError::Reference(error.to_string()))?;
    if let Some(error) = parse_error(&reference_outcome) {
        return Err(ReduceError::Reference(error.to_string()));
    }
    let outcome = engine.run(&path);
    if let Err(EngineError::Unavailable(reason)) = &outcome {
        return Err(ReduceError::Unavailable(reason.clone()));
    }
    let Some(wanted) = signature(&reference_outcome, &outcome) else {
        return Err(ReduceError::Agrees(engine.name()));
    };
    let completes = reference_outcome.errors.is_empty();
    // A crash, a hang or a lost report does not depend on what GNU R does, so the engine runs
    // first and GNU R only on a candidate that still shows it.
    let engine_first = matches!(
        wanted,
        Signature::Crashed(_) | Signature::TimedOut | Signature::Failed
    );
    let mut calls = 0;
    let mut write_failure = None;
    let reduced = reduce(program, budget, &mut |candidate| {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return false;
        }
        calls += 1;
        if let Err(error) = write(candidate) {
            write_failure.get_or_insert(error);
            return false;
        }
        let reference_runs = || match reference.run(&path) {
            Ok(outcome) if parse_error(&outcome).is_some() => None,
            Ok(outcome) if completes && !outcome.errors.is_empty() => None,
            Ok(outcome) => Some(outcome),
            Err(_) => None,
        };
        if engine_first {
            let outcome = engine.run(&path);
            signature(&Outcome::default(), &outcome).as_ref() == Some(&wanted)
                && reference_runs().is_some()
        } else {
            let Some(reference_outcome) = reference_runs() else {
                return false;
            };
            signature(&reference_outcome, &engine.run(&path)).as_ref() == Some(&wanted)
        }
    });
    if let Err(error) = std::fs::remove_file(&path) {
        eprintln!("could not remove {}: {error}", path.display());
    }
    if let Some(error) = write_failure {
        return Err(error);
    }
    Ok(Reduction {
        bytes_before: program.len(),
        bytes_after: reduced.len(),
        program: reduced,
        signature: wanted,
        calls,
    })
}

/// The error the driver reports when GNU R cannot parse the file.
fn parse_error(outcome: &Outcome) -> Option<&str> {
    outcome
        .errors
        .iter()
        .find(|error| error.starts_with("parse error:"))
        .map(String::as_str)
}

/// `signal: 11 (SIGSEGV)` out of the message of a crashed run, which embeds the exit status.
fn signal_name(message: &str) -> String {
    let Some(start) = message.find("signal: ") else {
        return "an unknown signal".to_string();
    };
    let rest = &message[start..];
    match rest.find(')') {
        Some(end) => rest[..=end].to_string(),
        None => rest
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

struct Reducer<'a> {
    current: String,
    calls: usize,
    budget: usize,
    interesting: &'a mut dyn FnMut(&str) -> bool,
}

/// What a candidate must gain over the current program before the predicate sees it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gain {
    /// Strictly fewer bytes.
    Shorter,
    /// No more bytes; the caller has checked that the candidate is smaller in another
    /// well-founded order, as a literal shrink from `8` to `4` is.
    NotLonger,
}

impl Reducer<'_> {
    fn spent(&self) -> bool {
        self.calls >= self.budget
    }

    fn root(&self) -> SyntaxNode {
        parse(&self.current).syntax_node()
    }

    /// Replaces `range` of the current program by `replacement` when the result qualifies.
    fn attempt(&mut self, range: TextRange, replacement: &str, gain: Gain) -> bool {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let candidate = format!(
            "{}{replacement}{}",
            &self.current[..start],
            &self.current[end..]
        );
        self.consider(candidate, gain)
    }

    fn consider(&mut self, candidate: String, gain: Gain) -> bool {
        let qualifies = match gain {
            Gain::Shorter => candidate.len() < self.current.len(),
            Gain::NotLonger => candidate.len() <= self.current.len(),
        };
        if self.spent()
            || !qualifies
            || candidate == self.current
            || !parse(&candidate).errors().is_empty()
        {
            return false;
        }
        self.calls += 1;
        if (self.interesting)(&candidate) {
            self.current = candidate;
            true
        } else {
            false
        }
    }
}

/// Tries the program without comments and blank lines, in one candidate.
fn strip_comments(reducer: &mut Reducer) {
    let root = reducer.root();
    let mut stripped = String::with_capacity(reducer.current.len());
    for element in root.descendants_with_tokens() {
        if let NodeOrToken::Token(token) = element
            && token.kind() != COMMENT
        {
            stripped.push_str(token.text());
        }
    }
    let mut lines: Vec<&str> = stripped
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    lines.push("");
    reducer.consider(lines.join("\n"), Gain::Shorter);
}

/// Delta debugging over the items of every list, outer lists first. A list keeps its index in
/// preorder while its own items shrink, because only lists after it in preorder can change.
fn remove_list_items(reducer: &mut Reducer) {
    let mut index = 0;
    while !reducer.spent() {
        let Some(count) = list_items(&reducer.root(), index).map(|items| items.len()) else {
            return;
        };
        let mut chunk = count;
        while chunk >= 1 && !reducer.spent() {
            let mut start = 0;
            while let Some(items) = list_items(&reducer.root(), index)
                && start < items.len()
                && !reducer.spent()
            {
                let end = (start + chunk).min(items.len());
                let range = removal_range(&items, start, end);
                if !reducer.attempt(range, "", Gain::Shorter) {
                    start += chunk;
                }
            }
            chunk /= 2;
        }
        index += 1;
    }
}

/// The ranges of the items of the `index`th list in preorder.
fn list_items(root: &SyntaxNode, index: usize) -> Option<Vec<TextRange>> {
    let list = root
        .descendants()
        .filter(|node| {
            matches!(
                node.kind(),
                SOURCE_FILE | BRACE_EXPR | ARGUMENT_LIST | PARAMETER_LIST
            )
        })
        .nth(index)?;
    let item_kind = |kind: SyntaxKind| match list.kind() {
        ARGUMENT_LIST => kind == ARGUMENT,
        PARAMETER_LIST => kind == PARAMETER,
        _ => is_expression_kind(kind),
    };
    Some(
        list.children()
            .filter(|child| item_kind(child.kind()))
            .map(|child| child.text_range())
            .collect(),
    )
}

/// The text to cut to remove items `start..end`: up to the next item, so the separator after
/// them goes too, or from the end of the item before them when they are the last.
fn removal_range(items: &[TextRange], start: usize, end: usize) -> TextRange {
    let last = items[end - 1].end();
    match (
        items.get(end),
        start.checked_sub(1).map(|before| items[before]),
    ) {
        (Some(next), _) => TextRange::new(items[start].start(), next.start()),
        (None, Some(before)) => TextRange::new(before.end(), last),
        (None, None) => TextRange::new(items[start].start(), last),
    }
}

/// Replaces each expression by one of its parts, retrying an expression after it changes.
fn hoist(reducer: &mut Reducer) {
    let mut index = 0;
    while !reducer.spent() {
        let Some(node) = expression(&reducer.root(), index) else {
            return;
        };
        let range = node.text_range();
        let parts: Vec<String> = hoistable_parts(&node)
            .iter()
            .map(|part| part.text().to_string())
            .collect();
        let hoisted = parts
            .iter()
            .any(|part| reducer.attempt(range, part, Gain::Shorter));
        if !hoisted {
            index += 1;
        }
    }
}

fn expression(root: &SyntaxNode, index: usize) -> Option<SyntaxNode> {
    root.descendants()
        .filter(|node| is_expression_kind(node.kind()))
        .nth(index)
}

/// The parts of `node` that can stand in for it: what a construct evaluates to in some case.
fn hoistable_parts(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let children: Vec<SyntaxNode> = node
        .children()
        .filter(|child| is_expression_kind(child.kind()))
        .collect();
    match node.kind() {
        IF_EXPR => children.into_iter().skip(1).collect(),
        FOR_EXPR | WHILE_EXPR | REPEAT_EXPR => children.into_iter().last().into_iter().collect(),
        PAREN_EXPR | UNARY_EXPR | DOLLAR_EXPR | AT_EXPR => children.into_iter().take(1).collect(),
        BRACE_EXPR if children.len() == 1 => children,
        BINARY_EXPR => match assignment_target(node) {
            Some(target) => children
                .into_iter()
                .filter(|child| child.text_range() != target.text_range())
                .collect(),
            None => children,
        },
        CALL_EXPR => {
            let mut parts = Vec::new();
            if let Some(body) = children.first().and_then(function_literal_body) {
                parts.push(body);
            }
            parts.extend(argument_values(node));
            parts
        }
        SUBSET_EXPR | SUBSET2_EXPR => {
            let mut parts: Vec<SyntaxNode> = children.into_iter().take(1).collect();
            parts.extend(argument_values(node));
            parts
        }
        _ => Vec::new(),
    }
}

/// The body of `function(...) body`, also inside parentheses, as the callee of a call.
fn function_literal_body(callee: &SyntaxNode) -> Option<SyntaxNode> {
    let mut node = callee.clone();
    while node.kind() == PAREN_EXPR {
        node = node
            .children()
            .find(|child| is_expression_kind(child.kind()))?;
    }
    if node.kind() != FUNCTION_DEF {
        return None;
    }
    node.children()
        .filter(|child| is_expression_kind(child.kind()))
        .last()
}

/// The value expressions of the arguments of a call or subscript, without their names.
fn argument_values(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let Some(arguments) = node.children().find(|child| child.kind() == ARGUMENT_LIST) else {
        return Vec::new();
    };
    arguments
        .children()
        .filter(|child| child.kind() == ARGUMENT)
        .filter_map(|argument| {
            let value_start = argument
                .children_with_tokens()
                .find(|element| element.kind() == EQ)
                .map_or(argument.text_range().start(), |equals| {
                    equals.text_range().end()
                });
            argument.children().find(|child| {
                is_expression_kind(child.kind()) && child.text_range().start() >= value_start
            })
        })
        .collect()
}

/// The side of an assignment that is assigned to, which no other expression can replace.
fn assignment_target(node: &SyntaxNode) -> Option<SyntaxNode> {
    let operator = node
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|token| !token.kind().is_trivia())?;
    let mut operands = node
        .children()
        .filter(|child| is_expression_kind(child.kind()));
    match operator.kind() {
        LESS_MINUS | LESS2_MINUS | EQ | COLON_EQ => operands.next(),
        MINUS_GREATER | MINUS_GREATER2 => operands.nth(1),
        _ => None,
    }
}

const LITERALS: [&str; 5] = ["NULL", "0", "1L", "\"\"", "TRUE"];

/// Replaces each compound expression by the first simple literal that keeps the program
/// interesting. Names and literals are left to the other passes: a name is short already.
fn replace_with_literals(reducer: &mut Reducer) {
    let mut index = 0;
    while !reducer.spent() {
        let Some(node) = expression(&reducer.root(), index) else {
            return;
        };
        index += 1;
        if matches!(node.kind(), NAME | LITERAL) || !replaceable(&node) {
            continue;
        }
        let range = node.text_range();
        for literal in LITERALS {
            if reducer.attempt(range, literal, Gain::Shorter) {
                break;
            }
        }
    }
}

/// Whether a value can stand where `node` is: not an assignment target or a called function.
fn replaceable(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let first_operand = || {
        parent
            .children()
            .find(|child| is_expression_kind(child.kind()))
            .is_some_and(|first| first.text_range() == node.text_range())
    };
    match parent.kind() {
        CALL_EXPR | FOR_EXPR => !first_operand(),
        BINARY_EXPR => {
            assignment_target(&parent).is_none_or(|target| target.text_range() != node.text_range())
        }
        _ => true,
    }
}

/// Shrinks string literals toward `""` and number literals toward 0, 1 and 2.
fn shrink_literals(reducer: &mut Reducer) {
    let mut index = 0;
    while !reducer.spent() {
        let root = reducer.root();
        let Some(token) = root
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|token| matches!(token.kind(), STRING | INTEGER | DOUBLE))
            .nth(index)
        else {
            return;
        };
        let range = token.text_range();
        let candidates = match token.kind() {
            STRING => smaller_strings(token.text()),
            _ => smaller_numbers(token.text()),
        };
        let shrunk = candidates
            .iter()
            .any(|candidate| reducer.attempt(range, candidate, Gain::NotLonger));
        if !shrunk {
            index += 1;
        }
    }
}

/// Shorter spellings of a quoted string: empty, either half, and one character less at
/// either end. A cut that splits an escape fails to parse and never reaches the predicate.
fn smaller_strings(literal: &str) -> Vec<String> {
    let Some(quote) = literal.chars().next() else {
        return Vec::new();
    };
    let Some(content) = literal
        .strip_prefix(quote)
        .and_then(|rest| rest.strip_suffix(quote))
    else {
        return Vec::new();
    };
    let characters: Vec<char> = content.chars().collect();
    let count = characters.len();
    if count == 0 {
        return Vec::new();
    }
    let half = count / 2;
    let slices = [
        (0, 0),
        (0, half),
        (count - half, count),
        (0, count - 1),
        (1, count),
    ];
    let mut candidates: Vec<String> = Vec::new();
    for (start, end) in slices {
        let text: String = characters[start..end].iter().collect();
        let candidate = format!("{quote}{text}{quote}");
        if candidate.len() < literal.len() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// Smaller numbers in the same type: 0, 1, 2 and half the value, each only when it is shorter
/// or, as long, of smaller magnitude, so repeated shrinking ends.
fn smaller_numbers(literal: &str) -> Vec<String> {
    let (digits, suffix) = match literal.strip_suffix('L') {
        Some(digits) => (digits, "L"),
        None => (literal, ""),
    };
    let Ok(value) = digits.parse::<f64>() else {
        return Vec::new();
    };
    let magnitude = value.abs();
    let half = (magnitude / 2.0).trunc();
    let mut candidates: Vec<String> = Vec::new();
    for number in [0.0, 1.0, 2.0, half] {
        let candidate = format!("{number}{suffix}");
        let smaller = candidate.len() < literal.len()
            || (candidate.len() == literal.len() && number < magnitude);
        if smaller && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reduced(program: &str, budget: usize, predicate: impl Fn(&str) -> bool) -> (String, usize) {
        let mut calls = 0;
        let text = reduce(program, budget, &mut |candidate| {
            calls += 1;
            predicate(candidate)
        });
        assert!(
            parse(&text).errors().is_empty(),
            "the reduced program does not parse:\n{text}"
        );
        (text, calls)
    }

    #[test]
    fn a_long_program_reduces_to_the_one_statement_that_matters() {
        let mut program = String::from("# a generated program\n");
        for line in 0..200 {
            if line == 117 {
                program.push_str("print(boom(x117, \"some text\"))\n");
            } else {
                program.push_str(&format!("x{line} <- c({line}, {line} + 1L) # note\n"));
            }
        }
        let (text, _) = reduced(&program, 10_000, |candidate| candidate.contains("boom("));
        assert_eq!(text, "boom()\n");
    }

    #[test]
    fn two_distant_statements_are_both_kept() {
        let mut program = String::new();
        for line in 0..120 {
            match line {
                10 => program.push_str("alpha <- 1\n"),
                100 => program.push_str("omega(alpha)\n"),
                _ => program.push_str(&format!("filler{line}(1, 2)\n")),
            }
        }
        let (text, _) = reduced(&program, 10_000, |candidate| {
            candidate.contains("alpha <- 1") && candidate.contains("omega(alpha)")
        });
        assert_eq!(text, "alpha <- 1\nomega(alpha)\n");
    }

    #[test]
    fn nested_blocks_reduce_from_the_outside_in() {
        let program = "f <- function(x) {\n  a <- 1\n  if (x > 0) {\n    b <- 2\n    for (i in 1:3) {\n      boom(i)\n      c <- 3\n    }\n    d <- 4\n  }\n  a\n}\nf(1)\n";
        let (text, _) = reduced(program, 10_000, |candidate| {
            candidate.contains("boom(") && candidate.contains("function")
        });
        assert_eq!(text, "function() boom()\n");
    }

    #[test]
    fn arguments_and_elements_are_dropped_one_at_a_time() {
        let program =
            "x <- list(a = 1, b = c(10, 20, 30, \"keep\"), d = 3)\nprint(x, digits = 3)\n";
        let (text, _) = reduced(program, 10_000, |candidate| {
            candidate.contains("\"keep\")") && candidate.contains("list(b = c(")
        });
        assert_eq!(text, "list(b = c(\"keep\"))\n");
    }

    #[test]
    fn literals_shrink_while_the_predicate_holds() {
        let program = "m <- matrix(0, 800L, 64)\nlabel <- \"a long label here\"\n";
        let (text, _) = reduced(program, 10_000, |candidate| {
            let tree = parse(candidate).syntax_node();
            let numbers: Vec<String> = tree
                .descendants_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .filter(|token| matches!(token.kind(), INTEGER | DOUBLE))
                .map(|token| token.text().to_string())
                .collect();
            candidate.contains("matrix(")
                && candidate.contains("label")
                && candidate.contains("here\"")
                && numbers.iter().any(|number| {
                    number
                        .trim_end_matches('L')
                        .parse::<f64>()
                        .is_ok_and(|value| value >= 5.0)
                })
        });
        assert_eq!(text, "matrix(8)\n\"label here\"\n");
    }

    #[test]
    fn hoisting_unwraps_calls_of_function_literals_and_conditionals() {
        let program = "(function(x) { if (TRUE) { y <- x; boom(y) } else stop(\"no\") })(42)\n";
        let (text, _) = reduced(program, 10_000, |candidate| candidate.contains("boom(y)"));
        assert_eq!(text, "boom(y)\n");
    }

    #[test]
    fn a_spent_budget_returns_a_valid_program() {
        let mut program = String::new();
        for line in 0..300 {
            program.push_str(&format!("f{line} <- function(a) {{ a + {line} }}\n"));
        }
        let (text, calls) = reduced(&program, 7, |candidate| candidate.contains("f150"));
        assert_eq!(calls, 7);
        assert!(text.contains("f150"));
        assert!(text.len() < program.len());
    }

    #[test]
    fn signatures_name_the_kind_of_difference() {
        let reference = Outcome {
            stdout: "[1] 1\n".to_string(),
            ..Outcome::default()
        };
        let same = Ok(reference.clone());
        assert_eq!(signature(&reference, &same), None);
        let printed = Ok(Outcome {
            stdout: "[1] NA\n".to_string(),
            ..Outcome::default()
        });
        assert_eq!(
            signature(&reference, &printed),
            Some(Signature::StdoutDiffers)
        );
        let printed_at = |address: &str| Outcome {
            stdout: format!("<environment: {address}>\n[1] 0x1F\n"),
            ..Outcome::default()
        };
        assert_eq!(
            signature(
                &printed_at("0x55d0c3a81f28"),
                &Ok(printed_at("0x7f1e2c004410"))
            ),
            None
        );
        let boom = Outcome {
            errors: vec!["boom".to_string()],
            ..Outcome::default()
        };
        let errored = Ok(boom.clone());
        assert_eq!(
            signature(&reference, &errored),
            Some(Signature::ErrorsDiffer(None, Some("boom".to_string())))
        );
        let other_error = Ok(Outcome {
            errors: vec!["bang (in f(1))".to_string()],
            ..Outcome::default()
        });
        assert_eq!(
            signature(&boom, &other_error),
            Some(Signature::ErrorsDiffer(
                Some("boom".to_string()),
                Some("bang".to_string())
            ))
        );
        let observed = |entry: &str| Outcome {
            observed: vec![entry.to_string()],
            ..reference.clone()
        };
        assert_eq!(
            signature(
                &observed("[1] visible double[1] 1:3ff0000000000000"),
                &Ok(observed("[1] visible integer[1] 1"))
            ),
            Some(Signature::ObservedDiffers(Some(Difference::Value)))
        );
        assert_eq!(
            signature(&reference, &Ok(observed("[1] visible integer[1] 1"))),
            Some(Signature::ObservedDiffers(None))
        );
        let crashed = Err(EngineError::Crashed(
            "ry-jit wrote no report (signal: 11 (SIGSEGV) (core dumped)): Segmentation fault"
                .to_string(),
        ));
        assert_eq!(
            signature(&reference, &crashed),
            Some(Signature::Crashed("signal: 11 (SIGSEGV)".to_string()))
        );
        let killed = Err(EngineError::Failed(
            "ry was killed after 5 seconds".to_string(),
        ));
        assert_eq!(signature(&reference, &killed), Some(Signature::TimedOut));
        let unavailable = Err(EngineError::Unavailable("no ry".to_string()));
        assert_eq!(signature(&reference, &unavailable), None);
    }
}
