//! Approximate matching: `agrep` and `agrepl`.
//!
//! A pattern, literal or a regular expression, compiles to a Thompson automaton, and the text
//! runs through it while every automaton state carries the cheapest edit cost that reaches it.
//! An edit is an insertion (a text character the pattern does not have), a deletion (a pattern
//! character the text lacks) or a substitution, each with its own cost, as in TRE's approximate
//! matcher that GNU R uses. The match may start and end anywhere in the text.
//!
//! The limits on single kinds of edits are checked against the cheapest path to each state, so
//! a text that only a costlier path could match within those limits is not found.

use crate::{
    arith::real_at,
    builtins::{BuiltinDef, Call, internal},
    conditions::warning,
    strings::{Syntax, flag, subject_at, translate},
    unwind::{R, error},
    values::{self, alloc, get_attr, length, set_attr, set_elt, symbols},
};
use heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, is_na_real};
use regex_syntax::hir::{Class, Hir, HirKind, Look};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("agrep", 8, |call| unsafe { agrep(call, false) }),
    internal("agrepl", 8, |call| unsafe { agrep(call, true) }),
];

/// `.Internal(agrep(pattern, x, ignore.case, value, costs, bounds, useBytes, fixed))`, and
/// `agrepl` with the same arguments.
unsafe fn agrep(call: &Call<'_>, logical: bool) -> R<Sexp> {
    let pattern = call.arg(0);
    let text = call.arg(1);
    let ignore_case = flag(call.arg(2));
    let value = flag(call.arg(3)) && !logical;
    let use_bytes = flag(call.arg(6));
    let fixed = flag(call.arg(7));
    if pattern.kind() != SexpType::String || length(pattern) < 1 {
        return Err(call.error("invalid 'pattern' argument"));
    }
    if length(pattern) > 1 {
        warning("argument 'pattern' has length > 1 and only the first element will be used")?;
    }
    if text.kind() != SexpType::String && text != values::nil() {
        return Err(call.error("invalid 'x' argument"));
    }
    let count = length(text);
    let Some(pattern) = subject_at(pattern, 0, use_bytes) else {
        return na_result(text, count, logical, value);
    };
    if pattern.is_empty() {
        return Err(call.error("'pattern' must be a non-empty character string"));
    }
    let expression = if fixed {
        regex_syntax::escape(&pattern)
    } else {
        let translated = translate(&pattern, Syntax::Extended);
        if let Some(problem) = translated.problem {
            return Err(call.error(format!(
                "invalid regular expression '{pattern}', reason '{problem}'"
            )));
        }
        translated.pattern
    };
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(ignore_case)
        .build()
        .parse(&expression)
        .map_err(|failure| {
            call.error(format!(
                "invalid regular expression '{pattern}', reason '{failure}'"
            ))
        })?;
    let automaton = Automaton::compile(&hir);
    let limits = Limits::new(call.arg(4), call.arg(5), pattern.chars().count())?;
    let mut hits = Vec::with_capacity(count);
    for index in 0..count {
        let found = match subject_at(text, index, use_bytes) {
            Some(element) => {
                let characters: Vec<char> = element.chars().collect();
                automaton.matches(&characters, &limits)
            }
            None => false,
        };
        hits.push(found);
    }
    if logical {
        let result = alloc(SexpType::Logical, count)?;
        for (index, &hit) in hits.iter().enumerate() {
            values::ints_mut(result)[index] = hit as i32;
        }
        return Ok(result);
    }
    let matched: Vec<usize> = (0..count).filter(|&index| hits[index]).collect();
    if value {
        let result = alloc(SexpType::String, matched.len())?;
        for (slot, &index) in matched.iter().enumerate() {
            set_elt(result, slot, text.element(index));
        }
        let names = get_attr(text, symbols().names);
        if names != values::nil() {
            let kept = alloc(SexpType::String, matched.len())?;
            for (slot, &index) in matched.iter().enumerate() {
                set_elt(kept, slot, names.element(index));
            }
            set_attr(result, symbols().names, kept)?;
        }
        return Ok(result);
    }
    let indices: Vec<i32> = matched.iter().map(|&index| index as i32 + 1).collect();
    values::integer_vector(&indices)
}

/// The result for an `NA` pattern: `NA` for every element, keeping the names when `value`.
unsafe fn na_result(text: Sexp, count: usize, logical: bool, value: bool) -> R<Sexp> {
    if value {
        let result = alloc(SexpType::String, count)?;
        for index in 0..count {
            set_elt(result, index, values::na_string());
        }
        let names = get_attr(text, symbols().names);
        if names != values::nil() {
            set_attr(result, symbols().names, names)?;
        }
        return Ok(result);
    }
    let result = alloc(
        if logical {
            SexpType::Logical
        } else {
            SexpType::Integer
        },
        count,
    )?;
    for index in 0..count {
        values::ints_mut(result)[index] = if logical { NA_LOGICAL } else { NA_INTEGER };
    }
    Ok(result)
}

