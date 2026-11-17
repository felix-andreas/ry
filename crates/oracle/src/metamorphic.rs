//! Metamorphic R programs over a zoo of values, one per seed.
//!
//! [`fuzz::program`](crate::fuzz::program) draws from shapes its author thought of. The programs
//! here start from the other end: a *value* drawn from a zoo that covers every atomic type, the
//! lengths around the runtime's size thresholds, compact sequences, names and strings in UTF-8,
//! latin1 and bytes, matrices, arrays, data frames, factors and dates, and a random *body* of
//! one to five statements that mutate and read it. A body alone says little, so each one runs in
//! many forms that R's semantics require to agree: called again and again (a body compiles after
//! its first call), in `local()`, in an anonymous function, at top level, with the variable
//! renamed, on a value that went through `serialize`, through `do.call` and `lapply`, with a
//! collection between its statements, and after calls on values of other types and sizes.
//! Every form's result is compared with the first by `identical()` inside the program, and the
//! first is printed, so GNU R checks both the result and the claim that the forms agree. A form
//! that shares its input with a second variable also checks that the mutation did not write
//! through to it. A program therefore checks itself: a line that ends in `DIFFERENT` is a bug
//! in the engine that printed it, with or without GNU R to compare with.
//!
//! [`builtins_program`] is the same zoo fed to builtins instead: one or two values and a few
//! dozen calls on them, each a top-level value the deep observer dumps. Its calls are written
//! by hand, so each is a sensible use of its function. [`api_program`] draws its calls from
//! the table of every function base, stats and utils export instead, with arguments nobody
//! chose: it reaches the functions and the argument checks no one thought to test.
//! [`io_program`] writes zoo values to files and connections and reads them back: a string's
//! bytes and encoding mark, a number's digits and a table's cells must come back as GNU R
//! brings them back, whatever the writer, the reader and the encoding between them.
//!
//! Swarm testing: a seed first draws the subset of [`Feature`]s its programs may use, so a rare
//! pairing (raw vectors and `dim<-`, say) is not drowned out by the common ones.
//!
//! Every program is deterministic under GNU R, and none prints an environment or a closure.

use {
    std::{
        collections::{BTreeMap, BTreeSet},
        sync::LazyLock,
    },
    syntax::testing::SplitMix64,
};

/// One thing a program may contain. A seed enables each with probability one half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Feature {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
    List,
    Matrix,
    Array,
    DataFrame,
    Factor,
    Date,
    Names,
    NonAscii,
    OtherEncodings,
    SpecialNumbers,
    LargeSizes,
    CompactSequences,
    Attributes,
    IllTyped,
    Loops,
    SuperAssignment,
    Nested,
    Aliases,
    Guards,
    Repeated,
    Scopes,
    Serialized,
    Renamed,
    Callers,
    Shared,
    Collected,
    Warmed,
}

const FEATURES: [Feature; 33] = [
    Feature::Logical,
    Feature::Integer,
    Feature::Double,
    Feature::Complex,
    Feature::Character,
    Feature::Raw,
    Feature::List,
    Feature::Matrix,
    Feature::Array,
    Feature::DataFrame,
    Feature::Factor,
    Feature::Date,
    Feature::Names,
    Feature::NonAscii,
    Feature::OtherEncodings,
    Feature::SpecialNumbers,
    Feature::LargeSizes,
    Feature::CompactSequences,
    Feature::Attributes,
    Feature::IllTyped,
    Feature::Loops,
    Feature::SuperAssignment,
    Feature::Nested,
    Feature::Aliases,
    Feature::Guards,
    Feature::Repeated,
    Feature::Scopes,
    Feature::Serialized,
    Feature::Renamed,
    Feature::Callers,
    Feature::Shared,
    Feature::Collected,
    Feature::Warmed,
];

/// The set of [`Feature`]s one seed's program draws from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Features(u64);

impl Features {
    pub fn all() -> Self {
        Features((1 << FEATURES.len()) - 1)
    }

    /// Each feature with probability one half, with at least one element type.
    pub fn swarm(rng: &mut SplitMix64) -> Self {
        let mut bits = rng.next_u64() & Features::all().0;
        const ELEMENT_TYPES: u64 = (1 << (Feature::List as u8 + 1)) - 1;
        if bits & ELEMENT_TYPES == 0 {
            bits |= 1 << rng.below(Feature::List as usize + 1);
        }
        Features(bits)
    }

    pub fn has(self, feature: Feature) -> bool {
        self.0 & (1 << feature as u8) != 0
    }

    /// The enabled features by name, for a program's header.
    pub fn names(self) -> String {
        let names: Vec<String> = FEATURES
            .iter()
            .filter(|feature| self.has(**feature))
            .map(|feature| format!("{feature:?}"))
            .collect();
        names.join(" ")
    }
}

/// The metamorphic program for `seed`. The same seed always gives the same text.
pub fn program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x6d65_7461_6d6f_7270);
    // One seed in eight uses everything, so that the whole space is also sampled densely.
    let features = if rng.chance(1, 8) {
        Features::all()
    } else {
        Features::swarm(&mut rng)
    };
    let mut zoo = Zoo {
        rng,
        features,
        longest: usize::MAX,
    };
    let mut text = format!(
        "# oracle metamorphic program, seed {seed}\n# features: {}\n{PREAMBLE}",
        features.names()
    );
    // An engine takes longer to start than to run a trial, so a program holds many.
    for trial in 1..=6 + zoo.rng.below(7) {
        text.push_str(&zoo.trial(trial));
    }
    text
}

/// A program that binds one or two zoo values and calls a few dozen builtins on them, each
/// call a top-level value. An error is caught and its message is the value.
pub fn builtins_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x6275_696c_7469_6e73);
    let features = if rng.chance(1, 8) {
        Features::all()
    } else {
        Features::swarm(&mut rng)
    };
    let mut zoo = Zoo {
        rng,
        features,
        longest: usize::MAX,
    };
    let mut text = format!(
        "# oracle builtins program, seed {seed}\n# features: {}\ntry_ <- function(expr) tryCatch(expr, error = function(e) paste(\"error:\", conditionMessage(e)))\n",
        features.names()
    );
    for round in 0..1 + zoo.rng.below(2) {
        let first = zoo.value();
        let second = zoo.value();
        text.push_str(&format!(
            "v <- try_({})\nw <- try_({})\n",
            first.text, second.text
        ));
        for _ in 0..10 + zoo.rng.below(14) {
            let template = BUILTIN_CALLS[zoo.rng.below(BUILTIN_CALLS.len())];
            // A call whose result has a cell per pair of elements would ask for gigabytes.
            let quadratic = QUADRATIC_CALLS.iter().any(|name| template.contains(name));
            let template = if quadratic && first.length.max(second.length) > 300 {
                "length(#)"
            } else {
                template
            };
            let small = zoo.small_count().to_string();
            let call = template
                .replace('#', "v")
                .replace('@', "w")
                .replace('~', &small);
            let line = match zoo.rng.below(12) {
                // The same call twice must give the same value, and a call must not change
                // its argument.
                0 => format!("try_(identical({call}, {call}))"),
                1 => format!("try_(local({call}))"),
                2 => format!("try_((function(v, w) {call})(v, w))"),
                _ => format!("try_({call})"),
            };
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str(&format!(
            "try_(identical(v, {}))\ntry_(identical(w, {}))\n",
            first.text, second.text
        ));
        if round == 0 && zoo.rng.chance(1, 2) {
            text.push_str("v <- unserialize(serialize(v, NULL))\n");
        }
    }
    text
}

/// One function of base, stats or utils, from the table `api_functions.R` writes.
pub struct ApiFunction {
    pub package: &'static str,
    pub name: &'static str,
    /// An S3 method, which a program calls less often than a function users call by name.
    pub method: bool,
    /// How many leading arguments a call may pass by position, or `None` for a function no
    /// program calls: one that reaches outside the process or need not agree between engines.
    pub positional: Option<usize>,
    /// The formals a call may pass by name, each with its position.
    formals: Vec<(usize, &'static str)>,
}

/// Every function GNU R's base, stats and utils export, in the table's order.
pub fn api_functions() -> &'static [ApiFunction] {
    static TABLE: LazyLock<Vec<ApiFunction>> = LazyLock::new(|| {
        include_str!("api_functions.txt")
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let package = fields.next()?;
                let name = fields.next()?;
                let method = fields.next()? == "method";
                let positional = fields.next()?.parse().ok();
                let formals = fields
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .enumerate()
                    .filter(|(_, formal)| {
                        !formal.is_empty() && *formal != "..." && !formal.ends_with('!')
                    })
                    .collect();
                Some(ApiFunction {
                    package,
                    name,
                    method,
                    positional,
                    formals,
                })
            })
            .collect()
    });
    &TABLE
}

/// The R program that calls functions of the whole base, stats and utils interface on the zoo:
/// two values and a few dozen calls, each with up to three arguments by position and sometimes
/// one by name. The arguments are the two values, a small count, or a constant of another type,
/// so most calls are ones nobody wrote a test for: a function given a type it must reject, an
/// argument of length zero, or a name in another encoding. Every call's value is a top-level
/// value the deep observer dumps, with its warnings, and some are printed as well.
pub fn api_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x6170_695f_6361_6c6c);
    let features = if rng.chance(1, 8) {
        Features::all()
    } else {
        Features::swarm(&mut rng)
    };
    let mut zoo = Zoo {
        rng,
        features,
        longest: API_LONGEST,
    };
    // Base functions get most of the calls: every other function is built from them.
    let pools = [
        "base", "base", "base", "base", "base", "stats", "stats", "utils",
    ]
    .map(|package| {
        let methods = package == "base";
        api_functions()
            .iter()
            .filter(|function| {
                function.positional.is_some()
                    && function.package == package
                    && !(methods && function.method)
            })
            .collect::<Vec<_>>()
    });
    let methods: Vec<&ApiFunction> = api_functions()
        .iter()
        .filter(|function| function.positional.is_some() && function.method)
        .collect();
    let mut text = format!(
        "# oracle api program, seed {seed}\n# features: {}\n{API_PREAMBLE}set.seed({})\n",
        features.names(),
        seed % 1000
    );
    for _ in 0..1 + zoo.rng.below(2) {
        let first = zoo.value();
        let second = zoo.value();
        text.push_str(&format!(
            "v <- try_({})\nw <- try_({})\n",
            first.text, second.text
        ));
        for _ in 0..12 + zoo.rng.below(12) {
            // A generic reaches its methods anyway, so a direct call of one is rare.
            let pool = match zoo.rng.chance(1, 8) {
                true => &methods,
                false => &pools[zoo.rng.below(pools.len())],
            };
            let function = pool[zoo.rng.below(pool.len())];
            let call = api_call(&mut zoo, function);
            text.push_str(&match zoo.rng.chance(1, 3) {
                true => format!("r <- show_(try_({call}))\n"),
                false => format!("r <- try_({call})\n"),
            });
        }
        text.push_str(&format!(
            "try_(identical(v, {}))\ntry_(identical(w, {}))\n",
            first.text, second.text
        ));
    }
    text
}

/// The longest value an api program builds. A function may be quadratic in its argument, and
/// 513 elements keep such a call to a quarter of a million cells.
const API_LONGEST: usize = 513;

/// `show_` prints a value unless it holds a function, an environment or a pointer, whose
/// printed form would carry an address.
const API_PREAMBLE: &str = r#"try_ <- function(expr) tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))
plain_ <- function(x) {
  if (is.function(x) || is.environment(x) || typeof(x) %in% c("externalptr", "weakref", "S4", "bytecode")) return(FALSE)
  all(vapply(c(if (is.list(x)) unclass(x), attributes(x)), plain_, NA))
}
show_ <- function(x) {
  try_(if (plain_(x)) print(x))
  invisible(x)
}
"#;

/// Arguments of other types than the zoo's two values: a call that takes a function, a
/// language object, a count or a flag gets one now and then.
const API_CONSTANTS: [&str; 22] = [
    "NULL",
    "TRUE",
    "FALSE",
    "NA",
    "0L",
    "1L",
    "2",
    "-1",
    "0.5",
    "Inf",
    "NA_character_",
    "\"a\"",
    "\"\"",
    "\"sum\"",
    "character(0)",
    "1:3",
    "c(2, 1)",
    "list(1, \"b\")",
    "length",
    "function(x, ...) x",
    "quote(v + 1)",
    "quote(x)",
];

/// One call of `function`. A replacement function is sometimes called as a replacement, which
/// goes through the `*tmp*` protocol instead of an ordinary call.
fn api_call(zoo: &mut Zoo, function: &ApiFunction) -> String {
    let limit = function.positional.unwrap_or(0);
    let count = [1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 0][zoo.rng.below(16)].min(limit);
    let mut arguments: Vec<String> = (0..count)
        .map(|position| match (position, zoo.rng.below(10)) {
            (0, 0..=7) => "v".to_owned(),
            (1, 0..=5) => "w".to_owned(),
            (2, 0..=4) => zoo.small_count().to_string(),
            (_, 8) => ["w", "v", "v"][position].to_owned(),
            _ => zoo.pick(&API_CONSTANTS).to_owned(),
        })
        .collect();
    let named: Vec<&str> = function
        .formals
        .iter()
        .filter(|(position, _)| *position >= count)
        .map(|(_, formal)| *formal)
        .collect();
    if !named.is_empty() && zoo.rng.chance(1, 3) {
        let formal = named[zoo.rng.below(named.len())];
        let value = match zoo.rng.below(8) {
            0..=2 => "TRUE",
            3 => "FALSE",
            4 => "NA",
            5 => "w",
            _ => zoo.pick(&API_CONSTANTS),
        };
        arguments.push(format!("{} = {value}", syntactic(formal)));
    }
    let replaced = function
        .name
        .strip_suffix("<-")
        .filter(|target| !target.is_empty() && !target.ends_with('<') && count >= 2);
    match replaced {
        Some(target) if !matches!(target, "[" | "[[" | "$" | "@") && zoo.rng.chance(1, 2) => {
            let value = arguments.remove(count - 1);
            let object = arguments.remove(0);
            let rest: String = arguments.iter().map(|text| format!(", {text}")).collect();
            format!(
                "{{ x <- {object}; {}(x{rest}) <- {value}; x }}",
                syntactic(target)
            )
        }
        _ => format!("{}({})", syntactic(function.name), arguments.join(", ")),
    }
}

