//! The tools package's native routines: checksums, the text scanners behind package checks,
//! format strings, file permissions and processes. Rd parsing lives in `rd`; LaTeX parsing and
//! the help server are not available yet and fail with a clear error.

use crate::{
    builtin_libraries::{NativeCall, NativeFn},
    coerce::coerce_vector,
    unwind::{R, error},
    values::{self, alloc, length, nil, scalar_bool, scalar_integer, string_vector, symbols},
};
use heap::object::{NA_INTEGER, Sexp, SexpType};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("Rmd5", do_md5),
    ("check_nonASCII", do_check_non_ascii),
    ("check_nonASCII2", do_check_non_ascii2),
    ("nonASCII", do_non_ascii),
    ("delim_match", do_delim_match),
    ("doTabExpand", do_tab_expand),
    ("splitString", do_split_string),
    ("getfmts", do_getfmts),
    ("codeFilesAppend", do_code_files_append),
    ("dirchmod", do_dirchmod),
    ("ps_kill", do_ps_kill),
    ("ps_sigs", do_ps_sigs),
    ("ps_priority", do_ps_priority),
    ("package_dependencies_scan", do_package_dependencies_scan),
    // The help server cannot start, so `startDynamicHelp` warns and carries on without it.
    ("startHTTPD", |call| unsafe {
        let port = values::as_integer_scalar(call.arg(1));
        if port != NA_INTEGER && !(0..=65535).contains(&port) {
            return Err(error(format!(
                "Invalid port number {port}: should be in 0:65535, typically above 1024"
            )));
        }
        scalar_integer(-1)
    }),
    ("stopHTTPD", |_| Ok(nil())),
    ("parseRd", crate::rd::parse_rd),
    ("deparseRd", crate::rd::deparse_rd),
    ("parseLatex", crate::latex::parse_latex),
];

/// `Rmd5(files)`: the MD5 digest of each file in hexadecimal, `NA` when it cannot be read.
unsafe fn do_md5(call: &NativeCall<'_>) -> R<Sexp> {
    let files = call.arg(0);
    if files.kind() != SexpType::String {
        return Err(error("argument 'files' must be character"));
    }
    let digests: Vec<Option<String>> = values::strings(files)
        .into_iter()
        .map(|file| {
            let path = crate::connections::expand_path(&file?);
            let bytes = std::fs::read(path).ok()?;
            Some(
                md5(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            )
        })
        .collect();
    values::string_vector_na(&digests)
}

/// MD5 as RFC 1321 specifies it.
fn md5(message: &[u8]) -> [u8; 16] {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let constants: Vec<u32> = (0..64)
        .map(|index: i32| (((index + 1) as f64).sin().abs() * 4294967296.0) as u32)
        .collect();
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64).wrapping_mul(8)).to_le_bytes());
    for block in padded.chunks_exact(64) {
        let words: Vec<u32> = block
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for round in 0..64 {
            let (mixed, word) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            let rotated = a
                .wrapping_add(mixed)
                .wrapping_add(constants[round])
                .wrapping_add(words[word]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated.rotate_left(SHIFTS[round]));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 16];
    for (chunk, word) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    digest
}

/// `check_nonASCII(text, ignore_quotes)`: whether any line of R code has a non-ASCII byte
/// outside a comment. Quotes are always followed, so a `#` inside a string starts no comment,
/// but a string's own bytes only count when `ignore_quotes`.
unsafe fn do_check_non_ascii(call: &NativeCall<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("invalid input"));
    }
    let ignore_quotes = values::as_logical_scalar(call.arg(1)) == 1;
    for index in 0..length(text) {
        let charsxp = text.element(index);
        if charsxp == values::na_string() {
            continue;
        }
        let mut quote: Option<u8> = None;
        let mut escaped = false;
        for &byte in charsxp.bytes() {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
                continue;
            } else if let Some(open) = quote {
                if byte == open {
                    quote = None;
                }
            } else if byte == b'#' {
                break;
            } else if byte == b'"' || byte == b'\'' {
                quote = Some(byte);
                continue;
            }
            if byte > 127 && (quote.is_none() || ignore_quotes) {
                return scalar_bool(true);
            }
        }
    }
    scalar_bool(false)
}