/// The edit costs and the bounds a match must stay within.
struct Limits {
    insertion: u32,
    deletion: u32,
    substitution: u32,
    cost: u32,
    insertions: u32,
    deletions: u32,
    substitutions: u32,
    edits: u32,
}

impl Limits {
    /// Reads `costs` (insertion, deletion and substitution costs) and `bounds` (the maximal
    /// cost, insertions, deletions, substitutions and edits, `NA` for no bound). A bound below 1
    /// is a fraction of the pattern length, times the largest edit cost for the cost bound, and
    /// rounds up.
    unsafe fn new(costs: Sexp, bounds: Sexp, pattern_length: usize) -> R<Limits> {
        let cost_at = |index: usize| -> u32 {
            let cost = if index < length(costs) {
                real_at(costs, index)
            } else {
                1.0
            };
            if cost.is_nan() {
                1
            } else {
                cost.clamp(0.0, f64::from(u32::MAX)) as u32
            }
        };
        let (insertion, deletion, substitution) = (cost_at(0), cost_at(1), cost_at(2));
        let largest = insertion.max(deletion).max(substitution);
        let bound_at = |index: usize, scale: f64| -> R<u32> {
            if index >= length(bounds) {
                return Ok(u32::MAX);
            }
            let bound = real_at(bounds, index);
            if is_na_real(bound) || bound.is_nan() {
                return Ok(u32::MAX);
            }
            if bound < 0.0 {
                return Err(error("match distance components must be non-negative"));
            }
            let bound = if bound < 1.0 {
                (bound * scale).ceil()
            } else {
                bound.ceil()
            };
            Ok(if bound >= f64::from(u32::MAX) {
                u32::MAX
            } else {
                bound as u32
            })
        };
        let length = pattern_length as f64;
        Ok(Limits {
            insertion,
            deletion,
            substitution,
            cost: bound_at(0, length * f64::from(largest))?,
            insertions: bound_at(1, length)?,
            deletions: bound_at(2, length)?,
            substitutions: bound_at(3, length)?,
            edits: bound_at(4, length)?,
        })
    }
}

/// The edits on a path through the automaton.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Edits {
    cost: u32,
    insertions: u32,
    deletions: u32,
    substitutions: u32,
}

impl Edits {
    const NONE: Edits = Edits {
        cost: 0,
        insertions: 0,
        deletions: 0,
        substitutions: 0,
    };

    fn count(self) -> u32 {
        self.insertions + self.deletions + self.substitutions
    }

    fn better_than(self, other: Edits) -> bool {
        (self.cost, self.count()) < (other.cost, other.count())
    }

    fn insert(self, limits: &Limits) -> Option<Edits> {
        Edits {
            cost: self.cost.checked_add(limits.insertion)?,
            insertions: self.insertions + 1,
            ..self
        }
        .within(limits)
    }

    fn delete(self, limits: &Limits) -> Option<Edits> {
        Edits {
            cost: self.cost.checked_add(limits.deletion)?,
            deletions: self.deletions + 1,
            ..self
        }
        .within(limits)
    }

    fn substitute(self, limits: &Limits) -> Option<Edits> {
        Edits {
            cost: self.cost.checked_add(limits.substitution)?,
            substitutions: self.substitutions + 1,
            ..self
        }
        .within(limits)
    }

    fn within(self, limits: &Limits) -> Option<Edits> {
        (self.cost <= limits.cost
            && self.insertions <= limits.insertions
            && self.deletions <= limits.deletions
            && self.substitutions <= limits.substitutions
            && self.count() <= limits.edits)
            .then_some(self)
    }
}

/// A Thompson automaton; `start` is the entry state.
struct Automaton {
    states: Vec<State>,
    start: usize,
}

enum State {
    /// Consumes one character in `ranges`.
    Character {
        ranges: Vec<(char, char)>,
        next: usize,
    },
    Split(Vec<usize>),
    Look {
        look: Look,
        next: usize,
    },
    Match,
}

impl Automaton {
    fn compile(hir: &Hir) -> Automaton {
        let mut states = vec![State::Match];
        let start = compile_hir(hir, 0, &mut states);
        Automaton { states, start }
    }

