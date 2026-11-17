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
//! dozen calls on them, each a top-level value the deep observer dumps.
//!
//! Swarm testing: a seed first draws the subset of [`Feature`]s its programs may use, so a rare
//! pairing (raw vectors and `dim<-`, say) is not drowned out by the common ones.
//!
//! Every program is deterministic under GNU R, and none prints an environment or a closure.

use syntax::testing::SplitMix64;

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
    let mut zoo = Zoo { rng, features };
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
    let mut zoo = Zoo { rng, features };
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
enum Kind {
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
struct Value {
    text: String,
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

struct Zoo {
    rng: SplitMix64,
    features: Features,
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
        match self.rng.below(10) {
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
        }
    }

    fn kind(&mut self) -> Kind {
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
    fn element(&mut self, kind: Kind) -> &'static str {
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

    fn string(&mut self) -> &'static str {
        match self.rng.below(8) {
            0 | 1 if self.has(Feature::NonAscii) => self.pick(&NON_ASCII_STRINGS),
            2 if self.has(Feature::OtherEncodings) => self.pick(&OTHER_ENCODING_STRINGS),
            3 if self.has(Feature::IllTyped) => self.pick(&ODD_STRINGS),
            _ => self.pick(&STRINGS),
        }
    }

    fn name(&mut self) -> &'static str {
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

    fn value(&mut self) -> Value {
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
            for program in [program(seed), builtins_program(seed)] {
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
