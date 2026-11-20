// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of the TRE sources that GNU R ships,
// and keep their copyright notices:
//
// GNU R: src/extra/tre/ (tre-parse.c, tre-compile.c, tre-match-parallel.c, tre-match-utils.h,
// regexec.c)
//     Copyright (c) 2001-2009 Ville Laurikari <vl@iki.fi>
//     All rights reserved.
//
//     Redistribution and use in source and binary forms, with or without modification, are
//     permitted provided that the following conditions are met:
//
//       1. Redistributions of source code must retain the above copyright notice, this list of
//          conditions and the following disclaimer.
//
//       2. Redistributions in binary form must reproduce the above copyright notice, this list
//          of conditions and the following disclaimer in the documentation and/or other
//          materials provided with the distribution.
//
//     THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDER AND CONTRIBUTORS ``AS IS'' AND ANY
//     EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
//     MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
//     COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
//     EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
//     SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
//     HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR
//     TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//     SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! The capture groups of a POSIX pattern, as GNU R's TRE reports them.
//!
//! TRE matches a pattern without backreferences on a tagged NFA. Tags record where each group
//! starts and ends, each tag prefers either its smallest or its largest value, and when two paths
//! reach the same state at the same position the one whose tags compare better survives. Which
//! groups that picks follows neither a backtracker's rule nor the POSIX subexpression rule:
//! `(a|ab)(bc|c)?` on `"abc"` gives `"ab"` and `"c"`, while `(a)?(ab)?b*` on `"abb"` leaves the
//! first group out and gives the second `"ab"`.
//!
//! This module builds TRE's automaton from the pattern (its parse tree, the tags it places and
//! their directions, the expansion of bounded repetitions, and the position sets) and runs TRE's
//! parallel matcher on it. The caller's DFA finds where the leftmost match starts, and the
//! matcher starts there instead of at the start of the text: a path that starts earlier never
//! reaches a match, and a path that shares a state with it has the same future, so dropping those
//! paths changes no result.
//!
//! An atom's characters come from the same translated snippet the DFA compiles, so both agree on
//! what it matches. A pattern outside what this module models (backreferences, `\Q`, approximate
//! bounds, an inline flag group that changes how the pattern groups) does not compile, and its
//! groups come from the backtracker.

//!
//! GNU R runs TRE on single bytes when the pattern and the text are ASCII, and on wide
//! characters otherwise. The two modes parse a bracket expression with a named class
//! differently (in byte mode the class becomes the runs of the 256 byte values it holds, in wide
//! mode one item), which changes the tags, so such a pattern gets one automaton per mode.

use {
    crate::strings::{Groups, parse_bound, translate_class, translate_escape},
    regex_syntax::hir::{Class, ClassUnicode, ClassUnicodeRange, HirKind},
    smallvec::SmallVec,
    std::{cell::RefCell, cmp::Ordering},
};

/// TRE's automata for one pattern.
pub(crate) struct Tnfa {
    wide: Machine,
    /// The byte-mode automaton, where it differs from the wide one.
    bytes: Option<Machine>,
}

/// Compiles `pattern` as TRE does, or returns `None` for a pattern this module does not model
/// or whose group count is not `groups`.
pub(crate) fn compile(pattern: &str, ignore_case: bool, groups: usize) -> Option<Tnfa> {
    let (wide, mode_dependent) = compile_mode(pattern, ignore_case, groups, Mode::Wide)?;
    let bytes = if mode_dependent && pattern.is_ascii() {
        Some(compile_mode(pattern, ignore_case, groups, Mode::Bytes)?.0)
    } else {
        None
    };
    Some(Tnfa { wide, bytes })
}

/// The match TRE finds in `text` when its leftmost match starts at byte `start`, and its groups
/// as byte ranges, or `None` when no match starts there.
pub(crate) fn search(tnfa: &Tnfa, text: &str, start: usize) -> Option<Groups> {
    let machine = match &tnfa.bytes {
        Some(bytes) if text.is_ascii() => bytes,
        _ => &tnfa.wide,
    };
    match machine.workspace.try_borrow_mut() {
        Ok(mut workspace) => run(machine, &mut workspace, text, start),
        Err(_) => run(machine, &mut Workspace::default(), text, start),
    }
}

/// TRE's automaton in one mode, with the buffers its matcher reuses.
struct Machine {
    /// The characters each state consumes, as an index into `sets`.
    state_sets: Vec<u32>,
    /// Where each state's transitions start in `transitions`, with one more entry at the end.
    transition_starts: Vec<u32>,
    transitions: Vec<Transition>,
    /// The transitions into the states a match can start in.
    initial: Vec<Transition>,
    /// The tags each transition sets, as ranges of this array.
    transition_tags: Vec<u32>,
    sets: Vec<CharSet>,
    /// The non-ASCII ranges of every set, each set's sorted and disjoint.
    ranges: Vec<(u32, u32)>,
    final_state: u32,
    tag_count: usize,
    /// Per tag, whether it prefers its smallest value over its largest.
    minimize: Vec<bool>,
    /// Pairs of tags (end, start) that bound a minimal repetition.
    minimal_pairs: Vec<(u32, u32)>,
    submatches: Vec<Submatch>,
    /// The enclosing groups of each group, as ranges of this array.
    parents: Vec<u32>,
    workspace: RefCell<Workspace>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Bytes,
    Wide,
}

/// TRE's assertion bits.
const AT_LINE_START: u8 = 1;
const AT_LINE_END: u8 = 2;
const AT_WORD_START: u8 = 4;
const AT_WORD_END: u8 = 8;
const AT_WORD_BOUNDARY: u8 = 16;
const NOT_WORD_BOUNDARY: u8 = 32;

/// Limits past which a pattern runs on the backtracker instead, since the expansion of nested
/// bounds and the position sets grow multiplicatively.
const NODE_LIMIT: usize = 1 << 18;
const ENTRY_LIMIT: usize = 1 << 22;
const TRANSITION_LIMIT: usize = 1 << 21;
/// The widest last item of a bracket expression whose own characters [`split_items`] lists.
const SPLIT_LIMIT: u32 = 1 << 16;

const CHAR_MAX: u32 = char::MAX as u32;
/// The `guard` of a transition that every character of its state takes.
const UNGUARDED: u32 = u32::MAX;

/// A transition between states. A state whose items take different tags has one transition per
/// item and target, and `guard` holds the characters of that item.
#[derive(Clone, Copy)]
struct Transition {
    target: u32,
    assertions: u8,
    guard: u32,
    tags_start: u32,
    tags_end: u32,
}

#[derive(Clone, Copy)]
struct Submatch {
    start_tag: u32,
    end_tag: u32,
    parents_start: u32,
    parents_end: u32,
}

/// A set of characters: a bitmap of the ASCII ones and a range of [`Machine::ranges`].
struct CharSet {
    ascii: u128,
    ranges_start: u32,
    ranges_end: u32,
}

/// The matcher's buffers. Each thread owns one row of `slots` (its tag values), and the rows
/// move between threads by swapping indices, never by copying, as TRE swaps tag pointers.
#[derive(Default)]
struct Workspace {
    reach: Vec<Thread>,
    reach_next: Vec<Thread>,
    spare: u32,
    slots: Vec<i64>,
    /// Per state, the position it was last reached at and its thread's index in `reach_next`.
    reached: Vec<(i64, u32)>,
    match_tags: Vec<i64>,
}

#[derive(Clone, Copy)]
struct Thread {
    state: u32,
    slot: u32,
}

