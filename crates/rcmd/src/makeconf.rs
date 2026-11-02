//! A reader for the `NAME = value` assignments of GNU R's `etc/Makeconf`.
//!
//! `R CMD` reads GNU R's toolchain from it, and the interpreter reads which BLAS and LAPACK GNU R
//! links (`interp/src/linalg.rs`), so the reader lives here once and the interpreter includes this
//! file by path, because it does not link `rcmd`.

use std::collections::BTreeMap;

/// The `NAME = value` assignments of a makefile, with backslash continuations joined. `+=`
/// appends and `?=` assigns only once, as make does.
pub fn make_assignments(text: &str) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    let mut logical = String::new();
    for line in text.lines() {
        // make joins a continuation and the whitespace around it into one space.
        let line = if logical.is_empty() {
            line
        } else {
            line.trim_start()
        };
        if let Some(stripped) = line.strip_suffix('\\') {
            logical.push_str(stripped.trim_end());
            logical.push(' ');
            continue;
        }
        logical.push_str(line);
        let statement = std::mem::take(&mut logical);
        if statement.starts_with(['\t', '#']) {
            continue;
        }
        let Some((name, value)) = statement.split_once('=') else {
            continue;
        };
        // A `#` starts a comment anywhere on an assignment line, as in make.
        let value = value.split('#').next().unwrap_or("").trim().to_owned();
        let (name, operator) = match name.trim_end().strip_suffix(['+', '?', ':']) {
            Some(stripped) => (stripped.trim(), name.trim_end().chars().last()),
            None => (name.trim(), None),
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        match operator {
            Some('+') => {
                let entry: &mut String = variables.entry(name.to_owned()).or_default();
                if !entry.is_empty() {
                    entry.push(' ');
                }
                entry.push_str(&value);
            }
            Some('?') => {
                variables.entry(name.to_owned()).or_insert(value);
            }
            _ => {
                variables.insert(name.to_owned(), value);
            }
        }
    }
    variables
}

/// Expands `$(NAME)` and `${NAME}` in `value`, including a name built by a nested reference
/// (`$(CXX$(STD))`), and `$$` to `$`. An unknown name expands to nothing, as in make, and the
/// depth bound stops a self-referencing assignment. Whitespace inside the value is kept, so a
/// quoted path with a space survives.
pub fn expand_make(
    value: &str,
    variables: &BTreeMap<String, String>,
    home: &str,
    depth: usize,
) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('$') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let (open, close) = match after.chars().next() {
            Some('(') => ('(', ')'),
            Some('{') => ('{', '}'),
            Some('$') => {
                expanded.push('$');
                rest = &after[1..];
                continue;
            }
            _ => {
                expanded.push('$');
                rest = after;
                continue;
            }
        };
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
            rest = "";
            break;
        };
        let name = expand_make(&after[1..end], variables, home, depth + 1);
        let name = name.as_str();
        if name == "R_HOME" {
            expanded.push_str(home);
        } else if depth < 16
            && let Some(inner) = variables.get(name)
        {
            expanded.push_str(&expand_make(inner, variables, home, depth + 1));
        }
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    expanded.trim().to_owned()
}