/// The function a top-level expression of an api program calls, so a run can count its
/// differences by function. `None` for any other expression.
pub fn called_function(expression: &str) -> Option<String> {
    let call = expression.strip_prefix("r <- ")?;
    let call = call.strip_prefix("show_(").unwrap_or(call);
    let call = call.strip_prefix("try_(")?;
    if let Some(replacement) = call.strip_prefix("{ x <- ") {
        let target = replacement.split_once("; ")?.1.split_once("(x")?.0;
        return Some(format!("{}<-", target.trim_matches('`')));
    }
    match call.strip_prefix('`') {
        Some(quoted) => Some(quoted.split_once('`')?.0.to_owned()),
        None => Some(call.split_once('(')?.0.to_owned()),
    }
}

/// The functions a program names in a call: each name or backticked name before `(`, `name<-`
/// when the call is assigned to, and each operator of the table that occurs in the text. It
/// reads text inside string literals as code, so it is a close upper bound, which is what a
/// coverage report over thousands of programs needs.
pub fn called_names(program: &str) -> BTreeSet<&str> {
    static OPERATORS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        api_functions()
            .iter()
            .map(|function| function.name)
            .filter(|name| !name.contains(|c: char| c.is_ascii_alphanumeric()))
            .collect()
    });
    static REPLACEMENTS: LazyLock<BTreeMap<&'static str, &'static str>> = LazyLock::new(|| {
        api_functions()
            .iter()
            .filter_map(|function| Some((function.name.strip_suffix("<-")?, function.name)))
            .collect()
    });
    let mut names: BTreeSet<&str> = OPERATORS
        .iter()
        .copied()
        .filter(|operator| program.contains(operator))
        .collect();
    let bytes = program.as_bytes();
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_';
    for (open, _) in program.match_indices('(') {
        let name = if open > 0 && bytes[open - 1] == b'`' {
            let Some(start) = program[..open - 1].rfind('`') else {
                continue;
            };
            &program[start + 1..open - 1]
        } else {
            let start = bytes[..open]
                .iter()
                .rposition(|&byte| !is_name(byte))
                .map_or(0, |position| position + 1);
            &program[start..open]
        };
        if name.is_empty() {
            continue;
        }
        names.insert(name);
        let mut depth = 0usize;
        let close = bytes[open..].iter().position(|&byte| {
            match byte {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            depth == 0
        });
        if let Some(close) = close
            && program[open + close + 1..].trim_start().starts_with("<-")
            && let Some(replacement) = REPLACEMENTS.get(name)
        {
            names.insert(replacement);
        }
    }
    names
}

/// `name` as R source: itself when it is a syntactic name, in backticks otherwise.
fn syntactic(name: &str) -> String {
    const RESERVED: [&str; 12] = [
        "if", "else", "repeat", "while", "function", "for", "next", "break", "TRUE", "FALSE",
        "NULL", "in",
    ];
    let mut characters = name.chars();
    let starts = characters.next().is_some_and(|first| {
        first.is_ascii_alphabetic()
            || (first == '.' && !name[1..].starts_with(|c: char| c.is_ascii_digit()))
    });
    let plain = starts
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        && !RESERVED.contains(&name)
        && !name.starts_with("NA")
        && name != "Inf"
        && name != "NaN";
    match plain {
        true => name.to_owned(),
        false => format!("`{name}`"),
    }
}

/// A program of round trips through files and connections. Each episode binds a zoo value,
/// writes it with one writer, and reads the file with a few readers, each read a top-level
/// value. Every file is a relative name under the session's temporary directory, so no message
/// holds a path that differs between engines, and every connection is closed by `with_`.
pub fn io_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x696f_5f72_6f75_6e64);
    let features = if rng.chance(1, 8) {
        Features::all()
    } else {
        Features::swarm(&mut rng)
    };
    let mut zoo = Zoo {
        rng,
        features,
        longest: IO_LONGEST,
    };
    let mut text = format!(
        "# oracle io program, seed {seed}\n# features: {}\n{IO_PREAMBLE}",
        features.names()
    );
    for episode in 1..=4 + zoo.rng.below(4) {
        text.push_str(&io_episode(&mut zoo, episode));
    }
    text
}

/// The longest value an io program writes: a few hundred lines keep a printed read short.
const IO_LONGEST: usize = 300;

/// `with_` closes its connection however the body ends, because an engine reports a connection
/// left open whenever it happens to collect it. `marks_` shows what printing hides, each
/// string's encoding mark and bytes, and `sum_` stands in for a value too long to print.
const IO_PREAMBLE: &str = r#"try_ <- function(expr) tryCatch(expr, error = function(e) paste("error:", conditionMessage(e)))
with_ <- function(con, body) { force(con); on.exit(close(con)); body(con) }
bytes_ <- function(file) readBin(file, "raw", 200000L)
marks_ <- function(x) {
  if (is.factor(x)) x <- levels(x)
  if (is.character(x)) list(Encoding(x), lapply(unname(x), function(s) if (is.na(s)) NA else charToRaw(s)))
  else if (is.list(x)) lapply(unname(x), marks_)
  else x
}
sum_ <- function(x) {
  if (!is.character(x)) return(list(class(x), length(x)))
  list(length(x), Encoding(x), nchar(x, "bytes"), vapply(x, function(s) sum(as.integer(charToRaw(s))), 0, USE.NAMES = FALSE))
}
setwd(tempdir())
"#;

/// What an episode's file holds, which decides the readers that understand it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileFormat {
    /// Text, a line per element.
    Lines,
    /// A delimited table of a data frame or a matrix.
    Table,
    /// One serialized value whose bytes are the same under every engine.
    Serialized,
    /// One serialized value, compressed: the bytes depend on the compressor.
    Compressed,
    /// A saved workspace image.
    Image,
    /// R source that evaluates to the value.
    Source,
    /// The elements' binary representations.
    Binary,
}

/// Strings that stress a text round trip beyond the zoo's: bytes that are not valid UTF-8 with
/// no mark, a byte order mark, and every character a reader may take for a separator, a quote,
/// a comment or a missing value.
const IO_STRINGS: [&str; 26] = [
    "\"a\\xffb\"",
    "\"\\xe9t\\xe9\"",
    "\"\\xc3\"",
    "\"\\ufeffbom\"",
    "\"\\u00a0\"",
    "\"tab\\tsep\"",
    "\"comma,sep\"",
    "\"semi;colon\"",
    "\"quo\\\"te\"",
    "\"single'quote\"",
    "\"#hash\"",
    "\" lead\"",
    "\"trail \"",
    "\"  \"",
    "\"\"",
    "\"NA\"",
    "NA_character_",
    "\"back\\\\slash\"",
    "\"cr\\rmid\"",
    "\"line\\nbreak\"",
    "\"1e-300\"",
    "\"0x1A\"",
    "\"T\"",
    "\"-.5\"",
    "\"1,5\"",
    "\"2024-02-29\"",
];

/// Line lengths on both sides of the buffers a reader fills: 4 KiB, 8 KiB and 64 KiB.
const IO_LINE_LENGTHS: [usize; 10] = [
    1023, 4095, 4096, 4097, 8191, 8192, 8193, 10000, 65535, 65537,
];

/// Each writer leaves the value `v` in the file `f`.
const IO_WRITERS: &[(FileFormat, &str)] = &[
    (FileFormat::Lines, "writeLines(v, f)"),
    (FileFormat::Lines, "writeLines(v, f, sep = \"\\r\\n\")"),
    (FileFormat::Lines, "writeLines(v, f, useBytes = TRUE)"),
    (FileFormat::Lines, "writeLines(enc2utf8(v), f)"),
    (FileFormat::Lines, "writeLines(as.character(v), f)"),
    (FileFormat::Lines, "writeLines(format(v), f)"),
    (FileFormat::Lines, "writeLines(capture.output(print(v)), f)"),
    (FileFormat::Lines, "cat(v, file = f, sep = \"\\n\")"),
    (FileFormat::Lines, "cat(v, file = f)"),
    (FileFormat::Lines, "cat(v, \"\\n\", file = f, fill = 20)"),
    (
        FileFormat::Lines,
        "{ cat(v, file = f, sep = \"\\n\"); cat(v, file = f, sep = \",\", append = TRUE) }",
    ),
    (FileFormat::Lines, "write(v, f)"),
    (
        FileFormat::Lines,
        "write(v, f, ncolumns = 2, sep = \"\\t\")",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"w\"), function(con) { writeLines(v, con); cat(\"tail\", file = con) })",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"wb\"), function(con) writeLines(v, con))",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"w\", encoding = \"latin1\"), function(con) writeLines(v, con))",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"w\", encoding = \"UTF-16LE\"), function(con) writeLines(v, con))",
    ),
    (
        FileFormat::Lines,
        "with_(gzfile(f, \"w\"), function(con) writeLines(v, con))",
    ),
    (
        FileFormat::Lines,
        "with_(bzfile(f, \"w\"), function(con) writeLines(v, con))",
    ),
    (
        FileFormat::Lines,
        "with_(xzfile(f, \"w\"), function(con) writeLines(v, con))",
    ),
    (FileFormat::Table, "write.table(v, f)"),
    (
        FileFormat::Table,
        "write.table(v, f, quote = FALSE, sep = \"\\t\", row.names = FALSE)",
    ),
    (
        FileFormat::Table,
        "write.table(v, f, sep = \",\", col.names = NA, qmethod = \"double\")",
    ),
    (
        FileFormat::Table,
        "write.table(v, f, na = \"\", dec = \",\", sep = \";\")",
    ),
    (
        FileFormat::Table,
        "write.table(v, f, eol = \"\\r\\n\", col.names = FALSE)",
    ),
    (
        FileFormat::Table,
        "write.table(v, f, fileEncoding = \"latin1\")",
    ),
    (FileFormat::Table, "write.csv(v, f)"),
    (
        FileFormat::Table,
        "write.csv(v, f, row.names = FALSE, na = \"\")",
    ),
    (
        FileFormat::Table,
        "write.csv(v, f, fileEncoding = \"UTF-8\")",
    ),
    (FileFormat::Table, "write.csv2(v, f)"),
    (FileFormat::Serialized, "saveRDS(v, f, compress = FALSE)"),
    (
        FileFormat::Serialized,
        "saveRDS(v, f, ascii = TRUE, compress = FALSE)",
    ),
    (
        FileFormat::Serialized,
        "saveRDS(v, f, version = 2, compress = FALSE)",
    ),
    (FileFormat::Serialized, "writeBin(serialize(v, NULL), f)"),
    (
        FileFormat::Serialized,
        "writeBin(serialize(v, NULL, xdr = FALSE), f)",
    ),
    (
        FileFormat::Serialized,
        "writeBin(serialize(v, NULL, ascii = TRUE), f)",
    ),
    (
        FileFormat::Serialized,
        "with_(file(f, \"wb\"), function(con) serialize(v, con))",
    ),
    (FileFormat::Compressed, "saveRDS(v, f)"),
    (
        FileFormat::Compressed,
        "saveRDS(v, f, compress = \"bzip2\")",
    ),
    (FileFormat::Compressed, "saveRDS(v, f, compress = \"xz\")"),
    (
        FileFormat::Compressed,
        "with_(gzfile(f, \"wb\"), function(con) serialize(v, con))",
    ),
    (FileFormat::Image, "save(v, file = f)"),
    (FileFormat::Image, "save(v, file = f, ascii = TRUE)"),
    (
        FileFormat::Image,
        "save(list = \"v\", file = f, compress = FALSE)",
    ),
    // A second object, and not a closure: GNU R saves one its JIT compiled as bytecode, so the
    // file would depend on how often the closure ran.
    (FileFormat::Image, "save(v, f, file = f, version = 2)"),
    (FileFormat::Source, "dput(v, f)"),
    (FileFormat::Source, "dput(v, f, control = \"all\")"),
    (FileFormat::Source, "dput(v, f, control = NULL)"),
    (FileFormat::Source, "dump(\"v\", f)"),
    (FileFormat::Source, "writeLines(deparse(v), f)"),
    (
        FileFormat::Source,
        "cat(deparse(v, width.cutoff = 500L), file = f, sep = \"\\n\")",
    ),
    (FileFormat::Binary, "writeBin(v, f)"),
    (FileFormat::Binary, "writeBin(v, f, size = 4)"),
    (FileFormat::Binary, "writeBin(v, f, size = 2)"),
    (FileFormat::Binary, "writeBin(v, f, size = 1)"),
    (FileFormat::Binary, "writeBin(v, f, endian = \"big\")"),
    (FileFormat::Binary, "writeBin(as.integer(v), f, size = 8)"),
    (FileFormat::Binary, "writeBin(as.character(v), f)"),
    (FileFormat::Binary, "writeChar(as.character(v), f)"),
    (
        FileFormat::Binary,
        "writeChar(as.character(v), f, eos = NULL)",
    ),
    (
        FileFormat::Binary,
        "writeBin(charToRaw(paste(v, collapse = \"\")), f)",
    ),
    (
        FileFormat::Binary,
        "with_(file(f, \"wb\"), function(con) { writeBin(v, con); writeBin(1:2, con, size = 2) })",
    ),
];