fn compile_mode(
    pattern: &str,
    ignore_case: bool,
    groups: usize,
    mode: Mode,
) -> Option<(Machine, bool)> {
    let mut nodes = Vec::new();
    let mut sets = CharSets::default();
    let parsed = parse(pattern, ignore_case, mode, &mut nodes, &mut sets)?;
    if parsed.submatch_count != groups + 1 {
        return None;
    }
    let mut tagging = Tagging {
        minimize: Vec::new(),
        submatches: vec![(0, 0); parsed.submatch_count],
        parents: vec![Vec::new(); parsed.submatch_count],
        minimal_pairs: Vec::new(),
    };
    add_tags(&mut nodes, parsed.root, true, &mut tagging);
    let tag_count = add_tags(&mut nodes, parsed.root, false, &mut tagging) as usize;
    let mut positions = parsed.positions;
    expand(
        &mut nodes,
        parsed.root,
        &mut tagging.minimize,
        &mut positions,
    )?;
    let final_set = sets.add(std::iter::empty());
    let final_leaf = push(
        &mut nodes,
        Kind::Leaf {
            set: final_set,
            guard: UNGUARDED,
            position: positions,
            union: false,
        },
    );
    let root = push(&mut nodes, Kind::Concat(parsed.root, final_leaf));
    let automaton = build_automaton(&nodes, root, final_leaf, positions + 1, tag_count)?;

    let fits = |tag: u32| tag as usize <= tag_count;
    if !tagging
        .submatches
        .iter()
        .all(|&(start, end)| fits(start) && fits(end))
        || !tagging
            .minimal_pairs
            .iter()
            .all(|&(_, start)| (start as usize) < tag_count)
    {
        return None;
    }
    let mut minimize = tagging.minimize;
    minimize.resize(tag_count.max(minimize.len()), false);
    let mut parents = Vec::new();
    let submatches = tagging
        .submatches
        .iter()
        .zip(&tagging.parents)
        .map(|(&(start_tag, end_tag), enclosing)| {
            let parents_start = parents.len() as u32;
            parents.extend_from_slice(enclosing);
            Submatch {
                start_tag,
                end_tag,
                parents_start,
                parents_end: parents.len() as u32,
            }
        })
        .collect();
    let Automaton {
        state_sets,
        transition_starts,
        transitions,
        initial,
        transition_tags,
        final_state,
    } = automaton;
    let machine = Machine {
        state_sets,
        transition_starts,
        transitions,
        initial,
        transition_tags,
        sets: sets.sets,
        ranges: sets.ranges,
        final_state,
        tag_count,
        minimize,
        minimal_pairs: tagging.minimal_pairs,
        submatches,
        parents,
        workspace: RefCell::default(),
    };
    Some((machine, parsed.mode_dependent))
}

/// TRE's `tre_tnfa_run_parallel` from `start`, then `tre_fill_pmatch`.
fn run(machine: &Machine, workspace: &mut Workspace, text: &str, start: usize) -> Option<Groups> {
    let states = machine.state_sets.len();
    let tag_count = machine.tag_count;
    if workspace.reach.len() != states {
        *workspace = Workspace {
            reach: (0..states as u32)
                .map(|slot| Thread { state: 0, slot })
                .collect(),
            reach_next: (states as u32..2 * states as u32)
                .map(|slot| Thread { state: 0, slot })
                .collect(),
            spare: 2 * states as u32,
            slots: vec![-1; (2 * states + 1) * tag_count],
            reached: vec![(-1, 0); states],
            match_tags: vec![-1; tag_count],
        };
    }
    let Workspace {
        reach,
        reach_next,
        spare,
        slots,
        reached,
        match_tags,
    } = workspace;
    reached.fill((-1, 0));
    let mut position = start;
    let mut previous = text.get(..start)?.chars().next_back().unwrap_or('\0');
    let mut next = text.get(start..)?.chars().next().unwrap_or('\0');
    let mut match_end: i64 = -1;
    let mut new_match = false;
    let mut next_length = 0;
    let row = |slot: u32| slot as usize * tag_count..(slot as usize + 1) * tag_count;
    loop {
        let here = position as i64;
        if match_end < 0 {
            for transition in &machine.initial {
                let target = transition.target as usize;
                if reached[target].0 >= here
                    || assertions_fail(transition.assertions, position, previous, next)
                {
                    continue;
                }
                let thread = &mut reach_next[next_length];
                thread.state = transition.target;
                let tags = &mut slots[row(thread.slot)];
                tags.fill(-1);
                for &tag in &machine.transition_tags
                    [transition.tags_start as usize..transition.tags_end as usize]
                {
                    tags[tag as usize] = here;
                }
                if transition.target == machine.final_state {
                    match_end = here;
                    new_match = true;
                    match_tags.copy_from_slice(tags);
                }
                reached[target] = (here, next_length as u32);
                next_length += 1;
            }
        } else if tag_count == 0 || next_length == 0 {
            break;
        }
        if position >= text.len() {
            break;
        }
        previous = next;
        position += next.len_utf8();
        next = text[position..].chars().next().unwrap_or('\0');
        std::mem::swap(reach, reach_next);
        let mut reach_length = next_length;
        if new_match && !machine.minimal_pairs.is_empty() {
            new_match = false;
            let mut kept = 0;
            for index in 0..reach_length {
                let tags = &slots[row(reach[index].slot)];
                let pruned = machine.minimal_pairs.iter().any(|&(end, start)| {
                    let (end, start) = (end as usize, start as usize);
                    end >= tag_count
                        || (tags[start] == match_tags[start] && tags[end] < match_tags[end])
                });
                if !pruned {
                    reach.swap(kept, index);
                    kept += 1;
                }
            }
            reach_length = kept;
        }
        next_length = 0;
        let here = position as i64;
        for index in 0..reach_length {
            let Thread { state, slot } = reach[index];
            if !contains(machine, machine.state_sets[state as usize], previous) {
                continue;
            }
            let transitions = machine.transition_starts[state as usize] as usize
                ..machine.transition_starts[state as usize + 1] as usize;
            for transition in &machine.transitions[transitions] {
                if (transition.guard != UNGUARDED && !contains(machine, transition.guard, previous))
                    || assertions_fail(transition.assertions, position, previous, next)
                {
                    continue;
                }
                let spare_row = row(*spare);
                slots.copy_within(row(slot), spare_row.start);
                for &tag in &machine.transition_tags
                    [transition.tags_start as usize..transition.tags_end as usize]
                {
                    slots[spare_row.start + tag as usize] = here;
                }
                let target = transition.target as usize;
                let (reached_at, thread_index) = reached[target];
                let is_final = transition.target == machine.final_state;
                if reached_at < here {
                    let thread = &mut reach_next[next_length];
                    std::mem::swap(&mut thread.slot, spare);
                    thread.state = transition.target;
                    reached[target] = (here, next_length as u32);
                    let tags = &slots[row(thread.slot)];
                    if is_final && (match_end == -1 || (tag_count > 0 && tags[0] <= match_tags[0]))
                    {
                        match_end = here;
                        new_match = true;
                        match_tags.copy_from_slice(tags);
                    }
                    next_length += 1;
                } else {
                    let existing = &mut reach_next[thread_index as usize].slot;
                    if tag_order(&machine.minimize, &slots[spare_row], &slots[row(*existing)]) {
                        std::mem::swap(existing, spare);
                        if is_final {
                            match_end = here;
                            new_match = true;
                            match_tags.copy_from_slice(&slots[row(*existing)]);
                        }
                    }
                }
            }
        }
    }
    if match_end < 0 {
        return None;
    }
    let tag_value = |tag: u32| {
        if tag as usize == tag_count {
            match_end
        } else {
            match_tags[tag as usize]
        }
    };
    let mut spans: SmallVec<[(i64, i64); 4]> = machine
        .submatches
        .iter()
        .map(|submatch| {
            let (start, end) = (tag_value(submatch.start_tag), tag_value(submatch.end_tag));
            if start == -1 || end == -1 {
                (-1, -1)
            } else {
                (start, end)
            }
        })
        .collect();
    for (index, submatch) in machine.submatches.iter().enumerate() {
        for &parent in
            &machine.parents[submatch.parents_start as usize..submatch.parents_end as usize]
        {
            let (parent_start, parent_end) = spans[parent as usize];
            let (start, end) = spans[index];
            if start < parent_start || end > parent_end {
                spans[index] = (-1, -1);
            }
        }
    }
    Some(
        spans
            .into_iter()
            .map(|(start, end)| {
                (start >= 0 && start <= end).then_some((start as usize, end as usize))
            })
            .collect(),
    )
}