    /// Whether some part of `text` matches within `limits`.
    fn matches(&self, text: &[char], limits: &Limits) -> bool {
        let mut current: Vec<Option<Edits>> = vec![None; self.states.len()];
        let mut worklist = Vec::new();
        for position in 0..=text.len() {
            // A match may start at any position.
            relax(&mut current, &mut worklist, self.start, Edits::NONE);
            while let Some(state) = worklist.pop() {
                let Some(edits) = current[state] else {
                    continue;
                };
                match &self.states[state] {
                    State::Match => return true,
                    State::Split(targets) => {
                        for &target in targets {
                            relax(&mut current, &mut worklist, target, edits);
                        }
                    }
                    State::Look { look, next } => {
                        if look_holds(*look, text, position) {
                            relax(&mut current, &mut worklist, *next, edits);
                        }
                    }
                    State::Character { next, .. } => {
                        if let Some(edits) = edits.delete(limits) {
                            relax(&mut current, &mut worklist, *next, edits);
                        }
                    }
                }
            }
            let Some(&character) = text.get(position) else {
                break;
            };
            let mut following: Vec<Option<Edits>> = vec![None; self.states.len()];
            for (state, edits) in current.iter().enumerate() {
                let Some(edits) = *edits else { continue };
                if let Some(inserted) = edits.insert(limits) {
                    relax(&mut following, &mut worklist, state, inserted);
                }
                if let State::Character { ranges, next } = &self.states[state] {
                    let hit = ranges
                        .iter()
                        .any(|&(low, high)| low <= character && character <= high);
                    let consumed = if hit {
                        Some(edits)
                    } else {
                        edits.substitute(limits)
                    };
                    if let Some(consumed) = consumed {
                        relax(&mut following, &mut worklist, *next, consumed);
                    }
                }
            }
            current = following;
        }
        false
    }
}

fn relax(states: &mut [Option<Edits>], worklist: &mut Vec<usize>, state: usize, edits: Edits) {
    let Some(slot) = states.get_mut(state) else {
        return;
    };
    if slot.is_none_or(|known| edits.better_than(known)) {
        *slot = Some(edits);
        worklist.push(state);
    }
}

/// Compiles `hir` into states that continue at `next`, and returns its entry state.
fn compile_hir(hir: &Hir, next: usize, states: &mut Vec<State>) -> usize {
    match hir.kind() {
        HirKind::Empty => next,
        HirKind::Literal(literal) => {
            let text = String::from_utf8_lossy(&literal.0);
            text.chars().rev().fold(next, |next, character| {
                push(
                    states,
                    State::Character {
                        ranges: vec![(character, character)],
                        next,
                    },
                )
            })
        }
        HirKind::Class(Class::Unicode(class)) => {
            let ranges = class
                .ranges()
                .iter()
                .map(|range| (range.start(), range.end()))
                .collect();
            push(states, State::Character { ranges, next })
        }
        HirKind::Class(Class::Bytes(class)) => {
            let ranges = class
                .ranges()
                .iter()
                .map(|range| (char::from(range.start()), char::from(range.end())))
                .collect();
            push(states, State::Character { ranges, next })
        }
        HirKind::Look(look) => push(states, State::Look { look: *look, next }),
        HirKind::Capture(capture) => compile_hir(&capture.sub, next, states),
        HirKind::Concat(parts) => parts
            .iter()
            .rev()
            .fold(next, |next, part| compile_hir(part, next, states)),
        HirKind::Alternation(branches) => {
            let entries = branches
                .iter()
                .map(|branch| compile_hir(branch, next, states))
                .collect();
            push(states, State::Split(entries))
        }
        HirKind::Repetition(repetition) => {
            let mut entry = match repetition.max {
                None => {
                    // A loop: the split either runs the body again or leaves.
                    let split = push(states, State::Split(Vec::new()));
                    let body = compile_hir(&repetition.sub, split, states);
                    if let Some(State::Split(targets)) = states.get_mut(split) {
                        *targets = vec![body, next];
                    }
                    split
                }
                Some(max) => {
                    let mut entry = next;
                    for _ in repetition.min..max {
                        let body = compile_hir(&repetition.sub, entry, states);
                        entry = push(states, State::Split(vec![body, next]));
                    }
                    entry
                }
            };
            // The required copies come first; with no upper bound, the last of them is the loop
            // body's first pass.
            let required = match repetition.max {
                None => repetition.min.saturating_sub(1),
                Some(_) => repetition.min,
            };
            if repetition.max.is_none() && repetition.min > 0 {
                let body = compile_hir(&repetition.sub, entry, states);
                entry = body;
            }
            for _ in 0..required {
                entry = compile_hir(&repetition.sub, entry, states);
            }
            entry
        }
    }
}

fn push(states: &mut Vec<State>, state: State) -> usize {
    states.push(state);
    states.len() - 1
}

fn look_holds(look: Look, text: &[char], position: usize) -> bool {
    let before = position
        .checked_sub(1)
        .and_then(|index| text.get(index))
        .copied();
    let after = text.get(position).copied();
    let word = |character: Option<char>| {
        character.is_some_and(|character| character.is_alphanumeric() || character == '_')
    };
    match look {
        Look::Start => position == 0,
        Look::End => position == text.len(),
        Look::StartLF | Look::StartCRLF => before.is_none_or(|character| character == '\n'),
        Look::EndLF | Look::EndCRLF => after.is_none_or(|character| character == '\n'),
        Look::WordAscii | Look::WordUnicode => word(before) != word(after),
        Look::WordAsciiNegate | Look::WordUnicodeNegate => word(before) == word(after),
        Look::WordStartAscii | Look::WordStartUnicode => !word(before) && word(after),
        Look::WordEndAscii | Look::WordEndUnicode => word(before) && !word(after),
        _ => true,
    }
}