/// Each reader is a value read from the file `f`.
const IO_READERS: &[(FileFormat, &str)] = &[
    (FileFormat::Lines, "readLines(f)"),
    (FileFormat::Lines, "readLines(f, n = 1)"),
    (FileFormat::Lines, "readLines(f, n = 2, warn = FALSE)"),
    (FileFormat::Lines, "readLines(f, encoding = \"UTF-8\")"),
    (FileFormat::Lines, "readLines(f, encoding = \"latin1\")"),
    (FileFormat::Lines, "readLines(f, skipNul = TRUE)"),
    (FileFormat::Lines, "scan(f, what = \"\", quiet = TRUE)"),
    (
        FileFormat::Lines,
        "scan(f, what = \"\", sep = \"\\n\", quiet = TRUE)",
    ),
    (
        FileFormat::Lines,
        "scan(f, what = \"\", sep = \",\", quiet = TRUE, strip.white = TRUE)",
    ),
    (FileFormat::Lines, "scan(f, quiet = TRUE)"),
    (
        FileFormat::Lines,
        "scan(f, what = integer(), quiet = TRUE, na.strings = c(\"NA\", \"\"))",
    ),
    (
        FileFormat::Lines,
        "scan(f, what = list(\"\", \"\"), quiet = TRUE, fill = TRUE)",
    ),
    (
        FileFormat::Lines,
        "scan(f, what = \"\", quiet = TRUE, encoding = \"latin1\")",
    ),
    (
        FileFormat::Lines,
        "scan(f, what = \"\", quiet = TRUE, fileEncoding = \"latin1\")",
    ),
    (
        FileFormat::Lines,
        "scan(f, what = \"\", nmax = 2, skip = 1, quiet = TRUE)",
    ),
    (FileFormat::Lines, "scan(f, what = complex(), quiet = TRUE)"),
    (FileFormat::Lines, "scan(f, what = logical(), quiet = TRUE)"),
    (FileFormat::Lines, "count.fields(f)"),
    (FileFormat::Lines, "readChar(f, 5)"),
    (FileFormat::Lines, "readChar(f, c(1, 2, 3))"),
    (FileFormat::Lines, "readChar(f, 100, useBytes = TRUE)"),
    (FileFormat::Lines, "read.table(f)"),
    (
        FileFormat::Lines,
        "read.table(f, sep = \"\\n\", quote = \"\")",
    ),
    (
        FileFormat::Lines,
        "read.table(f, fill = TRUE, header = FALSE, stringsAsFactors = TRUE)",
    ),
    (FileFormat::Lines, "bytes_(f)"),
    (FileFormat::Lines, "file.size(f)"),
    (
        FileFormat::Lines,
        "with_(file(f, \"r\"), function(con) { a <- readLines(con, 1); pushBack(c(\"p1\", a), con); list(a, pushBackLength(con), readLines(con)) })",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, encoding = \"latin1\"), function(con) readLines(con))",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, encoding = \"UTF-8-BOM\"), function(con) readLines(con))",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, encoding = \"UTF-16LE\"), function(con) readLines(con))",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"rb\"), function(con) { a <- readBin(con, \"raw\", 4); list(a, readLines(con)) })",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"r\"), function(con) { a <- scan(con, \"\", n = 1, quiet = TRUE); list(a, readLines(con)) })",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"r\"), function(con) { a <- readLines(con, 1); list(a, isIncomplete(con), readLines(con, 1), isOpen(con)) })",
    ),
    (
        FileFormat::Lines,
        "with_(file(f, \"rb\"), function(con) { a <- readChar(con, 2); p <- seek(con); seek(con, 1); list(a, p, readChar(con, 3)) })",
    ),
    (FileFormat::Table, "read.table(f)"),
    (FileFormat::Table, "read.table(f, header = TRUE)"),
    (
        FileFormat::Table,
        "read.table(f, header = TRUE, sep = \"\\t\", quote = \"\")",
    ),
    (
        FileFormat::Table,
        "read.table(f, header = TRUE, sep = \",\", row.names = 1)",
    ),
    (
        FileFormat::Table,
        "read.table(f, header = TRUE, colClasses = \"character\")",
    ),
    (FileFormat::Table, "read.table(f, header = TRUE, nrows = 2)"),
    (FileFormat::Table, "read.table(f, skip = 1, fill = TRUE)"),
    (
        FileFormat::Table,
        "read.table(f, header = TRUE, comment.char = \"\", as.is = TRUE, check.names = FALSE)",
    ),
    (
        FileFormat::Table,
        "read.table(f, header = TRUE, sep = \";\", dec = \",\", na.strings = \"\")",
    ),
    (FileFormat::Table, "read.csv(f)"),
    (
        FileFormat::Table,
        "read.csv(f, row.names = 1, check.names = FALSE)",
    ),
    (
        FileFormat::Table,
        "read.csv(f, stringsAsFactors = TRUE, na.strings = \"\")",
    ),
    (FileFormat::Table, "read.csv(f, encoding = \"latin1\")"),
    (FileFormat::Table, "read.csv(f, fileEncoding = \"latin1\")"),
    (
        FileFormat::Table,
        "read.csv(f, header = FALSE, strip.white = TRUE)",
    ),
    (FileFormat::Table, "read.csv2(f)"),
    (FileFormat::Table, "read.delim(f)"),
    (FileFormat::Table, "read.csv(text = readLines(f))"),
    (FileFormat::Table, "sapply(read.csv(f), class)"),
    (
        FileFormat::Table,
        "utils::type.convert(read.table(f, header = TRUE, colClasses = \"character\"), as.is = TRUE)",
    ),
    (FileFormat::Table, "count.fields(f, sep = \",\")"),
    (FileFormat::Table, "readLines(f)"),
    (FileFormat::Table, "bytes_(f)"),
    (
        FileFormat::Table,
        "scan(f, what = \"\", sep = \",\", quiet = TRUE)",
    ),
    (FileFormat::Serialized, "readRDS(f)"),
    (FileFormat::Serialized, "identical(readRDS(f), v)"),
    (FileFormat::Serialized, "unserialize(bytes_(f))"),
    (FileFormat::Serialized, "bytes_(f)"),
    (FileFormat::Serialized, "infoRDS(f)"),
    (
        FileFormat::Serialized,
        "with_(file(f, \"rb\"), function(con) unserialize(con))",
    ),
    (FileFormat::Compressed, "readRDS(f)"),
    (FileFormat::Compressed, "identical(readRDS(f), v)"),
    (FileFormat::Compressed, "infoRDS(f)"),
    (
        FileFormat::Compressed,
        "with_(gzfile(f, \"rb\"), function(con) unserialize(con))",
    ),
    (
        FileFormat::Compressed,
        "with_(gzfile(f, \"rb\"), function(con) length(readBin(con, \"raw\", 200000L)))",
    ),
    (
        FileFormat::Image,
        "local({ e <- new.env(); n <- load(f, e); list(n, identical(e$v, v)) })",
    ),
    (FileFormat::Image, "local({ load(f); v })"),
    (
        FileFormat::Image,
        "with_(gzfile(f, \"rb\"), function(con) readBin(con, \"raw\", 5))",
    ),
    (FileFormat::Source, "dget(f)"),
    (FileFormat::Source, "identical(dget(f), v)"),
    (FileFormat::Source, "readLines(f)"),
    (FileFormat::Source, "parse(f)"),
    (FileFormat::Source, "eval(parse(f)[[1]])"),
    (FileFormat::Source, "parse(text = readLines(f))"),
    (FileFormat::Source, "parse(f, encoding = \"latin1\")"),
    (FileFormat::Source, "source(f, local = TRUE)$value"),
    (
        FileFormat::Source,
        "local({ e <- new.env(); sys.source(f, e); ls(e) })",
    ),
    (FileFormat::Source, "length(parse(f, keep.source = TRUE))"),
    (FileFormat::Binary, "bytes_(f)"),
    (FileFormat::Binary, "file.size(f)"),
    (FileFormat::Binary, "readBin(f, \"integer\", 100)"),
    (FileFormat::Binary, "readBin(f, \"double\", 100)"),
    (FileFormat::Binary, "readBin(f, \"double\", 100, size = 4)"),
    (
        FileFormat::Binary,
        "readBin(f, \"integer\", 100, size = 2, signed = FALSE)",
    ),
    (FileFormat::Binary, "readBin(f, \"integer\", 100, size = 1)"),
    (
        FileFormat::Binary,
        "readBin(f, integer(), 100, endian = \"big\")",
    ),
    (FileFormat::Binary, "readBin(f, \"character\", 10)"),
    (FileFormat::Binary, "readBin(f, \"logical\", 100)"),
    (FileFormat::Binary, "readBin(f, \"complex\", 100)"),
    (FileFormat::Binary, "readBin(f, \"numeric\", 3, size = 8)"),
    (FileFormat::Binary, "readChar(f, 3)"),
    (FileFormat::Binary, "readChar(f, c(2, 2), useBytes = TRUE)"),
    (
        FileFormat::Binary,
        "with_(file(f, \"rb\"), function(con) { a <- readBin(con, \"raw\", 3); p <- seek(con); seek(con, 1); list(a, p, readBin(con, \"raw\", 2)) })",
    ),
];

/// Round trips of the character vector `v` that touch no file.
const IO_MEMORY: [&str; 22] = [
    "with_(textConnection(v), function(con) list(readLines(con, 1), readLines(con)))",
    "local({ out <- NULL; with_(textConnection(\"out\", \"w\", local = TRUE), function(con) { writeLines(v, con); cat(\"x\", \"y\", file = con) }); out })",
    "with_(rawConnection(raw(0), \"wb\"), function(con) { writeLines(v, con); writeBin(1L, con); rawConnectionValue(con) })",
    "with_(rawConnection(charToRaw(paste(v, collapse = \"\\n\"))), function(con) readLines(con))",
    "with_(textConnection(v), function(con) { pushBack(c(\"x\", \"y\"), con); list(pushBackLength(con), readLines(con)) })",
    "with_(textConnection(v), function(con) scan(con, what = \"\", quiet = TRUE))",
    "with_(textConnection(v), function(con) read.table(con, fill = TRUE))",
    "read.table(text = v)",
    "read.csv(text = paste(v, collapse = \"\\n\"))",
    "scan(text = v, what = \"\", quiet = TRUE)",
    "scan(text = v, what = \"\", sep = \",\", quiet = TRUE)",
    "capture.output(print(v))",
    "capture.output(cat(v, sep = \"\\n\"))",
    "capture.output(str(v))",
    "capture.output(write.table(v))",
    "capture.output(dput(v))",
    "rawToChar(serialize(v, NULL, ascii = TRUE))",
    "identical(unserialize(serialize(v, NULL)), v)",
    "rawToChar(memDecompress(memCompress(charToRaw(paste(v, collapse = \"\")), \"gzip\"), \"gzip\"))",
    "iconv(v, \"UTF-8\", \"latin1\")",
    "iconv(v, \"latin1\", \"UTF-8\")",
    "iconv(v, \"UTF-8\", \"UTF-16LE\", toRaw = TRUE)",
];