/// TRE's `CHECK_ASSERTIONS`: whether `assertions` rule out a step at byte `position`, between
/// `previous` and `next` (`'\0'` past either end of the text).
fn assertions_fail(assertions: u8, position: usize, previous: char, next: char) -> bool {
    if assertions == 0 {
        return false;
    }
    let word = |character: char| character == '_' || character.is_alphanumeric();
    (assertions & AT_LINE_START != 0 && position > 0)
        || (assertions & AT_LINE_END != 0 && next != '\0')
        || (assertions & AT_WORD_START != 0 && (word(previous) || !word(next)))
        || (assertions & AT_WORD_END != 0 && (!word(previous) || word(next)))
        || (assertions & AT_WORD_BOUNDARY != 0
            && position != 0
            && next != '\0'
            && word(previous) == word(next))
        || (assertions & NOT_WORD_BOUNDARY != 0
            && (position == 0 || next == '\0' || word(previous) != word(next)))
}

/// TRE's `tre_tag_order`: whether the `candidate` tags beat the `existing` ones.
fn tag_order(minimize: &[bool], candidate: &[i64], existing: &[i64]) -> bool {
    for ((&new, &old), &smallest) in candidate.iter().zip(existing).zip(minimize) {
        if new != old {
            return if smallest { new < old } else { new > old };
        }
    }
    false
}

