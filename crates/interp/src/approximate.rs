//! Approximate matching: `agrep`, `agrepl` and `adist`, and the approximate regular expressions
//! of TRE, whose bounds such as `{+}` or `{~1}` allow edits in one part of a pattern
//! ([`ApproximateRegex`]).
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
        strings::{flag, subject_at, translate},
        unwind::{R, error},
        values::{self, alloc, get_attr, length, set_attr, set_elt, symbols},
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, is_na_real},
    regex_syntax::{
        ParserBuilder,
        hir::{Class, Hir, HirKind, Look},
    },
    rustc_hash::FxHashSet,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("agrep", 8, |call| unsafe { agrep(call, false) }),
    internal("agrepl", 8, |call| unsafe { agrep(call, true) }),
    internal("adist", 8, adist),
    internal("aregexec", 7, aregexec),
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
        let translated = translate(&pattern);
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
                let tally = b"IDS".map(|operation| {
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
                        let translated = translate(&pattern);
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
                for (kind, &count) in entries.iter().take(3).enumerate() {
                    values::ints_mut(edit_counts)[cell + kind * cells] = count;
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

/// `.Internal(aregexec(pattern, text, bounds, costs, ignore.case, fixed, useBytes))`: for each
/// text, the start and the length of the best approximate match and of each group in it, as
/// TRE's `regaexec` finds them with `bounds` and `costs` applying to the whole pattern. A group
/// the match leaves unset starts at 0 with length 0, and a text without a match gets -1.
unsafe fn aregexec(call: &Call<'_>) -> R<Sexp> {
    let pattern = call.arg(0);
    let text = call.arg(1);
    let mut ignore_case = flag(call.arg(4));
    let fixed = flag(call.arg(5));
    let use_bytes = flag(call.arg(6));
    if fixed && ignore_case {
        warning("argument 'ignore.case = TRUE' will be ignored")?;
        ignore_case = false;
    }
    if pattern.kind() != SexpType::String || length(pattern) < 1 {
        return Err(call.error("invalid 'pattern' argument"));
    }
    let Some(pattern_text) = subject_at(pattern, 0, use_bytes) else {
        return Err(call.error("invalid 'pattern' argument"));
    };
    if length(pattern) > 1 {
        warning("argument 'pattern' has length > 1 and only the first element will be used")?;
    }
    if text.kind() != SexpType::String {
        return Err(call.error("invalid 'text' argument"));
    }
    if pattern_text.is_empty() {
        return Err(call.error("'pattern' must be a non-empty character string"));
    }
    let (expression, scopes) = if fixed {
        (regex_syntax::escape(&pattern_text), Vec::new())
    } else {
        let translated = translate(&pattern_text);
        if let Some(problem) = translated.problem {
            return Err(call.error(format!("regcomp error: '{problem}'")));
        }
        (translated.pattern, translated.scopes)
    };
    let regex = ApproximateRegex::compile(&expression, &scopes, ignore_case)
        .map_err(|reason| call.error(format!("regcomp error: '{reason}'")))?;
    let (bounds, costs) = (call.arg(2), call.arg(3));
    if costs.kind() != SexpType::Integer || length(costs) < 3 || length(bounds) < 5 {
        return Err(error("invalid 'costs' or 'bounds' argument"));
    }
    let bounds: Vec<f64> = (0..5).map(|index| real_at(bounds, index)).collect();
    let params = whole_pattern_params(&bounds, values::ints(costs), pattern_text.chars().count());
    let match_length = values::install("match.length")?;
    let count = length(text);
    let result = alloc(SexpType::List, count)?;
    crate::heap().root(result);
    let mut characters: Vec<char> = Vec::new();
    for index in 0..count {
        let charsxp = text.element(index);
        let (starts, lengths) = if charsxp == values::na_string() {
            (vec![NA_INTEGER], vec![NA_INTEGER])
        } else {
            characters.clear();
            if use_bytes {
                characters.extend(charsxp.bytes().iter().map(|&byte| char::from(byte)));
            } else {
                characters.extend(values::char_str(charsxp).chars());
            }
            match regex.search_with(&characters, false, &params)? {
                Some((start, end, groups)) => std::iter::once(Some((start, end)))
                    .chain(groups)
                    .map(|span| match span {
                        Some((start, end)) => (start as i32 + 1, (end - start) as i32),
                        None => (0, 0),
                    })
                    .unzip(),
                None => (vec![-1], vec![-1]),
            }
        };
        let found = starts.len() > 1 || starts[0] > 0;
        let positions = values::integer_vector(&starts)?;
        set_elt(result, index, positions);
        set_attr(positions, match_length, values::integer_vector(&lengths)?)?;
        if use_bytes && found {
            set_attr(
                positions,
                values::install("useBytes")?,
                values::scalar_bool(true)?,
            )?;
        }
    }
    Ok(result)
}

/// GNU R's `amatch_regaparams`: the edit costs, and the bounds of `.amatch_bounds` (the cost,
/// insertions, deletions, substitutions and all edits, `NA` for none), a bound below 1 being a
/// fraction of the pattern length (times the largest cost, for the cost) rounded up. GNU R reads
/// the insertion bound as TRE's bound on deletions and the deletion bound as its bound on
/// insertions, and so does this.
fn whole_pattern_params(bounds: &[f64], costs: &[i32], pattern_length: usize) -> Params {
    let cost_at = |index: usize| costs[index];
    let (insertion, deletion, substitution) = (cost_at(0), cost_at(1), cost_at(2));
    let largest = f64::from(insertion.max(deletion).max(substitution));
    let length = pattern_length as f64;
    let bound_at = |index: usize, scale: f64| -> i32 {
        let bound = bounds[index];
        if is_na_real(bound) {
            return NO_BOUND;
        }
        let bound = if bound < 1.0 { bound * scale } else { bound };
        bound.ceil().clamp(f64::from(i32::MIN), f64::from(NO_BOUND)) as i32
    };
    Params {
        insertion_cost: Some(insertion),
        deletion_cost: Some(deletion),
        substitution_cost: Some(substitution),
        cost_limit: bound_at(0, length * largest),
        deletions: bound_at(1, length),
        insertions: bound_at(2, length),
        substitutions: bound_at(3, length),
        edits: bound_at(4, length),
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
    /// Records the text position in capture slot `slot`.
    Save {
        slot: usize,
        next: usize,
    },
    /// Enters or leaves approximate scope `scope` of a TRE pattern.
    Settings {
        scope: usize,
        enter: bool,
        next: usize,
    },
    Match,
}

impl Automaton {
    fn compile(hir: &Hir) -> Automaton {
        let mut states = vec![State::Match];
        let start = compile_hir(hir, 0, &mut states, &capture_roles(hir));
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
                    State::Save { next, .. } | State::Settings { next, .. } => {
                        relax(&mut current, &mut worklist, *next, edits);
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
fn compile_hir(hir: &Hir, next: usize, states: &mut Vec<State>, roles: &[Option<Role>]) -> usize {
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
        HirKind::Capture(capture) => match roles.get(capture.index as usize).copied().flatten() {
            Some(Role::Group(group)) => {
                let close = push(
                    states,
                    State::Save {
                        slot: 2 * group + 1,
                        next,
                    },
                );
                let body = compile_hir(&capture.sub, close, states, roles);
                push(
                    states,
                    State::Save {
                        slot: 2 * group,
                        next: body,
                    },
                )
            }
            Some(Role::Scope(scope)) => {
                let leave = push(
                    states,
                    State::Settings {
                        scope,
                        enter: false,
                        next,
                    },
                );
                let body = compile_hir(&capture.sub, leave, states, roles);
                push(
                    states,
                    State::Settings {
                        scope,
                        enter: true,
                        next: body,
                    },
                )
            }
            None => compile_hir(&capture.sub, next, states, roles),
        },
        HirKind::Concat(parts) => parts
            .iter()
            .rev()
            .fold(next, |next, part| compile_hir(part, next, states, roles)),
        HirKind::Alternation(branches) => {
            let entries = branches
                .iter()
                .map(|branch| compile_hir(branch, next, states, roles))
                .collect();
            push(states, State::Split(entries))
        }
        HirKind::Repetition(repetition) => {
            // The copies follow TRE's expansion, because which positions a path occupies decides
            // which match starts it blocks: `x*` and `x+` are one copy of `x` with a loop, `x{m,}`
            // is `m` copies followed by `x*`, and `x{m,n}` is `m` copies followed by nested
            // optional ones.
            let (mut entry, required) = match repetition.max {
                None => {
                    let split = push(states, State::Split(Vec::new()));
                    let body = compile_hir(&repetition.sub, split, states, roles);
                    if let Some(State::Split(targets)) = states.get_mut(split) {
                        *targets = vec![body, next];
                    }
                    match repetition.min {
                        1 => (body, 0),
                        min => (split, min),
                    }
                }
                Some(max) => {
                    let mut entry = next;
                    for _ in repetition.min..max {
                        let body = compile_hir(&repetition.sub, entry, states, roles);
                        entry = push(states, State::Split(vec![body, next]));
                    }
                    (entry, repetition.min)
                }
            };
            for _ in 0..required {
                entry = compile_hir(&repetition.sub, entry, states, roles);
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

/// What a capture group of a translated pattern stands for.
#[derive(Clone, Copy)]
enum Role {
    /// A capture group of the R pattern, numbered from 0.
    Group(usize),
    /// An approximate scope, which the translation writes as a group named
    /// `tre_approximate_<scope>`.
    Scope(usize),
}

/// The name prefix of the groups that mark approximate scopes in a translated pattern.
pub(crate) const SCOPE_GROUP: &str = "tre_approximate_";

/// The role of each capture group of `hir`, by capture index.
fn capture_roles(hir: &Hir) -> Vec<Option<Role>> {
    fn collect(hir: &Hir, captures: &mut Vec<(u32, Option<usize>)>) {
        match hir.kind() {
            HirKind::Capture(capture) => {
                let scope = capture
                    .name
                    .as_deref()
                    .and_then(|name| name.strip_prefix(SCOPE_GROUP))
                    .and_then(|scope| scope.parse().ok());
                captures.push((capture.index, scope));
                collect(&capture.sub, captures);
            }
            HirKind::Repetition(repetition) => collect(&repetition.sub, captures),
            HirKind::Concat(parts) | HirKind::Alternation(parts) => {
                for part in parts {
                    collect(part, captures);
                }
            }
            _ => {}
        }
    }
    let mut captures = Vec::new();
    collect(hir, &mut captures);
    captures.sort_unstable_by_key(|&(index, _)| index);
    let mut roles = vec![None; captures.last().map_or(0, |&(index, _)| index as usize + 1)];
    let mut groups = 0;
    for (index, scope) in captures {
        roles[index as usize] = Some(match scope {
            Some(scope) => Role::Scope(scope),
            None => {
                groups += 1;
                Role::Group(groups - 1)
            }
        });
    }
    roles
}

/// The settings of one approximate bound, as TRE's `tre_parse_bound` leaves them: `None` is a
/// setting the bound leaves unset, so that the enclosing settings stay in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApproximateSettings {
    pub(crate) insertion_cost: Option<i64>,
    pub(crate) deletion_cost: Option<i64>,
    pub(crate) substitution_cost: Option<i64>,
    pub(crate) cost_limit: i64,
    pub(crate) insertions: Option<i64>,
    pub(crate) deletions: Option<i64>,
    pub(crate) substitutions: Option<i64>,
    pub(crate) edits: i64,
}

/// A pattern with approximate scopes, matched as TRE's approximate matcher
/// (`tre_tnfa_run_approx`) does, because R code relies on its exact results: `^a{+}$` allows
/// insertions only before the `a`, so it matches `"ba"` but not `"ab"`.
///
/// TRE matches over positions, each of which consumes one pattern character, and the
/// transitions between them carry the assertions, capture tags and settings met on the way. The
/// pattern is built here as a Thompson automaton and then folded into the same shape: a
/// position per character state plus the final position, with the epsilon paths between them
/// as transitions.
///
/// Two of TRE's accidents are not reproduced. Nested scopes combine their settings in path
/// order, where TRE can read uninitialized memory, and ties between equally costly paths follow
/// the POSIX subexpression rule over the capture slots, where TRE compares the tags it places.
pub(crate) struct ApproximateRegex {
    positions: Vec<Position>,
    initial: Vec<Transition>,
    final_position: usize,
    group_count: usize,
    /// Whether some `{...}` scope sets costs or limits, so that paths count costs per scope
    /// depth as well as in total.
    scoped: bool,
}

/// A match: its character range and the character range of each capture group.
pub(crate) type ApproximateMatch = (usize, usize, Vec<Option<(usize, usize)>>);

struct Position {
    /// The characters this position consumes; the final position consumes none.
    ranges: Vec<(char, char)>,
    transitions: Vec<Transition>,
}

#[derive(Clone)]
struct Transition {
    target: usize,
    /// The assertions on the way, as a set of `ASSERT_*` bits.
    assertions: u8,
    /// The capture slots set to the position where the transition is taken.
    saves: Vec<usize>,
    /// The settings met on the way, combined; `None` when there were none.
    settings: Option<[Setting; SETTING_COUNT]>,
}

/// One entry of TRE's parameter array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    Unset,
    Default,
    Value(i32),
}

const COST_INSERTION: usize = 0;
const COST_DELETION: usize = 1;
const COST_SUBSTITUTION: usize = 2;
const COST_LIMIT: usize = 3;
const MOST_INSERTIONS: usize = 4;
const MOST_DELETIONS: usize = 5;
const MOST_SUBSTITUTIONS: usize = 6;
const MOST_EDITS: usize = 7;
const DEPTH: usize = 8;
const SETTING_COUNT: usize = 9;

const ASSERT_LINE_START: u8 = 1;
const ASSERT_LINE_END: u8 = 2;
const ASSERT_WORD_START: u8 = 4;
const ASSERT_WORD_END: u8 = 8;
const ASSERT_WORD_BOUNDARY: u8 = 16;
const ASSERT_NOT_WORD_BOUNDARY: u8 = 32;

/// TRE's `INT_MAX`, the bound that stands for no bound.
pub(crate) const UNBOUNDED: i64 = NO_BOUND as i64;

const NO_BOUND: i32 = i32::MAX;

/// The deepest nesting of settings TRE's matcher has room for.
const MOST_DEPTH: usize = 3;

/// Indices into a level of [`Reach::costs`].
const COST: usize = 0;
const INSERTIONS: usize = 1;
const DELETIONS: usize = 2;
const SUBSTITUTIONS: usize = 3;

/// The settings in force on a path: TRE's `regaparams_t`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Params {
    insertion_cost: Option<i32>,
    deletion_cost: Option<i32>,
    substitution_cost: Option<i32>,
    cost_limit: i32,
    insertions: i32,
    deletions: i32,
    substitutions: i32,
    edits: i32,
}

/// The cost of an edit when nothing sets it.
const DEFAULT_COST: i32 = 1;

/// What `regexec` passes to TRE's approximate matcher: unit costs and no edits at all, so
/// that edits happen only inside a scope that allows them. An edit whose cost a scope leaves
/// unset is free inside the scope, but still costs [`DEFAULT_COST`] toward the whole match.
const DEFAULT_PARAMS: Params = Params {
    insertion_cost: Some(DEFAULT_COST),
    deletion_cost: Some(DEFAULT_COST),
    substitution_cost: Some(DEFAULT_COST),
    cost_limit: 0,
    insertions: NO_BOUND,
    deletions: NO_BOUND,
    substitutions: NO_BOUND,
    edits: 0,
};

/// The cheapest known path to a position, at one text position: TRE's
/// `tre_tnfa_approx_reach_t` without the capture slots, which live beside it. Every path step
/// copies one, so it is kept small: the settings are an index into the search's table of them,
/// a pattern without `{...}` scopes has one cost level (`LEVELS`), and no field stores the count
/// of all edits, which is the sum of the three kinds.
#[derive(Clone, Copy)]
struct Reach<const LEVELS: usize> {
    /// The text position the entry belongs to; older entries are stale.
    at: i32,
    /// Where the match on this path starts.
    start: i32,
    /// The settings in force, as an index into the search's [`ParamsTable`].
    params: u32,
    depth: u8,
    /// The cost and the edit counts within each nesting level of settings, with the totals at
    /// level 0.
    costs: [[i32; 4]; LEVELS],
}

impl<const LEVELS: usize> Reach<LEVELS> {
    const UNREACHED: Self = Reach {
        at: -2,
        start: -1,
        params: 0,
        depth: 0,
        costs: [[0; 4]; LEVELS],
    };
}

/// The distinct settings the paths of one search have met, the whole pattern's first. Scopes
/// combine into few distinct settings, so a path refers to them by index.
struct ParamsTable(Vec<Params>);

impl ParamsTable {
    fn index_of(&mut self, params: Params) -> u32 {
        let index = match self.0.iter().position(|known| *known == params) {
            Some(index) => index,
            None => {
                self.0.push(params);
                self.0.len() - 1
            }
        };
        index as u32
    }
}

/// The number of edits of all kinds a cost level counts.
fn edits(level: [i32; 4]) -> i32 {
    level[INSERTIONS] + level[DELETIONS] + level[SUBSTITUTIONS]
}

impl ApproximateRegex {
    /// Compiles a translated pattern whose approximate scopes have `scopes` as their settings.
    pub(crate) fn compile(
        pattern: &str,
        scopes: &[ApproximateSettings],
        ignore_case: bool,
    ) -> Result<ApproximateRegex, String> {
        let hir = ParserBuilder::new()
            .case_insensitive(ignore_case)
            .build()
            .parse(pattern)
            .map_err(|failure| failure.to_string())?;
        let roles = capture_roles(&hir);
        let group_count = roles
            .iter()
            .filter(|role| matches!(role, Some(Role::Group(_))))
            .count();
        let mut states = vec![State::Match];
        let start = compile_hir(&hir, 0, &mut states, &roles);
        let mut position_of = vec![usize::MAX; states.len()];
        let mut positions = Vec::new();
        for (index, state) in states.iter().enumerate() {
            let ranges = match state {
                State::Character { ranges, .. } => ranges.clone(),
                State::Match => Vec::new(),
                _ => continue,
            };
            position_of[index] = positions.len();
            positions.push(Position {
                ranges,
                transitions: Vec::new(),
            });
        }
        let initial = transitions_from(start, &states, &position_of, scopes);
        for (index, state) in states.iter().enumerate() {
            if let State::Character { next, .. } = state {
                positions[position_of[index]].transitions =
                    transitions_from(*next, &states, &position_of, scopes);
            }
        }
        let scoped = initial
            .iter()
            .chain(positions.iter().flat_map(|position| &position.transitions))
            .any(|transition| {
                transition.settings.is_some_and(
                    |settings| matches!(settings[DEPTH], Setting::Value(depth) if depth > 0),
                )
            });
        Ok(ApproximateRegex {
            positions,
            initial,
            final_position: position_of[0],
            group_count,
            scoped,
        })
    }

    pub(crate) fn group_count(&self) -> usize {
        self.group_count
    }

    /// The best match in `text`, as TRE's approximate matcher finds it: the cheapest, then the
    /// one that starts first, then the one that ends last. `not_bol` keeps `^` from matching at
    /// the start, as `REG_NOTBOL` does for the searches after the first in `gsub`.
    pub(crate) fn search(&self, text: &[char], not_bol: bool) -> R<Option<ApproximateMatch>> {
        self.search_with(text, not_bol, &DEFAULT_PARAMS)
    }

    /// [`search`](Self::search) as TRE's `regaexec` runs it, with `defaults` as the settings
    /// of the whole pattern, which its approximate scopes start from.
    pub(crate) fn search_with(
        &self,
        text: &[char],
        not_bol: bool,
        defaults: &Params,
    ) -> R<Option<ApproximateMatch>> {
        if self.scoped {
            self.search_levels::<{ MOST_DEPTH + 1 }>(text, not_bol, defaults)
        } else {
            self.search_levels::<1>(text, not_bol, defaults)
        }
    }

    fn search_levels<const LEVELS: usize>(
        &self,
        text: &[char],
        not_bol: bool,
        defaults: &Params,
    ) -> R<Option<ApproximateMatch>> {
        let count = self.positions.len();
        let slot_count = 2 * self.group_count;
        let mut table = ParamsTable(vec![*defaults]);
        let mut reach = vec![Reach::<LEVELS>::UNREACHED; count];
        let mut reach_next = vec![Reach::<LEVELS>::UNREACHED; count];
        let mut slots = vec![-1i32; count * slot_count];
        let mut slots_next = vec![-1i32; count * slot_count];
        let mut path_slots = vec![-1i32; slot_count];
        let mut best = Best {
            end: -1,
            start: -1,
            cost: NO_BOUND,
            slots: vec![-1; slot_count],
        };
        // The deletion phase's worklist, read from `head` on.
        let mut queue: Vec<usize> = Vec::with_capacity(count);
        // The positions reached at the current and the next text position, so the loops below
        // visit only those; each phase sorts them, because ties go to the lower position.
        let mut active: Vec<usize> = Vec::with_capacity(count);
        let mut active_next: Vec<usize> = Vec::with_capacity(count);
        // The entry each initial transition makes, which differs between text positions only in
        // `at` and `start`. `None` marks settings TRE rejects, reported when the entry is made.
        let mut initial_entries = Vec::with_capacity(self.initial.len());
        for transition in &self.initial {
            let mut entry = Reach::<LEVELS>::UNREACHED;
            initial_entries.push(match &transition.settings {
                Some(settings) => apply_settings(&mut entry, settings, &mut table)
                    .ok()
                    .map(|()| entry),
                None => Some(entry),
            });
        }
        let mut position = 0;
        loop {
            let here = position as i32;
            // Initial positions join until a match without edits is found.
            if best.cost > 0 {
                for (transition, initial) in self.initial.iter().zip(&initial_entries) {
                    let target = transition.target;
                    if reach_next[target].at >= here
                        || assertions_fail(transition.assertions, text, position, not_bol)
                    {
                        continue;
                    }
                    let Some(initial) = initial else {
                        if let Some(settings) = &transition.settings {
                            apply_settings(&mut reach_next[target], settings, &mut table)?;
                        }
                        continue;
                    };
                    active_next.push(target);
                    let entry = &mut reach_next[target];
                    *entry = Reach {
                        at: here,
                        start: here,
                        ..*initial
                    };
                    let target_slots = &mut slots_next[target * slot_count..][..slot_count];
                    target_slots.fill(-1);
                    for &slot in &transition.saves {
                        target_slots[slot] = here;
                    }
                    if target == self.final_position {
                        best.record(here, entry, target_slots);
                    }
                }
            }
            // An insertion skips a text character and stays at the same position.
            for &id in &active {
                let source = &reach[id];
                let params = &table.0[source.params as usize];
                let depth = usize::from(source.depth);
                let level = source.costs[depth];
                let cost = level[COST].saturating_add(params.insertion_cost.unwrap_or(0));
                if cost > params.cost_limit
                    || level[INSERTIONS] + 1 > params.insertions
                    || edits(level) + 1 > params.edits
                {
                    continue;
                }
                let total = if depth > 0 {
                    source.costs[0][COST].saturating_add(
                        params
                            .insertion_cost
                            .or(defaults.insertion_cost)
                            .unwrap_or(DEFAULT_COST),
                    )
                } else {
                    cost
                };
                if reach_next[id].at == here {
                    if total >= reach_next[id].costs[0][COST] {
                        continue;
                    }
                } else {
                    active_next.push(id);
                }
                let mut entry = Reach {
                    at: here,
                    ..*source
                };
                count_edit(&mut entry, depth, cost, total, INSERTIONS, 1);
                reach_next[id] = entry;
                slots_next[id * slot_count..][..slot_count]
                    .copy_from_slice(&slots[id * slot_count..][..slot_count]);
            }
            // A deletion takes a transition without consuming its character, repeatedly, until
            // no position can be reached more cheaply.
            active_next.sort_unstable();
            queue.clear();
            queue.extend_from_slice(&active_next);
            let mut head = 0;
            while let Some(&id) = queue.get(head) {
                head += 1;
                let source = reach_next[id];
                let params = &table.0[source.params as usize];
                let depth = usize::from(source.depth);
                let level = source.costs[depth];
                let cost = level[COST].saturating_add(params.deletion_cost.unwrap_or(0));
                if cost > params.cost_limit
                    || level[DELETIONS] + 1 > params.deletions
                    || edits(level) + 1 > params.edits
                {
                    continue;
                }
                let total = if depth > 0 {
                    source.costs[0][COST].saturating_add(
                        params
                            .deletion_cost
                            .or(defaults.deletion_cost)
                            .unwrap_or(DEFAULT_COST),
                    )
                } else {
                    cost
                };
                for transition in &self.positions[id].transitions {
                    let target = transition.target;
                    if assertions_fail(transition.assertions, text, position, not_bol) {
                        continue;
                    }
                    path_slots.copy_from_slice(&slots_next[id * slot_count..][..slot_count]);
                    for &slot in &transition.saves {
                        path_slots[slot] = here;
                    }
                    let target_slots = &slots_next[target * slot_count..][..slot_count];
                    if loses(
                        &reach_next[target],
                        here,
                        total,
                        source.start,
                        &path_slots,
                        target_slots,
                    ) {
                        continue;
                    }
                    let mut entry = Reach { at: here, ..source };
                    if let Some(settings) = &transition.settings {
                        apply_settings(&mut entry, settings, &mut table)?;
                    }
                    // TRE takes the new settings but stays at the nesting level it came from.
                    entry.depth = source.depth;
                    count_edit(&mut entry, depth, cost, total, DELETIONS, 1);
                    if reach_next[target].at != here {
                        active_next.push(target);
                    }
                    reach_next[target] = entry;
                    slots_next[target * slot_count..][..slot_count].copy_from_slice(&path_slots);
                    if target == self.final_position {
                        best.record(here, &entry, &path_slots);
                    }
                    queue.push(target);
                }
            }
            let Some(&character) = text.get(position) else {
                break;
            };
            position += 1;
            let here = position as i32;
            std::mem::swap(&mut reach, &mut reach_next);
            std::mem::swap(&mut slots, &mut slots_next);
            std::mem::swap(&mut active, &mut active_next);
            active.sort_unstable();
            active_next.clear();
            // Each reached position consumes the character, exactly or as a substitution.
            for &id in &active {
                let source = &reach[id];
                let exact = self.positions[id]
                    .ranges
                    .iter()
                    .any(|&(low, high)| low <= character && character <= high);
                let depth = usize::from(source.depth);
                let level = source.costs[depth];
                let (cost, total, error) = if exact {
                    (level[COST], source.costs[0][COST], 0)
                } else {
                    let params = &table.0[source.params as usize];
                    let cost = level[COST].saturating_add(params.substitution_cost.unwrap_or(0));
                    if cost > params.cost_limit
                        || level[SUBSTITUTIONS] + 1 > params.substitutions
                        || edits(level) + 1 > params.edits
                    {
                        continue;
                    }
                    let total = if depth > 0 {
                        source.costs[0][COST].saturating_add(
                            params
                                .substitution_cost
                                .or(defaults.substitution_cost)
                                .unwrap_or(DEFAULT_COST),
                        )
                    } else {
                        cost
                    };
                    (cost, total, 1)
                };
                for transition in &self.positions[id].transitions {
                    let target = transition.target;
                    if assertions_fail(transition.assertions, text, position, not_bol) {
                        continue;
                    }
                    path_slots.copy_from_slice(&slots[id * slot_count..][..slot_count]);
                    for &slot in &transition.saves {
                        path_slots[slot] = here;
                    }
                    let target_slots = &slots_next[target * slot_count..][..slot_count];
                    if loses(
                        &reach_next[target],
                        here,
                        total,
                        source.start,
                        &path_slots,
                        target_slots,
                    ) {
                        continue;
                    }
                    let mut entry = Reach {
                        at: here,
                        ..*source
                    };
                    if let Some(settings) = &transition.settings {
                        apply_settings(&mut entry, settings, &mut table)?;
                    }
                    count_edit(&mut entry, depth, cost, total, SUBSTITUTIONS, error);
                    if reach_next[target].at != here {
                        active_next.push(target);
                    }
                    reach_next[target] = entry;
                    slots_next[target * slot_count..][..slot_count].copy_from_slice(&path_slots);
                    if target == self.final_position {
                        best.record(here, &entry, &path_slots);
                    }
                }
            }
        }
        if best.end < 0 {
            return Ok(None);
        }
        let groups = best
            .slots
            .chunks(2)
            .map(|pair| match pair {
                &[start, end] if start >= 0 && end >= 0 => Some((start as usize, end as usize)),
                _ => None,
            })
            .collect();
        Ok(Some((best.start as usize, best.end as usize, groups)))
    }
}

/// The best match found so far.
struct Best {
    end: i32,
    start: i32,
    cost: i32,
    slots: Vec<i32>,
}

impl Best {
    /// Takes a match ending at `end` if it is cheaper, or as cheap and starts no later.
    fn record<const LEVELS: usize>(&mut self, end: i32, entry: &Reach<LEVELS>, slots: &[i32]) {
        let cost = entry.costs[0][COST];
        if self.end < 0 || cost < self.cost || (cost == self.cost && entry.start <= self.start) {
            self.end = end;
            self.start = entry.start;
            self.cost = cost;
            self.slots.copy_from_slice(slots);
        }
    }
}

/// Whether a path costing `total` from a match start of `start`, with capture slots `slots`,
/// loses to the entry already at the target: a costlier path loses, and so does an equally
/// costly one that starts later. Between equal starts the POSIX subexpression rule decides, as
/// TRE's tag order does: the first group that differs should start earlier and end later.
fn loses<const LEVELS: usize>(
    existing: &Reach<LEVELS>,
    here: i32,
    total: i32,
    start: i32,
    slots: &[i32],
    existing_slots: &[i32],
) -> bool {
    if existing.at != here {
        return false;
    }
    let known = existing.costs[0][COST];
    if total != known {
        return total > known;
    }
    if start != existing.start {
        return start > existing.start;
    }
    for (index, (&new, &old)) in slots.iter().zip(existing_slots).enumerate() {
        if new != old {
            let is_start = index % 2 == 0;
            return if is_start { new > old } else { new < old };
        }
    }
    true
}

/// Charges an edit of kind `kind` (`count` of them, 0 or 1) at nesting level `depth`, whose cost
/// becomes `cost`, with `total` as the overall cost.
fn count_edit<const LEVELS: usize>(
    entry: &mut Reach<LEVELS>,
    depth: usize,
    cost: i32,
    total: i32,
    kind: usize,
    count: i32,
) {
    entry.costs[depth][COST] = cost;
    entry.costs[depth][kind] += count;
    if depth > 0 {
        entry.costs[0][COST] = total;
        entry.costs[0][kind] += count;
    }
}

/// TRE's `tre_set_params`. An unset substitution cost makes substitutions free within the scope
/// (TRE stores the unset marker as the cost), while they still count at the default cost
/// overall. TRE asserts that settings nest at most three deep, and R reports a failed
/// assertion as an error.
fn apply_settings<const LEVELS: usize>(
    entry: &mut Reach<LEVELS>,
    settings: &[Setting; SETTING_COUNT],
    table: &mut ParamsTable,
) -> R<()> {
    let current_depth = usize::from(entry.depth);
    let depth = match settings[DEPTH] {
        Setting::Value(depth) => depth.max(0) as usize,
        Setting::Unset | Setting::Default => current_depth,
    };
    if depth > MOST_DEPTH {
        return Err(error(
            "assertion 'value <= 3' failed in executing regexp: file 'tre-match-approx.c', line 141\n",
        ));
    }
    if depth > current_depth {
        for level in &mut entry.costs[current_depth + 1..=depth] {
            *level = [0; 4];
        }
    }
    entry.depth = depth as u8;
    let defaults = table.0[0];
    let mut params = table.0[entry.params as usize];
    let cost = |setting: Setting, current: Option<i32>, default: Option<i32>| match setting {
        Setting::Default => default,
        Setting::Unset => current,
        Setting::Value(value) => Some(value),
    };
    params.insertion_cost = cost(
        settings[COST_INSERTION],
        params.insertion_cost,
        defaults.insertion_cost,
    );
    params.deletion_cost = cost(
        settings[COST_DELETION],
        params.deletion_cost,
        defaults.deletion_cost,
    );
    params.substitution_cost = cost(
        settings[COST_SUBSTITUTION],
        None,
        defaults.substitution_cost,
    );
    let limit = |setting: Setting, current: i32, default: i32| match setting {
        Setting::Default => default,
        Setting::Unset => current,
        Setting::Value(value) => value,
    };
    params.cost_limit = limit(settings[COST_LIMIT], params.cost_limit, defaults.cost_limit);
    params.insertions = limit(
        settings[MOST_INSERTIONS],
        params.insertions,
        defaults.insertions,
    );
    params.deletions = limit(
        settings[MOST_DELETIONS],
        params.deletions,
        defaults.deletions,
    );
    params.substitutions = limit(
        settings[MOST_SUBSTITUTIONS],
        params.substitutions,
        defaults.substitutions,
    );
    params.edits = limit(settings[MOST_EDITS], params.edits, defaults.edits);
    entry.params = table.index_of(params);
    Ok(())
}

/// TRE's assertion check: whether any assertion in `assertions` fails at text position
/// `position`. TRE's `\b` holds at both ends of the text whatever is there, and its `\B` at
/// neither.
#[inline]
fn assertions_fail(assertions: u8, text: &[char], position: usize, not_bol: bool) -> bool {
    if assertions == 0 {
        return false;
    }
    let word = |character: Option<&char>| {
        character.is_some_and(|&character| character == '_' || character.is_alphanumeric())
    };
    let before = word(position.checked_sub(1).and_then(|index| text.get(index)));
    let at_end = position >= text.len();
    let after = word(text.get(position));
    let has = |bit: u8| assertions & bit != 0;
    (has(ASSERT_LINE_START) && (position > 0 || not_bol))
        || (has(ASSERT_LINE_END) && !at_end)
        || (has(ASSERT_WORD_START) && (before || !after))
        || (has(ASSERT_WORD_END) && (!before || after))
        || (has(ASSERT_WORD_BOUNDARY) && position != 0 && !at_end && before == after)
        || (has(ASSERT_NOT_WORD_BOUNDARY) && (position == 0 || at_end || before != after))
}

/// The transitions from Thompson state `from` to the positions its epsilon paths reach, in
/// priority order. A state is entered once per set of assertions, so the first path to it
/// decides its capture slots and settings.
fn transitions_from(
    from: usize,
    states: &[State],
    position_of: &[usize],
    scopes: &[ApproximateSettings],
) -> Vec<Transition> {
    let mut transitions = Vec::new();
    let mut seen = FxHashSet::default();
    let mut stack = vec![(
        from,
        Transition {
            target: usize::MAX,
            assertions: 0,
            saves: Vec::new(),
            settings: None,
        },
    )];
    while let Some((state, mut path)) = stack.pop() {
        if !seen.insert((state, path.assertions)) {
            continue;
        }
        match &states[state] {
            State::Character { .. } | State::Match => {
                path.target = position_of[state];
                transitions.push(path);
            }
            State::Split(targets) => {
                for &target in targets.iter().rev() {
                    stack.push((target, path.clone()));
                }
            }
            State::Look { look, next } => {
                path.assertions |= assertion_bit(*look);
                stack.push((*next, path));
            }
            State::Save { slot, next } => {
                path.saves.push(*slot);
                stack.push((*next, path));
            }
            State::Settings { scope, enter, next } => {
                let entered = scope_settings(*scope, *enter, scopes);
                let combined = path.settings.get_or_insert([Setting::Unset; SETTING_COUNT]);
                for (setting, new) in combined.iter_mut().zip(entered) {
                    if new != Setting::Unset {
                        *setting = new;
                    }
                }
                stack.push((*next, path));
            }
        }
    }
    transitions
}

/// The parameter array TRE puts on entering (`enter`) or leaving scope `scope`. Scopes are
/// numbered in the order TRE expands them, so the scope entered sits one level deeper than the
/// number of scopes before it, and leaving restores the defaults at that number.
fn scope_settings(
    scope: usize,
    enter: bool,
    scopes: &[ApproximateSettings],
) -> [Setting; SETTING_COUNT] {
    let depth = Setting::Value(scope as i32 + i32::from(enter));
    let settings = match scopes.get(scope) {
        Some(settings) if enter => settings,
        _ => {
            let mut defaults = [Setting::Default; SETTING_COUNT];
            defaults[DEPTH] = depth;
            return defaults;
        }
    };
    let value = |value: i64| Setting::Value(value.clamp(i64::from(i32::MIN), UNBOUNDED) as i32);
    let optional = |bound: Option<i64>| bound.map_or(Setting::Unset, value);
    let mut array = [Setting::Unset; SETTING_COUNT];
    array[COST_INSERTION] = optional(settings.insertion_cost);
    array[COST_DELETION] = optional(settings.deletion_cost);
    array[COST_SUBSTITUTION] = optional(settings.substitution_cost);
    array[COST_LIMIT] = value(settings.cost_limit);
    array[MOST_INSERTIONS] = optional(settings.insertions);
    array[MOST_DELETIONS] = optional(settings.deletions);
    array[MOST_SUBSTITUTIONS] = optional(settings.substitutions);
    array[MOST_EDITS] = value(settings.edits);
    array[DEPTH] = depth;
    array
}

fn assertion_bit(look: Look) -> u8 {
    match look {
        Look::Start | Look::StartLF | Look::StartCRLF => ASSERT_LINE_START,
        Look::End | Look::EndLF | Look::EndCRLF => ASSERT_LINE_END,
        Look::WordStartAscii | Look::WordStartUnicode => ASSERT_WORD_START,
        Look::WordEndAscii | Look::WordEndUnicode => ASSERT_WORD_END,
        Look::WordAscii | Look::WordUnicode => ASSERT_WORD_BOUNDARY,
        Look::WordAsciiNegate | Look::WordUnicodeNegate => ASSERT_NOT_WORD_BOUNDARY,
        _ => 0,
    }
}