/// One episode: a value, a writer, and the reads of what it wrote.
fn io_episode(zoo: &mut Zoo, number: usize) -> String {
    const FORMATS: [FileFormat; 10] = [
        FileFormat::Lines,
        FileFormat::Lines,
        FileFormat::Lines,
        FileFormat::Table,
        FileFormat::Table,
        FileFormat::Serialized,
        FileFormat::Compressed,
        FileFormat::Image,
        FileFormat::Source,
        FileFormat::Binary,
    ];
    let format = FORMATS[zoo.rng.below(FORMATS.len())];
    let mut long = false;
    let value = match format {
        FileFormat::Lines if zoo.has(Feature::LargeSizes) && zoo.rng.chance(1, 4) => {
            long = true;
            let ascii = IO_LINE_LENGTHS[zoo.rng.below(IO_LINE_LENGTHS.len())];
            let wide = IO_LINE_LENGTHS[zoo.rng.below(IO_LINE_LENGTHS.len())];
            format!(
                "c(strrep(\"a\", {ascii}), strrep(\"\\u00e4\", {}), \"\\u65e5\", \"end\")",
                wide / 2 + wide % 2
            )
        }
        FileFormat::Lines if zoo.rng.chance(2, 3) => io_strings(zoo),
        FileFormat::Lines => {
            let kind = [
                Kind::Character,
                Kind::Integer,
                Kind::Double,
                Kind::Logical,
                Kind::Complex,
            ][zoo.rng.below(5)];
            let length = zoo.length();
            zoo.vector(kind, length).text
        }
        FileFormat::Table => match zoo.rng.below(4) {
            0 => format!(
                "data.frame(s = {}, n = seq_len(4) / 3, stringsAsFactors = FALSE)",
                io_strings_of(zoo, 4)
            ),
            1 => zoo.matrix().text,
            _ => zoo.data_frame().text,
        },
        FileFormat::Binary => {
            let kind = [
                Kind::Integer,
                Kind::Double,
                Kind::Logical,
                Kind::Complex,
                Kind::Character,
                Kind::Raw,
            ][zoo.rng.below(6)];
            let length = zoo.length();
            zoo.vector(kind, length).text
        }
        _ => zoo.value().text,
    };
    let mut text = format!("f <- \"f{number}\"\nv <- try_({value})\n");
    if zoo.rng.chance(1, 5) {
        text.push_str("v <- try_(as.character(v))\n");
        for _ in 0..2 + zoo.rng.below(4) {
            let call = IO_MEMORY[zoo.rng.below(IO_MEMORY.len())];
            text.push_str(&io_observed(zoo, call, long));
        }
        return text;
    }
    let writers: Vec<&str> = IO_WRITERS
        .iter()
        .filter(|(written, _)| *written == format)
        .map(|(_, writer)| *writer)
        .collect();
    let writer = writers[zoo.rng.below(writers.len())];
    text.push_str(&format!("try_({writer})\n"));
    // A gzip stream's bytes depend on the compressor, so no reader of such a file may show the
    // bytes or their count: it reads through a connection that decompresses.
    let gzip =
        writer.contains("gzfile(") || matches!(format, FileFormat::Compressed | FileFormat::Image);
    const STORED_BYTES: [&str; 5] = [
        "bytes_(f)",
        "file.size(f)",
        "readBin(f,",
        "readChar(f,",
        "file(f, \"rb\")",
    ];
    for _ in 0..2 + zoo.rng.below(3) {
        // One read in eight is of another format's reader: a reader handed a file it does not
        // understand must refuse it the way GNU R does.
        let mismatched = zoo.rng.chance(1, 8);
        let readers: Vec<&str> = IO_READERS
            .iter()
            .filter(|(read, _)| mismatched || *read == format)
            .map(|(_, reader)| *reader)
            .filter(|reader| !(gzip && STORED_BYTES.iter().any(|read| reader.contains(read))))
            .collect();
        let reader = readers[zoo.rng.below(readers.len())];
        text.push_str(&io_observed(zoo, reader, long));
    }
    text
}

/// A top-level line that shows `call`'s value: printed, as its marks and bytes, compared with
/// `v`, or, for a value too long to print, summed up.
fn io_observed(zoo: &mut Zoo, call: &str, long: bool) -> String {
    if long {
        return format!("try_(sum_({call}))\n");
    }
    match zoo.rng.below(6) {
        0 | 1 => format!("try_(marks_({call}))\n"),
        2 => format!("try_(identical({call}, v))\n"),
        _ => format!("try_({call})\n"),
    }
}

/// A character vector of a few elements, half of them from [`IO_STRINGS`].
fn io_strings(zoo: &mut Zoo) -> String {
    const LENGTHS: [usize; 8] = [0, 1, 2, 3, 3, 5, 8, 33];
    let length = LENGTHS[zoo.rng.below(LENGTHS.len())];
    io_strings_of(zoo, length)
}

fn io_strings_of(zoo: &mut Zoo, length: usize) -> String {
    if length == 0 {
        return "character(0)".to_owned();
    }
    let items: Vec<&str> = (0..length)
        .map(|_| match zoo.rng.chance(1, 2) {
            true => IO_STRINGS[zoo.rng.below(IO_STRINGS.len())],
            false => zoo.string(),
        })
        .collect();
    format!("c({})", items.join(", "))
}

/// `check_` prints one line per form: whether it gave what the first run gave. `same_` treats
/// two results as the same when every part is identical, which `identical()` decides bit by bit
/// for numbers, attribute by attribute, and in order.
const PREAMBLE: &str = r#"safe_ <- function(f) tryCatch(f(), error = function(e) paste("error:", conditionMessage(e)))
check_ <- function(label, reference, value) cat(label, if (identical(reference, value)) "same" else "DIFFERENT", "\n")
`second_<-` <- function(x, value) { x[2] <- value; x }
`first_<-` <- function(x, value) { x[[1]] <- value; x }
`tag_<-` <- function(x, value) { attr(x, "tag") <- value; x }
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Vector,
    Matrix,
    Array,
    DataFrame,
    Factor,
    Date,
}

/// An R expression that builds a value, and what a body needs to know to index it.
pub(crate) struct Value {
    pub(crate) text: String,
    kind: Kind,
    shape: Shape,
    /// Elements of a vector, matrix or array; columns of a data frame.
    length: usize,
    rows: usize,
    columns: usize,
    /// R string literals that name elements (or columns), when the value has names.
    names: Vec<&'static str>,
}

/// Lengths on both sides of the runtime's thresholds: the inline capacities (8, 32, 64), the
/// 512-byte small-vector limit for each element width (32 complex, 64 doubles, 128 integers,
/// 512 raws), the 64 KiB arena chunk and the 1 MiB mapped-object limit.
const SMALL_LENGTHS: [usize; 12] = [0, 1, 1, 2, 2, 3, 3, 4, 5, 7, 8, 9];
const MIDDLE_LENGTHS: [usize; 15] = [
    15, 16, 17, 31, 32, 33, 63, 64, 65, 100, 127, 128, 129, 255, 257,
];
const LARGE_LENGTHS: [usize; 9] = [511, 512, 513, 1000, 4096, 8191, 8192, 8193, 131073];

/// Names for elements, columns and attributes, as R string literals: ASCII, empty, duplicated,
/// `NA`, and UTF-8 in one, two, three and four bytes, combining and precomposed.
const ASCII_NAMES: [&str; 8] = [
    "\"a\"",
    "\"b\"",
    "\"alpha\"",
    "\"a b\"",
    "\"x1\"",
    "\"\"",
    "\"a\"",
    "\"NA\"",
];
const NON_ASCII_NAMES: [&str; 6] = [
    "\"a\\u00e4\"",
    "\"\\u00fc\"",
    "\"\\u65e5\\u672c\"",
    "\"\\U0001F600\"",
    "\"e\\u0301\"",
    "\"\\u00e9\"",
];

const LOGICALS: [&str; 3] = ["TRUE", "FALSE", "NA"];
const INTEGERS: [&str; 7] = ["0L", "1L", "-1L", "2L", "7L", "100L", "-3L"];
const SPECIAL_INTEGERS: [&str; 3] = ["NA_integer_", ".Machine$integer.max", "-2147483647L"];
const DOUBLES: [&str; 8] = ["0", "1", "-1", "0.5", "2.5", "3", "0.1", "123456.789"];
const SPECIAL_DOUBLES: [&str; 10] = [
    "-0",
    "NA_real_",
    "NaN",
    "Inf",
    "-Inf",
    "1e-300",
    "1e300",
    "5e-324",
    ".Machine$double.eps",
    "1e15",
];
const COMPLEXES: [&str; 5] = [
    "0i",
    "1+2i",
    "-1.5i",
    "NA_complex_",
    "complex(real = Inf, imaginary = -0)",
];
const STRINGS: [&str; 10] = [
    "\"a\"",
    "\"\"",
    "\"b c\"",
    "\"1\"",
    "\" 2 \"",
    "\"1e3\"",
    "\"TRUE\"",
    "\"NA\"",
    "NA_character_",
    "\"Zed\"",
];
const NON_ASCII_STRINGS: [&str; 7] = [
    "\"a\\u00e4\"",
    "\"\\u00df\"",
    "\"\\u65e5\\u672c\\u8a9e\"",
    "\"\\U0001F600\"",
    "\"e\\u0301\"",
    "\"\\u00c4\\u00d6\"",
    "\"na\\u00efve caf\\u00e9\"",
];
const OTHER_ENCODING_STRINGS: [&str; 4] = [
    "`Encoding<-`(\"caf\\xe9\", \"latin1\")",
    "`Encoding<-`(\"\\xfc\", \"latin1\")",
    "`Encoding<-`(\"a\\xffb\", \"bytes\")",
    "enc2native(\"\\u00e9\")",
];
const ODD_STRINGS: [&str; 5] = [
    "\"a\\tb\"",
    "\"q\\\"uote\"",
    "\"back\\\\slash\"",
    "\"line\\nbreak\"",
    "strrep(\"ab\", 40)",
];
const RAWS: [&str; 4] = ["as.raw(0)", "as.raw(255)", "as.raw(65)", "as.raw(10)"];
const LIST_ITEMS: [&str; 10] = [
    "1L",
    "\"a\"",
    "NULL",
    "TRUE",
    "2.5",
    "1:3",
    "c(k = 2)",
    "list(1, \"b\")",
    "list(deep = list(1L))",
    "sum",
];

pub(crate) struct Zoo {
    pub(crate) rng: SplitMix64,
    pub(crate) features: Features,
    /// No value is longer than this.
    pub(crate) longest: usize,
}