fn contains(machine: &Machine, set: u32, character: char) -> bool {
    let set = &machine.sets[set as usize];
    let code = character as u32;
    if code < 128 {
        return set.ascii >> code & 1 != 0;
    }
    machine.ranges[set.ranges_start as usize..set.ranges_end as usize]
        .binary_search_by(|&(low, high)| {
            if high < code {
                Ordering::Less
            } else if low > code {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// A node of TRE's parse tree, in an arena. TRE's `submatch_id`, `num_submatches` and
/// `num_tags` are `submatch`, `submatch_count` and `tag_count`.
#[derive(Clone, Copy)]
struct Node {
    kind: Kind,
    submatch: i32,
    submatch_count: u32,
    tag_count: u32,
}

#[derive(Clone, Copy)]
enum Kind {
    Empty,
    Assertion(u8),
    Tag(u32),
    /// A literal at TRE's `position`, which becomes one state consuming `set`. TRE parses an
    /// atom that matches one of several ranges (a bracket expression, or a letter under
    /// `ignore.case`) as a union of literals at one position, which tags like a literal but
    /// leaves the next tag maximizing (`union`). When a group holds such an atom alone, its
    /// union gets tags of its own, so the atom splits into two literals at the same position,
    /// each consuming only its `guard` of `set`.
    Leaf {
        set: u32,
        guard: u32,
        position: u32,
        union: bool,
    },
    Concat(u32, u32),
    Union(u32, u32),
    Repeat {
        argument: u32,
        minimum: i32,
        maximum: i32,
        minimal: bool,
    },
}

/// Adds a node, which counts the submatches below it as TRE's constructors do.
fn push(nodes: &mut Vec<Node>, kind: Kind) -> u32 {
    let count = |node: u32| {
        nodes
            .get(node as usize)
            .map_or(0, |node| node.submatch_count)
    };
    let submatch_count = match kind {
        Kind::Concat(left, right) | Kind::Union(left, right) => count(left) + count(right),
        Kind::Repeat { argument, .. } => count(argument),
        Kind::Empty | Kind::Assertion(_) | Kind::Tag(_) | Kind::Leaf { .. } => 0,
    };
    nodes.push(Node {
        kind,
        submatch: -1,
        submatch_count,
        tag_count: 0,
    });
    (nodes.len() - 1) as u32
}

#[derive(Default)]
struct CharSets {
    sets: Vec<CharSet>,
    ranges: Vec<(u32, u32)>,
}

impl CharSets {
    fn add(&mut self, ranges: impl Iterator<Item = (u32, u32)>) -> u32 {
        let ranges_start = self.ranges.len() as u32;
        let mut ascii = 0u128;
        for (low, high) in ranges {
            for code in low..=high.min(127) {
                ascii |= 1 << code;
            }
            if high >= 128 {
                self.ranges.push((low.max(128), high));
            }
        }
        self.sets.push(CharSet {
            ascii,
            ranges_start,
            ranges_end: self.ranges.len() as u32,
        });
        (self.sets.len() - 1) as u32
    }

    fn add_class(&mut self, class: &ClassUnicode) -> u32 {
        self.add(
            class
                .ranges()
                .iter()
                .map(|range| (range.start() as u32, range.end() as u32)),
        )
    }
}

/// The characters a translated atom matches, read as the DFA reads it.
fn snippet_class(snippet: &str, ignore_case: bool) -> Option<ClassUnicode> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(ignore_case)
        .build()
        .parse(snippet)
        .ok()?;
    match hir.kind() {
        HirKind::Literal(literal) => {
            let mut characters = std::str::from_utf8(&literal.0).ok()?.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            Some(ClassUnicode::new([ClassUnicodeRange::new(
                character, character,
            )]))
        }
        HirKind::Class(Class::Unicode(class)) => Some(class.clone()),
        _ => None,
    }
}

/// The class of the code points `ranges` cover, leaving out surrogates.
fn class_of(ranges: impl IntoIterator<Item = (u32, u32)>) -> ClassUnicode {
    ClassUnicode::new(ranges.into_iter().filter_map(|(low, high)| {
        let low = if (0xD800..=0xDFFF).contains(&low) {
            0xE000
        } else {
            low
        };
        let high = if (0xD800..=0xDFFF).contains(&high) {
            0xD7FF
        } else {
            high
        };
        Some(ClassUnicodeRange::new(
            char::from_u32(low)?,
            char::from_u32(high.min(CHAR_MAX))?,
        ))
        .filter(|_| low <= high)
    }))
}

/// The states of TRE's `tre_parse`.
#[derive(Clone, Copy)]
enum Parse {
    Re,
    Branch,
    Piece,
    Catenation,
    PostCatenation(u32),
    Union,
    PostUnion(u32),
    Postfix,
    Atom,
    MarkForSubmatch(u32),
    RestoreCase(bool),
}

/// What [`parse`] returns: the root, the number of submatches (the whole match included), the
/// number of positions, and whether a bracket expression holds a named class.
struct Parsed {
    root: u32,
    submatch_count: usize,
    positions: u32,
    mode_dependent: bool,
}

/// An atom TRE parses as a union of literals at one position.
struct UnionAtom {
    node: u32,
    class: ClassUnicode,
    items: Bracket,
}

/// TRE's `tre_parse` for an extended pattern.
fn parse(
    pattern: &str,
    ignore_case: bool,
    mode: Mode,
    nodes: &mut Vec<Node>,
    sets: &mut CharSets,
) -> Option<Parsed> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut stack = vec![Parse::MarkForSubmatch(0), Parse::Re];
    let mut submatch_id = 1;
    let mut index = 0;
    let mut depth = 0;
    let mut positions = 0;
    let mut mode_dependent = false;
    let mut ignore_case = ignore_case;
    let mut branch_start = true;
    let mut result: Option<u32> = None;
    let mut union_atom: Option<UnionAtom> = None;
    while let Some(symbol) = stack.pop() {
        match symbol {
            Parse::Re => stack.extend([Parse::Union, Parse::Branch]),
            Parse::Branch => {
                branch_start = true;
                stack.extend([Parse::Catenation, Parse::Piece]);
            }
            Parse::Piece => stack.extend([Parse::Postfix, Parse::Atom]),
            Parse::Catenation => match chars.get(index) {
                None | Some('|') => {}
                Some(')') if depth > 0 => depth -= 1,
                Some(_) => {
                    branch_start = false;
                    stack.extend([
                        Parse::Catenation,
                        Parse::PostCatenation(result?),
                        Parse::Piece,
                    ]);
                }
            },
            Parse::PostCatenation(left) => result = Some(push(nodes, Kind::Concat(left, result?))),
            Parse::Union => match chars.get(index) {
                Some('|') => {
                    stack.extend([Parse::Union, Parse::PostUnion(result?), Parse::Branch]);
                    index += 1;
                }
                Some(')') => index += 1,
                _ => {}
            },
            Parse::PostUnion(left) => result = Some(push(nodes, Kind::Union(left, result?))),
            Parse::Postfix => match chars.get(index) {
                Some(&operator @ ('*' | '+' | '?')) => {
                    let minimal = match chars.get(index + 1) {
                        Some('?') => {
                            index += 1;
                            true
                        }
                        Some('*' | '+') => return None,
                        _ => false,
                    };
                    index += 1;
                    let argument = result?;
                    result = Some(push(
                        nodes,
                        Kind::Repeat {
                            argument,
                            minimum: i32::from(operator == '+'),
                            maximum: if operator == '?' { 1 } else { -1 },
                            minimal,
                        },
                    ));
                    stack.push(Parse::Postfix);
                }
                Some('{') => {
                    let bound = parse_bound(&chars, index).ok()?;
                    if bound.settings.is_some() {
                        return None;
                    }
                    index = bound.end;
                    let minimum = i32::try_from(bound.minimum).ok()?;
                    let maximum = i32::try_from(bound.maximum).ok()?;
                    result = Some(if minimum == 0 && maximum == 0 {
                        push(nodes, Kind::Empty)
                    } else {
                        let (minimum, maximum) = if minimum < 0 && maximum < 0 {
                            (1, 1)
                        } else {
                            (minimum, maximum)
                        };
                        let argument = result?;
                        push(
                            nodes,
                            Kind::Repeat {
                                argument,
                                minimum,
                                maximum,
                                minimal: bound.minimal,
                            },
                        )
                    });
                    stack.push(Parse::Postfix);
                }
                _ => {}
            },
            Parse::Atom => {
                let Some(&character) = chars.get(index) else {
                    result = Some(push(nodes, Kind::Empty));
                    continue;
                };
                let (class, items) = match character {
                    '(' if chars.get(index + 1) == Some(&'?') => {
                        index += 2;
                        let mut case = ignore_case;
                        let mut enable = true;
                        let mut scoped = false;
                        loop {
                            match chars.get(index) {
                                Some('i') => {
                                    case = enable;
                                    index += 1;
                                }
                                Some('-') => {
                                    enable = false;
                                    index += 1;
                                }
                                Some(':') => {
                                    index += 1;
                                    depth += 1;
                                    scoped = true;
                                    break;
                                }
                                Some(')') => {
                                    index += 1;
                                    break;
                                }
                                Some('#') => {
                                    index = (index..chars.len()).find(|&at| chars[at] == ')')? + 1;
                                    break;
                                }
                                _ => return None,
                            }
                        }
                        // TRE reads the rest of the enclosing group into a flag group without
                        // a `:`, which only groups as the translated pattern does at the start of
                        // a top-level branch.
                        if !scoped && !(depth == 0 && branch_start) {
                            return None;
                        }
                        stack.extend([Parse::RestoreCase(ignore_case), Parse::Re]);
                        ignore_case = case;
                        continue;
                    }
                    '(' => {
                        depth += 1;
                        stack.extend([Parse::MarkForSubmatch(submatch_id), Parse::Re]);
                        submatch_id += 1;
                        index += 1;
                        continue;
                    }
                    ')' if depth > 0 => {
                        result = Some(push(nodes, Kind::Empty));
                        continue;
                    }
                    '*' | '|' | '{' | '+' | '?' => {
                        result = Some(push(nodes, Kind::Empty));
                        continue;
                    }
                    '^' | '$' => {
                        index += 1;
                        let assertion = if character == '^' {
                            AT_LINE_START
                        } else {
                            AT_LINE_END
                        };
                        result = Some(push(nodes, Kind::Assertion(assertion)));
                        continue;
                    }
                    '.' => {
                        index += 1;
                        (class_of([(0, CHAR_MAX)]), None)
                    }
                    '[' => {
                        let (end, items) = bracket_items(&chars, index + 1, ignore_case, mode)?;
                        let mut snippet = String::new();
                        let mut problem = None;
                        let translated_end =
                            translate_class(&chars, index, true, &mut snippet, &mut problem);
                        if problem.is_some() || translated_end != end {
                            return None;
                        }
                        index = end;
                        (snippet_class(&snippet, ignore_case)?, Some(items))
                    }
                    '\\' => {
                        let escaped = *chars.get(index + 1)?;
                        let assertion = match escaped {
                            'b' => AT_WORD_BOUNDARY,
                            'B' => NOT_WORD_BOUNDARY,
                            '<' => AT_WORD_START,
                            '>' => AT_WORD_END,
                            _ => 0,
                        };
                        if assertion != 0 {
                            index += 2;
                            result = Some(push(nodes, Kind::Assertion(assertion)));
                            continue;
                        }
                        let (end, items) = match escaped {
                            'Q' | '0'..='9' => return None,
                            'w' | 'W' | 's' | 'S' | 'd' | 'D' => {
                                let expansion: Vec<char> = match escaped {
                                    'w' => "[[:alnum:]_]",
                                    'W' => "[^[:alnum:]_]",
                                    's' => "[[:space:]]",
                                    'S' => "[^[:space:]]",
                                    'd' => "[[:digit:]]",
                                    _ => "[^[:digit:]]",
                                }
                                .chars()
                                .collect();
                                let (_, items) = bracket_items(&expansion, 1, ignore_case, mode)?;
                                (index + 2, Some(items))
                            }
                            'x' => (hex_escape_end(&chars, index + 2)?, None),
                            _ => (index + 2, None),
                        };
                        let mut snippet = String::new();
                        let mut problem = None;
                        let translated_end = translate_escape(
                            &chars,
                            index,
                            true,
                            false,
                            &mut snippet,
                            &mut problem,
                        );
                        if problem.is_some() || translated_end != end {
                            return None;
                        }
                        index = end;
                        (snippet_class(&snippet, ignore_case)?, items)
                    }
                    _ => {
                        index += 1;
                        let items = (ignore_case
                            && (character.is_uppercase() || character.is_lowercase()))
                        .then(|| {
                            let (upper, lower) = (
                                simple_uppercase(character) as u32,
                                simple_lowercase(character) as u32,
                            );
                            Bracket {
                                negate: false,
                                items: SmallVec::from_slice(&[
                                    Item::Range(upper, upper),
                                    Item::Range(lower, lower),
                                ]),
                                has_class: false,
                            }
                        });
                        let snippet = regex::escape(character.encode_utf8(&mut [0; 4]));
                        (snippet_class(&snippet, ignore_case)?, items)
                    }
                };
                let union = items.as_ref().is_some_and(|items| leaf_count(items).0 >= 2);
                mode_dependent |= items.as_ref().is_some_and(|items| items.has_class);
                let node = push(
                    nodes,
                    Kind::Leaf {
                        set: sets.add_class(&class),
                        guard: UNGUARDED,
                        position: positions,
                        union,
                    },
                );
                positions += 1;
                if union && let Some(items) = items {
                    union_atom = Some(UnionAtom { node, class, items });
                }
                result = Some(node);
            }
            Parse::MarkForSubmatch(id) => {
                if let Some(mut node) = result {
                    if let Some(atom) = union_atom.take_if(|atom| atom.node == node) {
                        let Kind::Leaf { set, position, .. } = nodes[node as usize].kind else {
                            return None;
                        };
                        let (rest, last) = split_items(&atom.items, &atom.class)?;
                        let mut item = |guard: &ClassUnicode| {
                            let guard = sets.add_class(guard);
                            push(
                                nodes,
                                Kind::Leaf {
                                    set,
                                    guard,
                                    position,
                                    union: false,
                                },
                            )
                        };
                        let (left, right) = (item(&rest), item(&last));
                        nodes[node as usize].kind = Kind::Union(left, right);
                    }
                    if nodes[node as usize].submatch >= 0 {
                        let empty = push(nodes, Kind::Empty);
                        let wrapper = push(nodes, Kind::Concat(empty, node));
                        nodes[wrapper as usize].submatch_count =
                            nodes[node as usize].submatch_count;
                        node = wrapper;
                    }
                    let marked = &mut nodes[node as usize];
                    marked.submatch = id as i32;
                    marked.submatch_count += 1;
                    result = Some(node);
                }
            }
            Parse::RestoreCase(case) => ignore_case = case,
        }
    }
    if depth > 0 {
        return None;
    }
    Some(Parsed {
        root: result?,
        submatch_count: submatch_id as usize,
        positions,
        mode_dependent,
    })
}

/// The index after TRE's `\x` escape whose digits start at `start`, or `None` where TRE fails
/// or reads past the pattern.
fn hex_escape_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start)? {
        '{' => {
            let mut cursor = start + 1;
            loop {
                match chars.get(cursor)? {
                    '}' => return Some(cursor + 1),
                    digit if digit.is_ascii_hexdigit() => cursor += 1,
                    _ => return None,
                }
            }
        }
        _ => {
            let mut cursor = start;
            while cursor < start + 2 && chars.get(cursor).is_some_and(char::is_ascii_hexdigit) {
                cursor += 1;
            }
            Some(cursor)
        }
    }
}