/// `check_nonASCII2(text)`: the (1-based) indices of the elements with a non-ASCII byte.
unsafe fn do_check_non_ascii2(call: &NativeCall<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("invalid input"));
    }
    let indices: Vec<i32> = (0..length(text))
        .filter(|&index| {
            let charsxp = text.element(index);
            charsxp != values::na_string() && charsxp.bytes().iter().any(|byte| *byte > 127)
        })
        .map(|index| index as i32 + 1)
        .collect();
    values::integer_vector(&indices)
}

/// `nonASCII(text)`: whether each element has a non-ASCII byte; `NA` counts as ASCII.
unsafe fn do_non_ascii(call: &NativeCall<'_>) -> R<Sexp> {
    let text = call.arg(0);
    if text.kind() != SexpType::String {
        return Err(error("invalid input"));
    }
    let result = alloc(SexpType::Logical, length(text))?;
    for index in 0..length(text) {
        let charsxp = text.element(index);
        let non_ascii =
            charsxp != values::na_string() && charsxp.bytes().iter().any(|byte| *byte > 127);
        values::ints_mut(result)[index] = non_ascii as i32;
    }
    Ok(result)
}

/// `delim_match(x, delims)`: the position and length of the first balanced pair of the two
/// delimiters in each string, as `regexpr` reports a match, or -1. A backslash escapes the next
/// character, and `%` starts an Rd comment that runs to the end of the line.
unsafe fn do_delim_match(call: &NativeCall<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let delims = call.arg(1);
    if x.kind() != SexpType::String {
        return Err(error("invalid 'x' argument"));
    }
    let pair: Vec<char> = values::strings(delims)
        .into_iter()
        .flatten()
        .flat_map(|text| text.chars().collect::<Vec<_>>())
        .collect();
    if delims.kind() != SexpType::String || length(delims) != 2 || pair.len() != 2 {
        return Err(error("argument 'delim' must specify two characters"));
    }
    let (open, close) = (pair[0], pair[1]);
    let count = length(x);
    let mut starts = Vec::with_capacity(count);
    let mut lengths = Vec::with_capacity(count);
    for text in values::strings(x) {
        let found = text.as_deref().and_then(|text| {
            let mut depth = 0usize;
            let mut start = 0usize;
            let mut escaped = false;
            let mut in_comment = false;
            for (position, character) in text.chars().enumerate() {
                if in_comment {
                    in_comment = character != '\n';
                    continue;
                }
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '%' {
                    in_comment = true;
                } else if character == open {
                    if depth == 0 {
                        start = position;
                    }
                    depth += 1;
                } else if character == close && depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start as i32 + 1, (position - start) as i32 + 1));
                    }
                }
            }
            None
        });
        let (start, length) = found.unwrap_or((-1, -1));
        starts.push(start);
        lengths.push(length);
    }
    let result = values::integer_vector(&starts)?;
    let lengths = values::integer_vector(&lengths)?;
    values::set_attr(result, values::install("match.length")?, lengths)?;
    Ok(result)
}

/// `doTabExpand(strings, starts)`: tabs replaced by spaces up to the next multiple of 8
/// columns, counting from each string's starting column; a newline returns to column 0.
unsafe fn do_tab_expand(call: &NativeCall<'_>) -> R<Sexp> {
    let strings = call.arg(0);
    let starts = coerce_vector(call.arg(1), SexpType::Integer)?;
    let starts = values::ints(starts);
    let mut expanded = Vec::with_capacity(length(strings));
    for (index, text) in values::strings(strings).into_iter().enumerate() {
        let Some(text) = text else {
            expanded.push(None);
            continue;
        };
        let mut column = starts
            .get(index % starts.len().max(1))
            .copied()
            .unwrap_or(0)
            .max(0) as usize;
        let mut output = String::with_capacity(text.len());
        for character in text.chars() {
            match character {
                '\t' => {
                    let spaces = 8 - column % 8;
                    output.extend(std::iter::repeat_n(' ', spaces));
                    column += spaces;
                }
                '\n' => {
                    output.push('\n');
                    column = 0;
                }
                other => {
                    output.push(other);
                    column += 1;
                }
            }
        }
        expanded.push(Some(output));
    }
    values::string_vector_na(&expanded)
}