impl Zoo {
    fn has(&self, feature: Feature) -> bool {
        self.features.has(feature)
    }

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.rng.below(pool.len())]
    }

    fn small_count(&mut self) -> usize {
        [0, 1, 2, 3, 5, 64][self.rng.below(6)]
    }

    fn length(&mut self) -> usize {
        let length = match self.rng.below(10) {
            0..=4 => SMALL_LENGTHS[self.rng.below(SMALL_LENGTHS.len())],
            5..=7 => MIDDLE_LENGTHS[self.rng.below(MIDDLE_LENGTHS.len())],
            _ if self.has(Feature::LargeSizes) => {
                // The lengths above a thousand cost seconds per form in a debug build.
                let limit = if self.rng.chance(1, 6) {
                    LARGE_LENGTHS.len()
                } else {
                    4
                };
                LARGE_LENGTHS[self.rng.below(limit)]
            }
            _ => MIDDLE_LENGTHS[self.rng.below(MIDDLE_LENGTHS.len())],
        };
        length.min(self.longest)
    }

    pub(crate) fn kind(&mut self) -> Kind {
        const KINDS: [(Feature, Kind); 7] = [
            (Feature::Logical, Kind::Logical),
            (Feature::Integer, Kind::Integer),
            (Feature::Double, Kind::Double),
            (Feature::Complex, Kind::Complex),
            (Feature::Character, Kind::Character),
            (Feature::Raw, Kind::Raw),
            (Feature::List, Kind::List),
        ];
        let enabled: Vec<Kind> = KINDS
            .iter()
            .filter(|(feature, _)| self.has(*feature))
            .map(|(_, kind)| *kind)
            .collect();
        enabled[self.rng.below(enabled.len())]
    }

    /// One element of `kind`, as an R expression of length one.
    pub(crate) fn element(&mut self, kind: Kind) -> &'static str {
        let special = self.has(Feature::SpecialNumbers) && self.rng.chance(1, 3);
        match kind {
            Kind::Logical => self.pick(&LOGICALS),
            Kind::Integer if special => self.pick(&SPECIAL_INTEGERS),
            Kind::Integer => self.pick(&INTEGERS),
            Kind::Double if special => self.pick(&SPECIAL_DOUBLES),
            Kind::Double => self.pick(&DOUBLES),
            Kind::Complex => self.pick(&COMPLEXES),
            Kind::Character => self.string(),
            Kind::Raw => self.pick(&RAWS),
            Kind::List => self.pick(&LIST_ITEMS),
        }
    }

    pub(crate) fn string(&mut self) -> &'static str {
        match self.rng.below(8) {
            0 | 1 if self.has(Feature::NonAscii) => self.pick(&NON_ASCII_STRINGS),
            2 if self.has(Feature::OtherEncodings) => self.pick(&OTHER_ENCODING_STRINGS),
            3 if self.has(Feature::IllTyped) => self.pick(&ODD_STRINGS),
            _ => self.pick(&STRINGS),
        }
    }

    pub(crate) fn name(&mut self) -> &'static str {
        if self.has(Feature::NonAscii) && self.rng.chance(1, 2) {
            self.pick(&NON_ASCII_NAMES)
        } else {
            self.pick(&ASCII_NAMES)
        }
    }

    /// A vector of `kind` and `length` without names or attributes.
    fn bare_vector(&mut self, kind: Kind, length: usize) -> String {
        if length == 0 {
            return match kind {
                Kind::Logical => "logical(0)",
                Kind::Integer => "integer(0)",
                Kind::Double => "numeric(0)",
                Kind::Complex => "complex(0)",
                Kind::Character => "character(0)",
                Kind::Raw => "raw(0)",
                Kind::List => "list()",
            }
            .to_owned();
        }
        if length <= 8 && self.rng.chance(2, 3) {
            let items: Vec<&str> = (0..length).map(|_| self.element(kind)).collect();
            let constructor = if kind == Kind::List { "list" } else { "c" };
            return format!("{constructor}({})", items.join(", "));
        }
        let n = length;
        let compact = self.has(Feature::CompactSequences);
        let pool: Vec<&str> = (0..2 + self.rng.below(4))
            .map(|_| self.element(kind))
            .collect();
        let constructor = if kind == Kind::List { "list" } else { "c" };
        let recycled = format!("rep_len({constructor}({}), {n})", pool.join(", "));
        match (kind, self.rng.below(6)) {
            (Kind::Logical, 0) => format!("logical({n})"),
            (Kind::Logical, 1) => format!("(seq_len({n}) %% 3L == 0L)"),
            (Kind::Integer, 0) if compact => format!("1:{n}"),
            (Kind::Integer, 1) if compact => format!("seq_len({n})"),
            (Kind::Integer, 2) if compact => format!("{n}:1"),
            (Kind::Integer, 3) => format!("rev(seq_len({n}))"),
            (Kind::Integer, 4) => format!("as.integer((seq_len({n}) * 7919L) %% 101L)"),
            (Kind::Double, 0) if compact => format!("as.numeric(1:{n})"),
            (Kind::Double, 1) if compact => format!("seq(1, {n}, by = 1)"),
            (Kind::Double, 2) => format!("(seq_len({n}) / 4)"),
            (Kind::Double, 3) => format!("numeric({n})"),
            (Kind::Double, 4) => format!("seq(0, 1, length.out = {n})"),
            (Kind::Complex, 0) => {
                format!("complex(real = seq_len({n}), imaginary = -seq_len({n}))")
            }
            (Kind::Character, 0) => format!("as.character(seq_len({n}))"),
            (Kind::Character, 1) => format!("paste0(\"k\", seq_len({n}))"),
            (Kind::Character, 2) => format!("letters[(seq_len({n}) - 1L) %% 26L + 1L]"),
            (Kind::Character, 3) if self.has(Feature::NonAscii) => {
                format!("paste0(\"\\u00e4\", seq_len({n}))")
            }
            (Kind::Raw, 0) => format!("as.raw(seq_len({n}) %% 256L)"),
            (Kind::Raw, 1) => format!("raw({n})"),
            (Kind::List, 0) => format!("as.list(seq_len({n}))"),
            (Kind::List, 1) => format!("vector(\"list\", {n})"),
            (Kind::List, 2) => format!("lapply(seq_len({n}), function(i) i * 2)"),
            _ => recycled,
        }
    }

    /// A plain vector, perhaps with names and other attributes.
    fn vector(&mut self, kind: Kind, length: usize) -> Value {
        let mut text = self.bare_vector(kind, length);
        let mut names = Vec::new();
        if self.has(Feature::Names) && length > 0 && self.rng.chance(2, 5) {
            let given = if length <= 8 {
                names = (0..length).map(|_| self.name()).collect();
                format!("c({})", names.join(", "))
            } else {
                match self.rng.below(3) {
                    0 => format!("paste0(\"n\", seq_len({length}))"),
                    1 => format!("as.character({length}:1)"),
                    _ => {
                        names = (0..3).map(|_| self.name()).collect();
                        format!("rep_len(c({}), {length})", names.join(", "))
                    }
                }
            };
            // The same names reach a vector as strings, through `names<-`, and as argument
            // tags, which are symbols.
            let combine = if kind == Kind::List { "list" } else { "c" };
            text = match self.rng.below(6) {
                0 | 1 => format!("setNames({text}, {given})"),
                2 => format!("structure({text}, names = {given})"),
                3 => format!("local({{ named <- {text}; names(named) <- {given}; named }})"),
                4 if length <= 8 && kind != Kind::List => {
                    let elements: Vec<String> = names
                        .iter()
                        .enumerate()
                        .map(|(index, name)| match *name {
                            // R has no empty tag: an untagged argument has the empty name.
                            "\"\"" => format!("bare[[{}]]", index + 1),
                            name => format!("{name} = bare[[{}]]", index + 1),
                        })
                        .collect();
                    format!("local({{ bare <- {text}; c({}) }})", elements.join(", "))
                }
                _ => format!("do.call({combine}, setNames(as.list({text}), {given}))"),
            };
        }
        if self.has(Feature::Attributes) && self.rng.chance(1, 4) {
            let attributes = [
                "class = \"zoo_class\"",
                "note = \"n\", tag = 1:2",
                "class = c(\"zoo_a\", \"zoo_b\"), extra = list(1)",
                "tag = NULL, units = \"m\"",
            ];
            text = format!("structure({text}, {})", self.pick(&attributes));
        }
        Value {
            text,
            kind,
            shape: Shape::Vector,
            length,
            rows: 0,
            columns: 0,
            names,
        }
    }

    fn matrix(&mut self) -> Value {
        const SIDES: [usize; 12] = [0, 1, 1, 2, 2, 3, 3, 4, 7, 8, 9, 16];
        // Dimensions whose product straddles the lengths at which storage changes.
        const THRESHOLDS: [(usize, usize); 12] = [
            (3, 5),
            (4, 4),
            (1, 17),
            (7, 9),
            (8, 8),
            (5, 13),
            (64, 1),
            (1, 65),
            (9, 14),
            (16, 8),
            (3, 43),
            (32, 16),
        ];
        let (rows, columns) = if self.rng.chance(1, 3) {
            THRESHOLDS[self.rng.below(THRESHOLDS.len())]
        } else {
            (
                SIDES[self.rng.below(SIDES.len())],
                SIDES[self.rng.below(SIDES.len())],
            )
        };
        let kind = self.kind();
        let data = self.bare_vector(kind, rows * columns);
        let mut arguments = format!("{data}, {rows}, {columns}");
        if self.rng.chance(1, 5) {
            arguments.push_str(", byrow = TRUE");
        }
        let mut names = Vec::new();
        if self.has(Feature::Names) && rows > 0 && columns > 0 && self.rng.chance(1, 2) {
            let row_names = match self.rng.below(3) {
                0 => "NULL".to_owned(),
                1 => format!("paste0(\"r\", seq_len({rows}))"),
                _ => format!("rep_len(c({}, {}), {rows})", self.name(), self.name()),
            };
            let pool: Vec<&'static str> = (0..2).map(|_| self.name()).collect();
            let column_names = match self.rng.below(3) {
                0 => "NULL".to_owned(),
                1 => format!("paste0(\"c\", seq_len({columns}))"),
                _ => {
                    names = pool.clone();
                    format!("rep_len(c({}), {columns})", pool.join(", "))
                }
            };
            arguments.push_str(&format!(", dimnames = list({row_names}, {column_names})"));
        }
        Value {
            text: format!("matrix({arguments})"),
            kind,
            shape: Shape::Matrix,
            length: rows * columns,
            rows,
            columns,
            names,
        }
    }

    fn array(&mut self) -> Value {
        const SIDES: [usize; 6] = [1, 2, 2, 3, 4, 5];
        let dims: Vec<usize> = (0..3).map(|_| SIDES[self.rng.below(SIDES.len())]).collect();
        let kind = self.kind();
        let length = dims.iter().product();
        let data = self.bare_vector(kind, length);
        Value {
            text: format!("array({data}, c({}, {}, {}))", dims[0], dims[1], dims[2]),
            kind,
            shape: Shape::Array,
            length,
            rows: dims[0],
            columns: dims[1],
            names: Vec::new(),
        }
    }

    fn data_frame(&mut self) -> Value {
        const ROWS: [usize; 9] = [0, 1, 2, 3, 3, 5, 64, 65, 130];
        let rows = ROWS[self.rng.below(ROWS.len())];
        let columns = 1 + self.rng.below(4);
        let mut names: Vec<&'static str> = Vec::new();
        let mut parts = Vec::new();
        const COLUMN_NAMES: [&str; 6] = ["\"a\"", "\"b\"", "\"val\"", "\"key\"", "\"x1\"", "\"n\""];
        const NON_ASCII_COLUMNS: [&str; 3] =
            ["\"gr\\u00f6\\u00dfe\"", "\"\\u00e4\"", "\"\\u65e5\""];
        for index in 0..columns {
            let name = if self.has(Feature::NonAscii) && self.rng.chance(1, 3) {
                NON_ASCII_COLUMNS[index % NON_ASCII_COLUMNS.len()]
            } else {
                COLUMN_NAMES[(index + self.rng.below(3)) % COLUMN_NAMES.len()]
            };
            if names.contains(&name) {
                continue;
            }
            let kind =
                [Kind::Integer, Kind::Double, Kind::Character, Kind::Logical][self.rng.below(4)];
            let data = self.bare_vector(kind, rows);
            let column =
                if kind == Kind::Character && self.has(Feature::Factor) && self.rng.chance(1, 4) {
                    format!("factor({data})")
                } else {
                    data
                };
            // A name in backticks reads as written, and `data.frame` keeps it with
            // `check.names = FALSE`.
            parts.push(format!("`{}` = {column}", unquote(name)));
            names.push(name);
        }
        let mut arguments = parts.join(", ");
        arguments.push_str(", check.names = FALSE");
        if self.rng.chance(1, 4) {
            arguments.push_str(", stringsAsFactors = TRUE");
        }
        if rows > 0 && self.has(Feature::Names) && self.rng.chance(1, 4) {
            arguments.push_str(&format!(", row.names = paste0(\"r\", seq_len({rows}))"));
        }
        Value {
            text: format!("data.frame({arguments})"),
            kind: Kind::List,
            shape: Shape::DataFrame,
            length: names.len(),
            rows,
            columns: names.len(),
            names,
        }
    }

    fn factor(&mut self) -> Value {
        let length = self.length().min(300);
        let non_ascii = self.has(Feature::NonAscii);
        let levels = if non_ascii && self.rng.chance(1, 2) {
            "c(\"lo\", \"h\\u00f6\", \"mid\")"
        } else {
            "c(\"lo\", \"hi\", \"mid\")"
        };
        let data = format!("rep_len(c({levels}, NA), {length})");
        let text = match self.rng.below(5) {
            0 => format!("factor({data})"),
            1 => format!("factor({data}, levels = c({levels}, \"unused\"))"),
            2 => format!("factor({data}, levels = {levels}, ordered = TRUE)"),
            3 => format!("addNA(factor({data}))"),
            _ => format!("factor(rep_len(c(3L, 1L, 2L), {length}))"),
        };
        Value {
            text,
            kind: Kind::Integer,
            shape: Shape::Factor,
            length,
            rows: 0,
            columns: 0,
            names: Vec::new(),
        }
    }

    fn date(&mut self) -> Value {
        let length = self.length().clamp(1, 200);
        let text = match self.rng.below(5) {
            0 => format!("as.Date(\"2024-02-29\") + 0:{}", length - 1),
            1 => format!("rep_len(as.Date(c(\"2024-01-31\", NA, \"1999-12-31\")), {length})"),
            2 => format!(
                "as.POSIXct(\"2024-03-10 12:34:56\", tz = \"UTC\") + seq_len({length}) * 3600"
            ),
            3 => format!("as.difftime(seq_len({length}) / 2, units = \"hours\")"),
            _ => format!("structure(seq_len({length}), class = \"Date\")"),
        };
        Value {
            text,
            kind: Kind::Double,
            shape: Shape::Date,
            length,
            rows: 0,
            columns: 0,
            names: Vec::new(),
        }
    }

    pub(crate) fn value(&mut self) -> Value {
        let mut shapes = vec![Shape::Vector, Shape::Vector, Shape::Vector];
        for (feature, shape) in [
            (Feature::Matrix, Shape::Matrix),
            (Feature::Array, Shape::Array),
            (Feature::DataFrame, Shape::DataFrame),
            (Feature::Factor, Shape::Factor),
            (Feature::Date, Shape::Date),
        ] {
            if self.has(feature) {
                shapes.push(shape);
                if shape == Shape::Matrix {
                    shapes.push(shape);
                }
            }
        }
        match shapes[self.rng.below(shapes.len())] {
            Shape::Vector => {
                let kind = self.kind();
                let length = self.length();
                self.vector(kind, length)
            }
            Shape::Matrix => self.matrix(),
            Shape::Array => self.array(),
            Shape::DataFrame => self.data_frame(),
            Shape::Factor => self.factor(),
            Shape::Date => self.date(),
        }
    }

    /// One trial: a value, a body, and the forms the body runs in.
    fn trial(&mut self, number: usize) -> String {
        let value = self.value();
        let mut body = Body::default();
        for _ in 0..1 + self.rng.below(5) {
            let statement = self.statement(&value, &mut body);
            let statement = if self.has(Feature::Guards) && self.rng.chance(1, 4) {
                format!("tryCatch({{ {statement} }}, error = function(e) NULL)")
            } else {
                statement
            };
            body.statements.push(statement);
        }
        let mut result = vec!["@".to_owned()];
        result.extend((1..=body.reads).map(|read| format!("read_{read}")));
        result.extend((1..=body.aliases).map(|alias| format!("alias_{alias}")));
        // A read or an alias exists only once its statement ran, and a guard may have skipped it.
        let prelude: String = (1..=body.reads)
            .map(|read| format!("read_{read} <- NULL; "))
            .chain((1..=body.aliases).map(|alias| format!("alias_{alias} <- NULL; ")))
            .collect();
        let result = format!("list({})", result.join(", "));
        let lines = |variable: &str, separator: &str| -> String {
            let mut text = prelude.clone();
            for statement in &body.statements {
                text.push_str(statement);
                text.push_str(separator);
            }
            text.push_str(&result);
            text.replace('@', variable)
        };
        let make = format!("make_{number}");
        let step = format!("step_{number}");
        let reference = format!("ref_{number}");
        let mut text = format!(
            "\n{make} <- function() {}\n{step} <- function(x) {{\n  {}\n}}\n{reference} <- safe_(function() {step}({make}()))\n",
            value.text,
            lines("x", "\n  ")
        );
        let mut check = |label: &str, expression: String| {
            text.push_str(&format!(
                "check_(\"{number} {label}\", {reference}, safe_(function() {expression}))\n"
            ));
        };
        if self.has(Feature::Repeated) {
            let count = [1, 2, 3, 10, 50, 300][self.rng.below(6)];
            // A long vector three hundred times over costs minutes in a debug build.
            let count = if value.length > 1000 {
                count.min(3)
            } else {
                count
            };
            check(
                &format!("again x{count}"),
                format!(
                    "{{ out <- NULL; for (k_ in seq_len({count})) out <- {step}({make}()); out }}"
                ),
            );
        }
        if self.has(Feature::Scopes) {
            match self.rng.below(4) {
                0 => check(
                    "local",
                    format!("local({{ x <- {make}(); {} }})", lines("x", "; ")),
                ),
                1 => check(
                    "inline",
                    format!("(function() {{ x <- {make}(); {} }})()", lines("x", "; ")),
                ),
                2 => check(
                    "evaluated",
                    format!(
                        "{{ e_ <- new.env(); assign(\"x\", {make}(), e_); eval(quote({{ {} }}), e_) }}",
                        lines("x", "; ")
                    ),
                ),
                _ => check(
                    "on.exit",
                    format!("(function() {{ on.exit(NULL); {step}({make}()) }})()"),
                ),
            }
            // Top level: the variable is a global, and the statements run one by one in the
            // driver's `eval`. A body that assigns through `<<-` means something else there.
            if !lines("x", "; ").contains("<<-") && self.rng.chance(1, 2) {
                text.push_str(&format!(
                    "top_{number} <- tryCatch({{ x <- {make}(); {} }}, error = function(e) paste(\"error:\", conditionMessage(e)))\ncheck_(\"{number} top level\", {reference}, top_{number})\n",
                    lines("x", "; ")
                ));
            }
        }
        let mut check = |label: &str, expression: String| {
            text.push_str(&format!(
                "check_(\"{number} {label}\", {reference}, safe_(function() {expression}))\n"
            ));
        };
        if self.has(Feature::Serialized) {
            let version = 2 + self.rng.below(2);
            check(
                &format!("serialized v{version}"),
                format!("{step}(unserialize(serialize({make}(), NULL, version = {version})))"),
            );
        }
        if self.has(Feature::Renamed) {
            let variable = ["value", ".v", "`my var`", "`x\u{e4}`", "T", "c"][self.rng.below(6)];
            check(
                &format!("renamed {}", variable.replace('`', "")),
                format!(
                    "(function({variable}) {{ {} }})({make}())",
                    lines(variable, "; ")
                ),
            );
        }
        if self.has(Feature::Callers) {
            let call = match self.rng.below(8) {
                0 => format!("do.call({step}, list({make}()))"),
                1 => format!("lapply(list({make}()), {step})[[1]]"),
                2 => format!("Map({step}, list({make}()))[[1]]"),
                3 => format!("{step}({{ {make}() }})"),
                4 => format!("{step}(identity({make}()))"),
                5 => format!("replicate(2, {step}({make}()), simplify = FALSE)[[2]]"),
                6 => format!("Reduce(function(acc, k) {step}({make}()), 1:3, NULL)"),
                _ => format!("suppressWarnings(tryCatch({step}({make}()), finally = NULL))"),
            };
            check("caller", call);
        }
        if self.has(Feature::Collected) {
            check(
                "collected",
                format!("(function(x) {{ {} }})({make}())", lines("x", "; gc(); ")),
            );
        }
        if self.has(Feature::Warmed) {
            // The body first runs on values of other types and sizes, so whatever a first call
            // specialized it for (a compiled body, a quickened instruction, an inline cache)
            // meets a value it was not specialized for.
            let others: Vec<String> = (0..1 + self.rng.below(3))
                .map(|_| format!("safe_(function() {step}({})); ", self.value().text))
                .collect();
            let fresh = format!("fresh_{number}");
            text.push_str(&format!(
                "{fresh} <- {step}\nenvironment({fresh}) <- globalenv()\nbody({fresh}) <- body({step})\ncheck_(\"{number} warmed\", {reference}, safe_(function() {{ {}{step}({make}()) }}))\ncheck_(\"{number} cold\", {reference}, safe_(function() {fresh}({make}())))\n",
                others.concat()
            ));
        }
        if self.has(Feature::Shared) {
            // The body's argument is also held by a second variable, a list, an environment
            // or a closure. The holder's copy must come out unchanged.
            let (setup, argument, holder) = match self.rng.below(5) {
                0 => ("keep <- input", "input", "keep"),
                1 => ("keep <- list(input)", "keep[[1]]", "keep[[1]]"),
                2 => ("keep <- new.env(); keep$v <- input", "keep$v", "keep$v"),
                3 => (
                    "keep <- (function(captured) function() captured)(input)",
                    "keep()",
                    "keep()",
                ),
                _ => (
                    "keep <- structure(1, held = input)",
                    "attr(keep, \"held\")",
                    "attr(keep, \"held\")",
                ),
            };
            text.push_str(&format!(
                "shared_{number} <- safe_(function() {{ input <- {make}(); {setup}; out <- safe_(function() {step}({argument})); list(out, identical({holder}, {make}()), identical(input, {make}())) }})\ncheck_(\"{number} shared\", list({reference}, TRUE, TRUE), shared_{number})\n",
            ));
        }
        // A long result prints as thousands of lines that say little; its dump shows both ends
        // and a summary, and the forms above compared every element.
        if value.length > 300 {
            text.push_str(&format!("invisible({reference})\n"));
        } else {
            text.push_str(&format!("{reference}\n"));
        }
        text
    }

    /// A scalar index into `count` positions: mostly inside, sometimes one past the end, and
    /// with ill-typed programs zero, negative, fractional or missing.
    fn position(&mut self, count: usize) -> String {
        let count = count.max(1);
        match self.rng.below(12) {
            0..=2 => "1".to_owned(),
            3 => "2L".to_owned(),
            4 | 5 => count.to_string(),
            6 => (1 + self.rng.below(count)).to_string(),
            7 => format!("{}L", 1 + self.rng.below(count)),
            8 => (count + 1).to_string(),
            9 if self.has(Feature::IllTyped) => self
                .pick(&[
                    "0",
                    "-1",
                    "NA",
                    "2.9",
                    "NA_integer_",
                    "1e10",
                    "TRUE",
                    "\"1\"",
                ])
                .to_owned(),
            10 => (count + 3).to_string(),
            _ => (1 + self.rng.below(count)).to_string(),
        }
    }

    /// A vector index into `count` positions.
    fn positions(&mut self, count: usize, names: &[&'static str]) -> String {
        let count = count.max(1);
        if !names.is_empty() && self.rng.chance(1, 4) {
            let name = names[self.rng.below(names.len())];
            return match self.rng.below(3) {
                0 => name.to_owned(),
                1 => format!("c({name}, \"zz\")"),
                _ => format!("c({name}, {})", names[self.rng.below(names.len())]),
            };
        }
        match self.rng.below(16) {
            0 => "1:2".to_owned(),
            1 => format!("c(1, {count})"),
            2 => "-1".to_owned(),
            3 => "c(TRUE, FALSE)".to_owned(),
            4 => "TRUE".to_owned(),
            5 => format!("seq(1, {count}, by = 2)"),
            6 => format!("{count}:1"),
            7 => "c(2, 2)".to_owned(),
            8 => "integer(0)".to_owned(),
            9 => format!("-(1:{})", count.min(2)),
            10 => format!("{}:{}", count, count + 2),
            11 if self.has(Feature::IllTyped) => self
                .pick(&[
                    "NA", "c(1, NA)", "0", "c(-1, 1)", "c(0, 1)", "1.9", "\"zz\"", "NULL",
                    "list(1)", "-99",
                ])
                .to_owned(),
            12 => "FALSE".to_owned(),
            13 => format!("c({}, 1)", count + 2),
            _ => self.position(count),
        }
    }

    /// A value to store into an element of `kind`: usually the same type, sometimes a wider or
    /// narrower one, so the store has to coerce the target.
    fn stored(&mut self, kind: Kind) -> String {
        let other = [
            Kind::Logical,
            Kind::Integer,
            Kind::Double,
            Kind::Complex,
            Kind::Character,
            Kind::Raw,
            Kind::List,
        ][self.rng.below(7)];
        match self.rng.below(10) {
            0..=5 => self.element(kind).to_owned(),
            6 | 7 => self.element(other).to_owned(),
            8 if self.has(Feature::IllTyped) => self
                .pick(&[
                    "NULL",
                    "list(1)",
                    "integer(0)",
                    "c(1, 2)",
                    "sum",
                    "quote(a)",
                    "NA",
                ])
                .to_owned(),
            _ => format!("c({}, {})", self.element(kind), self.element(kind)),
        }
    }

    /// One statement on the variable `@`.
    fn statement(&mut self, value: &Value, body: &mut Body) -> String {
        let length = value.length;
        let kind = value.kind;
        match value.shape {
            Shape::Matrix | Shape::Array if self.rng.chance(3, 5) => {
                return self.matrix_statement(value);
            }
            Shape::DataFrame if self.rng.chance(4, 5) => return self.data_frame_statement(value),
            Shape::Factor if self.rng.chance(3, 5) => return self.factor_statement(value),
            Shape::Date if self.rng.chance(2, 5) => {
                let position = self.position(length);
                return [
                    format!("@[{position}] <- as.Date(\"2000-01-01\")"),
                    "@ <- @ + 1".to_owned(),
                    format!("@[{position}] <- NA"),
                    "@ <- rev(@)".to_owned(),
                    "@ <- c(@, @[1])".to_owned(),
                    format!("@[{position}] <- 5"),
                    "@ <- sort(@, decreasing = TRUE)".to_owned(),
                    "@ <- unique(c(@, @))".to_owned(),
                ][self.rng.below(8)]
                .clone();
            }
            _ => {}
        }
        let position = self.position(length);
        let positions = self.positions(length, &value.names);
        let stored = self.stored(kind);
        // A lookup by name mostly asks for a name the value has.
        let name = if !value.names.is_empty() && self.rng.chance(2, 3) {
            value.names[self.rng.below(value.names.len())]
        } else {
            self.name()
        };
        match self.rng.below(34) {
            0..=3 => format!("@[{position}] <- {stored}"),
            4..=6 => format!("@[[{position}]] <- {stored}"),
            7 | 8 => format!("@[{positions}] <- {stored}"),
            9 => format!("names(@)[{position}] <- {name}"),
            10 => match self.rng.below(4) {
                0 => "names(@) <- NULL".to_owned(),
                1 => format!("names(@) <- c({name}, {})", self.name()),
                2 => "names(@) <- rev(names(@))".to_owned(),
                _ => format!("names(@) <- rep_len(c({name}, \"k\"), length(@))"),
            },
            11 if self.has(Feature::Attributes) => match self.rng.below(8) {
                0 => format!("attr(@, \"note\") <- {stored}"),
                1 => "attr(@, \"note\") <- NULL".to_owned(),
                2 => "attributes(@) <- NULL".to_owned(),
                3 => "class(@) <- \"zoo_class\"".to_owned(),
                4 => "oldClass(@) <- NULL".to_owned(),
                5 => format!("attr(@, {name}) <- 1L"),
                6 => "comment(@) <- \"c\"".to_owned(),
                _ => "tag_(@) <- \"t\"".to_owned(),
            },
            12 => format!(
                "length(@) <- {}",
                [0, 1, length / 2, length + 1, length + 70][self.rng.below(5)]
            ),
            13 => match self.rng.below(8) {
                0 => format!("@ <- c(@, {stored})"),
                1 => format!("@ <- append(@, {stored}, after = 1)"),
                2 => format!("@ <- @[-{}]", 1 + self.rng.below(length.max(1))),
                3 => "@ <- rev(@)".to_owned(),
                4 => "@ <- head(@, -1)".to_owned(),
                5 => format!("@ <- @[{positions}]"),
                6 => "@ <- rep(@, 2)".to_owned(),
                _ => format!("@[[length(@) + 1L]] <- {stored}"),
            },
            14 | 15 if self.has(Feature::Aliases) => {
                body.aliases += 1;
                format!("alias_{} <- @", body.aliases)
            }
            16 | 17 => {
                body.reads += 1;
                let read = match self.rng.below(13) {
                    9 => format!("names(@) == {name}"),
                    10 => format!("match({name}, names(@))"),
                    11 => format!("@[[{name}]]"),
                    12 => format!("{name} %in% names(@)"),
                    0 => format!("@[{positions}]"),
                    1 => format!("@[[{position}]]"),
                    2 => "length(@)".to_owned(),
                    3 => "names(@)".to_owned(),
                    4 => "attributes(@)".to_owned(),
                    5 => format!("@[-{}]", 1 + self.rng.below(length.max(1))),
                    6 => "rev(@)".to_owned(),
                    7 => format!("@[{name}]"),
                    _ => "typeof(@)".to_owned(),
                };
                format!(
                    "read_{} <- tryCatch({read}, error = function(e) conditionMessage(e))",
                    body.reads
                )
            }
            18 if self.has(Feature::Loops) => match self.rng.below(5) {
                0 => format!(
                    "for (k_ in seq_len({})) @[k_] <- {stored}",
                    self.small_count().min(length + 2)
                ),
                1 => "for (k_ in seq_along(@)) @[[k_]] <- @[[length(@) - k_ + 1L]]".to_owned(),
                2 => format!("for (k_ in 1:3) @[[k_ + 1L]] <- {stored}"),
                3 => format!("k_ <- 0L; while (k_ < 3L) {{ k_ <- k_ + 1L; @[k_] <- {stored} }}"),
                _ => "for (k_ in seq_along(@)) if (k_ %% 2L == 0L) @[k_] <- @[k_ - 1L]".to_owned(),
            },
            19 => format!("is.na(@) <- {positions}"),
            20 => format!("@[{positions}] <- @[{position}]"),
            21 => match self.rng.below(5) {
                0 if length > 0 && length % 2 == 0 => format!("dim(@) <- c(2, {})", length / 2),
                1 => "dim(@) <- length(@)".to_owned(),
                2 => "dim(@) <- NULL".to_owned(),
                3 if length > 0 => format!("dim(@) <- c({length}, 1)"),
                _ => "dim(@) <- c(1, length(@))".to_owned(),
            },
            22 if self.has(Feature::SuperAssignment) => match self.rng.below(3) {
                0 => format!("(function() @[{position}] <<- {stored})()"),
                1 => format!("(function() @[[{position}]] <<- {stored})()"),
                _ => format!("local(@[{positions}] <<- {stored})"),
            },
            // A callee that changes its own copy of the argument.
            23 => format!("(function(z) {{ z[{position}] <- {stored}; z }})(@)"),
            24 => match self.rng.below(2) {
                0 => format!("second_(@) <- {stored}"),
                _ => format!("first_(@) <- {stored}"),
            },
            25 => match self.rng.below(5) {
                0 => "storage.mode(@) <- \"double\"".to_owned(),
                1 => "storage.mode(@) <- \"integer\"".to_owned(),
                2 => "mode(@) <- \"character\"".to_owned(),
                3 => "storage.mode(@) <- \"list\"".to_owned(),
                _ => "mode(@) <- \"logical\"".to_owned(),
            },
            26 => match self.rng.below(3) {
                0 => format!("@[] <- {stored}"),
                1 => "@[] <- rev(@)".to_owned(),
                _ => "@[] <- @".to_owned(),
            },
            27 | 28 => match (kind, self.rng.below(4)) {
                (Kind::Character, 0) => "substr(@, 1, 1) <- \"Z\"".to_owned(),
                (Kind::Character, 1) => "@ <- paste0(@, \"x\")".to_owned(),
                (Kind::Character, 2) => "@ <- toupper(@)".to_owned(),
                (Kind::Character, _) => "@[nchar(@) > 1] <- \"long\"".to_owned(),
                (Kind::Logical, 0) => "@ <- !@".to_owned(),
                (Kind::Logical, _) => "@[@] <- FALSE".to_owned(),
                (Kind::Integer | Kind::Double | Kind::Complex, 0) => "@ <- @ + 1L".to_owned(),
                (Kind::Integer | Kind::Double | Kind::Complex, 1) => "@ <- -@".to_owned(),
                (Kind::Integer | Kind::Double, 2) => "@[@ > 1] <- 0L".to_owned(),
                (Kind::Integer | Kind::Double | Kind::Complex, _) => "@ <- @ * 2".to_owned(),
                (Kind::Raw, _) => "@ <- rev(@)".to_owned(),
                (Kind::List, 0) => "@ <- lapply(@, identity)".to_owned(),
                (Kind::List, 1) => "@ <- unname(@)".to_owned(),
                (Kind::List, _) => "@ <- @[!vapply(@, is.null, NA)]".to_owned(),
            },
            29 | 30 if self.has(Feature::Nested) || kind == Kind::List => {
                match self.rng.below(10) {
                    0 => format!("@${} <- {stored}", bare(name)),
                    1 => format!("@[[{name}]] <- {stored}"),
                    2 => format!("@${}[{position}] <- {stored}", bare(name)),
                    3 => format!("@${}$deep <- {stored}", bare(name)),
                    4 => format!("@[[{position}]][[2]] <- {stored}"),
                    5 => format!("names(@${})[1] <- {name}", bare(name)),
                    6 => format!("attr(@[[{position}]], \"k\") <- {stored}"),
                    7 => format!("@[[{position}]] <- NULL"),
                    8 => format!("@[c({name}, \"b\")] <- list(1, 2)"),
                    _ => format!("@[[\"new\"]]$sub[3] <- {stored}"),
                }
            }
            31 => format!("@[{positions}] <- NULL"),
            _ => format!("@[{position}] <- {stored}"),
        }
    }

    fn matrix_statement(&mut self, value: &Value) -> String {
        let row = self.position(value.rows);
        let column = self.position(value.columns);
        let rows = self.positions(value.rows, &[]);
        let columns = self.positions(value.columns, &value.names);
        let stored = self.stored(value.kind);
        let name = self.name();
        if value.shape == Shape::Array && self.rng.chance(1, 2) {
            return match self.rng.below(4) {
                0 => format!("@[{row}, {column}, 1] <- {stored}"),
                1 => format!("@[{row}, , ] <- {stored}"),
                2 => format!("@[, {column}, 1] <- {stored}"),
                _ => "@ <- aperm(@)".to_owned(),
            };
        }
        match self.rng.below(22) {
            0..=4 => format!("@[{row}, {column}] <- {stored}"),
            5 => format!("@[{row}, ] <- {stored}"),
            6 => format!("@[, {column}] <- {stored}"),
            7 => format!("@[{rows}, {columns}] <- {stored}"),
            8 => format!("@[cbind({row}, {column})] <- {stored}"),
            9 => format!("diag(@) <- {stored}"),
            10 => format!("@[lower.tri(@)] <- {stored}"),
            11 => "dimnames(@) <- NULL".to_owned(),
            12 => format!("rownames(@) <- paste0({name}, seq_len(nrow(@)))"),
            13 => format!("colnames(@)[{column}] <- {name}"),
            14 => format!("dimnames(@)[[1]][{row}] <- {name}"),
            15 => "@ <- t(@)".to_owned(),
            16 => "@ <- @[, -1, drop = FALSE]".to_owned(),
            17 => format!("@ <- rbind(@, {stored})"),
            18 => format!("@ <- cbind(@, {stored})"),
            19 => format!("@[{row}, {column}] <- @[{column}, {row}]"),
            20 => "dim(@) <- rev(dim(@))".to_owned(),
            _ => format!("@[{rows}, ] <- @[{row}, ]"),
        }
    }

    fn data_frame_statement(&mut self, value: &Value) -> String {
        let row = self.position(value.rows);
        let column = self.position(value.columns);
        let name = if value.names.is_empty() {
            "\"a\""
        } else {
            value.names[self.rng.below(value.names.len())]
        };
        let kind = [Kind::Integer, Kind::Double, Kind::Character, Kind::Logical][self.rng.below(4)];
        let stored = self.stored(kind);
        let fresh = self.name();
        match self.rng.below(20) {
            0..=2 => format!("@[{row}, {column}] <- {stored}"),
            3 | 4 => format!("@[[{column}]][{row}] <- {stored}"),
            5 | 6 => format!("@${}[{row}] <- {stored}", bare(name)),
            7 => format!("@[{row}, {name}] <- {stored}"),
            8 => "@$new <- seq_len(nrow(@))".to_owned(),
            9 => format!("@[nrow(@) + 1L, ] <- @[{row}, ]"),
            10 => format!("names(@)[{column}] <- {fresh}"),
            11 => "row.names(@) <- rev(seq_len(nrow(@)))".to_owned(),
            12 => "@ <- @[order(@[[1]]), , drop = FALSE]".to_owned(),
            13 => "@ <- @[-1, , drop = FALSE]".to_owned(),
            14 => format!("@[[{column}]] <- NULL"),
            15 => "@ <- rbind(@, @)".to_owned(),
            16 => "@ <- cbind(@, k = 1)".to_owned(),
            17 => format!("@[@[[1]] %in% @[[1]][1], {column}] <- {stored}"),
            18 => format!("@[[{fresh}]] <- {stored}"),
            _ => format!("@ <- @[{row}, , drop = FALSE]"),
        }
    }

    fn factor_statement(&mut self, value: &Value) -> String {
        let position = self.position(value.length);
        let name = self.name();
        match self.rng.below(12) {
            0 => format!("levels(@)[{}] <- {name}", 1 + self.rng.below(3)),
            1 | 2 => format!("@[{position}] <- \"lo\""),
            3 => format!("@[{position}] <- \"not a level\""),
            4 => "levels(@) <- rev(levels(@))".to_owned(),
            5 => "@ <- droplevels(@)".to_owned(),
            6 => "@ <- factor(@)".to_owned(),
            7 => format!("@[{position}] <- NA"),
            8 => "@ <- c(@, @)".to_owned(),
            9 => "@ <- rev(@)".to_owned(),
            10 => "levels(@) <- c(levels(@), \"extra\")".to_owned(),
            _ => "@ <- @[-1, drop = TRUE]".to_owned(),
        }
    }
}