/// The items of a bracket expression, in TRE's order.
struct Bracket {
    negate: bool,
    items: SmallVec<[Item; 8]>,
    has_class: bool,
}

#[derive(Clone, Copy)]
enum Item {
    Range(u32, u32),
    /// A named class in wide mode, which TRE matches by its predicate.
    Class(fn(char) -> bool),
}

/// TRE's `tre_parse_bracket_items` on the bracket expression whose contents start at `start`,
/// after the `[`: the index after its `]`, and its items. In byte mode a named class expands
/// into the runs of the 256 byte values it holds; in wide mode it is one item, or none when
/// negated, since TRE then checks it on every item.
fn bracket_items(
    chars: &[char],
    start: usize,
    ignore_case: bool,
    mode: Mode,
) -> Option<(usize, Bracket)> {
    let mut cursor = start;
    let negate = chars.get(cursor) == Some(&'^');
    if negate {
        cursor += 1;
    }
    let first = cursor;
    let mut items: SmallVec<[Item; 8]> = SmallVec::new();
    let mut has_class = false;
    loop {
        let character = *chars.get(cursor)?;
        if character == ']' && cursor > first {
            cursor += 1;
            break;
        }
        let (low, high) =
            if cursor + 2 < chars.len() && chars[cursor + 1] == '-' && chars[cursor + 2] != ']' {
                let (low, high) = (character as u32, chars[cursor + 2] as u32);
                if low > high {
                    return None;
                }
                cursor += 3;
                (low, high)
            } else if character == '[' && matches!(chars.get(cursor + 1), Some('.' | '=')) {
                return None;
            } else if character == '[' && chars.get(cursor + 1) == Some(&':') {
                let colon = (cursor + 2..chars.len()).find(|&at| chars[at] == ':')?;
                let name: String = chars[cursor + 2..colon].iter().take(63).collect();
                let member = class_predicate(&name)?;
                cursor = colon + 2;
                has_class = true;
                match mode {
                    Mode::Wide if !negate => items.push(Item::Class(member)),
                    Mode::Wide => {}
                    Mode::Bytes => {
                        let mut run: Option<u32> = None;
                        for code in 0..256 {
                            let character = char::from_u32(code)?;
                            let included = member(character)
                                || (ignore_case
                                    && (member(simple_lowercase(character))
                                        || member(simple_uppercase(character))));
                            match (included, run) {
                                (true, None) => run = Some(code),
                                (false, Some(run_start)) => {
                                    items.push(Item::Range(run_start, code - 1));
                                    run = None;
                                }
                                _ => {}
                            }
                        }
                        if let Some(run_start) = run {
                            items.push(Item::Range(run_start, 255));
                        }
                    }
                }
                continue;
            } else {
                if character == '-' && chars.get(cursor + 1) != Some(&']') && cursor != first {
                    return None;
                }
                cursor += 1;
                (character as u32, character as u32)
            };
        items.push(Item::Range(low, high));
        if ignore_case {
            push_case_counterparts(low, high, &mut items);
        }
    }
    Some((
        cursor,
        Bracket {
            negate,
            items,
            has_class,
        },
    ))
}

/// The opposite-case ranges TRE adds for the range `low..=high` under `ignore.case`.
fn push_case_counterparts(low: u32, high: u32, items: &mut SmallVec<[Item; 8]>) {
    let lower = |code: u32| char::from_u32(code).is_some_and(char::is_lowercase);
    let upper = |code: u32| char::from_u32(code).is_some_and(char::is_uppercase);
    let to_upper = |code: u32| {
        char::from_u32(code).map_or(code, |character| simple_uppercase(character) as u32)
    };
    let to_lower = |code: u32| {
        char::from_u32(code).map_or(code, |character| simple_lowercase(character) as u32)
    };
    let mut code = low;
    while code <= high {
        let convert: &dyn Fn(u32) -> u32 = if lower(code) {
            &to_upper
        } else if upper(code) {
            &to_lower
        } else {
            code += 1;
            continue;
        };
        let same_case = lower(code);
        let first = convert(code);
        let mut current = first;
        code += 1;
        while code <= high
            && (if same_case { lower(code) } else { upper(code) })
            && convert(code) == current + 1
        {
            current = convert(code);
            code += 1;
        }
        items.push(Item::Range(first, current));
    }
}

/// How many literals TRE's bracket expression becomes, and where the last one starts when it is
/// negated: one per item, or for a negated one, one per gap between the sorted items and one
/// for the rest.
fn leaf_count(bracket: &Bracket) -> (usize, u32) {
    if !bracket.negate {
        return (bracket.items.len(), 0);
    }
    let mut ranges: SmallVec<[(u32, u32); 8]> = bracket
        .items
        .iter()
        .filter_map(|item| match *item {
            Item::Range(low, high) => Some((low, high)),
            Item::Class(_) => None,
        })
        .collect();
    ranges.sort_unstable_by_key(|&(low, _)| low);
    let (mut current_min, mut current_max) = (0i64, 0i64);
    let mut leaves = 1;
    for &(low, high) in &ranges {
        let (low, high) = (i64::from(low), i64::from(high));
        if low < current_max {
            current_max = current_max.max(high + 1);
        } else {
            current_max = low - 1;
            if current_max >= current_min {
                leaves += 1;
            }
            current_min = high + 1;
            current_max = high + 1;
        }
    }
    (leaves, current_min as u32)
}

/// The characters of `class` that take the left side of TRE's union of `bracket`'s literals,
/// and those that take only its last literal. A character in an earlier literal takes the left
/// side even when the last literal holds it too, since the left side's tag compares first.
fn split_items(bracket: &Bracket, class: &ClassUnicode) -> Option<(ClassUnicode, ClassUnicode)> {
    let last = if bracket.negate {
        class_of([(leaf_count(bracket).1, CHAR_MAX)])
    } else {
        let (Item::Range(low, high), others) = bracket.items.split_last()? else {
            return None;
        };
        if high - low > SPLIT_LIMIT {
            return None;
        }
        let mut ranges: SmallVec<[(u32, u32); 8]> = SmallVec::new();
        for code in *low..=*high {
            let Some(character) = char::from_u32(code) else {
                continue;
            };
            let covered = others.iter().any(|item| match *item {
                Item::Range(low, high) => (low..=high).contains(&code),
                Item::Class(member) => member(character),
            });
            if covered {
                continue;
            }
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == code => *end = code,
                _ => ranges.push((code, code)),
            }
        }
        class_of(ranges)
    };
    let mut rest = class.clone();
    rest.difference(&last);
    let mut only_last = class.clone();
    only_last.intersect(&last);
    Some((rest, only_last))
}

/// The named classes TRE knows, as glibc's wide-character classes decide them for the 256 code
/// points byte mode expands over.
fn class_predicate(name: &str) -> Option<fn(char) -> bool> {
    Some(match name {
        "alnum" => |character: char| character.is_alphabetic() || character.is_ascii_digit(),
        "alpha" => char::is_alphabetic,
        "blank" => |character: char| matches!(character, '\t' | ' '),
        "cntrl" => |character: char| matches!(character as u32, 0..=31 | 127..=159),
        "digit" => |character: char| character.is_ascii_digit(),
        "graph" => |character: char| matches!(character as u32, 33..=126 | 160..=255),
        "lower" => char::is_lowercase,
        "print" => |character: char| matches!(character as u32, 32..=126 | 160..=255),
        "punct" => |character: char| {
            matches!(character as u32, 33..=126 | 160..=255)
                && !(character.is_alphabetic() || character.is_ascii_digit())
        },
        "space" => |character: char| matches!(character as u32, 9..=13 | 32),
        "upper" => char::is_uppercase,
        "xdigit" => |character: char| character.is_ascii_hexdigit(),
        _ => return None,
    })
}