/// `splitString(string, delims)`: the string cut before and after every delimiter character,
/// each delimiter being an element of its own.
unsafe fn do_split_string(call: &NativeCall<'_>) -> R<Sexp> {
    let text = values::require_string(call.arg(0), "string")?;
    let delims = values::require_string(call.arg(1), "delims")?;
    let mut pieces = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if delims.contains(character) {
            if !current.is_empty() {
                pieces.push(std::mem::take(&mut current));
            }
            pieces.push(character.to_string());
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    string_vector(&pieces)
}

/// `getfmts(format)`: the conversion specifications of a `sprintf` format, one per argument
/// they consume: without `%%`, a `*` width or precision taking an argument of its own before
/// its specification, and an `n$` specification put at argument `n` without the position.
unsafe fn do_getfmts(call: &NativeCall<'_>) -> R<Sexp> {
    let format = values::require_string(call.arg(0), "format")?;
    let bytes = format.as_bytes();
    let mut specifications: Vec<String> = Vec::new();
    let mut argument = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'%') {
            index += 2;
            continue;
        }
        let start = index;
        index += 1;
        let digits_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        let mut specification = String::from("%");
        if bytes.get(index) == Some(&b'$') {
            let position: usize = format[digits_start..index].parse().unwrap_or(1);
            argument = position.max(1) - 1;
            index += 1;
        } else {
            index = digits_start;
        }
        let mut stars = 0;
        while let Some(&byte) = bytes.get(index) {
            if b"-+ #0.123456789".contains(&byte) {
                specification.push(byte as char);
            } else if byte == b'*' {
                stars += 1;
                specification.push('*');
            } else {
                break;
            }
            index += 1;
        }
        match bytes.get(index) {
            Some(&conversion) if b"aAdifeEgGosxX".contains(&conversion) => {
                specification.push(conversion as char);
                index += 1;
            }
            _ => {
                let end = (index + 1).min(bytes.len());
                let shown = String::from_utf8_lossy(&bytes[start..end]);
                return Err(error(format!(
                    "unrecognised format specification '{shown}'"
                )));
            }
        }
        for entry in
            std::iter::repeat_n("*".to_owned(), stars).chain(std::iter::once(specification))
        {
            if specifications.len() <= argument {
                specifications.resize(argument + 1, String::new());
            }
            specifications[argument] = entry;
            argument += 1;
        }
    }
    string_vector(&specifications)
}

/// `codeFilesAppend(file1, file2)`: appends each file of `file2` to `file1`, after a `#line`
/// directive naming it; whether each append worked.
unsafe fn do_code_files_append(call: &NativeCall<'_>) -> R<Sexp> {
    use std::io::Write;
    let target = values::require_string(call.arg(0), "file1")?;
    let sources = call.arg(1);
    if sources.kind() != SexpType::String {
        return Err(error("invalid 'file2' argument"));
    }
    let target = crate::connections::expand_path(&target);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&target)
        .map_err(|failure| error(format!("cannot open file '{target}': {failure}")))?;
    let mut outcomes = Vec::with_capacity(length(sources));
    for source in values::strings(sources) {
        let appended = source.is_some_and(|source| {
            let Ok(mut content) = std::fs::read(crate::connections::expand_path(&source)) else {
                return false;
            };
            if !content.ends_with(b"\n") && !content.is_empty() {
                content.push(b'\n');
            }
            let header = format!("#line 1 \"{source}\"\n");
            file.write_all(header.as_bytes())
                .and_then(|()| file.write_all(&content))
                .is_ok()
        });
        outcomes.push(appended as i32);
    }
    values::logical_vector(&outcomes)
}