/// What a body has declared so far, so its result can list every read and alias.
#[derive(Default)]
struct Body {
    statements: Vec<String>,
    reads: usize,
    aliases: usize,
}

/// The text an R string literal stands for. A name in backticks takes the characters
/// themselves, because R reads no `\u` escape there.
fn unquote(literal: &str) -> String {
    let inner = literal.trim_matches('"');
    let mut text = String::new();
    let mut rest = inner;
    while let Some(start) = rest.find('\\') {
        text.push_str(&rest[..start]);
        let digits = match rest.as_bytes().get(start + 1) {
            Some(b'u') => 4,
            Some(b'U') => 8,
            _ => 0,
        };
        let end = (start + 2 + digits).min(rest.len());
        let decoded = rest
            .get(start + 2..end)
            .filter(|_| digits > 0)
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .and_then(char::from_u32);
        match decoded {
            Some(character) => text.push(character),
            None => text.push_str(&rest[start..end]),
        }
        rest = &rest[end..];
    }
    text.push_str(rest);
    text
}

/// A name for `$`: a string literal in backticks, or `NA` as it is.
fn bare(literal: &str) -> String {
    match literal {
        "NA" => "`NA`".to_owned(),
        "\"\"" => "`empty`".to_owned(),
        _ => format!("`{}`", unquote(literal)),
    }
}

