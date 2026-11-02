//! Approximate matching: `agrep`, `agrepl` and `adist`.
//!
//! A pattern, literal or a regular expression, compiles to a Thompson automaton, and the text
//! runs through it while every automaton state carries the cheapest edit cost that reaches it.
//! An edit is an insertion (a text character the pattern does not have), a deletion (a pattern
//! character the text lacks) or a substitution, each with its own cost, as in TRE's approximate
//! matcher that GNU R uses. The match may start and end anywhere in the text.
//!
//! The limits on single kinds of edits are checked against the cheapest path to each state, so
//! a text that only a costlier path could match within those limits is not found.

use {
    crate::{
        arith::real_at,
        builtins::{BuiltinDef, Call, internal},
        conditions::warning,
        strings::{Syntax, flag, subject_at, translate},
        unwind::{R, error},
        values::{self, alloc, get_attr, length, set_attr, set_elt, symbols},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, is_na_real},
    regex_syntax::hir::{Class, Hir, HirKind, Look},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("agrep", 8, |call| unsafe { agrep(call, false) }),
    internal("agrepl", 8, |call| unsafe { agrep(call, true) }),
    internal("adist", 8, |call| unsafe { adist(call) }),
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

/// `.Internal(adist(x, y, costs, counts, fixed, partial, ignore.case, useBytes))`.
///
/// For a whole-string literal comparison, the R wrapper passes `x` and `y` as lists of
/// character codes and the distances come from a weighted edit-distance table, which also
/// yields the transformation (`trafos`). Otherwise `x` holds patterns, literal when `fixed`,
/// and the distance is that of the cheapest approximate match, of a substring of `y` when
/// `partial`, which also yields its offsets.
unsafe fn adist(call: &Call<'_>) -> R<Sexp> {
    let (x, y, costs) = (call.arg(0), call.arg(1), call.arg(2));
    let counts = flag(call.arg(3));
    let fixed = flag(call.arg(4));
    let partial = flag(call.arg(5));
    let (rows, columns) = (length(x), length(y));
    let cells = rows * columns;
    let distances = alloc(SexpType::Real, cells)?;
    let edit_counts = alloc(SexpType::Integer, cells * 3)?;
    let cost_at = |index: usize| -> f64 {
        if index < length(costs) {
            real_at(costs, index)
        } else {
            1.0
        }
    };
    let mut extra = Vec::new();
    if x.kind() == SexpType::List {
        let (insertion, deletion, substitution) = (cost_at(0), cost_at(1), cost_at(2));
        let trafos = alloc(SexpType::String, cells)?;
        let codes = |list: Sexp, index: usize| -> Option<Vec<i32>> {
            let element = list.element(index);
            let codes = match element.kind() {
                SexpType::Integer => values::ints(element).to_vec(),
                SexpType::Real => values::reals(element)
                    .iter()
                    .map(|&code| code as i32)
                    .collect(),
                _ => Vec::new(),
            };
            (!codes.contains(&NA_INTEGER)).then_some(codes)
        };
        for row in 0..rows {
            let source = codes(x, row);
            for column in 0..columns {
                let cell = row + column * rows;
                let (Some(source), Some(target)) = (&source, codes(y, column)) else {
                    values::reals_mut(distances)[cell] = values::na_real();
                    // GNU R leaves the insertion count at 0 for a missing string.
                    values::ints_mut(edit_counts)[cell] = 0;
                    for kind in 1..3 {
                        values::ints_mut(edit_counts)[cell + kind * cells] = NA_INTEGER;
                    }
                    set_elt(trafos, cell, values::mkchar("")?);
                    continue;
                };
                let alignment = align(source, &target, insertion, deletion, substitution);
                values::reals_mut(distances)[cell] = alignment.distance;
                let tally = [b'I', b'D', b'S'].map(|operation| {
                    alignment
                        .operations
                        .iter()
                        .filter(|&&op| op == operation)
                        .count()
                });
                for (kind, count) in tally.iter().enumerate() {
                    values::ints_mut(edit_counts)[cell + kind * cells] = *count as i32;
                }
                let text = String::from_utf8_lossy(&alignment.operations);
                set_elt(trafos, cell, values::mkchar(&text)?);
            }
        }
        if counts {
            extra.push(("trafos", trafos, false));
        }
    } else {
        let ignore_case = flag(call.arg(6));
        let use_bytes = flag(call.arg(7));
        let to_cost = |cost: f64| -> u32 {
            if cost.is_nan() {
                1
            } else {
                cost.clamp(0.0, f64::from(u32::MAX)) as u32
            }
        };
        let limits = Limits {
            insertion: to_cost(cost_at(0)),
            deletion: to_cost(cost_at(1)),
            substitution: to_cost(cost_at(2)),
            cost: u32::MAX,
            insertions: u32::MAX,
            deletions: u32::MAX,
            substitutions: u32::MAX,
            edits: u32::MAX,
        };
        let offsets = alloc(SexpType::Integer, cells * 2)?;
        for row in 0..rows {
            let automaton = match subject_at(x, row, use_bytes) {
                Some(pattern) => {
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
                    Some(Automaton::compile(&hir))
                }
                None => None,
            };
            for column in 0..columns {
                let cell = row + column * rows;
                let found = match (&automaton, subject_at(y, column, use_bytes)) {
                    (Some(automaton), Some(text)) => {
                        let characters: Vec<char> = text.chars().collect();
                        automaton.best_match(&characters, &limits, !partial)
                    }
                    _ => None,
                };
                let entries = match found {
                    Some((edits, start, end)) => {
                        values::reals_mut(distances)[cell] = f64::from(edits.cost);
                        [
                            edits.insertions as i32,
                            edits.deletions as i32,
                            edits.substitutions as i32,
                            start as i32 + 1,
                            end as i32,
                        ]
                    }
                    None => {
                        values::reals_mut(distances)[cell] = values::na_real();
                        [NA_INTEGER, NA_INTEGER, NA_INTEGER, -1, -1]
                    }
                };
                for kind in 0..3 {
                    values::ints_mut(edit_counts)[cell + kind * cells] = entries[kind];
                }
                values::ints_mut(offsets)[cell] = entries[3];
                values::ints_mut(offsets)[cell + cells] = entries[4];
            }
        }
        if counts {
            extra.push(("offsets", offsets, true));
        }
    }
    let row_names = get_attr(x, symbols().names);
    let column_names = get_attr(y, symbols().names);
    let nil = values::nil();
    let matrix_dim = values::integer_vector(&[rows as i32, columns as i32])?;
    let matrix_dimnames = if row_names == nil && column_names == nil {
        nil
    } else {
        values::list_vector(&[row_names, column_names])?
    };
    let shape = |object: Sexp, layers: &[&str]| -> R<()> {
        if layers.is_empty() {
            set_attr(object, symbols().dim, matrix_dim)?;
            if matrix_dimnames != nil {
                set_attr(object, symbols().dimnames, matrix_dimnames)?;
            }
            return Ok(());
        }
        let dim = values::integer_vector(&[rows as i32, columns as i32, layers.len() as i32])?;
        set_attr(object, symbols().dim, dim)?;
        let dimnames =
            values::list_vector(&[row_names, column_names, values::string_vector(layers)?])?;
        set_attr(object, symbols().dimnames, dimnames)
    };
    shape(distances, &[])?;
    if counts {
        shape(edit_counts, &["ins", "del", "sub"])?;
        set_attr(distances, values::install("counts")?, edit_counts)?;
        for (name, object, layered) in extra {
            shape(object, if layered { &["first", "last"] } else { &[] })?;
            set_attr(distances, values::install(name)?, object)?;
        }
    }
    Ok(distances)
}

/// A cheapest transformation of one sequence into another.
struct Alignment {
    distance: f64,
    /// `M`atch, `I`nsertion of a target element, `D`eletion of a source element, or
    /// `S`ubstitution, in order.
    operations: Vec<u8>,
}

/// The weighted edit distance from `source` to `target`. Among equally cheap
/// transformations, the traceback from the end prefers a match, then an insertion, then a
/// deletion, then a substitution, which is the transformation GNU R reports.
fn align(
    source: &[i32],
    target: &[i32],
    insertion: f64,
    deletion: f64,
    substitution: f64,
) -> Alignment {
    let width = target.len() + 1;
    let mut table = vec![0.0; (source.len() + 1) * width];
    for (row, source_code) in std::iter::once(None)
        .chain(source.iter().map(Some))
        .enumerate()
    {
        for (column, target_code) in std::iter::once(None)
            .chain(target.iter().map(Some))
            .enumerate()
        {
            table[row * width + column] = match (source_code, target_code) {
                (None, None) => 0.0,
                (None, Some(_)) => table[column - 1] + insertion,
                (Some(_), None) => table[(row - 1) * width] + deletion,
                (Some(source_code), Some(target_code)) => {
                    let diagonal = table[(row - 1) * width + column - 1]
                        + if source_code == target_code {
                            0.0
                        } else {
                            substitution
                        };
                    let inserted = table[row * width + column - 1] + insertion;
                    let deleted = table[(row - 1) * width + column] + deletion;
                    diagonal.min(inserted).min(deleted)
                }
            };
        }
    }
    let (mut row, mut column) = (source.len(), target.len());
    let distance = table[row * width + column];
    let mut operations = Vec::with_capacity(row.max(column));
    while row > 0 || column > 0 {
        let here = table[row * width + column];
        let matched = row > 0
            && column > 0
            && source[row - 1] == target[column - 1]
            && table[(row - 1) * width + column - 1] == here;
        let operation = if matched {
            b'M'
        } else if column > 0 && table[row * width + column - 1] + insertion == here {
            b'I'
        } else if row > 0 && table[(row - 1) * width + column] + deletion == here {
            b'D'
        } else if row > 0 && column > 0 {
            b'S'
        } else if column > 0 {
            b'I'
        } else {
            b'D'
        };
        if matches!(operation, b'M' | b'S' | b'D') {
            row -= 1;
        }
        if matches!(operation, b'M' | b'S' | b'I') {
            column -= 1;
        }
        operations.push(operation);
    }
    operations.reverse();
    Alignment {
        distance,
        operations,
    }
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
    /// The text position the path started at, so a best match can report its offsets.
    start: usize,
}

impl Edits {
    const NONE: Edits = Edits {
        cost: 0,
        insertions: 0,
        deletions: 0,
        substitutions: 0,
        start: 0,
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
        self.run(text, limits, false, |_, _| true)
    }

    /// The cheapest match within `limits` and the text range it covers. Among equally cheap
    /// matches, fewer insertions and deletions win, and then the one that ends first. When
    /// `anchored`, the match must cover the whole text.
    fn best_match(
        &self,
        text: &[char],
        limits: &Limits,
        anchored: bool,
    ) -> Option<(Edits, usize, usize)> {
        let mut best: Option<(Edits, usize, usize)> = None;
        let rank = |edits: Edits| (edits.cost, edits.insertions + edits.deletions);
        self.run(text, limits, anchored, |edits, end| {
            if best.is_none_or(|(known, _, _)| rank(edits) < rank(known)) {
                best = Some((edits, edits.start, end));
            }
            false
        });
        best
    }

    /// Runs `text` through the automaton, calling `on_match` with the edits and the end
    /// position of every match reached, until it returns true. Returns whether it did.
    fn run(
        &self,
        text: &[char],
        limits: &Limits,
        anchored: bool,
        mut on_match: impl FnMut(Edits, usize) -> bool,
    ) -> bool {
        let mut current: Vec<Option<Edits>> = vec![None; self.states.len()];
        let mut worklist = Vec::new();
        for position in 0..=text.len() {
            if !anchored || position == 0 {
                let start = Edits {
                    start: position,
                    ..Edits::NONE
                };
                relax(&mut current, &mut worklist, self.start, start);
            }
            while let Some(state) = worklist.pop() {
                let Some(edits) = current[state] else {
                    continue;
                };
                match &self.states[state] {
                    State::Match => {
                        if (!anchored || position == text.len()) && on_match(edits, position) {
                            return true;
                        }
                    }
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