/// `dirchmod(dir, group.writable)`: makes everything under `dir` readable by all: files 644
/// (or 664), or 755 (775) when the owner may execute them, and directories 755 (775).
unsafe fn do_dirchmod(call: &NativeCall<'_>) -> R<Sexp> {
    use std::os::unix::fs::PermissionsExt;
    let directory = values::require_string(call.arg(0), "dir")?;
    let group_writable = values::as_logical_scalar(call.arg(1)) == 1;
    let group = if group_writable { 0o020 } else { 0 };
    let mut pending = vec![std::path::PathBuf::from(crate::connections::expand_path(
        &directory,
    ))];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&entry_path) else {
                continue;
            };
            let mode = if metadata.is_dir() {
                pending.push(entry_path.clone());
                0o755 | group
            } else if metadata.is_file() {
                let executable = metadata.permissions().mode() & 0o100 != 0;
                (if executable { 0o755 } else { 0o644 }) | group
            } else {
                continue;
            };
            std::fs::set_permissions(&entry_path, std::fs::Permissions::from_mode(mode)).map_err(
                |failure| {
                    error(format!(
                        "cannot change permissions of '{}': {failure}",
                        entry_path.display()
                    ))
                },
            )?;
        }
    }
    Ok(nil())
}

/// `ps_kill(pid, signal)`: sends the signal to each process; whether each send worked.
unsafe fn do_ps_kill(call: &NativeCall<'_>) -> R<Sexp> {
    let pids = coerce_vector(call.arg(0), SexpType::Integer)?;
    let signal = values::as_integer_scalar(call.arg(1));
    let outcomes: Vec<i32> = values::ints(pids)
        .iter()
        .map(|&pid| {
            (pid != NA_INTEGER && pid > 0 && signal != NA_INTEGER && libc::kill(pid, signal) == 0)
                as i32
        })
        .collect();
    values::logical_vector(&outcomes)
}

/// `ps_sigs(code)`: the platform's number for a signal named by its BSD number, which is how
/// the `SIGxxx` constants of tools are defined; `NA` for the signals tools does not name.
unsafe fn do_ps_sigs(call: &NativeCall<'_>) -> R<Sexp> {
    let signal = match values::as_integer_scalar(call.arg(0)) {
        1 => libc::SIGHUP,
        2 => libc::SIGINT,
        3 => libc::SIGQUIT,
        9 => libc::SIGKILL,
        15 => libc::SIGTERM,
        17 => libc::SIGSTOP,
        18 => libc::SIGTSTP,
        19 => libc::SIGCONT,
        20 => libc::SIGCHLD,
        30 => libc::SIGUSR1,
        31 => libc::SIGUSR2,
        _ => NA_INTEGER,
    };
    scalar_integer(signal)
}

/// `ps_priority(pid, value)`: each process's niceness, after setting it to `value` unless
/// that is `NA`; `NA` where the process cannot be queried.
unsafe fn do_ps_priority(call: &NativeCall<'_>) -> R<Sexp> {
    let pids = coerce_vector(call.arg(0), SexpType::Integer)?;
    let value = values::as_integer_scalar(call.arg(1));
    let mut niceness = Vec::with_capacity(length(pids));
    for &pid in values::ints(pids) {
        if pid == NA_INTEGER || pid <= 0 {
            niceness.push(NA_INTEGER);
            continue;
        }
        // getpriority may return -1 legitimately, so success is told apart through errno.
        *libc::__errno_location() = 0;
        let current = libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t);
        if current == -1 && *libc::__errno_location() != 0 {
            niceness.push(NA_INTEGER);
            continue;
        }
        if value != NA_INTEGER {
            libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, value);
        }
        niceness.push(current);
    }
    let result = values::integer_vector(&niceness)?;
    let names = values::get_attr(call.arg(0), symbols().names);
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// `package_dependencies_scan(x)`: the package names a dependency field mentions: runs of
/// letters, digits and dots that start with a letter, other than `R` itself.
unsafe fn do_package_dependencies_scan(call: &NativeCall<'_>) -> R<Sexp> {
    let fields = call.arg(0);
    if fields.kind() != SexpType::String {
        return Err(error("non-character argument"));
    }
    let mut names = Vec::new();
    for field in values::strings(fields).into_iter().flatten() {
        let bytes = field.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if !bytes[index].is_ascii_alphabetic() {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'.')
            {
                index += 1;
            }
            let name = &field[start..index];
            if name != "R" {
                names.push(name.to_owned());
            }
        }
    }
    string_vector(&names)
}