const QUADRATIC_CALLS: [&str; 8] = [
    "outer(",
    "%o%",
    "%*%",
    "expand.grid(",
    "merge(",
    "crossprod(",
    "do.call(rbind",
    "mapply(",
];

/// Calls on the zoo values `#` and `@`; `~` is a small count. Nothing here reads the clock, an
/// address, the locale's collation of non-ASCII text, or a random number.
const BUILTIN_CALLS: &[&str] = &[
    "rev(#)",
    "sort(#)",
    "order(#)",
    "unique(#)",
    "duplicated(#)",
    "anyDuplicated(#)",
    "rank(#)",
    "table(#)",
    "length(#)",
    "names(#)",
    "attributes(#)",
    "dim(#)",
    "typeof(#)",
    "class(#)",
    "mode(#)",
    "storage.mode(#)",
    "is.na(#)",
    "anyNA(#)",
    "is.null(#)",
    "is.numeric(#)",
    "is.character(#)",
    "is.vector(#)",
    "is.list(#)",
    "is.atomic(#)",
    "is.function(#)",
    "is.element(#, @)",
    "is.object(#)",
    "is.matrix(#)",
    "is.array(#)",
    "is.finite(#)",
    "is.infinite(#)",
    "is.nan(#)",
    "sum(#)",
    "prod(#)",
    "mean(#)",
    "max(#)",
    "min(#)",
    "range(#)",
    "cumsum(#)",
    "cumprod(#)",
    "cummax(#)",
    "cummin(#)",
    "diff(#)",
    "var(#)",
    "median(#)",
    "which(#)",
    "which.max(#)",
    "which.min(#)",
    "any(#)",
    "all(#)",
    "xor(#, @)",
    "isTRUE(#)",
    "isFALSE(#)",
    "!#",
    "-#",
    "+#",
    "# + @",
    "# - @",
    "# * @",
    "# / @",
    "# ^ @",
    "# %% @",
    "# %/% @",
    "# == @",
    "# != @",
    "# < @",
    "# <= @",
    "# > @",
    "# >= @",
    "# & @",
    "# | @",
    "# && TRUE",
    "abs(#)",
    "sqrt(#)",
    "exp(#)",
    "log(#)",
    "log2(#)",
    "log10(#)",
    "log1p(#)",
    "expm1(#)",
    "floor(#)",
    "ceiling(#)",
    "round(#)",
    "round(#, 1)",
    "signif(#, 2)",
    "trunc(#)",
    "sign(#)",
    "sin(#)",
    "cos(#)",
    "tan(#)",
    "atan(#)",
    "sinh(#)",
    "tanh(#)",
    "gamma(#)",
    "lgamma(#)",
    "factorial(#)",
    "choose(#, 2)",
    "bitwAnd(#, @)",
    "bitwOr(#, 3L)",
    "bitwXor(#, @)",
    "Re(#)",
    "Im(#)",
    "Mod(#)",
    "Conj(#)",
    "as.integer(#)",
    "as.numeric(#)",
    "as.character(#)",
    "as.logical(#)",
    "as.complex(#)",
    "as.raw(#)",
    "as.list(#)",
    "as.vector(#)",
    "as.vector(#, \"list\")",
    "as.vector(#, \"character\")",
    "unlist(#)",
    "unlist(#, use.names = FALSE)",
    "as.factor(#)",
    "as.double(#)",
    "as.symbol(#)",
    "as.data.frame(#)",
    "as.matrix(#)",
    "as.array(#)",
    "as.Date(#)",
    "as.numeric_version(#)",
    "as.call(#)",
    "as.pairlist(#)",
    "c(#, @)",
    "c(#, NULL)",
    "c(#, list(1))",
    "c(a = #)",
    "append(#, @, after = 1)",
    "rep(#, 2)",
    "rep(#, each = 2)",
    "rep_len(#, ~)",
    "rep(#, times = ~)",
    "head(#, ~)",
    "tail(#, ~)",
    "head(#, -1)",
    "tail(#, -1)",
    "#[~]",
    "#[-1]",
    "#[[1]]",
    "#[c(TRUE, FALSE)]",
    "#[0]",
    "#[NA]",
    "#[\"a\"]",
    "#[[\"a\"]]",
    "#$a",
    "#[@]",
    "#[[~]]",
    "#[1, ]",
    "#[, 1]",
    "#[1, 1]",
    "#[1, , drop = FALSE]",
    "#[-1, ]",
    "#[cbind(1, 1)]",
    "#[# > 1]",
    "#[order(#)]",
    "paste(#)",
    "paste(#, collapse = \"|\")",
    "paste0(#, @)",
    "paste(#, @, sep = \"-\")",
    "format(#)",
    "format(#, width = 8)",
    "format(#, nsmall = 2)",
    "format(#, digits = 3)",
    "format(#, justify = \"right\")",
    "formatC(#)",
    "formatC(#, width = 6)",
    "formatC(#, digits = 2, format = \"f\")",
    "formatC(#, flag = \"0\", width = 5)",
    "prettyNum(#, big.mark = \",\")",
    "toString(#)",
    "sprintf(\"%s\", #)",
    "sprintf(\"%d\", #)",
    "sprintf(\"%5.2f\", #)",
    "sprintf(\"%-6s|\", #)",
    "sprintf(\"%e\", #)",
    "sprintf(\"%g\", #)",
    "sprintf(\"%x\", #)",
    "sprintf(\"%s=%s\", #, @)",
    "sprintf(\"%05d\", #)",
    "nchar(#)",
    "nchar(#, \"bytes\")",
    "nchar(#, \"width\")",
    "toupper(#)",
    "tolower(#)",
    "casefold(#, upper = TRUE)",
    "substr(#, 1, 2)",
    "substr(#, 2, 100)",
    "substring(#, 1:2, 2:3)",
    "strsplit(#, \"\")",
    "strsplit(#, \" \")",
    "strsplit(#, \"[0-9]\")",
    "strtoi(#)",
    "strtoi(#, 16L)",
    "trimws(#)",
    "rev(strsplit(paste(#, collapse = \"\"), NULL)[[1]])",
    "grepl(\"a\", #)",
    "grepl(\"^[[:alpha:]]+$\", #)",
    "grepl(\"\\\\d\", #, perl = TRUE)",
    "grepl(\"a\", #, fixed = TRUE)",
    "grep(\"a\", #)",
    "grep(\"a\", #, value = TRUE)",
    "sub(\"a\", \"<A>\", #)",
    "gsub(\"[aeiou]\", \"\", #)",
    "gsub(\"\", \"-\", #)",
    "gsub(\"(.)\", \"\\\\1\\\\1\", #)",
    "gsub(\".\", \"X\", #, fixed = TRUE)",
    "gsub(\"\\\\s+\", \" \", #)",
    "regexpr(\"a\", #)",
    "gregexpr(\"a\", #)",
    "regmatches(#, regexpr(\"[a-z]+\", #))",
    "startsWith(#, \"a\")",
    "endsWith(#, \"a\")",
    "chartr(\"ab\", \"xy\", #)",
    "make.names(#)",
    "make.unique(#)",
    "abbreviate(#, 3)",
    "shQuote(#)",
    "dQuote(#, FALSE)",
    "sQuote(#, FALSE)",
    "encodeString(#)",
    "encodeString(#, quote = '\"')",
    "noquote(#)",
    "utf8ToInt(#)",
    "intToUtf8(#)",
    "intToUtf8(#, multiple = TRUE)",
    "chartr(\"\\u00e4\", \"a\", #)",
    "iconv(#, \"UTF-8\", \"ASCII\", sub = \"?\")",
    "enc2utf8(#)",
    "enc2native(#)",
    "Encoding(#)",
    "validUTF8(#)",
    "charToRaw(#)",
    "rawToChar(#)",
    "basename(#)",
    "dirname(#)",
    "tools::file_ext(#)",
    "file.path(#, @)",
    "sprintf(\"%3s|\", #)",
    "formatC(#, width = -8)",
    "format(#, width = 4, justify = \"centre\")",
    "strtrim(#, 2)",
    "strrep(#, 2)",
    "rawToChar(rev(charToRaw(#)))",
    "match(#, @)",
    "# %in% @",
    "setdiff(#, @)",
    "union(#, @)",
    "intersect(#, @)",
    "tabulate(#)",
    "rle(#)",
    "findInterval(#, c(0, 1, 5))",
    "cut(#, 3)",
    "outer(#, @)",
    "crossprod(#)",
    "# %*% @",
    "# %o% @",
    "t(#)",
    "diag(#)",
    "det(#)",
    "nrow(#)",
    "ncol(#)",
    "NROW(#)",
    "NCOL(#)",
    "rowSums(#)",
    "colSums(#)",
    "rowMeans(#)",
    "colMeans(#)",
    "apply(#, 1, sum)",
    "apply(#, 2, function(column) length(unique(column)))",
    "sapply(#, length)",
    "sapply(#, class)",
    "lapply(#, rev)",
    "vapply(#, length, 1L)",
    "Map(function(a, b) list(a, b), #, @)",
    "mapply(function(a, b) paste(a, b), #, @)",
    "Reduce(`+`, #)",
    "Reduce(function(a, b) paste(a, b), #, accumulate = TRUE)",
    "Filter(Negate(is.na), #)",
    "Position(is.na, #)",
    "do.call(c, as.list(#))",
    "do.call(rbind, as.list(#))",
    "do.call(cbind, list(#, @))",
    "rbind(#, @)",
    "cbind(#, @)",
    "rbind(#)",
    "cbind(#, 1)",
    "merge(#, @)",
    "split(#, rep_len(1:2, length(#)))",
    "unsplit(split(#, rep_len(1:2, length(#))), rep_len(1:2, length(#)))",
    "stack(#)",
    "expand.grid(#, @)",
    "ifelse(#, @, NA)",
    "ifelse(is.na(#), 0, #)",
    "replace(#, 1, NA)",
    "identical(#, @)",
    "all.equal(#, @)",
    "isTRUE(all.equal(#, #))",
    "Negate(is.null)(#)",
    "invisible(#)",
    "seq_along(#)",
    "seq_len(length(#))",
    "seq(#)",
    "rev(seq_along(#))",
    "max(-Inf, #)",
    "pmin(#, @)",
    "pmax(#, 0)",
    "nlevels(#)",
    "levels(#)",
    "droplevels(#)",
    "as.integer(factor(#))",
    "factor(#, levels = unique(#))",
    "summary(#)",
    "str(#)",
    "print(#)",
    "deparse(#)",
    "dput(#)",
    "format(#, nsmall = 1L)",
    "noquote(format(#))",
    "toupper(names(#))",
    "unname(#)",
    "setNames(#, NULL)",
    "structure(#, class = \"zoo\")",
    "unclass(#)",
    "inherits(#, \"factor\")",
    "nchar(deparse(#))",
    "object.size(#) > 0",
    "lengths(#)",
    "nchar(names(#))",
    "rapply(#, function(leaf) leaf, how = \"unlist\")",
    "rapply(#, toupper, classes = \"character\", how = \"replace\")",
    "mapply(rep, #, 2)",
    "array(#, c(2, 2))",
    "matrix(#, 2)",
    "matrix(#, ncol = 2, byrow = TRUE)",
    "aperm(#)",
    "dim(t(#))",
    "upper.tri(#)",
    "row(#)",
    "col(#)",
    "sort(#, decreasing = TRUE)",
    "sort(#, na.last = TRUE)",
    "order(#, decreasing = TRUE)",
    "sort(#, method = \"radix\")",
    "order(#, @)",
    "rev(order(#))",
    "xtfrm(#)",
    "is.unsorted(#)",
    "trimws(format(#))",
    "weekdays(#)",
    "months(#)",
    "format(#, \"%Y-%m-%d\")",
    "julian(#)",
    "difftime(#, @)",
    "seq(#[1], by = 1, length.out = 3)",
    "as.numeric(#) %% 7",
    "serialize(#, NULL, version = 2)[-(1:14)]",
    "identical(unserialize(serialize(#, NULL)), #)",
    "nchar(#, \"chars\", allowNA = TRUE)",
    "nchar(#, keepNA = FALSE)",
    "sprintf(\"%s\", list(#))",
    "vapply(#, function(item) paste(class(item), collapse = \"/\"), \"\")",
    "environmentName(environment(#))",
    "is.primitive(#)",
    "mean(#, na.rm = TRUE)",
    "sum(#, na.rm = TRUE)",
    "max(#, na.rm = TRUE)",
    "range(#, na.rm = TRUE)",
    "quantile(#, 0.5, names = FALSE)",
    "cor(#, @)",
    "sd(#)",
    "scale(#)",
    "trunc(# / @)",
    "bitwNot(#)",
    "bitwShiftL(#, 1L)",
    "rowsum(#, rep_len(1:2, NROW(#)))",
    "aggregate(#, list(rep_len(1:2, NROW(#))), length)",
    "tapply(#, rep_len(c(\"p\", \"q\"), length(#)), length)",
    "by(#, rep_len(1:2, NROW(#)), NROW)",
    "nchar(toString(#))",
    "rev(as.list(#))",
    "append(#, list(NULL))",
    "modifyList(as.list(#), list(a = NULL))",
    "within(#, extra <- 1)",
    "with(#, 1)",
    "subset(#, TRUE)",
    "transform(#, extra = 1)",
    "head(as.data.frame(#), 2)",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn parses(text: &str) -> bool {
        syntax::parse(text).errors().is_empty()
    }

    #[test]
    fn programs_are_deterministic_and_parse() {
        for seed in 0..400 {
            for program in [
                program(seed),
                builtins_program(seed),
                api_program(seed),
                io_program(seed),
            ] {
                assert!(parses(&program), "seed {seed} does not parse:\n{program}");
            }
            assert_eq!(program(seed), program(seed));
        }
    }

    #[test]
    fn backtick_names_hold_the_characters_themselves() {
        assert_eq!(bare("\"gr\\u00f6\\u00dfe\""), "`größe`");
        assert_eq!(bare("\"\\U0001F600\""), "`\u{1F600}`");
        assert_eq!(bare("\"a b\""), "`a b`");
        for seed in 0..400 {
            for line in program(seed).lines() {
                let escaped_in_backticks = line
                    .split('`')
                    .skip(1)
                    .step_by(2)
                    .any(|name| name.contains("\\u") || name.contains("\\U"));
                assert!(!escaped_in_backticks, "seed {seed}: {line}");
            }
        }
    }

    #[test]
    fn the_function_table_parses_and_names_what_a_program_calls() {
        let functions = api_functions();
        assert!(functions.len() > 1500);
        let callable = functions
            .iter()
            .filter(|function| function.positional.is_some())
            .count();
        assert!(callable > 1000, "{callable} callable functions");
        for function in functions {
            assert!(matches!(function.package, "base" | "stats" | "utils"));
            assert!(function.positional.is_none_or(|count| count <= 3));
        }
        for seed in 0..100 {
            for line in api_program(seed)
                .lines()
                .filter(|line| line.starts_with("r <- "))
            {
                let name = called_function(line)
                    .unwrap_or_else(|| panic!("seed {seed}: no function in {line}"));
                assert!(
                    functions
                        .iter()
                        .any(|function| function.name == name && function.positional.is_some()),
                    "seed {seed}: {name} is not a callable function of the table"
                );
            }
        }
    }

    #[test]
    fn a_call_names_its_function() {
        let name = |line: &str| called_function(line);
        assert_eq!(name("r <- try_(sum(v, w))").as_deref(), Some("sum"));
        assert_eq!(name("r <- show_(try_(`[[`(v, 1L)))").as_deref(), Some("[["));
        assert_eq!(
            name("r <- try_({ x <- v; names(x) <- w; x })").as_deref(),
            Some("names<-")
        );
        assert_eq!(name("v <- try_(c(1, 2))"), None);
        let names = called_names("r <- try_({ x <- v; names(x) <- `[[`(w, 1L) + sum(v) })");
        for name in ["try_", "names", "names<-", "[[", "+", "sum"] {
            assert!(names.contains(name), "{name} is missing from {names:?}");
        }
        assert!(!names.contains("mean"));
        assert_eq!(syntactic("is.na<-"), "`is.na<-`");
        assert_eq!(syntactic("if"), "`if`");
        assert_eq!(syntactic("rowSums"), "rowSums");
    }

    #[test]
    fn a_swarm_always_has_an_element_type() {
        let mut rng = SplitMix64(1);
        for _ in 0..1000 {
            let features = Features::swarm(&mut rng);
            assert!(FEATURES[..7].iter().any(|feature| features.has(*feature)));
        }
    }

    #[test]
    fn every_feature_is_listed_once_in_declaration_order() {
        for (index, feature) in FEATURES.iter().enumerate() {
            assert_eq!(*feature as usize, index);
        }
    }
}
