//! A reader for the assignments of a makefile such as GNU R's `etc/Makeconf` or a package's
//! `Makevars`, expanding variables exactly as GNU make does.
//!
//! `R CMD` reads GNU R's toolchain from it, and the interpreter reads which BLAS and LAPACK GNU R
//! links (`interp/src/linalg.rs`), so the reader lives here once and the interpreter includes this
//! file by path, because it does not link `rcmd`.

use std::collections::{BTreeMap, BTreeSet};

/// The variables a makefile defines, each held as text that `expand_make` expands to its
/// value. `+=` appends and `?=` assigns only once, as make does. `:=` expands its value at
/// the point of assignment, and the result is stored with each `$` doubled, so expanding it
/// later yields that text unchanged. `R_HOME` is `home` whatever the makefile assigns,
/// because R passes it on make's command line, which overrides the file.
///
/// An `include`, `-include` or `sinclude` line reads each file it names, after expanding the
/// line, through `read`, and its assignments take effect at that point, as in make. A file
/// `read` cannot supply is skipped; make stops on one that `include` names.
pub fn make_assignments(
    text: &str,
    home: &str,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut makefile = Makefile {
        variables: BTreeMap::from([("R_HOME".to_owned(), escape_make(home))]),
        simple: BTreeSet::from(["R_HOME".to_owned()]),
        read,
    };
    assign(&mut makefile, text, 0);
    makefile.variables
}

/// make has no limit on nested includes, but a file that includes itself never ends there.
const INCLUDE_DEPTH: usize = 32;

struct Makefile<'a> {
    variables: BTreeMap<String, String>,
    /// The names assigned with `:=`, whose appended text `+=` also expands at once.
    simple: BTreeSet<String>,
    read: &'a mut dyn FnMut(&str) -> Option<String>,
}

fn assign(makefile: &mut Makefile, text: &str, depth: usize) {
    let mut logical = String::new();
    for line in text.lines() {
        // make joins a continuation and the whitespace around it into one space.
        let line = if logical.is_empty() {
            line
        } else {
            line.trim_start()
        };
        if let Some(stripped) = line.strip_suffix('\\') {
            // Trimming the whole buffer collapses a run of blank continuation lines too.
            logical.push_str(stripped);
            logical.truncate(logical.trim_end().len());
            logical.push(' ');
            continue;
        }
        logical.push_str(line);
        let statement = std::mem::take(&mut logical);
        if statement.starts_with(['\t', '#']) {
            continue;
        }
        if let Some(files) = ["include", "-include", "sinclude"]
            .iter()
            .find_map(|keyword| statement.trim_start().strip_prefix(keyword))
            .filter(|rest| rest.starts_with([' ', '\t']) && !rest.contains('='))
        {
            let files = expand_make(files.split('#').next().unwrap_or(""), &makefile.variables);
            if depth < INCLUDE_DEPTH {
                for name in files.split_whitespace() {
                    if let Some(included) = (makefile.read)(name) {
                        assign(makefile, &included, depth + 1);
                    }
                }
            }
            continue;
        }
        let Some((name, value)) = statement.split_once('=') else {
            continue;
        };
        // A `#` starts a comment anywhere on an assignment line, as in make. make strips the
        // whitespace before a value but keeps the whitespace after it, even before a comment.
        let value = value.split('#').next().unwrap_or("").trim_start();
        let name = name.trim_end();
        let (name, operator) = match name.strip_suffix("::").or_else(|| name.strip_suffix(':')) {
            Some(stripped) => (stripped, Some(':')),
            None => match name.strip_suffix(['+', '?']) {
                Some(stripped) => (stripped, name.chars().last()),
                None => (name, None),
            },
        };
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) || name == "R_HOME" {
            continue;
        }
        let Makefile {
            variables, simple, ..
        } = &mut *makefile;
        match operator {
            Some('+') => {
                let value = match simple.contains(name) {
                    true => escape_make(&expand_make(value, variables)),
                    false => value.to_owned(),
                };
                let entry: &mut String = variables.entry(name.to_owned()).or_default();
                // make appends nothing, not even the joining space, for empty text.
                if value.is_empty() {
                    continue;
                }
                if !entry.is_empty() {
                    entry.push(' ');
                }
                entry.push_str(&value);
            }
            Some('?') => {
                variables
                    .entry(name.to_owned())
                    .or_insert_with(|| value.to_owned());
            }
            Some(':') => {
                let value = escape_make(&expand_make(value, variables));
                variables.insert(name.to_owned(), value);
                simple.insert(name.to_owned());
            }
            _ => {
                variables.insert(name.to_owned(), value.to_owned());
                simple.remove(name);
            }
        }
    }
}

/// Expands the variable references in `value` as make does: `$(NAME)`, `${NAME}`, a
/// one-character `$N`, a name built by a nested reference (`$(CXX$(STD))`), and `$$` to `$`.
/// An unknown name expands to nothing. A variable that refers to itself, directly or through
/// others, is an error in make; here the inner reference expands to nothing, so expansion
/// always ends and never repeats the cycle. Whitespace is kept, so a quoted path with a
/// space survives, and callers trim the result.
pub fn expand_make(value: &str, variables: &BTreeMap<String, String>) -> String {
    let mut expanded = String::with_capacity(value.len());
    expand_make_into(value, variables, &mut Vec::new(), &mut expanded);
    expanded
}

fn expand_make_into<'a>(
    value: &str,
    variables: &'a BTreeMap<String, String>,
    active: &mut Vec<&'a str>,
    expanded: &mut String,
) {
    let mut rest = value;
    while let Some(start) = rest.find('$') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let mut characters = after.chars();
        let (name, remainder) = match characters.next() {
            Some('$') => {
                expanded.push('$');
                rest = &after[1..];
                continue;
            }
            Some(open @ ('(' | '{')) => {
                let close = if open == '(' { ')' } else { '}' };
                let mut level = 0usize;
                let end = after.char_indices().find_map(|(index, character)| {
                    if character == open {
                        level += 1;
                    } else if character == close {
                        level -= 1;
                        if level == 0 {
                            return Some(index);
                        }
                    }
                    None
                });
                let Some(end) = end else {
                    expanded.push_str(&rest[start..]);
                    return;
                };
                let mut name = String::new();
                expand_make_into(&after[1..end], variables, active, &mut name);
                (name, &after[end + 1..])
            }
            Some(character) => (character.to_string(), characters.as_str()),
            None => {
                // make keeps a lone `$` at the end of a value.
                expanded.push('$');
                return;
            }
        };
        if let Some((name, inner)) = variables.get_key_value(name.as_str())
            && !active.contains(&name.as_str())
        {
            active.push(name);
            expand_make_into(inner, variables, active, expanded);
            active.pop();
        }
        rest = remainder;
    }
    expanded.push_str(rest);
}

/// Text that `expand_make` expands to `text` itself.
fn escape_make(text: &str) -> String {
    text.replace('$', "$$")
}