fn simple_uppercase(character: char) -> char {
    let mut mapped = character.to_uppercase();
    match (mapped.next(), mapped.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

fn simple_lowercase(character: char) -> char {
    let mut mapped = character.to_lowercase();
    match (mapped.next(), mapped.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

/// What `tre_add_tags` records besides the tags it inserts into the tree.
struct Tagging {
    minimize: Vec<bool>,
    /// Per submatch, the tags of its start and its end.
    submatches: Vec<(u32, u32)>,
    parents: Vec<Vec<u32>>,
    minimal_pairs: Vec<(u32, u32)>,
}

/// The steps of TRE's `tre_add_tags`.
enum TagStep {
    Recurse(u32),
    SubmatchEnd(u32),
    AfterRepeat {
        node: u32,
        argument: u32,
        tagged: bool,
        enter_tag: u32,
        minimal: bool,
    },
    AfterUnionLeft,
    AfterUnionRight {
        node: u32,
        left: u32,
        right: u32,
        tagged: bool,
        bottom: usize,
        left_tag: u32,
        right_tag: u32,
    },
    AfterConcatLeft {
        next_tag: u32,
        reserved: Option<u32>,
    },
    AfterConcatRight {
        node: u32,
        left: u32,
        right: u32,
    },
}

/// TRE's `tre_add_tags`. The first pass counts the tags each subtree needs, which the second
/// pass reads to reserve tags while it inserts them and records their directions. Returns the
/// number of tags.
fn add_tags(nodes: &mut Vec<Node>, root: u32, first_pass: bool, tagging: &mut Tagging) -> u32 {
    // The submatch starts and ends waiting for the next tag; a union hides the entries its left
    // side left behind from its right side by raising `bottom`.
    let mut regset: Vec<u32> = Vec::new();
    let mut bottom = 0;
    let mut parents: Vec<u32> = Vec::new();
    let mut tag_count = 0;
    let mut tag = 0;
    let mut next_tag = 1;
    let mut minimal_tag: Option<u32> = None;
    let mut minimize = true;
    let mut stack = vec![TagStep::Recurse(root)];
    while let Some(step) = stack.pop() {
        match step {
            TagStep::SubmatchEnd(id) => {
                regset.push(id * 2 + 1);
                parents.pop();
            }
            TagStep::Recurse(node) => {
                let Node {
                    kind,
                    submatch,
                    submatch_count,
                    ..
                } = nodes[node as usize];
                if submatch >= 0 {
                    let id = submatch as u32;
                    regset.push(id * 2);
                    if !first_pass {
                        tagging.parents[id as usize] = parents.clone();
                    }
                    stack.push(TagStep::SubmatchEnd(id));
                }
                let mut place_tag = |nodes: &mut Vec<Node>, tag: u32, smallest: bool| {
                    tag_left(nodes, node, tag);
                    set_direction(&mut tagging.minimize, tag, smallest);
                    if let Some(start) = minimal_tag.take() {
                        tagging.minimal_pairs.push((tag, start));
                    }
                    purge(&regset[bottom..], &mut tagging.submatches, tag);
                };
                match kind {
                    Kind::Leaf { union, .. } => {
                        if regset.len() > bottom {
                            if first_pass {
                                nodes[node as usize].tag_count = 1;
                            } else {
                                place_tag(nodes, tag, minimize);
                            }
                            regset.truncate(bottom);
                            tag = next_tag;
                            tag_count += 1;
                            next_tag += 1;
                        }
                        if union {
                            minimize = false;
                        }
                    }
                    Kind::Concat(left, right) => {
                        stack.push(TagStep::AfterConcatRight { node, left, right });
                        stack.push(TagStep::Recurse(right));
                        let after_left = next_tag + nodes[left as usize].tag_count;
                        let reserved = (nodes[left as usize].tag_count > 0
                            && nodes[right as usize].tag_count > 0)
                            .then(|| {
                                next_tag += 1;
                                next_tag - 1
                            });
                        stack.push(TagStep::AfterConcatLeft {
                            next_tag: after_left,
                            reserved,
                        });
                        stack.push(TagStep::Recurse(left));
                    }
                    Kind::Repeat {
                        argument, minimal, ..
                    } => {
                        let tagged = regset.len() > bottom || minimal;
                        stack.push(TagStep::AfterRepeat {
                            node,
                            argument,
                            tagged,
                            enter_tag: tag,
                            minimal,
                        });
                        stack.push(TagStep::Recurse(argument));
                        if tagged {
                            if !first_pass {
                                place_tag(nodes, tag, !minimal && minimize);
                            }
                            regset.truncate(bottom);
                            tag = next_tag;
                            tag_count += 1;
                            next_tag += 1;
                        }
                        minimize = true;
                    }
                    Kind::Union(left, right) => {
                        let tagged = regset.len() > bottom;
                        let (left_tag, right_tag) = if tagged {
                            (next_tag, next_tag + 1)
                        } else {
                            (tag, next_tag)
                        };
                        stack.push(TagStep::AfterUnionRight {
                            node,
                            left,
                            right,
                            tagged,
                            bottom,
                            left_tag,
                            right_tag,
                        });
                        stack.push(TagStep::Recurse(right));
                        stack.push(TagStep::AfterUnionLeft);
                        stack.push(TagStep::Recurse(left));
                        if tagged {
                            if !first_pass {
                                place_tag(nodes, tag, minimize);
                            }
                            regset.truncate(bottom);
                            tag = next_tag;
                            tag_count += 1;
                            next_tag += 1;
                        }
                        if submatch_count > 0 {
                            next_tag += 1;
                            tag = next_tag;
                            next_tag += 1;
                        }
                    }
                    Kind::Empty | Kind::Assertion(_) | Kind::Tag(_) => {}
                }
                if submatch >= 0 {
                    parents.push(submatch as u32);
                }
            }
            TagStep::AfterRepeat {
                node,
                argument,
                tagged,
                enter_tag,
                minimal,
            } => {
                if first_pass {
                    nodes[node as usize].tag_count =
                        nodes[argument as usize].tag_count + u32::from(tagged);
                    minimal_tag = None;
                } else if minimal {
                    minimal_tag = Some(enter_tag);
                    minimize = true;
                } else {
                    minimize = false;
                }
            }
            TagStep::AfterConcatLeft {
                next_tag: after_left,
                reserved,
            } => {
                next_tag = after_left;
                if let Some(reserved) = reserved {
                    tag = reserved;
                }
            }
            TagStep::AfterConcatRight { node, left, right } => {
                if first_pass {
                    nodes[node as usize].tag_count =
                        nodes[left as usize].tag_count + nodes[right as usize].tag_count;
                }
            }
            TagStep::AfterUnionLeft => bottom = regset.len(),
            TagStep::AfterUnionRight {
                node,
                left,
                right,
                tagged,
                bottom: saved_bottom,
                left_tag,
                right_tag,
            } => {
                let has_submatches = nodes[node as usize].submatch_count > 0;
                if first_pass {
                    nodes[node as usize].tag_count = nodes[left as usize].tag_count
                        + nodes[right as usize].tag_count
                        + u32::from(tagged)
                        + if has_submatches { 2 } else { 0 };
                }
                bottom = saved_bottom;
                // The left side's tag is smaller, so a match through the left side wins.
                if has_submatches {
                    if !first_pass {
                        tag_right(nodes, left, left_tag);
                        set_direction(&mut tagging.minimize, left_tag, false);
                        tag_right(nodes, right, right_tag);
                        set_direction(&mut tagging.minimize, right_tag, false);
                    }
                    tag_count += 2;
                }
                minimize = false;
            }
        }
    }
    if !first_pass {
        purge(&regset[bottom..], &mut tagging.submatches, tag);
        if let Some(start) = minimal_tag {
            tagging.minimal_pairs.push((tag, start));
        }
    }
    tag_count
}

/// TRE's `tre_purge_regset`: the submatch starts and ends in `regset` take `tag`.
fn purge(regset: &[u32], submatches: &mut [(u32, u32)], tag: u32) {
    for &entry in regset {
        let submatch = &mut submatches[(entry / 2) as usize];
        if entry % 2 == 1 {
            submatch.1 = tag;
        } else {
            submatch.0 = tag;
        }
    }
}

fn set_direction(minimize: &mut Vec<bool>, tag: u32, smallest: bool) {
    let tag = tag as usize;
    if minimize.len() <= tag {
        minimize.resize(tag + 1, false);
    }
    minimize[tag] = smallest;
}

/// TRE's `tre_add_tag_left`: `node` becomes the tag followed by what `node` was.
fn tag_left(nodes: &mut Vec<Node>, node: u32, tag: u32) {
    let copy = push(nodes, nodes[node as usize].kind);
    let tag_node = push(nodes, Kind::Tag(tag));
    nodes[node as usize].kind = Kind::Concat(tag_node, copy);
}

/// TRE's `tre_add_tag_right`: `node` becomes what `node` was followed by the tag.
fn tag_right(nodes: &mut Vec<Node>, node: u32, tag: u32) {
    let copy = push(nodes, nodes[node as usize].kind);
    let tag_node = push(nodes, Kind::Tag(tag));
    nodes[node as usize].kind = Kind::Concat(copy, tag_node);
}

#[derive(Clone, Copy, PartialEq)]
enum CopyTags {
    Keep,
    Remove,
    MaximizeFirst,
}

/// TRE's `tre_expand_ast`: a repetition with a bound above one becomes copies of its argument,
/// innermost first, so that only `?`, `*` and `+` remain. Each copy's literals take new
/// positions after `positions`.
fn expand(
    nodes: &mut Vec<Node>,
    root: u32,
    minimize: &mut Vec<bool>,
    positions: &mut u32,
) -> Option<()> {
    let mut stack = vec![(root, false)];
    while let Some((node, after)) = stack.pop() {
        match nodes[node as usize].kind {
            Kind::Concat(left, right) | Kind::Union(left, right) => {
                stack.push((right, false));
                stack.push((left, false));
            }
            Kind::Repeat { argument, .. } if !after => {
                stack.push((node, true));
                stack.push((argument, false));
            }
            Kind::Repeat {
                argument,
                minimum,
                maximum,
                ..
            } if minimum > 1 || maximum > 1 => {
                let mut copy = |nodes: &mut Vec<Node>, tags: CopyTags| {
                    copy_tree(nodes, argument, tags, minimize, positions)
                };
                let mut sequence: Option<u32> = None;
                for copy_index in 0..minimum {
                    // Only the last of the required copies keeps its tags, and its first tag
                    // maximizes.
                    let tags = if copy_index + 1 < minimum {
                        CopyTags::Remove
                    } else {
                        CopyTags::MaximizeFirst
                    };
                    let copied = copy(nodes, tags);
                    sequence = Some(match sequence {
                        Some(before) => push(nodes, Kind::Concat(before, copied)),
                        None => copied,
                    });
                    if nodes.len() > NODE_LIMIT {
                        return None;
                    }
                }
                let mut optional: Option<u32> = None;
                if maximum == -1 {
                    let copied = copy(nodes, CopyTags::Keep);
                    optional = Some(push(
                        nodes,
                        Kind::Repeat {
                            argument: copied,
                            minimum: 0,
                            maximum: -1,
                            minimal: false,
                        },
                    ));
                } else {
                    for _ in minimum..maximum {
                        let copied = copy(nodes, CopyTags::Keep);
                        let rest = match optional {
                            Some(rest) => push(nodes, Kind::Concat(copied, rest)),
                            None => copied,
                        };
                        let empty = push(nodes, Kind::Empty);
                        optional = Some(push(nodes, Kind::Union(empty, rest)));
                        if nodes.len() > NODE_LIMIT {
                            return None;
                        }
                    }
                }
                let expanded = match (sequence, optional) {
                    (Some(sequence), Some(optional)) => {
                        push(nodes, Kind::Concat(sequence, optional))
                    }
                    (Some(only), None) | (None, Some(only)) => only,
                    (None, None) => continue,
                };
                nodes[node as usize].kind = nodes[expanded as usize].kind;
            }
            _ => {}
        }
        if nodes.len() > NODE_LIMIT {
            return None;
        }
    }
    Some(())
}

/// TRE's `tre_copy_ast`: a copy of the subtree at `source`, whose literals move to new
/// positions after `positions`, keeping which of them share one.
fn copy_tree(
    nodes: &mut Vec<Node>,
    source: u32,
    tags: CopyTags,
    minimize: &mut Vec<bool>,
    positions: &mut u32,
) -> u32 {
    let mut lowest = u32::MAX;
    let mut walk = vec![source];
    while let Some(node) = walk.pop() {
        match nodes[node as usize].kind {
            Kind::Leaf { position, .. } => lowest = lowest.min(position),
            Kind::Concat(left, right) | Kind::Union(left, right) => walk.extend([left, right]),
            Kind::Repeat { argument, .. } => walk.push(argument),
            Kind::Empty | Kind::Assertion(_) | Kind::Tag(_) => {}
        }
    }
    let offset = *positions;
    let root = push(nodes, Kind::Empty);
    let mut stack = vec![(source, root)];
    let mut first_tag = true;
    while let Some((from, to)) = stack.pop() {
        let kind = match nodes[from as usize].kind {
            Kind::Tag(_) if tags == CopyTags::Remove => Kind::Empty,
            Kind::Tag(tag) if tags == CopyTags::MaximizeFirst && first_tag => {
                set_direction(minimize, tag, false);
                first_tag = false;
                Kind::Tag(tag)
            }
            Kind::Leaf {
                set,
                guard,
                position,
                union,
            } => {
                let moved = position - lowest + offset;
                *positions = (*positions).max(moved + 1);
                Kind::Leaf {
                    set,
                    guard,
                    position: moved,
                    union,
                }
            }
            Kind::Concat(left, right) => {
                let (new_left, new_right) = (push(nodes, Kind::Empty), push(nodes, Kind::Empty));
                stack.push((right, new_right));
                stack.push((left, new_left));
                Kind::Concat(new_left, new_right)
            }
            Kind::Union(left, right) => {
                let (new_left, new_right) = (push(nodes, Kind::Empty), push(nodes, Kind::Empty));
                stack.push((right, new_right));
                stack.push((left, new_left));
                Kind::Union(new_left, new_right)
            }
            Kind::Repeat {
                argument,
                minimum,
                maximum,
                minimal,
            } => {
                let new_argument = push(nodes, Kind::Empty);
                stack.push((argument, new_argument));
                Kind::Repeat {
                    argument: new_argument,
                    minimum,
                    maximum,
                    minimal,
                }
            }
            other => other,
        };
        nodes[to as usize].kind = kind;
    }
    root
}

/// One member of a position set (TRE's `tre_pos_and_tags_t`): a state, with the assertions and
/// tags on the empty path to it. The tags are a range of [`Positions::tags`].
#[derive(Clone, Copy)]
struct Entry {
    state: u32,
    guard: u32,
    assertions: u8,
    tags_start: u32,
    tags_end: u32,
}

/// The position sets of every node, as ranges of `entries`.
struct Positions {
    entries: Vec<Entry>,
    tags: Vec<u32>,
}

impl Positions {
    /// TRE's `tre_set_union`: `first`'s members with `tags` and `assertions` added, then
    /// `second`'s unchanged.
    fn union(
        &mut self,
        first: (u32, u32),
        second: (u32, u32),
        tags: &[u32],
        assertions: u8,
    ) -> Option<(u32, u32)> {
        let start = self.entries.len() as u32;
        for index in first.0..first.1 {
            let entry = self.entries[index as usize];
            let (tags_start, tags_end) = if tags.is_empty() {
                (entry.tags_start, entry.tags_end)
            } else {
                let tags_start = self.tags.len();
                self.tags
                    .extend_from_within(entry.tags_start as usize..entry.tags_end as usize);
                self.tags.extend_from_slice(tags);
                (tags_start as u32, self.tags.len() as u32)
            };
            self.entries.push(Entry {
                assertions: entry.assertions | assertions,
                tags_start,
                tags_end,
                ..entry
            });
        }
        for index in second.0..second.1 {
            self.entries.push(self.entries[index as usize]);
        }
        (self.entries.len() + self.tags.len() <= ENTRY_LIMIT)
            .then_some((start, self.entries.len() as u32))
    }
}

/// The automaton's tables, before [`compile_mode`] adds the tag bookkeeping.
struct Automaton {
    state_sets: Vec<u32>,
    transition_starts: Vec<u32>,
    transitions: Vec<Transition>,
    initial: Vec<Transition>,
    transition_tags: Vec<u32>,
    final_state: u32,
}

/// TRE's `tre_compute_nfl` and `tre_ast_to_tnfa`: every position becomes a state, and each
/// concatenation and each `*` or `+` links the last positions of one side to the first
/// positions of the next. A transition keeps only the tags below `tag_count`, which are the
/// only ones TRE's matcher sets.
fn build_automaton(
    nodes: &[Node],
    root: u32,
    final_leaf: u32,
    position_count: u32,
    tag_count: usize,
) -> Option<Automaton> {
    let count = nodes.len();
    let mut nullable = vec![false; count];
    let mut first = vec![(0u32, 0u32); count];
    let mut last = vec![(0u32, 0u32); count];
    let mut position_states = vec![u32::MAX; position_count as usize];
    let mut state_sets = Vec::new();
    let mut positions = Positions {
        entries: Vec::new(),
        tags: Vec::new(),
    };
    let mut empty_path: Vec<u32> = Vec::new();
    let mut walk: Vec<u32> = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, after)) = stack.pop() {
        let index = node as usize;
        match nodes[index].kind {
            Kind::Leaf {
                set,
                guard,
                position,
                ..
            } => {
                let state = position_states.get_mut(position as usize)?;
                if *state == u32::MAX {
                    *state = state_sets.len() as u32;
                    state_sets.push(set);
                }
                let entry = positions.entries.len() as u32;
                positions.entries.push(Entry {
                    state: *state,
                    guard,
                    assertions: 0,
                    tags_start: 0,
                    tags_end: 0,
                });
                first[index] = (entry, entry + 1);
                last[index] = (entry, entry + 1);
            }
            Kind::Empty | Kind::Assertion(_) | Kind::Tag(_) => nullable[index] = true,
            Kind::Concat(left, right) | Kind::Union(left, right) if !after => {
                stack.push((node, true));
                stack.push((right, false));
                stack.push((left, false));
            }
            Kind::Repeat { argument, .. } if !after => {
                stack.push((node, true));
                stack.push((argument, false));
            }
            Kind::Union(left, right) => {
                let (left, right) = (left as usize, right as usize);
                nullable[index] = nullable[left] || nullable[right];
                first[index] = positions.union(first[left], first[right], &[], 0)?;
                last[index] = positions.union(last[left], last[right], &[], 0)?;
            }
            Kind::Concat(left, right) => {
                let (left, right) = (left as usize, right as usize);
                nullable[index] = nullable[left] && nullable[right];
                first[index] = if nullable[left] {
                    let assertions =
                        match_empty(nodes, left as u32, &nullable, &mut empty_path, &mut walk);
                    positions.union(first[right], first[left], &empty_path, assertions)?
                } else {
                    first[left]
                };
                last[index] = if nullable[right] {
                    let assertions =
                        match_empty(nodes, right as u32, &nullable, &mut empty_path, &mut walk);
                    positions.union(last[left], last[right], &empty_path, assertions)?
                } else {
                    last[right]
                };
            }
            Kind::Repeat {
                argument, minimum, ..
            } => {
                let argument = argument as usize;
                nullable[index] = minimum == 0 || nullable[argument];
                first[index] = first[argument];
                last[index] = last[argument];
            }
        }
    }

    let states = state_sets.len();
    let mut sources: Vec<u32> = Vec::new();
    let mut transitions: Vec<Transition> = Vec::new();
    let mut transition_tags: Vec<u32> = Vec::new();
    let mut make = |from: (u32, u32), to: (u32, u32)| -> Option<()> {
        for source in from.0..from.1 {
            let source = positions.entries[source as usize];
            let source_tags = &positions.tags[source.tags_start as usize..source.tags_end as usize];
            let mut previous_state = None;
            for target in to.0..to.1 {
                let target = positions.entries[target as usize];
                if previous_state == Some(target.state) {
                    continue;
                }
                previous_state = Some(target.state);
                let tags_start = transition_tags.len() as u32;
                transition_tags.extend(
                    source_tags
                        .iter()
                        .copied()
                        .chain(
                            positions.tags[target.tags_start as usize..target.tags_end as usize]
                                .iter()
                                .copied()
                                .filter(|tag| !source_tags.contains(tag)),
                        )
                        .filter(|&tag| (tag as usize) < tag_count),
                );
                sources.push(source.state);
                transitions.push(Transition {
                    target: target.state,
                    assertions: source.assertions | target.assertions,
                    guard: source.guard,
                    tags_start,
                    tags_end: transition_tags.len() as u32,
                });
            }
        }
        (transitions.len() <= TRANSITION_LIMIT).then_some(())
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match nodes[node as usize].kind {
            Kind::Concat(left, right) => {
                make(last[left as usize], first[right as usize])?;
                stack.push(right);
                stack.push(left);
            }
            Kind::Union(left, right) => {
                stack.push(right);
                stack.push(left);
            }
            Kind::Repeat {
                argument,
                minimum,
                maximum,
                ..
            } => {
                if maximum != -1 && maximum != 1 {
                    return None;
                }
                if maximum == -1 {
                    if minimum != 0 && minimum != 1 {
                        return None;
                    }
                    make(last[argument as usize], first[argument as usize])?;
                }
                stack.push(argument);
            }
            _ => {}
        }
    }

    let mut transition_starts = vec![0u32; states + 1];
    for &source in &sources {
        transition_starts[source as usize + 1] += 1;
    }
    for state in 0..states {
        transition_starts[state + 1] += transition_starts[state];
    }
    let mut fill = transition_starts.clone();
    let mut ordered = transitions.clone();
    for (&source, &transition) in sources.iter().zip(&transitions) {
        ordered[fill[source as usize] as usize] = transition;
        fill[source as usize] += 1;
    }
    let (initial_start, initial_end) = first[root as usize];
    let initial = (initial_start..initial_end)
        .map(|index| {
            let entry = positions.entries[index as usize];
            let tags_start = transition_tags.len() as u32;
            transition_tags.extend(
                positions.tags[entry.tags_start as usize..entry.tags_end as usize]
                    .iter()
                    .copied()
                    .filter(|&tag| (tag as usize) < tag_count),
            );
            Transition {
                target: entry.state,
                assertions: entry.assertions,
                guard: UNGUARDED,
                tags_start,
                tags_end: transition_tags.len() as u32,
            }
        })
        .collect();
    let final_state = positions.entries[first[final_leaf as usize].0 as usize].state;
    Some(Automaton {
        state_sets,
        transition_starts,
        transitions: ordered,
        initial,
        transition_tags,
        final_state,
    })
}

/// TRE's `tre_match_empty`: the tags (into `tags`) and assertions on the path through the
/// nullable `node` that matches the empty string, the left side of a union first.
fn match_empty(
    nodes: &[Node],
    node: u32,
    nullable: &[bool],
    tags: &mut Vec<u32>,
    walk: &mut Vec<u32>,
) -> u8 {
    tags.clear();
    walk.clear();
    walk.push(node);
    let mut assertions = 0;
    while let Some(node) = walk.pop() {
        match nodes[node as usize].kind {
            Kind::Tag(tag) => {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            Kind::Assertion(assertion) => assertions |= assertion,
            Kind::Union(left, right) => {
                if nullable[left as usize] {
                    walk.push(left);
                } else if nullable[right as usize] {
                    walk.push(right);
                }
            }
            Kind::Concat(left, right) => {
                walk.push(left);
                walk.push(right);
            }
            Kind::Repeat { argument, .. } => {
                if nullable[argument as usize] {
                    walk.push(argument);
                }
            }
            Kind::Empty | Kind::Leaf { .. } => {}
        }
    }
    assertions
}
