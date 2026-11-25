//! Random R programs for differential fuzzing, one per seed.
//!
//! A program is a sequence of independent blocks, each a shape the runtime has to get right:
//! hot functions with scalar and vector loops (so `RY_JIT=stress` compiles them), element reads
//! and writes, closures, lazy arguments, S3 dispatch, `switch`, `repeat`, `Recall`, environments,
//! attributes, NA propagation, integer overflow, recycling and condition handling. Most blocks
//! are well-typed and print what they compute, so a difference shows up in stdout as well as in
//! the last value. A share of the expressions are ill-typed on purpose (a string in arithmetic, a
//! length-two condition, a missing variable, a call with the wrong arity); those sit inside
//! `tryCatch` and print the error message, and a few programs end in an uncaught error.
//!
//! Every program also carries a block of replacement calls (`x[i] <- v`, `f(x, i) <- v` and their
//! kin), each run three times inside a function, in a loop, at top level and under `replicate`,
//! because a replacement function written in R receives its index arguments as promises and a
//! compiled body must not evaluate them first. The block draws from its own stream, so adding it
//! left the rest of every seed's program unchanged.
//!
//! A last block, from a stream of its own too, assigns elements of a function's argument, so the
//! function copies the argument once and writes the rest into the copy. The argument's length
//! falls on both sides of 64, where GNU R's shallow duplicate turns into an ALTREP wrapper with no
//! element storage of its own, and every function runs three times, because its body compiles on
//! the second call.
//!
//! Every program is deterministic under GNU R: nothing reads the clock, addresses, the
//! environment or an unseeded random number, and no environment or closure is ever printed.

use {
    crate::{Engine, EngineError, ExpressionDifference, Outcome, diff, differing_expressions},
    std::path::Path,
    syntax::testing::SplitMix64,
};

/// The R program for `seed`. The same seed always gives the same text.
pub fn program(seed: u64) -> String {
    let mut generator = Generator {
        rng: SplitMix64(seed),
        block: 0,
        globals: Vec::new(),
        ill_typed: false,
    };
    let mut text = format!("# oracle fuzz program, seed {seed}\n");
    for _ in 0..4 + generator.rng.below(6) {
        generator.block += 1;
        // About one block in five carries an ill-typed expression, always behind `tryCatch`.
        generator.ill_typed = generator.rng.chance(1, 5);
        text.push_str(&generator.block_text());
        text.push('\n');
    }
    generator.ill_typed = false;
    text.push_str(&replacement_promises(&mut SplitMix64(
        seed ^ 0x9e37_79b9_7f4a_7c15,
    )));
    text.push_str(&argument_element_stores(&mut SplitMix64(
        seed ^ 0x6a09_e667_f3bc_c909,
    )));
    text.push_str(&generator.ending());
    text
}

/// Replacement calls whose index arguments the replacement function receives as promises: S3
/// `[<-`, `[[<-` and `$<-` methods, replacement closures, nested targets, and builtin replacements
/// for the evaluation order. Each method reports what it sees of its argument (its code, whether it
/// is missing, `nargs()`, `match.call()`, or its value once or twice), and `note_rp` records every
/// evaluation, so a subscript evaluated too early, too often or not at all shows up in the trace.
/// Every shape runs three times in each context, because a body compiles after its first call.
fn replacement_promises(rng: &mut SplitMix64) -> String {
    const INDICES: [&str; 5] = [
        "note_rp(\"i\", 1)",
        "undefined_rp == 1",
        "k_rp + 1",
        "1L",
        "note_rp(\"i\", k_rp)",
    ];
    // `@` stands for the parameter the method reports on.
    const BODIES: [&str; 5] = [
        "paste(deparse(substitute(@)), missing(@), nargs())",
        "paste(deparse(match.call()), collapse = \" \")",
        "paste(@, deparse(substitute(@)))",
        "{ @; @; paste(@) }",
        "paste(nargs(), if (missing(@)) \"missing\" else \"given\")",
    ];
    let mut text = String::from(
        "trace_rp <- character()\nnote_rp <- function(tag, value) { trace_rp <<- c(trace_rp, tag); value }\nk_rp <- 1L\nreport_rp <- function(label, n, f) {\n  trace_rp <<- character()\n  out <- tryCatch(paste(deparse(f()), collapse = \" \"), error = function(e) paste(\"error:\", conditionMessage(e)))\n  cat(label, n, out, \"| trace:\", paste(trace_rp, collapse = \",\"), \"\\n\")\n}\n",
    );
    for shape_number in 0..1 + rng.below(3) {
        let index = INDICES[rng.below(INDICES.len())];
        let body = BODIES[rng.below(BODIES.len())];
        let class = format!("rp{shape_number}");
        let start = format!("z <- structure(list(), class = \"{class}\")");
        let report = |parameter: &str| body.replace('@', parameter);
        let (definition, setup, assignment, observe) = match rng.below(8) {
            0 => (
                format!(
                    "`[<-.{class}` <- function(x, i, j, value) {{ attr(x, \"seen\") <- {}; x }}",
                    report("i")
                ),
                start,
                ["z[@] <- 5", "z[@, \"a\"] <- 5"][rng.below(2)].replace('@', index),
                "attr(z, \"seen\")".to_owned(),
            ),
            1 => (
                format!(
                    "`[<-.{class}` <- function(x, i, j, value) {{ attr(x, \"seen\") <- {}; x }}",
                    report("j")
                ),
                start,
                format!("z[, {index}] <- 5"),
                "attr(z, \"seen\")".to_owned(),
            ),
            2 => (
                format!(
                    "`[[<-.{class}` <- function(x, i, value) {{ attr(x, \"seen\") <- {}; x }}",
                    report("i")
                ),
                start,
                format!("z[[{index}]] <- 5"),
                "attr(z, \"seen\")".to_owned(),
            ),
            // GNU R's own `match.call()` in a `$<-` method shows the name as `abc` or `"abc"`
            // depending on whether GNU R compiled the caller, so that body is no oracle here.
            3 => (
                format!(
                    "`$<-.{class}` <- function(x, name, value) {{ attr(x, \"seen\") <- {}; x }}",
                    if body.contains("match.call") {
                        BODIES[0].replace('@', "name")
                    } else {
                        report("name")
                    }
                ),
                start,
                "z$abc <- 5".to_owned(),
                "attr(z, \"seen\")".to_owned(),
            ),
            4 => (
                format!(
                    "`sub_{class}<-` <- function(x, i, value) {{ attr(x, \"seen\") <- {}; x }}",
                    report("i")
                ),
                start,
                format!("sub_{class}(z, {index}) <- 5"),
                "attr(z, \"seen\")".to_owned(),
            ),
            5 => (
                format!(
                    "`sub_{class}<-` <- function(x, i, value) {{ attr(x, \"seen\") <- {}; x }}\n`[<-.{class}` <- `sub_{class}<-`",
                    report("i")
                ),
                format!("z <- list(a = structure(list(), class = \"{class}\"))"),
                [
                    format!("sub_{class}(z$a, {index}) <- 5"),
                    format!("z$a[{index}] <- 5"),
                ][rng.below(2)]
                .clone(),
                "attr(z$a, \"seen\")".to_owned(),
            ),
            6 => (
                String::new(),
                "z <- c(a = 1, b = 2)".to_owned(),
                format!("names(z)[{index}] <- note_rp(\"v\", \"q\")"),
                "names(z)".to_owned(),
            ),
            _ => (
                String::new(),
                "z <- matrix(1:4, 2)".to_owned(),
                format!("z[{index}, note_rp(\"j\", 2)] <- note_rp(\"v\", 9L)"),
                "z".to_owned(),
            ),
        };
        let label = format!("rp{shape_number}");
        text.push_str(&format!(
            "{definition}\nfor (n in 1:3) report_rp(\"{label} function\", n, function() {{ {setup}; {assignment}; {observe} }})\nfor (n in 1:3) report_rp(\"{label} loop\", n, function() {{ for (r in 1:3) {{ {setup}; {assignment} }}; {observe} }})\nfor (n in 1:3) {{\n  trace_rp <- character()\n  out <- tryCatch({{ {setup}; {assignment}; {observe} }}, error = function(e) paste(\"error:\", conditionMessage(e)))\n  cat(\"{label} top level\", n, out, \"| trace:\", paste(trace_rp, collapse = \",\"), \"\\n\")\n}}\nreport_rp(\"{label} replicate\", 1, function() replicate(3, {{ {setup}; {assignment}; {observe} }}))\n"
        ));
    }
    text
}

/// Functions that assign elements of their argument and return it: one to four stores through
/// `[`, `[[` or a matrix subscript into a vector of every element type, sized around 64 elements.
/// Each runs three times on the same argument, and the block prints a checksum of every result and
/// whether the caller's value is unchanged.
fn argument_element_stores(rng: &mut SplitMix64) -> String {
    const TYPES: [(&str, &str); 7] = [
        ("numeric(", "2.5"),
        ("integer(", "7L"),
        ("logical(", "TRUE"),
        ("character(", "\"s\""),
        ("complex(", "1i"),
        ("raw(", "as.raw(9)"),
        ("vector(\"list\", ", "list(1)"),
    ];
    let mut text = String::from(
        "sum_ae <- function(x, value) if (is.list(x)) sum(lengths(x)) else sum(as.character(x) == as.character(value))\n",
    );
    for shape in 0..1 + rng.below(3) {
        let side = [7, 8, 9, 12][rng.below(4)];
        let (constructor, value) = TYPES[rng.below(TYPES.len())];
        let list = constructor.starts_with("vector");
        let matrix = !list && rng.chance(1, 2);
        let length = side * side;
        let setup = match matrix {
            true => format!("{constructor}{length})\ndim(a_ae{shape}) <- c({side}L, {side}L)"),
            false => format!("{constructor}{})", [63, 64, 65, length][rng.below(4)]),
        };
        let stores: Vec<String> = (0..1 + rng.below(4))
            .map(|store| {
                let row = 1 + (store * 3 + rng.below(side)) % side;
                let column = 1 + rng.below(side);
                match (matrix, rng.below(3)) {
                    (true, 0) => format!("  x[{row}, {column}] <- {value}"),
                    (true, 1) => format!("  x[{row}L, {column}L] <- {value}"),
                    (_, 2) if !list => format!("  x[[{row}]] <- {value}"),
                    _ => format!("  x[{row}] <- {value}"),
                }
            })
            .collect();
        let stores = stores.join("\n");
        text.push_str(&format!(
            "a_ae{shape} <- {setup}\nkeep_ae{shape} <- a_ae{shape}\nf_ae{shape} <- function(x) {{\n{stores}\n  x\n}}\nfor (n in 1:3) {{\n  r_ae <- f_ae{shape}(a_ae{shape})\n  cat(\"ae{shape}\", n, typeof(r_ae), length(r_ae), sum_ae(r_ae, {value}), identical(a_ae{shape}, keep_ae{shape}), \"\\n\")\n}}\n"
        ));
    }
    text
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Integer,
    Double,
    Logical,
    Character,
}

struct Generator {
    rng: SplitMix64,
    /// The current block's number, which suffixes every name it defines so blocks never clash.
    block: usize,
    /// Top-level scalars earlier blocks defined, which later blocks may read.
    globals: Vec<(String, Kind)>,
    /// Whether leaves may turn into values of the wrong type.
    ill_typed: bool,
}

const WORDS: [&str; 8] = ["alpha", "b", "", "Gamma", "delta e", "x1", "NA", "zz"];

impl Generator {
    fn block_text(&mut self) -> String {
        match self.rng.below(29) {
            0..=3 => self.hot_scalar_loop(),
            4..=5 => self.vector_writes(),
            6 => self.while_and_repeat(),
            7 => self.closures(),
            8 => self.s3_dispatch(),
            9 => self.switch_block(),
            10 => self.recall_block(),
            11 => self.environments(),
            12 => self.attributes(),
            13 => self.na_and_overflow(),
            14 => self.recycling(),
            15 => self.conditions(),
            16 => self.functionals(),
            17 => self.arity_and_missing(),
            18 => self.strings(),
            19 => self.list_loop(),
            20 => self.integer_arithmetic(),
            21 => self.matrix_block(),
            22 => self.ill_typed_block(),
            23 => self.constant_defaults(),
            24 => self.loop_variable_aliasing(),
            25 => self.named_appends(),
            26 => self.slabs_and_rounding(),
            27 => self.lazy_arguments(),
            _ => self.global_scalars(),
        }
    }

    /// The last top-level expression, whose value the report records. About one program in
    /// eight ends in an uncaught error instead.
    fn ending(&mut self) -> String {
        if self.rng.chance(1, 8) {
            return match self.rng.below(5) {
                0 => "stop(\"fuzz program stopped\")\n".to_owned(),
                1 => "undefined_variable_at_end + 1\n".to_owned(),
                2 => "f_end <- function(x) x + \"one\"\nf_end(1)\n".to_owned(),
                3 => "f_end <- function(a, b) a\nf_end(1, 2, 3)\n".to_owned(),
                _ => "if (c(TRUE, FALSE)) 1\n".to_owned(),
            };
        }
        if self.globals.is_empty() || self.rng.chance(1, 3) {
            let kind = self.kind();
            return format!("{}\n", self.scalar(kind, 2));
        }
        let names: Vec<String> = self.globals.iter().map(|(name, _)| name.clone()).collect();
        match self.rng.below(3) {
            0 => format!("list({})\n", names.join(", ")),
            1 => format!("invisible(c({}))\n", names.join(", ")),
            _ => format!("{}\n", names[self.rng.below(names.len())]),
        }
    }

    fn kind(&mut self) -> Kind {
        [Kind::Integer, Kind::Double, Kind::Logical, Kind::Character][self.rng.below(4)]
    }

    fn numeric_kind(&mut self) -> Kind {
        if self.rng.chance(1, 2) {
            Kind::Integer
        } else {
            Kind::Double
        }
    }

    /// Either `print(x)` or a `cat` line, so both printing paths are compared.
    fn show(&mut self, expression: &str) -> String {
        match self.rng.below(3) {
            0 => format!("cat(\"b{}:\", {expression}, \"\\n\")", self.block),
            _ => format!("print({expression})"),
        }
    }

    /// `line` itself in a well-typed block; in an ill-typed one, under `tryCatch` so the error is
    /// printed instead of ending the program.
    fn run(&mut self, line: &str) -> String {
        match self.ill_typed {
            true => format!(
                "tryCatch({line}, error = function(e) cat(\"error:\", conditionMessage(e), \"\\n\"))"
            ),
            false => line.to_owned(),
        }
    }

    /// `expression` evaluated under `tryCatch`, printing the error message when it fails.
    fn guarded(&mut self, expression: &str) -> String {
        let call = if self.rng.chance(1, 3) {
            " deparse(conditionCall(e)),"
        } else {
            ""
        };
        format!(
            "print(tryCatch({expression}, error = function(e) paste(\"error:\",{call} conditionMessage(e))))"
        )
    }

    fn hot_scalar_loop(&mut self) -> String {
        let k = self.block;
        let accumulator = self.numeric_kind();
        let initial = match accumulator {
            Kind::Integer => "0L",
            _ => "0",
        };
        let scope = vec![
            ("i".to_owned(), Kind::Integer),
            ("n".to_owned(), Kind::Integer),
            ("x".to_owned(), Kind::Double),
            ("acc".to_owned(), accumulator),
        ];
        let step = self.scalar_in(accumulator, 2, &scope);
        let skip = self.condition_in(&scope);
        let stop = self.condition_in(&scope);
        let operator = ["+", "-", "+", "*"][self.rng.below(4)];
        let body = match self.rng.below(3) {
            0 => format!(
                "for (i in seq_len(n)) {{\n    if ({skip}) next\n    acc <- acc {operator} {step}\n    if ({stop}) break\n  }}"
            ),
            1 => format!("for (i in seq_len(n)) acc <- acc {operator} {step}"),
            _ => format!(
                "i <- 0L\n  while (i < n) {{\n    i <- i + 1L\n    if ({skip}) next\n    acc <- acc {operator} {step}\n  }}"
            ),
        };
        let calls = 3 + self.rng.below(10);
        let size = 1 + self.rng.below(40);
        let argument = self.double_literal();
        let run = self.run(&format!(
            "for (r in 1:{calls}) r{k} <- f{k}({size}L, {argument})"
        ));
        let mut text = format!(
            "f{k} <- function(n, x) {{\n  acc <- {initial}\n  {body}\n  acc\n}}\nr{k} <- NA\n{run}\n"
        );
        let shown = self.show(&format!("r{k}"));
        text.push_str(&shown);
        text.push('\n');
        if self.rng.chance(1, 2) {
            let template = match accumulator {
                Kind::Integer => "integer(1)",
                _ => "numeric(1)",
            };
            let count = 1 + self.rng.below(8);
            let line = self.guarded(&format!(
                "vapply(1:{count}, function(j) f{k}(j, {argument}), {template})"
            ));
            text.push_str(&line);
            text.push('\n');
        }
        if !self.ill_typed {
            self.globals.push((format!("r{k}"), accumulator));
        }
        text
    }

    fn vector_writes(&mut self) -> String {
        let k = self.block;
        let size = 1 + self.rng.below(12);
        let scope = vec![("i".to_owned(), Kind::Integer)];
        let element = self.scalar_in(Kind::Double, 2, &scope);
        let (allocate, write) = match self.rng.below(7) {
            0 => ("numeric(n)", format!("v[i] <- {element}")),
            1 => ("integer(n)", "v[i] <- i * 2L".to_owned()),
            2 => (
                "numeric(0)",
                format!("v[i] <- v[max(1L, i - 1L)] + {element}"),
            ),
            3 => ("logical(n)", "v[[i]] <- i %% 2L == 0L".to_owned()),
            4 => ("vector(\"list\", n)", format!("v[i] <- list({element})")),
            5 => ("numeric(n)", format!("v[[(i %% n) + 1L]] <- {element}")),
            _ => ("character(n)", "v[i] <- paste0(\"s\", i)".to_owned()),
        };
        let read = match self.rng.below(4) {
            0 => "v".to_owned(),
            1 => "v[length(v)]".to_owned(),
            2 => "rev(v)".to_owned(),
            _ => "v[seq_len(min(3L, length(v)))]".to_owned(),
        };
        let run = self.run(&format!("for (r in 1:4) w{k} <- g{k}({size}L)"));
        let mut text = format!(
            "g{k} <- function(n) {{\n  v <- {allocate}\n  for (i in seq_len(n)) {write}\n  {read}\n}}\nw{k} <- NA\n{run}\n"
        );
        // `cat` rejects a list, so a list result is always printed.
        let shown = match allocate.starts_with("vector") {
            true => format!("print(w{k})"),
            false => self.show(&format!("w{k}")),
        };
        text.push_str(&shown);
        text.push('\n');
        if self.rng.chance(1, 2) {
            // Reads past the end give NA, and a write past the end grows the vector.
            text.push_str(&format!(
                "w{k}[{}] <- w{k}[1]\nprint(length(w{k}))\nprint(w{k}[{}])\n",
                size + 2,
                size + 1
            ));
        }
        text
    }

    fn while_and_repeat(&mut self) -> String {
        let k = self.block;
        let start = 1 + self.rng.below(60);
        let limit = 1 + self.rng.below(30);
        format!(
            "h{k} <- function(n) {{\n  steps <- 0L\n  repeat {{\n    if (n == 1) break\n    n <- if (n %% 2 == 0) n / 2 else 3 * n + 1\n    steps <- steps + 1L\n    if (steps >= {limit}L) break\n  }}\n  c(n = n, steps = steps)\n}}\nfor (r in 1:3) s{k} <- h{k}({start})\nprint(s{k})\nprint(vapply({start}:{}, function(j) h{k}(j)[[\"steps\"]], numeric(1)))\n",
            start + 4
        )
    }

    /// A root that passes a local on through two calls while a branch that has not run yet
    /// rebinds that local from the middle callee, before or after the inner callee would read
    /// the argument. Compiled code runs such calls without frames and may evaluate the argument
    /// ahead of the call, so when the branch runs, the frames it rebuilds must bind each formal
    /// forced or unforced exactly as the calls left it. Each callee runs alone first, because
    /// `RY_JIT=stress` compiles the root at its second call and inlines only callees it knows.
    fn lazy_arguments(&mut self) -> String {
        let k = self.block;
        let argument = ["x", "x * 2", "x + 1"][self.rng.below(3)];
        let (leaf, warm, call) = match self.rng.below(3) {
            0 => (
                "function(y) {\n  if (y < 0) stop(\"negative\")\n  y\n}",
                format!("leaf{k}(1)"),
                format!("leaf{k}({argument})"),
            ),
            1 => (
                "function(a, c) {\n  if (c < 0) stop(\"negative\")\n  if (c > 1) a else 0\n}",
                format!("leaf{k}(2, {})", [0, 2][self.rng.below(2)]),
                format!("leaf{k}({argument}, flag)"),
            ),
            _ => (
                "function(a, c) {\n  if (c < 0) stop(\"negative\")\n  c + 1\n}",
                format!("leaf{k}(2, 0)"),
                format!("leaf{k}({argument}, flag)"),
            ),
        };
        let rebind = "  if (flag > 0) assign(\"w\", 99, parent.frame())\n";
        let (before, after) = match self.rng.chance(1, 2) {
            true => (rebind, ""),
            false => ("", rebind),
        };
        format!(
            "leaf{k} <- {leaf}\nfor (i in 1:30) {warm}\nmiddle{k} <- function(x, flag) {{\n{before}  r <- {call}\n{after}  x + r\n}}\nfor (i in 1:30) middle{k}(1, 0)\nroot{k} <- function(flag) {{\n  w <- 1\n  middle{k}(w, flag)\n}}\nfor (i in 1:40) root{k}(0)\nfor (flag in c(0, 1, 2, 0)) print(root{k}(flag))\n"
        )
    }

    fn closures(&mut self) -> String {
        let k = self.block;
        let step = self.rng.below(5) + 1;
        let calls = 2 + self.rng.below(6);
        let mut text = format!(
            "make{k} <- function(start) {{\n  count <- start\n  function(by = {step}L) {{\n    count <<- count + by\n    count\n  }}\n}}\ncounter{k} <- make{k}({}L)\nfor (r in 1:{calls}) counter{k}()\nprint(counter{k}(0L))\n",
            self.rng.below(10)
        );
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "adders{k} <- lapply(1:3, function(i) function(x) x + i)\nprint(vapply(adders{k}, function(f) f(10), numeric(1)))\n"
            ));
        }
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "compose{k} <- function(f, g) function(x) g(f(x))\nprint(compose{k}(function(x) x * 2, function(x) x - 1)({}))\n",
                self.double_literal()
            ));
        }
        self.globals.push((format!("counter{k}()"), Kind::Integer));
        text
    }

    fn s3_dispatch(&mut self) -> String {
        let k = self.block;
        let class = ["circle", "square", "shape"][self.rng.below(3)];
        let value = self.double_literal();
        let mut text = format!(
            "area{k} <- function(s, ...) UseMethod(\"area{k}\")\narea{k}.default <- function(s, ...) stop(\"no area for \", class(s)[1])\narea{k}.circle <- function(s, ...) round(pi * s$r^2, 3)\narea{k}.square <- function(s, ...) s$r^2\narea{k}.shape <- function(s, ...) paste(\"shape\", NextMethod())\nobject{k} <- structure(list(r = {value}), class = c(\"{class}\", \"shape\"))\n"
        );
        let line = self.guarded(&format!("area{k}(object{k})"));
        text.push_str(&line);
        text.push('\n');
        let line = self.guarded(&format!("area{k}(1:3)"));
        text.push_str(&line);
        text.push('\n');
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "format.{class}{k} <- function(x, ...) \"formatted\"\nprint.{class}{k} <- function(x, ...) {{ cat(\"<{class}{k}\", x$r, \">\\n\"); invisible(x) }}\nprint(structure(list(r = {}), class = \"{class}{k}\"))\nprint(inherits(object{k}, \"shape\", which = TRUE))\n",
                self.rng.below(9)
            ));
        }
        if self.rng.chance(1, 2) {
            // Methods change between calls of one site, in the order a package and a user
            // session change them: a global mask and its removal, a registration, a frame-local
            // method, and a global mask over the registered method.
            let target = ["circle", "square", "shape"][self.rng.below(3)];
            let call = format!("area{k}(object{k})");
            let mut steps = [
                format!("area{k}.{target} <- function(s, ...) \"masked {target}\""),
                format!("rm(area{k}.{target})"),
                format!(
                    "registerS3method(\"area{k}\", \"{target}\", function(s, ...) \"registered {target}\")"
                ),
                format!(
                    "local{k} <- function(s) {{ area{k}.{target} <- function(s, ...) \"frame {target}\"; area{k}(s) }}\nprint(local{k}(object{k}))"
                ),
                format!("area{k}.{target} <- function(s, ...) \"global {target}\""),
                format!("rm(area{k}.{target})"),
            ];
            if self.rng.chance(1, 2) {
                steps[..3].rotate_right(1);
            }
            let line = self.guarded(&call);
            text.push_str(&format!("for (i in 1:2) {line}\n"));
            for step in steps {
                text.push_str(&step);
                text.push('\n');
                let line = self.guarded(&call);
                text.push_str(&line);
                text.push('\n');
            }
        }
        text
    }

    fn switch_block(&mut self) -> String {
        let k = self.block;
        let key = WORDS[self.rng.below(WORDS.len())];
        let numeric = 1 + self.rng.below(4);
        format!(
            "sw{k} <- function(key) switch(key, alpha = , b = \"first two\", Gamma = 3L, zz = {{ x <- 2; x * 4 }}, \"other\")\nfor (key in c(\"{key}\", \"alpha\", \"zz\", \"none\")) print(sw{k}(key))\nprint(switch({numeric}, \"one\", \"two\", \"three\"))\nprint(is.null(switch(\"missing\", a = 1)))\n{}\n",
            self.guarded("switch(NULL, a = 1)")
        )
    }

    fn recall_block(&mut self) -> String {
        let k = self.block;
        let n = self.rng.below(12);
        match self.rng.below(2) {
            0 => format!(
                "fact{k} <- function(n) if (n <= 1) 1 else n * Recall(n - 1)\nprint(fact{k}({n}))\nprint(sapply(0:{n}, fact{k}))\n"
            ),
            _ => format!(
                "fib{k} <- function(n) if (n < 2L) n else Recall(n - 1L) + Recall(n - 2L)\nprint(fib{k}({n}L))\nprint(vapply(0:{n}, fib{k}, integer(1)))\n"
            ),
        }
    }

    fn environments(&mut self) -> String {
        let k = self.block;
        let value = self.double_literal();
        let mut text = format!(
            "e{k} <- new.env()\nassign(\"a\", {value}, envir = e{k})\ne{k}$b <- 1:3\nlocal({{ c <- \"local\"; assign(\"c\", c, envir = e{k}) }})\nprint(sort(ls(e{k})))\nprint(mget(c(\"a\", \"b\"), envir = e{k}))\nprint(exists(\"zz{k}\", envir = e{k}, inherits = FALSE))\nprint(get0(\"zz{k}\", envir = e{k}, ifnotfound = \"absent\"))\n"
        );
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "bump{k} <- function(env) env$a <- env$a + 1\nfor (r in 1:5) bump{k}(e{k})\nprint(e{k}$a)\n"
            ));
        }
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "child{k} <- new.env(parent = e{k})\nprint(get(\"b\", envir = child{k}))\nprint(identical(parent.env(child{k}), e{k}))\nprint(environmentName(globalenv()))\n"
            ));
        }
        let line = self.guarded(&format!("get(\"nope{k}\", envir = e{k}, inherits = FALSE)"));
        text.push_str(&line);
        text.push('\n');
        text
    }

    fn attributes(&mut self) -> String {
        let k = self.block;
        let size = 2 + self.rng.below(5);
        let mut text = format!(
            "a{k} <- seq_len({size}) * {}\nnames(a{k}) <- letters[seq_len({size})]\nattr(a{k}, \"unit\") <- \"cm\"\nprint(a{k})\nprint(attributes(a{k}))\nprint(a{k}[c(\"b\", \"a\")])\n",
            self.double_literal()
        );
        match self.rng.below(3) {
            0 => text.push_str(&format!(
                "b{k} <- structure(1:{}, dim = c(2L, {size}L), dimnames = list(c(\"r1\", \"r2\"), NULL))\nprint(b{k})\nprint(dim(t(b{k})))\n",
                2 * size
            )),
            1 => text.push_str(&format!(
                "class(a{k}) <- \"tagged{k}\"\nprint(unclass(a{k}))\nprint(inherits(a{k}, \"tagged{k}\"))\n"
            )),
            _ => text.push_str(&format!(
                "a{k}[[\"z\"]] <- 0\nprint(names(a{k}))\nattributes(a{k}) <- NULL\nprint(a{k})\n"
            )),
        }
        let line = self.guarded(&format!("dim(a{k}) <- c(7L, 9L)"));
        text.push_str(&line);
        text.push('\n');
        text
    }

    fn na_and_overflow(&mut self) -> String {
        let k = self.block;
        let mut text = format!(
            "n{k} <- c(1L, NA, 3L, {}L)\nprint(sum(n{k}))\nprint(sum(n{k}, na.rm = TRUE))\nprint(mean(c(1.5, NA, NaN), na.rm = {}))\nprint(is.na(n{k} + NA_integer_))\nprint(NA > 1)\nprint(NA & FALSE)\nprint(NA || TRUE)\n",
            self.rng.below(100),
            ["TRUE", "FALSE"][self.rng.below(2)]
        );
        match self.rng.below(3) {
            0 => text.push_str(&format!(
                "big{k} <- function(x) x + 1L\nfor (r in 1:3) o{k} <- big{k}(.Machine$integer.max)\nprint(o{k})\n"
            )),
            1 => text.push_str(&format!(
                "mul{k} <- function(n) {{ x <- 1L; for (i in seq_len(n)) x <- x * 7L; x }}\nprint(mul{k}({}L))\n",
                5 + self.rng.below(10)
            )),
            _ => text.push_str("print(-.Machine$integer.max - 1L)\nprint(as.integer(3e9))\nprint(2147483647L %/% -1L)\n"),
        }
        text
    }

    fn recycling(&mut self) -> String {
        let k = self.block;
        let left = 1 + self.rng.below(6);
        let right = 1 + self.rng.below(4);
        format!(
            "rc{k} <- function(a, b) a + b\nfor (r in 1:3) q{k} <- rc{k}(1:{left}, seq_len({right}) * 10)\nprint(q{k})\nprint(c(1, 2, 3)[c(TRUE, FALSE)])\nprint(pmax(1:{left}, {right}L))\nprint(rep_len(c(\"x\", \"y\"), {left}))\n"
        )
    }

    fn conditions(&mut self) -> String {
        let k = self.block;
        let mut text = format!(
            "cond{k} <- function(x) {{\n  if (x > 2) warning(\"big x: \", x)\n  if (x > 4) stop(\"too big: \", x)\n  message(\"x is \", x)\n  x * 2\n}}\n"
        );
        // Values above 4 stop, so only the branches that catch errors get them.
        let value = self.rng.below(5);
        let risky = self.rng.below(7);
        text.push_str(&match self.rng.below(5) {
            0 => format!(
                "print(withCallingHandlers(cond{k}({value}), warning = function(w) {{ cat(\"caught:\", conditionMessage(w), \"\\n\"); invokeRestart(\"muffleWarning\") }}, message = function(m) invokeRestart(\"muffleMessage\")))\n"
            ),
            1 => format!(
                "print(tryCatch(cond{k}({risky}), warning = function(w) paste(\"warned\", conditionMessage(w)), error = function(e) conditionMessage(e), finally = cat(\"finally\\n\")))\n"
            ),
            2 => format!(
                "res{k} <- try(cond{k}({risky}), silent = TRUE)\nprint(class(res{k}))\nif (inherits(res{k}, \"try-error\")) print(conditionMessage(attr(res{k}, \"condition\")))\n"
            ),
            3 => format!(
                "custom{k} <- structure(class = c(\"custom{k}\", \"condition\"), list(message = \"custom signal\", call = NULL))\nprint(withCallingHandlers(tryCatch({{ signalCondition(custom{k}); \"not caught\" }}, custom{k} = function(c) \"caught custom\"), condition = function(c) cat(\"saw\", class(c)[1], \"\\n\")))\n"
            ),
            _ => format!(
                "print(tryCatch({{ warning(\"first\"); \"unreached\" }}, warning = function(w) conditionMessage(w)))\nouter{k} <- function() {{ on.exit(cat(\"exit {k}\\n\"), add = TRUE); cond{k}({risky}) }}\n{}\n",
                self.guarded(&format!("suppressMessages(suppressWarnings(outer{k}()))"))
            ),
        });
        if self.rng.chance(1, 3) {
            // Uncaught at top level, so the driver records the warning and its call.
            text.push_str(&format!(
                "w{k} <- suppressMessages(cond{k}({}))\n",
                self.rng.below(5)
            ));
        }
        text
    }

    fn functionals(&mut self) -> String {
        let k = self.block;
        let size = 1 + self.rng.below(8);
        let scope = vec![("x".to_owned(), Kind::Integer)];
        let body = self.scalar_in(Kind::Double, 2, &scope);
        let mut text = format!("fn{k} <- function(x) {body}\n");
        let sapply = self.run(&format!("print(sapply(1:{size}, fn{k}))"));
        let lapply = self.run(&format!("print(unlist(lapply(1:{size}, fn{k})))"));
        text.push_str(&match self.rng.below(6) {
            0 => format!("{sapply}\n"),
            1 => format!("{lapply}\n"),
            2 => format!("print(Reduce(`+`, 1:{size}, accumulate = TRUE))\nprint(Filter(function(v) v %% 2L == 0L, 1:{size}))\n"),
            3 => format!("print(Map(function(a, b) a * b, 1:{size}, {size}:1)[[1]])\nprint(mapply(function(a, b) a + b, 1:3, c(10, 20, 30)))\n"),
            4 => format!("print(do.call(\"sum\", as.list(1:{size})))\nprint(do.call(paste, list(\"a\", \"b\", sep = \"-\")))\n"),
            _ => format!("{}\n", self.guarded(&format!("vapply(1:{size}, fn{k}, character(1))"))),
        });
        text
    }

    fn arity_and_missing(&mut self) -> String {
        let k = self.block;
        let mut text = format!(
            "two{k} <- function(a, b = a * 2) a + b\nprint(two{k}(1))\nprint(two{k}(b = 1, a = 5))\n"
        );
        let lines = [
            format!("two{k}(1, 2, 3)"),
            format!("two{k}(c = 1)"),
            format!("two{k}()"),
            format!("missing_function{k}(1)"),
            format!("undefined{k} * 2"),
            format!("(function(x) {{ force(x); 1 }})(undefined{k})"),
            "(function(x) missing(x))()".to_owned(),
        ];
        for _ in 0..2 {
            let choice = self.rng.below(lines.len());
            let line = self.guarded(&lines[choice]);
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    /// An atomic or `NULL` default may be bound without a promise, so `missing()`, reassigning
    /// the formal, and a later default that reads it must all still behave as in GNU R.
    fn constant_defaults(&mut self) -> String {
        let k = self.block;
        let default = ["1L", "2.5", "NULL", "\"word\"", "TRUE", "NA"][self.rng.below(6)];
        let calls = 2 + self.rng.below(4);
        format!(
            "cd{k} <- function(x = {default}, y = x) {{\n  first <- missing(x)\n  if (is.null(x)) x <- 0L\n  x <- c(x, 1L)\n  list(first, missing(y), x, y)\n}}\nfor (r in 1:{calls}) cd{k}()\nstr(cd{k}())\nstr(cd{k}(5L))\nstr(cd{k}(y = 3))\nprint(identical(cd{k}(), cd{k}()))\nnd{k} <- function(n = {default}) {{ n <- 7L; n }}\nprint(nd{k}())\nprint(nd{k}())\n"
        )
    }

    /// A loop may overwrite its scalar in place when only the binding holds it, so every way
    /// of keeping a reference to the loop value must see the value of its own iteration.
    fn loop_variable_aliasing(&mut self) -> String {
        let k = self.block;
        let values = [
            "c(1.5, 2.5, 3.5)",
            "1:4",
            "c(TRUE, FALSE, NA)",
            "c(\"a\", \"b\")",
        ][self.rng.below(4)];
        format!(
            "kept{k} <- list()\nclosures{k} <- list()\nfor (v in {values}) {{\n  kept{k}[[length(kept{k}) + 1L]] <- v\n  local({{ w <- v; closures{k}[[length(closures{k}) + 1L]] <<- function() w }})\n  alias{k} <- v\n}}\nstr(kept{k})\nstr(lapply(closures{k}, function(f) f()))\nprint(alias{k})\nprint(v)\nfor (v in {values}) {{ v <- rev(c(v, v)); last{k} <- v }}\nprint(last{k})\nenv{k} <- new.env()\nfor (v in {values}) assign(paste0(\"n\", length(ls(env{k}))), v, envir = env{k})\nstr(mget(sort(ls(env{k})), envir = env{k}))\n"
        )
    }

    fn strings(&mut self) -> String {
        let k = self.block;
        let first = WORDS[self.rng.below(WORDS.len())];
        let second = WORDS[self.rng.below(WORDS.len())];
        let number = self.double_literal();
        format!(
            "s{k} <- c(\"{first}\", \"{second}\", NA)\nprint(nchar(s{k}))\nprint(toupper(s{k}))\nprint(paste(s{k}, collapse = \"|\"))\nprint(paste0(\"<\", s{k}, \">\"))\nprint(substr(\"{first}{second}\", 2, 4))\nprint(sprintf(\"%5.2f|%d|%s\", {number}, {}L, \"{first}\"))\nprint(format({number}, nsmall = 2))\nprint(as.character({number}))\nprint(rev(strsplit(\"a,b,,c\", \",\")[[1]]))\n{}\n",
            self.rng.below(1000),
            self.guarded("sprintf(\"%d\", 1.5)")
        )
    }

    fn list_loop(&mut self) -> String {
        let k = self.block;
        let size = 1 + self.rng.below(6);
        let mut text = format!(
            "l{k} <- list()\nfor (i in 1:{size}) l{k}[[paste0(\"k\", i)]] <- i * 1.5\nl{k}$extra <- NULL\nprint(names(l{k}))\nprint(l{k}[[{size}]])\ntotal{k} <- 0\nfor (item in l{k}) total{k} <- total{k} + item\nprint(total{k})\n"
        );
        if self.rng.chance(1, 2) {
            text.push_str(&format!(
                "nested{k} <- list(a = list(b = list(c = {size})))\nnested{k}$a$b$c <- nested{k}$a$b$c + 1\nprint(nested{k}[[c(\"a\", \"b\", \"c\")]])\nprint(rapply(list(1, list(2, 3)), function(x) x * 10, how = \"unlist\"))\n"
            ));
        }
        let line = self.guarded(&format!("l{k}[[{}]]", size + 3));
        text.push_str(&line);
        text.push('\n');
        self.globals.push((format!("total{k}"), Kind::Double));
        text
    }

    /// Names appended and replaced with `$<-`, `[[<-` and `[<-` on locals, shared copies and
    /// arguments, over lists and atomic vectors, so every in-place and copying path of a
    /// compiled element assignment meets GNU R's replacement functions.
    fn named_appends(&mut self) -> String {
        let k = self.block;
        let start = [
            "list()",
            "list(a = 1)",
            "c(a = 1)",
            "c(a = 1L)",
            "c(a = \"x\")",
            "list(1, 2)",
            "c(1, 2)",
        ][self.rng.below(7)];
        let value = match start {
            "c(a = 1L)" => "i",
            "c(a = \"x\")" => "as.character(i)",
            _ => "i * 0.5",
        };
        let name = [
            "paste0(\"k\", i %% 3)",
            "\"a\"",
            "letters[i]",
            "NA_character_",
        ][self.rng.below(4)];
        let form = match self.rng.below(3) {
            0 => format!("x[[{name}]] <- {value}"),
            1 => format!("x[{name}] <- {value}"),
            _ => format!("x$k <- {value}"),
        };
        let share = if self.rng.chance(1, 2) {
            "kept <- x\n    "
        } else {
            ""
        };
        let size = 1 + self.rng.below(6);
        format!(
            "ap{k} <- function(x, n) {{\n  for (i in seq_len(n)) {{\n    {share}{form}\n  }}\n  x\n}}\nbase{k} <- {start}\nstr(ap{k}(base{k}, {size}))\nstr(base{k})\nstr(Reduce(function(acc, i) {{ acc[[paste0(\"r\", i %% 2)]] <- i; acc }}, seq_len({size}), {start}))\n"
        )
    }

    /// Whole rows, columns and slabs assigned with recycling, and `round` over vectors with
    /// ties, signed zeros, NA and a random digit count.
    fn slabs_and_rounding(&mut self) -> String {
        let k = self.block;
        let rows = 1 + self.rng.below(5);
        let columns = 1 + self.rng.below(4);
        let column = 1 + self.rng.below(columns);
        let row = 1 + self.rng.below(rows);
        let recycle = if rows % 2 == 0 { "c(-1, 1)" } else { "-1" };
        let digits = self.rng.below(7) as i64 - 2;
        let kind = ["0", "0L", "FALSE"][self.rng.below(3)];
        let subscript =
            ["i", "-i", "i + 0.5", "-(i + 0L)", "c(i, NA)", "i - 1L"][self.rng.below(6)];
        let fill = match kind {
            "0L" => format!("seq_len({rows})"),
            "FALSE" => format!("rep(c(TRUE, NA), length.out = {rows})"),
            _ => format!("seq_len({rows}) / 3"),
        };
        format!(
            "sm{k} <- matrix({kind}, {rows}, {columns})\nsalias{k} <- sm{k}\nsm{k}[, {column}] <- {fill}\nprint(sm{k})\nprint(salias{k})\nsm{k}[{row}, ] <- sm{k}[{row}, ]\nsn{k} <- matrix(0, {rows}, {columns})\nsn{k}[, {column}] <- {recycle}\nprint(sn{k})\nprint(round(c(0.5, 1.5, 2.5, -0.5, -0, 0.125, 2.675, NA, NaN, Inf, {rows}.{columns}5), {digits}))\nprint(round(seq_len({rows}) / 7 * 100, {digits}))\nprint(c(a = {rows}, b = {columns}, {rows}))\nfor (i in seq_len({rows} + 1L)) print(list(sm{k}[{subscript}], c(p = 1, q = 2)[{subscript}], -i))\n"
        )
    }

    fn integer_arithmetic(&mut self) -> String {
        let k = self.block;
        let left = self.rng.below(41) as i64 - 20;
        let right = self.rng.below(11) as i64 - 5;
        format!(
            "ia{k} <- c({left}L %/% {right}L, {left}L %% {right}L, {left} %/% {right}, {left} %% {right}, {left}L / {right}L)\nprint(ia{k})\nprint(class({left}L * {right}L))\nprint(bitwAnd({}L, {}L))\nprint(5L:1L - 1L)\nprint(seq(1, 2, by = 0.25))\n",
            self.rng.below(64),
            self.rng.below(64)
        )
    }

    fn matrix_block(&mut self) -> String {
        let k = self.block;
        let rows = 1 + self.rng.below(3);
        let columns = 1 + self.rng.below(3);
        format!(
            "m{k} <- matrix(seq_len({}), nrow = {rows})\nfor (i in seq_len(nrow(m{k}))) for (j in seq_len(ncol(m{k}))) m{k}[i, j] <- m{k}[i, j] * i + j\nprint(m{k})\nprint(rowSums(m{k}))\nprint(m{k} %*% t(m{k}))\nprint(m{k}[, 1, drop = FALSE])\n{}\n",
            rows * columns,
            self.guarded(&format!("m{k}[{}, 1]", rows + 1))
        )
    }

    /// Ill-typed expressions under `tryCatch`, so the error message itself is compared.
    fn ill_typed_block(&mut self) -> String {
        self.ill_typed = true;
        let mut text = String::new();
        for _ in 0..1 + self.rng.below(3) {
            let kind = self.kind();
            let expression = self.scalar(kind, 3);
            let line = self.guarded(&expression);
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    fn global_scalars(&mut self) -> String {
        let k = self.block;
        let kind = self.kind();
        let expression = self.scalar(kind, 3);
        let mut text = if self.ill_typed {
            format!(
                "v{k} <- tryCatch({expression}, error = function(e) {})\n",
                self.default_value(kind)
            )
        } else {
            format!("v{k} <- {expression}\n")
        };
        // An ill-typed value may be a list, which `cat` rejects.
        let shown = match self.ill_typed {
            true => format!("print(v{k})"),
            false => self.show(&format!("v{k}")),
        };
        text.push_str(&shown);
        text.push('\n');
        if !self.ill_typed {
            self.globals.push((format!("v{k}"), kind));
        }
        text
    }

    fn default_value(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::Integer => "-1L",
            Kind::Double => "-1",
            Kind::Logical => "NA",
            Kind::Character => "\"error\"",
        }
    }

    fn double_literal(&mut self) -> String {
        [
            "0.5", "1", "2.25", "-3", "10", "1e-3", "1/3", "100.125", "0", "7",
        ][self.rng.below(10)]
        .to_owned()
    }

    fn scalar(&mut self, kind: Kind, depth: usize) -> String {
        let scope = self.globals.clone();
        self.scalar_in(kind, depth, &scope)
    }

    fn condition_in(&mut self, scope: &[(String, Kind)]) -> String {
        let left = self.scalar_in(Kind::Integer, 1, scope);
        let right = self.scalar_in(Kind::Integer, 0, scope);
        let operator = ["<", ">", "==", "!=", "<=", ">="][self.rng.below(6)];
        if self.ill_typed {
            return format!("{left} {operator} {right}");
        }
        // A well-typed condition must not be NA, and either side may be NA at run time (an
        // overflowed accumulator, a literal `NA_integer_`), so both are tested first.
        match self.rng.below(2) {
            0 => format!("isTRUE({left} {operator} {right})"),
            _ => format!("!is.na({left}) && !is.na({right}) && {left} {operator} {right}"),
        }
    }

    /// A scalar expression of `kind` over the variables in `scope`.
    fn scalar_in(&mut self, kind: Kind, depth: usize, scope: &[(String, Kind)]) -> String {
        if self.ill_typed && self.rng.chance(1, 6) {
            return self.wrong_value(kind);
        }
        if depth == 0 || self.rng.chance(1, 3) {
            return self.leaf(kind, scope);
        }
        let below = depth - 1;
        match kind {
            Kind::Integer => match self.rng.below(9) {
                0 | 1 => {
                    let operator = ["+", "-", "*"][self.rng.below(3)];
                    format!(
                        "({} {operator} {})",
                        self.scalar_in(Kind::Integer, below, scope),
                        self.scalar_in(Kind::Integer, below, scope)
                    )
                }
                2 => format!(
                    "({} %/% {})",
                    self.scalar_in(Kind::Integer, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                3 => format!(
                    "({} %% {})",
                    self.scalar_in(Kind::Integer, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                4 => format!("abs({})", self.scalar_in(Kind::Integer, below, scope)),
                5 => format!("as.integer({})", self.scalar_in(Kind::Double, below, scope)),
                6 => format!("nchar({})", self.scalar_in(Kind::Character, below, scope)),
                7 => format!(
                    "length(c({}, {}))",
                    self.scalar_in(Kind::Integer, below, scope),
                    self.scalar_in(Kind::Double, below, scope)
                ),
                _ => self.branch(Kind::Integer, below, scope),
            },
            Kind::Double => match self.rng.below(10) {
                0..=2 => {
                    let operator = ["+", "-", "*", "/"][self.rng.below(4)];
                    let right = self.numeric_kind();
                    format!(
                        "({} {operator} {})",
                        self.scalar_in(Kind::Double, below, scope),
                        self.scalar_in(right, below, scope)
                    )
                }
                3 => format!(
                    "({} ^ {})",
                    self.scalar_in(Kind::Double, below, scope),
                    self.rng.below(4)
                ),
                4 => format!("sqrt(abs({}))", self.scalar_in(Kind::Double, below, scope)),
                5 => format!(
                    "round({}, {})",
                    self.scalar_in(Kind::Double, below, scope),
                    self.rng.below(4)
                ),
                6 => format!(
                    "sum(c({}, {}))",
                    self.scalar_in(Kind::Double, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                7 => format!(
                    "as.numeric({})",
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                8 => format!("floor({})", self.scalar_in(Kind::Double, below, scope)),
                _ => self.branch(Kind::Double, below, scope),
            },
            Kind::Logical => match self.rng.below(6) {
                0 | 1 => {
                    let operator = ["<", ">", "==", "!=", "<=", ">="][self.rng.below(6)];
                    let operand = self.numeric_kind();
                    format!(
                        "({} {operator} {})",
                        self.scalar_in(operand, below, scope),
                        self.scalar_in(Kind::Double, below, scope)
                    )
                }
                2 => format!(
                    "({} & {})",
                    self.scalar_in(Kind::Logical, below, scope),
                    self.scalar_in(Kind::Logical, below, scope)
                ),
                3 => format!("!{}", self.scalar_in(Kind::Logical, below, scope)),
                4 => {
                    let operand = self.kind();
                    format!("is.na({})", self.scalar_in(operand, below, scope))
                }
                _ => format!(
                    "identical({}, {})",
                    self.scalar_in(Kind::Integer, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
            },
            Kind::Character => match self.rng.below(6) {
                0 => format!(
                    "paste0({}, {})",
                    self.scalar_in(Kind::Character, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                1 => format!("toupper({})", self.scalar_in(Kind::Character, below, scope)),
                2 => format!(
                    "as.character({})",
                    self.scalar_in(Kind::Double, below, scope)
                ),
                3 => format!(
                    "substr({}, 1L, 3L)",
                    self.scalar_in(Kind::Character, below, scope)
                ),
                4 => format!(
                    "sprintf(\"%s=%d\", {}, {})",
                    self.scalar_in(Kind::Character, below, scope),
                    self.scalar_in(Kind::Integer, below, scope)
                ),
                _ => self.branch(Kind::Character, below, scope),
            },
        }
    }

    fn branch(&mut self, kind: Kind, depth: usize, scope: &[(String, Kind)]) -> String {
        let condition = self.scalar_in(Kind::Logical, depth, scope);
        // `isTRUE` keeps an NA condition from stopping a well-typed program; an ill-typed one
        // tests the condition bare, so `if (NA)` and `if (c(TRUE, FALSE))` are reached.
        let condition = match self.ill_typed {
            true => condition,
            false => format!("isTRUE({condition})"),
        };
        format!(
            "(if ({condition}) {} else {})",
            self.scalar_in(kind, depth, scope),
            self.scalar_in(kind, depth, scope)
        )
    }

    fn leaf(&mut self, kind: Kind, scope: &[(String, Kind)]) -> String {
        let variables: Vec<&String> = scope
            .iter()
            .filter(|(_, variable_kind)| *variable_kind == kind)
            .map(|(name, _)| name)
            .collect();
        if !variables.is_empty() && self.rng.chance(1, 2) {
            return variables[self.rng.below(variables.len())].clone();
        }
        match kind {
            Kind::Integer => [
                "0L",
                "1L",
                "2L",
                "3L",
                "-4L",
                "7L",
                "12L",
                "NA_integer_",
                "2147483647L",
            ][self.rng.below(9)]
            .to_owned(),
            Kind::Double => self.double_literal(),
            Kind::Logical => ["TRUE", "FALSE", "NA"][self.rng.below(3)].to_owned(),
            Kind::Character => format!("\"{}\"", WORDS[self.rng.below(WORDS.len())]),
        }
    }

    /// A value of the wrong type or shape for a slot of `kind`.
    fn wrong_value(&mut self, kind: Kind) -> String {
        match kind {
            Kind::Integer | Kind::Double => [
                "\"seven\"",
                "NULL",
                "list(1)",
                "c(1, 2)",
                "undefined_value",
                "sum",
            ][self.rng.below(6)],
            Kind::Logical => {
                ["NA", "c(TRUE, FALSE)", "\"maybe\"", "NULL", "logical(0)"][self.rng.below(5)]
            }
            Kind::Character => {
                ["list(\"a\")", "undefined_text", "c(\"a\", \"b\")", "NULL"][self.rng.below(4)]
            }
        }
        .to_owned()
    }
}

/// A family of generated programs. Each maps a seed to one program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgramKind {
    /// [`program`]: several blocks from a grammar of language features.
    Grammar,
    /// [`metamorphic::program`](crate::metamorphic::program): a value from the zoo, a body that
    /// mutates it, and the forms the body must agree in.
    Metamorphic,
    /// [`metamorphic::builtins_program`](crate::metamorphic::builtins_program): builtins called
    /// on values from the zoo.
    Builtins,
    /// [`metamorphic::api_program`](crate::metamorphic::api_program): calls drawn from every
    /// function base, stats and utils export, on values from the zoo.
    Api,
    /// [`metamorphic::io_program`](crate::metamorphic::io_program): zoo values written to files
    /// and connections and read back.
    Io,
    /// [`language::program`](crate::language::program): closures built from the constructs
    /// that depend on the call stack, evaluation order and dispatch, called repeatedly.
    Language,
    /// A grammar, metamorphic or language program after [`mutate::perturb`](crate::mutate::perturb):
    /// the same computation in bodies the compiler sees differently.
    Perturbed,
    /// [`parser::program`](crate::parser::program): source fragments through `parse(text =)`,
    /// with the tree or the error of each.
    Parser,
    /// [`frame_program`]: DataFrame verb chains and relations, against a base-R model of the
    /// documented contract under GNU R.
    Frame,
}

/// The writable memory a fuzzed engine may map. GNU R and ry both run every generated program
/// in a fraction of it, and a program that asks for more gets R's allocation error.
pub const MEMORY_LIMIT: u64 = 2 << 30;

pub const KINDS: [ProgramKind; 9] = [
    ProgramKind::Grammar,
    ProgramKind::Metamorphic,
    ProgramKind::Builtins,
    ProgramKind::Api,
    ProgramKind::Io,
    ProgramKind::Language,
    ProgramKind::Perturbed,
    ProgramKind::Parser,
    ProgramKind::Frame,
];

impl ProgramKind {
    pub fn name(self) -> &'static str {
        match self {
            ProgramKind::Grammar => "grammar",
            ProgramKind::Metamorphic => "metamorphic",
            ProgramKind::Builtins => "builtins",
            ProgramKind::Api => "api",
            ProgramKind::Io => "io",
            ProgramKind::Language => "language",
            ProgramKind::Perturbed => "perturbed",
            ProgramKind::Parser => "parser",
            ProgramKind::Frame => "frame",
        }
    }

    pub fn named(name: &str) -> Option<ProgramKind> {
        KINDS.into_iter().find(|kind| kind.name() == name)
    }

    /// The program of this kind for `seed`.
    pub fn program(self, seed: u64) -> String {
        match self {
            ProgramKind::Grammar => program(seed),
            ProgramKind::Metamorphic => crate::metamorphic::program(seed),
            ProgramKind::Builtins => crate::metamorphic::builtins_program(seed),
            ProgramKind::Api => crate::metamorphic::api_program(seed),
            ProgramKind::Io => crate::metamorphic::io_program(seed),
            ProgramKind::Language => crate::language::program(seed),
            ProgramKind::Perturbed => {
                let base = match seed % 3 {
                    0 => program(seed),
                    1 => crate::metamorphic::program(seed),
                    _ => crate::language::program(seed),
                };
                crate::mutate::perturb(&base, seed)
            }
            ProgramKind::Parser => crate::parser::program(seed),
            ProgramKind::Frame => frame_program(seed),
        }
    }
}

/// What one seed's program did under the reference engine and every other engine.
pub struct SeedReport {
    pub kind: ProgramKind,
    pub seed: u64,
    pub program: String,
    /// GNU R's outcome, against which every other engine is compared.
    pub reference: Outcome,
    /// One entry per engine that disagreed with the reference or failed to run.
    pub divergences: Vec<Divergence>,
    /// Engines that could not run here at all.
    pub unavailable: Vec<String>,
}

pub struct Divergence {
    pub engine: String,
    /// A unified diff against the reference, or why the engine produced no outcome.
    pub detail: String,
    /// The top-level expressions whose observed values or conditions differ (at most 64), when both
    /// engines were observed.
    pub expressions: Vec<DifferingExpression>,
    /// The engine finished the program and reported an outcome.
    pub ran: bool,
    /// The warnings, messages or errors differ in text. With no differing expression (the
    /// engines were not observed), it tells a condition difference from a printing one.
    pub conditions: bool,
    /// The engine died on a signal or a panic instead of finishing the program.
    pub crashed: bool,
}

impl Divergence {
    /// The names of the ways this engine differs, for a tally: `crash`, `failed`, or each
    /// differing expression's [`Difference`]. With no differing expression it is `conditions`
    /// when the condition text differs and `printed` when only the printed text does.
    pub fn kinds(&self) -> Vec<&'static str> {
        if self.crashed {
            return vec!["crash"];
        }
        let mut kinds: Vec<&'static str> = self
            .expressions
            .iter()
            .map(|expression| expression.difference.name())
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        if kinds.is_empty() {
            kinds.push(match (self.ran, self.conditions) {
                (false, _) => "failed",
                (true, true) => "conditions",
                (true, false) => "printed",
            });
        }
        kinds
    }
}

pub struct DifferingExpression {
    pub source: String,
    pub difference: Difference,
}

/// How an engine's result for one expression differs from the reference's. A generated
/// program catches an error and makes its message the value, as a string that starts with
/// `error:`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Difference {
    /// Both gave a value, and the values differ.
    Value,
    /// The engine gave a value where the reference signals an error.
    Accepts,
    /// The engine signals an error where the reference gave a value.
    Rejects,
    /// Both signal an error, with different messages.
    Message,
    /// The values agree; warnings, messages or their classes differ.
    Conditions,
}

impl Difference {
    pub fn name(self) -> &'static str {
        match self {
            Difference::Value => "value",
            Difference::Accepts => "accepts",
            Difference::Rejects => "rejects",
            Difference::Message => "message",
            Difference::Conditions => "conditions",
        }
    }
}

/// How the engine's entries for one expression differ from the reference's.
pub fn classify(difference: &ExpressionDifference) -> Difference {
    fn value(entries: &[String]) -> Option<&String> {
        entries
            .iter()
            .find(|entry| entry.starts_with("visible ") || entry.starts_with("invisible "))
    }
    // The value's own line, not a string nested in a list: `visible character[1] -"error: ...`,
    // or the string's bytes when it holds a non-ASCII character, under any encoding mark.
    fn caught(value: &str) -> bool {
        let Some(token) = value
            .split_once(' ')
            .and_then(|(_, dump)| dump.strip_prefix("character[1] "))
        else {
            return false;
        };
        let mut characters = token.chars();
        characters.next();
        let rest = characters.as_str();
        rest.starts_with("\"error: ") || rest.starts_with("<6572726f723a20")
    }
    match (value(&difference.reference), value(&difference.other)) {
        (Some(reference), Some(other)) if reference == other => Difference::Conditions,
        (Some(reference), Some(other)) => match (caught(reference), caught(other)) {
            (true, true) => Difference::Message,
            (true, false) => Difference::Accepts,
            (false, true) => Difference::Rejects,
            (false, false) => Difference::Value,
        },
        (Some(_), None) => Difference::Rejects,
        (None, Some(_)) => Difference::Accepts,
        (None, None) => Difference::Message,
    }
}

/// Runs `seed`'s program under `reference` and under each of `engines`, in a file written to
/// `directory`, and compares every engine with the reference.
pub fn check_seed(
    kind: ProgramKind,
    seed: u64,
    reference: &dyn Engine,
    engines: &[&dyn Engine],
    directory: &Path,
) -> Result<SeedReport, EngineError> {
    let source = kind.program(seed);
    let path = directory.join(format!("{}_{seed}.R", kind.name()));
    std::fs::write(&path, &source)
        .map_err(|error| EngineError::Failed(format!("{}: {error}", path.display())))?;
    let reference_outcome = reference.run(&path)?;
    let (divergences, unavailable) =
        compare_engines(&source, reference, &reference_outcome, engines, &path);
    if let Err(error) = std::fs::remove_file(&path) {
        eprintln!("could not remove {}: {error}", path.display());
    }
    Ok(SeedReport {
        kind,
        seed,
        program: source,
        reference: reference_outcome,
        divergences,
        unavailable,
    })
}

/// Runs the program `source`, stored at `path`, under each of `engines` and compares every
/// outcome with the reference's. Returns the engines that differ and the reasons of the engines
/// that cannot run here.
pub fn compare_engines(
    source: &str,
    reference: &dyn Engine,
    reference_outcome: &Outcome,
    engines: &[&dyn Engine],
    path: &Path,
) -> (Vec<Divergence>, Vec<String>) {
    let top_level = top_level_expressions(source);
    let mut divergences = Vec::new();
    let mut unavailable = Vec::new();
    for engine in engines {
        match engine.run(path) {
            Ok(outcome) => {
                if let Some(detail) = diff(
                    &reference.name(),
                    reference_outcome,
                    &engine.name(),
                    &outcome,
                ) {
                    let expressions = differing_expressions(reference_outcome, &outcome)
                        .iter()
                        .take(64)
                        .filter_map(|difference| {
                            Some(DifferingExpression {
                                source: top_level.get(difference.position.checked_sub(1)?)?.clone(),
                                difference: classify(difference),
                            })
                        })
                        .collect();
                    divergences.push(Divergence {
                        engine: engine.name(),
                        detail,
                        expressions,
                        ran: true,
                        conditions: reference_outcome.errors != outcome.errors
                            || reference_outcome.warnings != outcome.warnings
                            || reference_outcome.messages != outcome.messages,
                        crashed: false,
                    });
                }
            }
            Err(EngineError::Unavailable(reason)) => {
                unavailable.push(format!("{}: {reason}", engine.name()))
            }
            Err(error @ (EngineError::Failed(_) | EngineError::Crashed(_))) => {
                divergences.push(Divergence {
                    engine: engine.name(),
                    crashed: matches!(error, EngineError::Crashed(_)),
                    detail: error.to_string(),
                    expressions: Vec::new(),
                    ran: false,
                    conditions: false,
                })
            }
        }
    }
    (divergences, unavailable)
}

/// The failure report for one seed: the program, every engine's difference, and the commands
/// that reproduce it.
pub fn render_divergences(report: &SeedReport) -> String {
    let engines: Vec<&str> = report
        .divergences
        .iter()
        .map(|divergence| divergence.engine.as_str())
        .collect();
    let (kind, seed) = (report.kind.name(), report.seed);
    let mut text = format!(
        "{kind} seed {seed} diverges under {}\nreproduce: cargo run -q -p oracle -- fuzz --kind {kind} --seed {seed} --engines {}\nprogram:   cargo run -q -p oracle -- fuzz --kind {kind} --seed {seed} --print > {kind}_{seed}.R\n--- program ---\n{}",
        engines.join(", "),
        engines.join(","),
        report.program
    );
    for divergence in &report.divergences {
        text.push_str(&render_divergence(divergence));
    }
    text
}

/// One engine's section of a failure report: the expressions that differ and how, then the
/// diff or the reason the engine produced no outcome.
pub fn render_divergence(divergence: &Divergence) -> String {
    let mut text = format!("--- {} ---\n", divergence.engine);
    if divergence.ran && divergence.expressions.is_empty() {
        text.push_str(if divergence.conditions {
            "differs (conditions): the text of a warning, message or error differs\n"
        } else {
            "differs (printed): the values and conditions agree, the printed text does not\n"
        });
    }
    for expression in &divergence.expressions {
        text.push_str(&format!(
            "differs ({}): {}\n",
            expression.difference.name(),
            expression.source
        ));
    }
    text.push_str(&format!("{}\n", divergence.detail));
    text
}

/// The source of each top-level expression of `program`, in order, as R's `parse` counts them.
fn top_level_expressions(program: &str) -> Vec<String> {
    syntax::parse(program)
        .syntax_node()
        .children()
        .filter(|child| syntax::ast::is_expression_kind(child.kind()))
        .map(|child| child.text().to_string())
        .collect()
}

/// The stack shapes [`trace_back_program`] nests, by the name its R preamble gives each wrapper.
const TRACE_WRAPPERS: &[&str] = &[
    "closure",
    "operator",
    "namespaced",
    "do_call",
    "lapply",
    "vapply",
    "reduce",
    "try_catch",
    "calling",
    "restarts",
    "eval",
    "local",
    "dispatch",
    "recall",
    "mark",
];

/// An R program for `seed` that builds random call stacks from [`TRACE_WRAPPERS`] and, at the
/// bottom of each, compares rlang's `trace_back()` with the trace its own R body returns from
/// the same frame. Every stack prints one line, `stack <n>: TRUE` when the two are identical,
/// and the shapes and trimming of the stack otherwise. The program needs rlang; without it, it
/// prints only `rlang is not installed`.
pub fn trace_back_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed);
    let mut text = format!("# trace_back fuzz program, seed {seed}\n{TRACE_PREAMBLE}");
    for stack in 0..8 {
        let depth = rng.below(10);
        let kinds: Vec<String> = (0..depth)
            .map(|_| format!("\"{}\"", TRACE_WRAPPERS[rng.below(TRACE_WRAPPERS.len())]))
            .collect();
        let trim = ["none", "top", "bottom"][rng.below(3)];
        text.push_str(&format!(
            "check({stack}, c({}), \"{trim}\")\n",
            kinds.join(", ")
        ));
    }
    text
}

const TRACE_PREAMBLE: &str = r#"if (!requireNamespace("rlang", quietly = TRUE)) {
    cat("rlang is not installed\n")
    # An error ends the oracle driver's run with a report; quit() would end it without one.
    stop("skipped", call. = FALSE)
}
body_only <- body(rlang:::trace_back)
if (identical(body_only[[1]], as.name("{")) && length(body_only) == 3L &&
    is.call(body_only[[2]]) && identical(body_only[[2]][[1]], as.name(".Internal"))) {
    body_only <- body_only[[3]]
}
slow_trace_back <- rlang:::trace_back
body(slow_trace_back) <- body_only
leaf <- function(state) {
    marks <- state$marks
    top <- switch(state$trim, top = marks[[length(marks)]], NULL)
    bottom <- switch(state$trim, bottom = marks[[min(2L, length(marks))]], NULL)
    fast <- rlang::trace_back(top = top, bottom = bottom)
    slow <- slow_trace_back(top = top, bottom = bottom)
    identical(fast, slow)
}
`%then%` <- function(state, step) step(state)
print.marker <- function(x, ...) x$run()
wrappers <- list(
    closure = function(state, next_step) next_step(state),
    operator = function(state, next_step) state %then% next_step,
    namespaced = function(state, next_step) base::identity(next_step(state)),
    do_call = function(state, next_step) do.call(function(a, b, c) next_step(state), list(1:3, mtcars[1:2, 1:2], sum)),
    lapply = function(state, next_step) lapply(1L, function(i) next_step(state))[[1L]],
    vapply = function(state, next_step) vapply(1L, function(i) next_step(state), logical(1)),
    reduce = function(state, next_step) Reduce(function(a, b) next_step(state), 1:2),
    try_catch = function(state, next_step) tryCatch(next_step(state), error = function(e) NULL),
    calling = function(state, next_step) withCallingHandlers(next_step(state), warning = function(w) NULL),
    restarts = function(state, next_step) withRestarts(next_step(state), skip = function() NULL),
    eval = function(state, next_step) eval(quote(next_step(state))),
    local = function(state, next_step) local(next_step(state)),
    dispatch = function(state, next_step) print(structure(list(run = function() next_step(state)), class = "marker")),
    recall = function(state, next_step) (function(n) if (n > 0L) Recall(n - 1L) else next_step(state))(2L),
    mark = function(state, next_step) {
        state$marks[[length(state$marks) + 1L]] <- environment()
        next_step(state)
    }
)
check <- function(stack, kinds, trim) {
    step <- leaf
    for (kind in rev(kinds)) {
        step <- local({
            wrapper <- wrappers[[kind]]
            inner <- step
            function(state) wrapper(state, inner)
        })
    }
    same <- step(list(marks = list(globalenv()), trim = trim))
    if (isTRUE(same)) cat("stack ", stack, ": TRUE\n", sep = "")
    else cat("stack ", stack, ": differs for ", paste(kinds, collapse = " "), " trimmed ", trim, "\n", sep = "")
}
"#;

/// An R program for `seed` that calls withr 3.0.3's environment-variable and option helpers and
/// base R's `Sys.getenv()`, `Sys.setenv()`, `Sys.unsetenv()`, `intersect()` and `structure()`,
/// which ry runs natively, with random arguments: names that repeat, are empty or `NA`, values
/// that are `NULL`, `NA`, empty, non-ASCII or not strings, and invalid options. Each call runs
/// once natively and once through the function's own R body from the same state, and prints
/// `check <n>: TRUE` when the two agree by `identical()` on the value, its visibility, the
/// condition raised, and the environment variables and options afterwards. Without withr
/// 3.0.3, the program prints only `withr 3.0.3 is not installed`.
pub fn native_helpers_program(seed: u64) -> String {
    fn pick(table: &[&str], rng: &mut SplitMix64) -> String {
        table[rng.below(table.len())].to_owned()
    }
    /// A vector or list of up to three of `values`, usually named from `names`.
    fn named(values: &[&str], names: &[&str], rng: &mut SplitMix64, list: bool) -> String {
        let count = rng.below(4);
        let items: Vec<String> = (0..count).map(|_| pick(values, rng)).collect();
        let body = match (items.is_empty(), list) {
            (true, true) => "list()".to_owned(),
            (true, false) => "character(0)".to_owned(),
            (false, true) => format!("list({})", items.join(", ")),
            (false, false) => format!("c({})", items.join(", ")),
        };
        if count == 0 || rng.chance(1, 6) {
            return body;
        }
        let tags: Vec<String> = (0..count).map(|_| pick(names, rng)).collect();
        format!("setNames({body}, c({}))", tags.join(", "))
    }
    const NAMES: &[&str] = &["\"A\"", "\"B\"", "\"C\"", "\"\"", "NA", "\"\\u00e9\""];
    const ENV_VALUES: &[&str] = &[
        "\"a\"",
        "\"\"",
        "NA",
        "NA_character_",
        "\"\\u00e9t\\u00e9\"",
        "\"x y\"",
        "NULL",
        "NA_real_",
        "TRUE",
        "1",
        "c(\"p\", \"q\")",
        "list(\"n\")",
        "structure(\"s\", note = 1)",
    ];
    const OPTION_NAMES: &[&str] = &[
        "\"ry.fuzz.a\"",
        "\"ry.fuzz.b\"",
        "\"width\"",
        "\"digits\"",
        "\"warn\"",
        "\"\"",
        "NA",
    ];
    const OPTION_VALUES: &[&str] = &[
        "1", "\"x\"", "NULL", "80L", "5", "30", "\"wide\"", "TRUE", "NA", "list(1)", "0L",
    ];
    const ACTIONS: &[&str] = &[
        "\"replace\"",
        "\"prefix\"",
        "\"suffix\"",
        "\"rep\"",
        "c(\"replace\", \"prefix\")",
        "1",
    ];
    const VECTORS: &[&str] = &[
        "c(3L, 1L, 3L)",
        "c(1, NA, 2.5)",
        "c(b = 2, a = 1)",
        "c(TRUE, NA)",
        "c(\"a\", NA, \"b\")",
        "character(0)",
        "NULL",
        "c(1i, 2i)",
        "factor(c(\"u\", \"v\"))",
        "list(1, \"a\")",
        "matrix(1:4, 2)",
        "c(NaN, NA)",
    ];
    let mut rng = SplitMix64(seed);
    let mut text = format!("# native helper fuzz program, seed {seed}\n{NATIVE_HELPERS_PREAMBLE}");
    for check in 0..12 {
        let call = match rng.below(12) {
            0 => format!(
                "withr:::list_combine({}, {})",
                named(OPTION_VALUES, NAMES, &mut rng, true),
                named(OPTION_VALUES, NAMES, &mut rng, true)
            ),
            1 => {
                let list = rng.chance(1, 2);
                format!(
                    "withr:::as_envvars({})",
                    named(ENV_VALUES, NAMES, &mut rng, list)
                )
            }
            2 => {
                let list = rng.chance(1, 2);
                format!(
                    "withr:::get_envvar({})",
                    named(ENV_VALUES, NAMES, &mut rng, list)
                )
            }
            3 => {
                let list = rng.chance(1, 2);
                let envs = named(ENV_VALUES, NAMES, &mut rng, list);
                format!(
                    "withr:::set_envvar({envs}, action = {})",
                    pick(ACTIONS, &mut rng)
                )
            }
            4 => format!(
                "withr:::get_options({})",
                named(OPTION_VALUES, OPTION_NAMES, &mut rng, true)
            ),
            5 => format!(
                "withr:::set_options({})",
                named(OPTION_VALUES, OPTION_NAMES, &mut rng, true)
            ),
            6 => format!(
                "withr:::reset_options({})",
                named(OPTION_VALUES, OPTION_NAMES, &mut rng, true)
            ),
            7 => "withr:::reset_gettext_cache()".to_owned(),
            8 => format!(
                "Sys.getenv({}, unset = {}, names = {})",
                named(NAMES, NAMES, &mut rng, false),
                pick(&["\"\"", "NA", "\"u\"", "c(\"u\", \"v\")", "1"], &mut rng),
                pick(&["NA", "TRUE", "FALSE", "\"yes\""], &mut rng)
            ),
            9 => {
                let count = rng.below(3);
                let items: Vec<String> = (0..count)
                    .map(|_| {
                        let value = pick(ENV_VALUES, &mut rng);
                        if rng.chance(1, 8) {
                            value
                        } else {
                            format!("{} = {value}", pick(&["A", "B", "C", "`\u{e9}`"], &mut rng))
                        }
                    })
                    .collect();
                format!("Sys.setenv({})", items.join(", "))
            }
            10 => format!("Sys.unsetenv({})", named(NAMES, NAMES, &mut rng, false)),
            _ => match rng.below(2) {
                0 => format!(
                    "intersect({}, {})",
                    pick(VECTORS, &mut rng),
                    pick(VECTORS, &mut rng)
                ),
                _ => format!(
                    "structure({}, names = {})",
                    pick(VECTORS, &mut rng),
                    pick(
                        &[
                            "c(\"p\", \"q\")",
                            "NULL",
                            "c(\"p\", NA)",
                            "\"p\"",
                            "1:2",
                            "c(\"p\", \"q\", \"r\")"
                        ],
                        &mut rng
                    )
                ),
            },
        };
        text.push_str(&format!("check({check}, quote({call}))\n"));
    }
    text
}

const NATIVE_HELPERS_PREAMBLE: &str = r#"if (!requireNamespace("withr", quietly = TRUE) || format(packageVersion("withr")) != "3.0.3") {
    cat("withr 3.0.3 is not installed\n")
    # An error ends the oracle driver's run with a report; quit() would end it without one.
    stop("skipped", call. = FALSE)
}
r_body <- function(f) {
    code <- body(f)
    if (is.call(code) && identical(code[[1]], as.name("{")) && length(code) == 3L &&
        is.call(code[[2]]) && identical(code[[2]][[1]], as.name(".Internal"))) {
        code <- code[[3]]
    }
    # A fresh closure, which a native entry does not recognize.
    body(f) <- code
    f
}
variables <- c("A", "B", "C", "\u00e9", "NA")
watched <- c("ry.fuzz.a", "ry.fuzz.b", "width", "digits", "warn", "NA")
Sys.setenv(A = "start", C = "")
options(ry.fuzz.b = "start")
state <- function() list(Sys.getenv(variables, unset = NA), sapply(watched, getOption, simplify = FALSE))
restore <- function(saved) {
    values <- saved[[1]]
    for (name in variables) {
        if (is.na(values[[name]])) Sys.unsetenv(name)
        else do.call(Sys.setenv, setNames(list(values[[name]]), name))
    }
    options(saved[[2]])
}
run <- function(call, env) {
    value <- tryCatch(
        withVisible(eval(call, env)),
        error = function(e) list(error = conditionMessage(e), call = conditionCall(e)),
        warning = function(w) list(warning = conditionMessage(w), call = conditionCall(w))
    )
    list(value, state())
}
check <- function(index, call) {
    function_name <- call[[1]]
    target <- eval(function_name)
    slow <- new.env()
    fast <- new.env()
    local_name <- if (is.call(function_name)) as.character(function_name[[3]]) else as.character(function_name)
    assign(local_name, target, envir = fast)
    assign(local_name, r_body(target), envir = slow)
    call[[1]] <- as.name(local_name)
    before <- state()
    native <- run(call, fast)
    restore(before)
    reference <- run(call, slow)
    restore(before)
    # reset_gettext_cache() binds a fresh path each time.
    if (identical(local_name, "reset_gettext_cache") && is.character(native[[1]]$value)) {
        same <- is.character(reference[[1]]$value)
    } else {
        same <- identical(native, reference)
    }
    if (isTRUE(same)) cat("check ", index, ": TRUE\n", sep = "")
    else {
        cat("check ", index, ": differs for ", deparse(call), "\n", sep = "")
        str(native)
        str(reference)
    }
}
"#;

/// An R program for `seed` that parses the text of [`program`]`(seed)` with `keep.source = TRUE`
/// and prints its parse data, one `|`-separated row per line, then every top-level srcref and
/// the whole-file srcref. Before parsing, the text gains comments on lines of their own and
/// after code, blank lines, and tab indentation, the inputs whose positions and parents are
/// easiest to get wrong. Parse data comes from the parser alone, so the program runs nothing it
/// parses.
pub fn parse_data_program(seed: u64) -> String {
    const COMMENTS: &[&str] = &["# note", "#", "#: (x: Int) -> Int", "## twice", "#'roxygen"];
    let mut rng = SplitMix64(seed ^ 0x7061_7273_6564_6174);
    let mut source = String::new();
    for line in program(seed).lines() {
        if rng.chance(1, 6) {
            let indent = ["", "  ", "\t", " \t "][rng.below(4)];
            source.push_str(&format!(
                "{indent}{}\n",
                COMMENTS[rng.below(COMMENTS.len())]
            ));
        }
        if rng.chance(1, 10) {
            source.push('\n');
        }
        let code = match line.strip_prefix("  ") {
            Some(rest) if rng.chance(1, 4) => format!("\t{rest}"),
            _ => line.to_owned(),
        };
        source.push_str(&code);
        // A trailing comment is safe only where the line cannot end inside a string.
        let plain = !code.contains(['"', '\'', '`', '\\', '#']);
        if plain && !code.trim().is_empty() && rng.chance(1, 4) {
            source.push_str(&format!(" {}", COMMENTS[rng.below(COMMENTS.len())]));
        }
        source.push('\n');
    }
    format!(
        "# parse data fuzz program, seed {seed}\n\
         src <- r\"-----[{source}]-----\"\n\
         p <- parse(text = src, keep.source = TRUE)\n\
         d <- getParseData(p, includeText = TRUE)\n\
         writeLines(do.call(paste, c(unname(as.list(d)), sep = \"|\")))\n\
         for (ref in attr(p, \"srcref\")) cat(as.integer(ref), \"\\n\")\n\
         cat(as.integer(attr(p, \"wholeSrcref\")), \"\\n\")\n"
    )
}

/// An R program for `seed` that calls `format` on random doubles under random `digits`,
/// `nsmall`, `width`, `trim`, `na.encode`, `scientific` and `decimal.mark` arguments and random
/// `scipen`, `OutDec` and `digits` options, printing each result (or its error) on a line of
/// its own. The doubles are written as hexadecimal literals, so both engines format exactly the
/// same bits; they mix random bit patterns, decimals near rounding ties, exact binary ties,
/// subnormals and the extremes of the double range with NA, NaN, infinities and negative zero.
pub fn format_numbers_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x666f_726d_6174_0000);
    let mut text = format!("# format fuzz program, seed {seed}\n");
    for line in 0..400 {
        let count = [1, 1, 1, 2, 3, 5, 8][rng.below(7)];
        let values: Vec<String> = (0..count).map(|_| fuzz_double(&mut rng)).collect();
        let mut arguments = vec![format!("c({})", values.join(", "))];
        // `digits = 0` is out of range and must fail the same way.
        if rng.chance(3, 5) {
            arguments.push(format!("digits = {}L", rng.below(23)));
        }
        if rng.chance(2, 5) {
            arguments.push(format!("nsmall = {}L", rng.below(21)));
        }
        if rng.chance(1, 5) {
            arguments.push(format!("width = {}L", rng.below(30)));
        }
        if rng.chance(1, 5) {
            arguments.push("trim = TRUE".to_owned());
        }
        if rng.chance(1, 5) {
            arguments.push("na.encode = FALSE".to_owned());
        }
        if rng.chance(1, 4) {
            let scientific = match rng.below(4) {
                0 => "TRUE".to_owned(),
                1 => "FALSE".to_owned(),
                2 => "NA".to_owned(),
                _ => format!("{}L", rng.below(21) as i64 - 10),
            };
            arguments.push(format!("scientific = {scientific}"));
        }
        if rng.chance(3, 20) {
            arguments.push("decimal.mark = \",\"".to_owned());
        }
        let mut options = Vec::new();
        if rng.chance(3, 20) {
            options.push(format!("scipen = {}", rng.below(20) as i64 - 9));
        }
        if rng.chance(1, 10) {
            options.push("OutDec = \",\"".to_owned());
        }
        if rng.chance(3, 20) {
            options.push(format!("digits = {}", 1 + rng.below(22)));
        }
        if !options.is_empty() {
            text.push_str(&format!("options({})\n", options.join(", ")));
        }
        text.push_str(&format!(
            "r <- tryCatch(format({}), error = function(e) paste(\"E:\", conditionMessage(e)))\n\
             cat({line}L, encodeString(r, quote = '\"'), sep = \"|\"); cat(\"\\n\")\n",
            arguments.join(", ")
        ));
        if !options.is_empty() {
            text.push_str("options(scipen = 0, OutDec = \".\", digits = 7)\n");
        }
    }
    text
}

/// One double as an R literal for [`format_numbers_program`].
fn fuzz_double(rng: &mut SplitMix64) -> String {
    let value = match rng.below(13) {
        0 => {
            return ["0", "-0", "NaN", "Inf", "-Inf", "NA_real_"][rng.below(6)].to_owned();
        }
        1 => f64::from_bits(rng.next_u64()),
        2 => {
            // A decimal of up to 16 digits, sometimes nudged one unit off it.
            let digits = 1 + rng.below(16) as i32;
            let low = 10u64.pow(digits as u32 - 1);
            let mantissa = low + rng.next_u64() % (9 * low);
            let exponent = rng.below(40) as i32 - 20 - digits + 1;
            let value: f64 = format!("{mantissa}e{exponent}").parse().unwrap_or(0.0);
            match rng.below(4) {
                0 => f64::from_bits(value.to_bits() + 1),
                1 => f64::from_bits(value.to_bits().saturating_sub(1)),
                _ => value,
            }
        }
        3 => {
            // A decimal ending in 5, the tie of the digit before it.
            let digits = 1 + rng.below(15) as u32;
            let low = 10u64.pow(digits - 1);
            let mantissa = (low + rng.next_u64() % (9 * low)) * 10 + 5;
            let exponent = rng.below(16) as i32 - 8 - digits as i32;
            format!("{mantissa}e{exponent}").parse().unwrap_or(0.0)
        }
        4 => {
            // An exact binary tie for fixed notation: an odd multiple of a small power of two.
            let numerator = (rng.next_u64() % 100_000) * 2 + 1;
            numerator as f64 / f64::from(1u32 << (1 + rng.below(12)))
        }
        5 => [
            1e15,
            1e15 - 1.0,
            1e16,
            999_999_999_999_999.0,
            9_999_999.5,
            1e-15,
            1e5,
            1e-5,
            0.1,
            0.15,
            1.0 / 3.0,
            123_456.7,
            1e-300,
            5e-324,
            f64::MIN_POSITIVE,
            f64::MAX,
            9.995,
            0.095,
            99.95,
            1e22,
            1e23,
            1e27,
            1e28,
            4.0270798167452e-09,
        ][rng.below(24)],
        6 => {
            // Spread over the whole exponent range, subnormals included.
            let exponent = rng.below(2046) as i64 - 1074;
            (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * 2f64.powi(exponent as i32 + 1)
        }
        7 => (rng.next_u64() % 2_000_000) as f64 - 1e6,
        8 => {
            // Just under a power of ten, where rounding adds a digit.
            let power = 10f64.powi(rng.below(30) as i32 - 10);
            power * (1.0 - (1 + rng.below(1000)) as f64 * 1e-7)
        }
        _ => {
            let scale = 10f64.powi(rng.below(6) as i32);
            (((rng.next_u64() % 2_000_000_000) as f64 - 1e9) / 1e6 * scale).round() / scale
        }
    };
    if rng.chance(3, 10) {
        hex_literal(-value)
    } else {
        hex_literal(value)
    }
}

/// `value` as an R hexadecimal literal, which reads back as exactly the same bits.
fn hex_literal(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Inf" } else { "Inf" }.to_owned();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0");
    }
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = if biased == 0 {
        (0, -1022)
    } else {
        (1, biased - 1023)
    };
    format!("{sign}0x{lead}.{fraction:013x}p{exponent}")
}

/// An R program for `seed` that calls `sprintf` with random format strings on random arguments,
/// printing each result (or its error, and any warning) on a line of its own. A format mixes
/// literal text, `%%` and conversions with random flags, widths, precisions, `*` widths and
/// precisions, and `n$` references; the arguments are doubles from [`fuzz_double`], integers,
/// logicals and strings, with NA, of lengths that recycle and lengths that do not.
pub fn sprintf_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x7370_7269_6e74_6600);
    let mut text = format!(
        "# sprintf fuzz program, seed {seed}\n\
         show <- function(line, expr) {{\n\
         \x20 warned <- NULL\n\
         \x20 r <- withCallingHandlers(\n\
         \x20   tryCatch(expr, error = function(e) paste(\"E:\", conditionMessage(e))),\n\
         \x20   warning = function(w) {{ warned <<- conditionMessage(w); invokeRestart(\"muffleWarning\") }})\n\
         \x20 cat(line, encodeString(r, quote = '\"'), if (!is.null(warned)) paste(\"W:\", warned), sep = \"|\"); cat(\"\\n\")\n\
         }}\n"
    );
    for line in 0..300 {
        let count = 1 + rng.below(3);
        let kinds: Vec<usize> = (0..count).map(|_| rng.below(4)).collect();
        let mut format = String::new();
        let mut arguments = Vec::new();
        let positional = rng.chance(1, 8);
        for (position, &kind) in kinds.iter().enumerate() {
            if rng.chance(1, 2) {
                format.push_str(["x", "a b", "=", "%%", "[", "é"][rng.below(6)]);
            }
            format.push('%');
            if positional {
                format.push_str(&format!("{}$", position + 1));
            }
            for flag in ["-", "+", " ", "0", "#"] {
                if rng.chance(1, 6) {
                    format.push_str(flag);
                }
            }
            let star_width = !positional && rng.chance(1, 10);
            if star_width {
                format.push('*');
                arguments.push(format!("{}L", rng.below(25) as i64 - 8));
            } else if rng.chance(1, 3) {
                format.push_str(&rng.below(25).to_string());
            }
            if !positional && !star_width && rng.chance(1, 12) {
                format.push_str(".*");
                arguments.push(format!("{}L", rng.below(18) as i64 - 2));
            } else if rng.chance(1, 2) {
                format.push_str(&format!(".{}", rng.below(18)));
            }
            let letters: &[&str] = match kind {
                0 => &["f", "e", "E", "g", "G", "s", "f", "g", "d"],
                1 => &["d", "i", "x", "X", "o", "s", "d"],
                2 => &["d", "i", "s"],
                _ => &["s", "s", "d"],
            };
            format.push_str(letters[rng.below(letters.len())]);
            let length = [1, 1, 1, 2, 3][rng.below(5)];
            let values: Vec<String> = (0..length)
                .map(|_| match kind {
                    0 => fuzz_double(&mut rng),
                    1 => match rng.below(6) {
                        0 => "NA_integer_".to_owned(),
                        1 => "-2147483647L".to_owned(),
                        _ => format!("{}L", (rng.next_u64() % 2_000_001) as i64 - 1_000_000),
                    },
                    2 => ["TRUE", "FALSE", "NA"][rng.below(3)].to_owned(),
                    _ => ["\"a\"", "\"\"", "NA_character_", "\"héllo\"", "\"ab cd\""][rng.below(5)]
                        .to_owned(),
                })
                .collect();
            arguments.push(format!("c({})", values.join(", ")));
        }
        if rng.chance(1, 2) {
            format.push_str([";", " end", "%%", "\\t"][rng.below(4)]);
        }
        let format = if rng.chance(1, 10) {
            format!("c(\"{format}\", \"<{format}>\")")
        } else {
            format!("\"{format}\"")
        };
        if rng.chance(1, 20) {
            arguments.push("1".to_owned());
        }
        text.push_str(&format!(
            "show({line}L, sprintf({format}, {}))\n",
            arguments.join(", ")
        ));
    }
    text
}

/// The frame kind's program for `seed`: tables built from generated columns, chains of DataFrame
/// verbs over them, and relations the results must satisfy, each dumped canonically. Under ry the
/// program calls the real `DataFrame.*`, `Expr.*` and `GroupBy.*`; under GNU R its prelude defines
/// the same names as a base-R model of `reference/dataframes.mdx`, so the text is the same on
/// both engines. A shape the contract leaves open is never generated (`frame_model.R` says which).
pub fn frame_program(seed: u64) -> String {
    let mut rng = SplitMix64(seed);
    let features = FrameFeatures {
        lazy: rng.chance(1, 2),
        relations: rng.chance(1, 3),
        errors: rng.chance(1, 4),
        specials: rng.chance(2, 3),
        collisions: rng.chance(2, 3),
    };
    let mut generator = FrameGenerator {
        rng,
        features,
        tables: Vec::new(),
    };
    let mut text = frame_preamble();
    for _ in 0..2 + generator.rng.below(3) {
        text.push_str(&generator.check());
    }
    if features.relations {
        for _ in 0..1 + generator.rng.below(3) {
            text.push_str(&generator.relation());
        }
    }
    text
}

/// Fixed frame programs for shapes a fix pinned, run with the generated ones: column arguments
/// named like a verb's formals, whole or in part, and `%%` and `%/%` on infinite, zero, missing,
/// negative, subnormal and huge operands and on an empty table against a single value, and a
/// minimum or maximum of an integer column, including a group left with no values.
pub fn frame_regression_programs() -> Vec<String> {
    FRAME_REGRESSIONS
        .iter()
        .map(|body| format!("{}{body}", frame_preamble()))
        .collect()
}

const FRAME_REGRESSIONS: &[&str] = &[
    r#"frame_check(TRUE, function() {
  t1 <- DataFrame(a = c(1L, 2L, 1L), b = c(2, 3, 4))
  t1 |>
    DataFrame.with_columns(d = Expr.col("a"), df = Expr.col("b"), g = Expr.col("a") * 2L) |>
    DataFrame.filter(d = Expr.col("d") > 0L, de = Expr.col("df") < 10) |>
    DataFrame.select("df", "d", group = Expr.col("g"), expr = Expr.col("b"), x = Expr.col("a"))
})
frame_check(TRUE, function() {
  DataFrame(a = c(1L, 2L, 1L), b = c(2, 3, 4)) |>
    DataFrame.group_by(df = Expr.col("a")) |>
    GroupBy.agg(g = Expr.sum(Expr.col("b")), group = Expr.len(), gr = Expr.max(Expr.col("b")))
})
frame_check(TRUE, function() {
  DataFrame(group = c(2L, 1L, 2L), expr = c(1, 5, 3)) |>
    DataFrame.with_columns(e = Expr.over(Expr.sum(Expr.col("expr")), ex = "group")) |>
    DataFrame.sort(d = "group", desc = "expr", descending = c(FALSE, TRUE))
})
frame_check(TRUE, function() {
  DataFrame(a = c(1L, 2L, 1L), b = c(2, 3, 4)) |>
    DataFrame.lazy() |>
    LazyFrame.with_columns(lf = Expr.col("a"), df = Expr.col("b")) |>
    LazyFrame.group_by(d = Expr.col("a")) |>
    LazyGroupBy.agg(group = Expr.sum(Expr.col("lf")), g = Expr.mean(Expr.col("df"))) |>
    LazyFrame.collect()
})
"#,
    r#"frame_check(TRUE, function() {
  x <- c(5, -5, 0, 5.5, -5.5, NA, Inf, -Inf, 3, -3, 7, -numeric(1))
  y <- c(Inf, -Inf, 0, 2, -2, 3, -3, NA, 0.1, 5, -numeric(1), 0.5)
  grid <- expand.grid(x = x, y = y)
  DataFrame(x = grid$x, y = grid$y) |>
    DataFrame.with_columns(
      m = Expr.col("x") %% Expr.col("y"),
      q = Expr.col("x") %/% Expr.col("y"),
      inf = Expr.col("x") %% Inf,
      ninf = Expr.col("x") %/% -Inf,
      zero = Expr.col("x") %% 0,
      sign = 1 / (Expr.col("x") %% -Inf)
    )
})
frame_check(TRUE, function() {
  grid <- expand.grid(x = c(5L, -5L, 0L, 7L, NA, -7L), y = c(0L, 2L, -2L, 3L, NA, 1L))
  DataFrame(x = grid$x, y = grid$y) |>
    DataFrame.lazy() |>
    LazyFrame.with_columns(
      m = Expr.col("x") %% Expr.col("y"),
      q = Expr.col("x") %/% Expr.col("y"),
      d = Expr.col("x") %% 2.5,
      n = Expr.col("x") %/% NA_real_,
      z = Expr.col("x") %/% 0
    ) |>
    LazyFrame.collect()
})
frame_check(TRUE, function() {
  DataFrame(x = double(0), y = integer(0)) |>
    DataFrame.lazy() |>
    LazyFrame.with_columns(m = Expr.col("x") %% 2, q = 7L %/% Expr.col("y")) |>
    LazyFrame.collect()
})
"#,
    r#"frame_check(TRUE, function() {
  DataFrame(x = c(4.9e-324, 1e308, -1e308, 5.5, -0.5), y = c(-5, 5, 5.5, 4.9e-324, 1e308)) |>
    DataFrame.lazy() |>
    LazyFrame.with_columns(m = Expr.col("x") %% Expr.col("y"), q = Expr.col("x") %/% Expr.col("y")) |>
    LazyFrame.collect() |>
    DataFrame.with_columns(m5 = Expr.col("x") %% -5, q5 = Expr.col("x") %/% 5)
})
frame_check(TRUE, function() {
  x <- c(0, -numeric(1), 1, -1, 5, -5, 5.5, -5.5, 1e308, 4.9e-324, Inf, -Inf, NA)
  grid <- expand.grid(x = x, y = x)
  DataFrame(x = grid$x, y = grid$y, i = rep(c(0L, 1L, -1L, 5L, -5L, NA), length.out = nrow(grid))) |>
    DataFrame.with_columns(
      m = Expr.col("x") %% Expr.col("y"),
      q = Expr.col("x") %/% Expr.col("y"),
      mi = Expr.col("i") %% Expr.col("y"),
      qi = Expr.col("x") %/% Expr.col("i"),
      big = (Expr.col("x") * 3) %% 7
    )
})
"#,
    r#"frame_check(FALSE, function() {
  DataFrame(k = c(1L, 0L, 1L, 2L, 2L), v = c(4L, 3L, NA, NA, NA), w = c(1, NA, 2, NA, NA)) |>
    DataFrame.group_by("k") |>
    GroupBy.agg(hi = Expr.max(Expr.col("v")), lo = Expr.min(Expr.col("v")), wh = Expr.max(Expr.col("w")))
})
frame_check(FALSE, function() {
  DataFrame(on = c(1L, 0L, -1L, 3L, 3L, 0L)) |>
    DataFrame.lazy() |>
    LazyFrame.group_by("on") |>
    LazyGroupBy.agg(by = Expr.len()) |>
    LazyFrame.group_by("by") |>
    LazyGroupBy.agg(top = Expr.max(Expr.col("on"))) |>
    LazyFrame.collect()
})
"#,
];

/// The prelude of every frame program: the model and the dump helpers. Under ry the guard skips
/// the model, because `package:ry` defines the names.
pub fn frame_preamble() -> String {
    format!(
        "if (!exists(\"DataFrame.select\")) {{\n{FRAME_MODEL}invisible(NULL)\n}}\n{FRAME_HELPERS}"
    )
}

const FRAME_MODEL: &str = include_str!("frame_model.R");

/// The canonical dump both engines print, through `as.data.frame`: names, types, then one line
/// per row, sorted where the program leaves the order open. Doubles print to 12 significant
/// digits, because the contract does not fix the order a sum adds in, and a zero prints without
/// its sign.
const FRAME_HELPERS: &str = r#"{
  frame_text <- function(text) vapply(text, function(one) {
    if (is.na(one)) return("NA")
    bytes <- charToRaw(enc2utf8(one))
    if (all(as.integer(bytes) < 128L)) one else paste0("<", paste(bytes, collapse = ""), ">")
  }, "", USE.NAMES = FALSE)
  frame_cells <- function(values) {
    if (is.double(values)) {
      out <- formatC(values + 0, digits = 12, format = "g")
      out[is.na(values)] <- "NA"
      out[is.nan(values)] <- "NaN"
      out
    } else if (is.character(values)) {
      ifelse(is.na(values), "NA", paste0("'", frame_text(values), "'"))
    } else {
      ifelse(is.na(values), "NA", as.character(values))
    }
  }
  frame_rows <- function(x, ordered) {
    x <- as.data.frame(x)
    cells <- lapply(unclass(x), frame_cells)
    rows <- if (length(cells)) do.call(paste, c(unname(cells), sep = " | ")) else rep("", nrow(x))
    if (!ordered) rows <- sort(rows, method = "radix")
    c(paste("names:", paste(frame_text(names(x)), collapse = ", ")),
      paste("types:", paste(vapply(unclass(x), typeof, ""), collapse = ", ")),
      paste("rows:", nrow(x)), rows)
  }
  frame_check <- function(ordered, body) {
    writeLines(tryCatch(frame_rows(suppressWarnings(body()), ordered), error = function(e) "error"))
    invisible(NULL)
  }
  frame_holds <- function(label, body) {
    held <- tryCatch(isTRUE(suppressWarnings(body())), error = function(e) NA)
    writeLines(paste(label, if (is.na(held)) "error" else held))
    invisible(NULL)
  }
  invisible(NULL)
}
"#;

/// The swarm features of one frame program, each on or off for the whole program.
#[derive(Clone, Copy)]
struct FrameFeatures {
    /// Chains run through `LazyFrame` and `collect`.
    lazy: bool,
    /// Relations between results that must hold inside one engine.
    relations: bool,
    /// One verb of a chain is wrong on purpose; only whether an error occurs is compared.
    errors: bool,
    /// Columns hold `NA`, `NaN`, infinities, negative zero and integers at the limit.
    specials: bool,
    /// Column names collide with the verbs' formals and need backquotes or UTF-8.
    collisions: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cell {
    Integer,
    Double,
    Character,
    Logical,
}

/// What the generator knows about a column or an expression's values.
#[derive(Clone, Debug)]
struct Values {
    cell: Cell,
    /// It may hold `NA`.
    missing: bool,
    /// It may hold `NaN`.
    nan: bool,
    /// It may hold an infinity.
    infinite: bool,
    /// It may hold a value beyond 2^40, or an integer at the limit, so a sum or a quotient may
    /// leave the range where every summation order and R's long double agree.
    big: bool,
}

impl Values {
    fn plain(&self) -> bool {
        !self.missing && !self.nan && !self.big
    }

    fn key(&self) -> bool {
        !self.missing && !self.nan
    }
}

#[derive(Clone, Debug)]
struct FrameColumn {
    name: String,
    values: Values,
}

#[derive(Clone, Debug)]
struct FrameSchema {
    columns: Vec<FrameColumn>,
    /// The contract fixes the row order. A join leaves it open, and so does anything after it.
    ordered: bool,
}

/// A generated expression: its R text, the name of its output column (the first column it
/// reads), the columns it reads, and its values.
#[derive(Clone, Debug)]
struct Term {
    text: String,
    output: Option<String>,
    reads: Vec<String>,
    values: Values,
    /// Built row-wise from literals and columns that are not big, so both engines hold the same
    /// double and `%%` and `%/%`, which magnify a last-bit difference, compare exactly.
    bounded: bool,
}

struct FrameGenerator {
    rng: SplitMix64,
    features: FrameFeatures,
    /// The table definitions of the check being generated, in order.
    tables: Vec<String>,
}

const PLAIN_NAMES: &[&str] = &["a", "b", "c", "k", "v", "w", "p", "q"];

/// Names that a verb's formals start with or equal, R's reserved words, and names that need
/// backquotes or UTF-8.
const COLLIDING_NAMES: &[&str] = &[
    "d",
    "df",
    "n",
    "by",
    "on",
    "g",
    "group",
    "gr",
    "expr",
    "e",
    "x",
    "lf",
    "other",
    "how",
    "suffix",
    "left_on",
    "right_on",
    "strategy",
    "index",
    "values",
    "agg",
    "name",
    "value",
    "test",
    "yes",
    "no",
    "first",
    "second",
    "cols",
    "desc",
    "descending",
    "a b",
    "if",
    "NA",
    "function",
    "\u{e9}t\u{e9}",
    "T",
    "x.1",
    "k",
    "v",
];

const CHARACTER_VALUES: &[&str] = &["a", "b", "ab", "zz", "B", "NA", "", "\u{e9}", "a b"];

impl FrameGenerator {
    /// One top-level check: a chain, a pivot, a keyed table or a description.
    fn check(&mut self) -> String {
        self.tables.clear();
        let (body, ordered) = match self.rng.below(10) {
            0 => self.pivot(),
            1 => self.keyed(),
            2 => {
                let (chain, schema) = self.chain(self.features.lazy);
                let body = format!(
                    "  result <- {chain}\n  cat(frame_text(names(DataFrame.schema(result))), unname(DataFrame.schema(result)), DataFrame.height(result), DataFrame.width(result), frame_text(DataFrame.columns(result)), \"\\n\")\n  result"
                );
                (body, schema.ordered)
            }
            _ => {
                let (chain, schema) = self.chain(self.features.lazy);
                (format!("  {chain}"), schema.ordered)
            }
        };
        format!(
            "frame_check({}, function() {{\n{}{body}\n}})\n",
            if ordered { "TRUE" } else { "FALSE" },
            self.tables.concat()
        )
    }

    /// A chain of one to six verbs over a new table, rendered eagerly or lazily, and the schema
    /// of its result.
    fn chain(&mut self, lazy: bool) -> (String, FrameSchema) {
        let (name, mut schema) = self.table(None);
        let mut steps = Vec::new();
        let length = 1 + self.rng.below(6);
        let error_at =
            (self.features.errors && self.rng.chance(1, 2)).then(|| self.rng.below(length));
        for position in 0..length {
            if error_at == Some(position) {
                steps.push(self.wrong_verb(&schema));
                break;
            }
            if let Some(step) = self.verb(&mut schema) {
                steps.push(step);
            }
        }
        (render_chain(&name, &steps, lazy), schema)
    }

    /// A new table bound to `t1`, `t2` and so on, with its definition added to the check, and
    /// its schema. `keys` gives columns the table must have, with their names and cells.
    fn table(&mut self, keys: Option<&[(String, Cell)]>) -> (String, FrameSchema) {
        let name = format!("t{}", self.tables.len() + 1);
        let rows = [0, 1, 1, 2, 3, 4, 5, 6, 8, 9, 13, 20][self.rng.below(12)];
        let mut columns: Vec<FrameColumn> = Vec::new();
        let mut arguments = Vec::new();
        let fixed = keys.map_or(0, <[_]>::len);
        let width = fixed
            + if keys.is_some() {
                self.rng.below(3)
            } else {
                1 + self.rng.below(4)
            };
        for index in 0..width {
            let (column_name, cell, key) = match keys.and_then(|keys| keys.get(index)) {
                Some((column_name, cell)) => (column_name.clone(), *cell, true),
                None => {
                    let taken: Vec<String> =
                        columns.iter().map(|column| column.name.clone()).collect();
                    let cell = [
                        Cell::Integer,
                        Cell::Double,
                        Cell::Character,
                        Cell::Logical,
                        Cell::Integer,
                        Cell::Double,
                    ][self.rng.below(6)];
                    (self.fresh_name(&taken), cell, false)
                }
            };
            let (text, values) = self.column_values(cell, rows, key);
            arguments.push(format!("{} = {text}", argument_name(&column_name)));
            columns.push(FrameColumn {
                name: column_name,
                values,
            });
        }
        let constructor = if self.rng.chance(1, 6) {
            format!(
                "as.DataFrame(data.frame({}, check.names = FALSE))",
                arguments.join(", ")
            )
        } else {
            format!("DataFrame({})", arguments.join(", "))
        };
        self.tables.push(format!("  {name} <- {constructor}\n"));
        (
            name,
            FrameSchema {
                columns,
                ordered: true,
            },
        )
    }

    /// A column of `rows` values of `cell` as R text, and what it may hold. A key column holds
    /// no `NA` or `NaN` and draws from few values, so groups and matches repeat.
    fn column_values(&mut self, cell: Cell, rows: usize, key: bool) -> (String, Values) {
        let specials = self.features.specials && !key;
        let mut values = Values {
            cell,
            missing: false,
            nan: false,
            infinite: false,
            big: false,
        };
        if rows == 0 {
            let empty = match cell {
                Cell::Integer => "integer(0)",
                Cell::Double => "double(0)",
                Cell::Character => "character(0)",
                Cell::Logical => "logical(0)",
            };
            return (empty.to_owned(), values);
        }
        let mut items = Vec::with_capacity(rows);
        for _ in 0..rows {
            let item = if specials && self.rng.chance(1, 5) {
                match cell {
                    Cell::Integer => match self.rng.below(3) {
                        0 => {
                            values.missing = true;
                            "NA_integer_"
                        }
                        1 => {
                            values.big = true;
                            "2147483647L"
                        }
                        _ => {
                            values.big = true;
                            "-2147483647L"
                        }
                    }
                    .to_owned(),
                    Cell::Double => match self.rng.below(6) {
                        0 => {
                            values.missing = true;
                            "NA_real_"
                        }
                        1 => {
                            values.nan = true;
                            "NaN"
                        }
                        2 => {
                            values.infinite = true;
                            "Inf"
                        }
                        3 => {
                            values.infinite = true;
                            "-Inf"
                        }
                        4 => "-numeric(1)",
                        _ => {
                            values.big = true;
                            "4503599627370496"
                        }
                    }
                    .to_owned(),
                    Cell::Character => {
                        values.missing = true;
                        "NA_character_".to_owned()
                    }
                    Cell::Logical => {
                        values.missing = true;
                        "NA".to_owned()
                    }
                }
            } else {
                let domain = if key { 3 } else { 10 };
                match cell {
                    Cell::Integer => [
                        "1L", "2L", "0L", "-1L", "3L", "7L", "10L", "-7L", "5L", "4L",
                    ][self.rng.below(domain)]
                    .to_owned(),
                    Cell::Double => [
                        "1", "2.5", "0", "-1", "0.5", "-3.25", "4", "100", "1e6", "3",
                    ][self.rng.below(domain)]
                    .to_owned(),
                    Cell::Character => string_literal(
                        CHARACTER_VALUES
                            [self.rng.below(if key { 3 } else { CHARACTER_VALUES.len() })],
                    ),
                    Cell::Logical => ["TRUE", "FALSE"][self.rng.below(2)].to_owned(),
                }
            };
            items.push(item);
        }
        if rows > 1 && self.rng.chance(1, 12) {
            items.truncate(1);
        }
        (format!("c({})", items.join(", ")), values)
    }

    /// The next verb of a chain as a template (see [`render_chain`]), updating `schema`, or
    /// `None` when the schema has nothing it can take.
    fn verb(&mut self, schema: &mut FrameSchema) -> Option<String> {
        match self.rng.below(12) {
            0 | 1 => self.select(schema),
            2 | 3 => self.with_columns(schema),
            4 | 5 => self.filter(schema),
            6 | 7 => self.aggregate(schema),
            8 => self.sort(schema),
            9 => self.head(schema),
            10 => self.join(schema),
            _ => self.join_asof(schema),
        }
    }

    fn select(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let mut items = Vec::new();
        let mut columns = Vec::new();
        for _ in 0..1 + self.rng.below(4) {
            let taken: Vec<String> = columns
                .iter()
                .map(|column: &FrameColumn| column.name.clone())
                .collect();
            if self.rng.chance(1, 2) {
                let column = self.pick(schema, |_| true)?;
                if taken.contains(&column.name) {
                    continue;
                }
                items.push(string_literal(&column.name));
                columns.push(column);
                continue;
            }
            let term = self.row_term(schema, 2)?;
            let (item, name) = self.named_item(&term, &taken);
            items.push(item);
            columns.push(FrameColumn {
                name,
                values: term.values,
            });
        }
        if columns.is_empty() {
            return None;
        }
        schema.columns = columns;
        Some(format!("{{F}}.select({})", items.join(", ")))
    }

    fn with_columns(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let mut items = Vec::new();
        let mut written: Vec<String> = Vec::new();
        let mut read: Vec<String> = Vec::new();
        let mut added = Vec::new();
        for _ in 0..1 + self.rng.below(3) {
            let term = self.row_term(schema, 2)?;
            // Whether a later item reads an earlier one's new value or the input's is not in
            // the contract, so no item reads a column another item of the call writes.
            if term.reads.iter().any(|name| written.contains(name)) {
                continue;
            }
            let name = if self.rng.chance(1, 3) && !schema.columns.is_empty() {
                let replaced = schema.columns[self.rng.below(schema.columns.len())]
                    .name
                    .clone();
                if read.contains(&replaced) || written.contains(&replaced) {
                    continue;
                }
                replaced
            } else {
                let mut taken: Vec<String> = schema
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect();
                taken.extend(written.iter().cloned());
                taken.extend(read.iter().cloned());
                self.fresh_name(&taken)
            };
            let item = if term.output.as_deref() == Some(name.as_str()) && self.rng.chance(1, 2) {
                term.text.clone()
            } else {
                format!("{} = {}", argument_name(&name), term.text)
            };
            read.extend(term.reads.iter().cloned());
            written.push(name.clone());
            items.push(item);
            added.push(FrameColumn {
                name,
                values: term.values,
            });
        }
        if items.is_empty() {
            return None;
        }
        for column in added {
            match schema
                .columns
                .iter_mut()
                .find(|existing| existing.name == column.name)
            {
                Some(existing) => *existing = column,
                None => schema.columns.push(column),
            }
        }
        Some(format!("{{F}}.with_columns({})", items.join(", ")))
    }

    fn filter(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let mut conditions = Vec::new();
        for _ in 0..1 + self.rng.below(2) {
            if self.rng.chance(1, 6)
                && let Some(column) = self.pick(schema, |values| values.cell == Cell::Logical)
            {
                conditions.push(string_literal(&column.name));
                continue;
            }
            let term = self.logical_term(schema, 2)?;
            conditions.push(self.maybe_named(term.text));
        }
        Some(format!("{{F}}.filter({})", conditions.join(", ")))
    }

    /// `group_by` and `agg`: one row per group, with the keys first.
    fn aggregate(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let mut keys = Vec::new();
        let mut columns: Vec<FrameColumn> = Vec::new();
        for _ in 0..1 + self.rng.below(2) {
            let key = self.pick(schema, Values::key)?;
            if columns.iter().any(|column| column.name == key.name) {
                continue;
            }
            // A named string is not renamed (backlog, "Differences the frame kind reports"), so
            // a renamed key is always an expression.
            if self.rng.chance(1, 3) {
                let taken: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
                let alias = self.fresh_name(&taken);
                keys.push(format!(
                    "{} = Expr.col({})",
                    argument_name(&alias),
                    string_literal(&key.name)
                ));
                columns.push(FrameColumn {
                    name: alias,
                    values: key.values,
                });
            } else {
                keys.push(string_literal(&key.name));
                columns.push(key);
            }
        }
        let key_count = columns.len();
        // Inside `agg`, ry reads a key column as the group's one key value, not as its rows,
        // and the contract does not say which; so no aggregation reads a key's name.
        let inputs = FrameSchema {
            columns: schema
                .columns
                .iter()
                .filter(|column| !columns.iter().any(|key| key.name == column.name))
                .cloned()
                .collect(),
            ordered: schema.ordered,
        };
        let mut items = Vec::new();
        if schema.ordered && self.rng.chance(1, 8) {
            let column = self.pick(&inputs, |_| true)?;
            let taken: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
            let name = self.fresh_name(&taken);
            let count = self.rng.below(11);
            items.push(format!(
                "{} = Expr.head(Expr.col({}), {count}L)",
                argument_name(&name),
                string_literal(&column.name)
            ));
            columns.push(FrameColumn {
                name,
                values: column.values,
            });
        } else {
            for _ in 0..1 + self.rng.below(3) {
                let Some(term) = self.aggregate_term(&inputs) else {
                    continue;
                };
                let taken: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
                let (item, name) = self.named_item(&term, &taken);
                items.push(item);
                columns.push(FrameColumn {
                    name,
                    values: term.values,
                });
            }
        }
        if items.is_empty() || key_count == 0 {
            return None;
        }
        schema.columns = columns;
        Some(format!(
            "{{F}}.group_by({}) |>\n    {{G}}.agg({})",
            keys.join(", "),
            items.join(", ")
        ))
    }

    fn sort(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let mut keys = Vec::new();
        for _ in 0..1 + self.rng.below(2) {
            let key = if self.rng.chance(1, 4) {
                self.row_term(schema, 1)?.text
            } else {
                string_literal(&self.pick(schema, |_| true)?.name)
            };
            keys.push(self.maybe_named(key));
        }
        let descending = match self.rng.below(5) {
            0 => ", descending = TRUE".to_owned(),
            1 => ", descending = FALSE".to_owned(),
            2 if keys.len() == 2 => ", descending = c(TRUE, FALSE)".to_owned(),
            _ => String::new(),
        };
        Some(format!("{{F}}.sort({}{descending})", keys.join(", ")))
    }

    fn head(&mut self, schema: &mut FrameSchema) -> Option<String> {
        if !schema.ordered {
            return None;
        }
        let verb = if self.rng.chance(1, 2) {
            "head"
        } else {
            "tail"
        };
        let count = ["", "0", "1", "2", "n = 3", "8", "9", "100L"][self.rng.below(8)];
        Some(format!("{{F}}.{verb}({count})"))
    }

    /// A join with a new table whose key columns match the chosen columns' cells.
    fn join(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let how = ["inner", "left", "full", "semi", "anti", "cross"][self.rng.below(6)];
        let mut left_keys = Vec::new();
        if how != "cross" {
            for _ in 0..1 + self.rng.below(2) {
                let key =
                    self.pick(schema, |values| values.key() && values.cell != Cell::Double)?;
                if !left_keys
                    .iter()
                    .any(|existing: &FrameColumn| existing.name == key.name)
                {
                    left_keys.push(key);
                }
            }
        }
        let renamed = how != "cross" && self.rng.chance(1, 3);
        let right_names: Vec<String> = left_keys
            .iter()
            .map(|key| {
                if renamed {
                    let taken: Vec<String> = left_keys.iter().map(|key| key.name.clone()).collect();
                    self.fresh_name(&taken)
                } else {
                    key.name.clone()
                }
            })
            .collect();
        let mut distinct = right_names.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() != right_names.len() {
            return None;
        }
        let specification: Vec<(String, Cell)> = right_names
            .iter()
            .zip(&left_keys)
            .map(|(name, key)| (name.clone(), key.values.cell))
            .collect();
        let (other, right) = self.table(Some(&specification));
        let suffix = ["", ", suffix = \"_r\"", ", suffix = \".y\""][self.rng.below(3)];
        let suffix_text = match suffix {
            "" => "_right",
            ", suffix = \"_r\"" => "_r",
            _ => ".y",
        };
        let right_extra: Vec<&FrameColumn> = right
            .columns
            .iter()
            .filter(|column| how == "cross" || !right_names.contains(&column.name))
            .collect();
        let mut columns = schema.columns.clone();
        for column in &right_extra {
            let name = if columns.iter().any(|existing| existing.name == column.name) {
                format!("{}{suffix_text}", column.name)
            } else {
                column.name.clone()
            };
            // A suffixed name that is taken as well is not in the contract.
            if columns.iter().any(|existing| existing.name == name)
                || right_extra
                    .iter()
                    .any(|other| other.name == name && other.name != column.name)
            {
                return None;
            }
            let mut values = column.values.clone();
            values.missing |= matches!(how, "left" | "full");
            columns.push(FrameColumn { name, values });
        }
        let left_names: Vec<String> = left_keys.iter().map(|key| key.name.clone()).collect();
        if how == "full" {
            for column in columns.iter_mut().take(schema.columns.len()) {
                if !left_names.contains(&column.name) {
                    column.values.missing = true;
                }
            }
        }
        if matches!(how, "semi" | "anti") {
            columns.truncate(schema.columns.len());
        }
        let on = if how == "cross" {
            String::new()
        } else if renamed {
            format!(
                ", left_on = {}, right_on = {}",
                character_vector(&left_names),
                character_vector(&right_names)
            )
        } else {
            format!(", on = {}", character_vector(&left_names))
        };
        let how_text = if how == "inner" && self.rng.chance(1, 2) {
            String::new()
        } else {
            format!(", how = \"{how}\"")
        };
        let other_text = if self.rng.chance(1, 3) {
            format!("DataFrame.lazy({other})")
        } else {
            other
        };
        schema.columns = columns;
        schema.ordered = false;
        Some(format!("{{F}}.join({other_text}{on}{how_text}{suffix})"))
    }

    /// `join_asof` on an integer column, after a sort by it. The right table's values of `on`
    /// rise in odd steps, so no left value lies halfway between two of them and `nearest` has
    /// one answer.
    fn join_asof(&mut self, schema: &mut FrameSchema) -> Option<String> {
        let on = self.pick(schema, |values| {
            values.key() && values.cell == Cell::Integer && !values.big
        })?;
        let by = if self.rng.chance(1, 2) {
            self.pick(schema, |values| {
                values.key() && values.cell == Cell::Character
            })
        } else {
            None
        };
        let strategy = match (by.is_some(), self.rng.below(4)) {
            (_, 0) => "backward",
            (_, 1) => "forward",
            (false, 2) => "nearest",
            _ => "",
        };
        let name = format!("t{}", self.tables.len() + 1);
        let rows = self.rng.below(7);
        let mut start: i64 = -9 + self.rng.below(5) as i64;
        let mut on_values = Vec::with_capacity(rows);
        for _ in 0..rows {
            on_values.push(format!("{start}L"));
            start += [1, 3, 5, 7][self.rng.below(4)];
        }
        let mut arguments = vec![format!(
            "{} = c({})",
            argument_name(&on.name),
            on_values.join(", ")
        )];
        let mut right_columns = Vec::new();
        if let Some(by) = &by {
            let groups: Vec<String> = (0..rows)
                .map(|_| string_literal(CHARACTER_VALUES[self.rng.below(3)]))
                .collect();
            arguments.push(format!(
                "{} = c({})",
                argument_name(&by.name),
                groups.join(", ")
            ));
        }
        let taken: Vec<String> = [on.name.clone()]
            .into_iter()
            .chain(by.iter().map(|by| by.name.clone()))
            .collect();
        let extra = self.fresh_name(&taken);
        let (text, mut values) = self.column_values(Cell::Double, rows, false);
        arguments.push(format!(
            "{} = {}",
            argument_name(&extra),
            if rows == 0 {
                text
            } else {
                format!("rep_len({text}, {rows})")
            }
        ));
        values.missing = true;
        right_columns.push(FrameColumn {
            name: extra.clone(),
            values,
        });
        let empty = |cell: &str| format!("{cell}(0)");
        if rows == 0 {
            arguments[0] = format!("{} = {}", argument_name(&on.name), empty("integer"));
            if let Some(by) = &by {
                arguments[1] = format!("{} = {}", argument_name(&by.name), empty("character"));
            }
        }
        self.tables
            .push(format!("  {name} <- DataFrame({})\n", arguments.join(", ")));
        let mut columns = schema.columns.clone();
        for column in right_columns {
            let name = if columns.iter().any(|existing| existing.name == column.name) {
                format!("{}_right", column.name)
            } else {
                column.name
            };
            if columns.iter().any(|existing| existing.name == name) {
                return None;
            }
            columns.push(FrameColumn {
                name,
                values: column.values,
            });
        }
        schema.columns = columns;
        schema.ordered = false;
        let by_text = by
            .map(|by| format!(", by = {}", string_literal(&by.name)))
            .unwrap_or_default();
        let strategy_text = if strategy.is_empty() {
            String::new()
        } else {
            format!(", strategy = \"{strategy}\"")
        };
        Some(format!(
            "{{F}}.sort({}) |>\n    {{F}}.join_asof({name}, on = {}{by_text}{strategy_text})",
            string_literal(&on.name),
            string_literal(&on.name)
        ))
    }

    /// A verb that must fail: a missing column, text in arithmetic, a row expression in `agg`, or
    /// a join on a key the other table lacks.
    fn wrong_verb(&mut self, schema: &FrameSchema) -> String {
        let text_column = schema
            .columns
            .iter()
            .find(|column| column.values.cell == Cell::Character);
        match self.rng.below(6) {
            0 => "{F}.select(\"no such column\")".to_owned(),
            1 => "{F}.filter(Expr.col(\"no such column\") > 1L)".to_owned(),
            2 => "{F}.sort(\"no such column\")".to_owned(),
            3 if text_column.is_some() => format!(
                "{{F}}.with_columns(Expr.col({}) * 2)",
                string_literal(
                    &text_column
                        .map(|column| column.name.clone())
                        .unwrap_or_default()
                )
            ),
            4 if schema.columns.len() > 1 => format!(
                "{{F}}.group_by({}) |>\n    {{G}}.agg(Expr.col({}))",
                string_literal(&schema.columns[0].name),
                string_literal(&schema.columns[schema.columns.len() - 1].name)
            ),
            _ => "{F}.join(DataFrame(zz_key = 1L), on = \"zz_key\")".to_owned(),
        }
    }

    /// `DataFrame.pivot` on a table of its own, because the columns it makes depend on the data.
    fn pivot(&mut self) -> (String, bool) {
        let rows = [0, 1, 2, 3, 5, 8, 12][self.rng.below(7)];
        let labels = ["a", "b", "zz", "B", "\u{e9}"];
        let on: Vec<String> = (0..rows)
            .map(|_| string_literal(labels[self.rng.below(labels.len())]))
            .collect();
        let index_cell = [Cell::Integer, Cell::Double, Cell::Character][self.rng.below(3)];
        let (index, _) = self.column_values(index_cell, rows, true);
        let value_cell =
            [Cell::Integer, Cell::Double, Cell::Character, Cell::Logical][self.rng.below(4)];
        let (values, _) = self.column_values(value_cell, rows, true);
        let aggregates: &[&str] = match value_cell {
            // ry fills an empty cell with 0 under "sum" and `NaN` under "mean", where the model
            // gives `NA`, and the contract does not say what an empty cell holds.
            Cell::Integer | Cell::Double => &["min", "max", "first", "last"],
            _ => &["first", "last"],
        };
        let agg = aggregates[self.rng.below(aggregates.len())];
        let names = if self.features.collisions {
            ["on", "index", "values"]
        } else {
            ["p", "q", "w"]
        };
        let on_text = if rows == 0 {
            "character(0)".to_owned()
        } else {
            format!("c({})", on.join(", "))
        };
        self.tables.push(format!(
            "  t1 <- DataFrame({} = {on_text}, {} = {index}, {} = {values})\n",
            names[0], names[1], names[2]
        ));
        let call = if self.features.lazy {
            "t1 |> DataFrame.lazy() |> LazyFrame.pivot"
        } else {
            "t1 |> DataFrame.pivot"
        };
        (
            format!(
                "  {call}(on = \"{}\", index = \"{}\", values = \"{}\", agg = \"{agg}\")",
                names[0], names[1], names[2]
            ),
            true,
        )
    }

    /// `DataFrame.set_key` on an integer column, then `key` and `copy`.
    fn keyed(&mut self) -> (String, bool) {
        let specification = [("k".to_owned(), Cell::Integer)];
        let (name, _) = self.table(Some(&specification));
        let copy = if self.rng.chance(1, 2) {
            format!("  {name} <- DataFrame.copy({name})\n")
        } else {
            String::new()
        };
        (
            format!(
                "{copy}  DataFrame.set_key({name}, \"k\")\n  cat(DataFrame.key({name}), \"\\n\")\n  {name}"
            ),
            true,
        )
    }

    /// One relation that must hold within an engine, as a `frame_holds` top-level call.
    fn relation(&mut self) -> String {
        self.tables.clear();
        let (label, body) = match self.rng.below(8) {
            0 => {
                let (name, mut schema) = self.table(None);
                let mut steps = Vec::new();
                for _ in 0..1 + self.rng.below(5) {
                    if let Some(step) = self.verb(&mut schema) {
                        steps.push(step);
                    }
                }
                let ordered = if schema.ordered { "TRUE" } else { "FALSE" };
                (
                    "lazy_is_eager",
                    format!(
                        "  eager <- {}\n  lazy <- {}\n  identical(frame_rows(eager, {ordered}), frame_rows(lazy, {ordered}))",
                        render_chain(&name, &steps, false),
                        render_chain(&name, &steps, true)
                    ),
                )
            }
            1 => {
                let (name, schema) = self.table(None);
                let Some(term) = self.logical_term(&schema, 2) else {
                    return String::new();
                };
                (
                    "filters_partition",
                    format!(
                        "  p <- {}\n  rows <- function(x) frame_rows(x, FALSE)[-(1:3)]\n  parts <- c(rows(DataFrame.filter({name}, p)), rows(DataFrame.filter({name}, !p)), rows(DataFrame.filter({name}, Expr.is_null(p))))\n  identical(sort(parts, method = \"radix\"), rows({name}))",
                        term.text
                    ),
                )
            }
            2 => {
                let specification = [
                    ("k".to_owned(), Cell::Character),
                    ("c".to_owned(), Cell::Integer),
                ];
                let (name, _) = self.table(Some(&specification));
                (
                    "groups_add_up",
                    format!(
                        "  s <- {name} |> DataFrame.group_by(\"k\") |> GroupBy.agg(total = Expr.sum(Expr.col(\"c\")), count = Expr.len())\n  s <- as.data.frame(s)\n  sum(s$total) == sum(as.data.frame({name})$c) && sum(s$count) == DataFrame.height({name})"
                    ),
                )
            }
            3 => {
                let (name, schema) = self.table(None);
                let Some(key) = self.pick(&schema, |_| true) else {
                    return String::new();
                };
                let keys = string_literal(&key.name);
                (
                    "sort_is_idempotent",
                    format!(
                        "  once <- DataFrame.sort({name}, {keys})\n  twice <- DataFrame.sort(once, {keys})\n  identical(frame_rows(once, TRUE), frame_rows(twice, TRUE)) && identical(frame_rows(once, FALSE), frame_rows({name}, FALSE))"
                    ),
                )
            }
            4 => {
                let (name, schema) = self.table(None);
                let all: Vec<String> = schema
                    .columns
                    .iter()
                    .map(|column| string_literal(&column.name))
                    .collect();
                (
                    "select_all_is_identity",
                    format!(
                        "  identical(frame_rows(DataFrame.select({name}, {}), TRUE), frame_rows({name}, TRUE))",
                        all.join(", ")
                    ),
                )
            }
            5 => {
                let (name, schema) = self.table(None);
                let there: Vec<String> = schema
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        format!(
                            "renamed_{index} = Expr.col({})",
                            string_literal(&column.name)
                        )
                    })
                    .collect();
                let back: Vec<String> = schema
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        format!(
                            "{} = Expr.col(\"renamed_{index}\")",
                            argument_name(&column.name)
                        )
                    })
                    .collect();
                (
                    "rename_and_back",
                    format!(
                        "  there <- DataFrame.select({name}, {})\n  back <- DataFrame.select(there, {})\n  identical(frame_rows(back, TRUE), frame_rows({name}, TRUE))",
                        there.join(", "),
                        back.join(", ")
                    ),
                )
            }
            6 => {
                let (name, schema) = self.table(None);
                let rows = [0, 1, 3, 7][self.rng.below(4)];
                let left: Vec<String> = schema
                    .columns
                    .iter()
                    .map(|column| string_literal(&column.name))
                    .collect();
                (
                    "self_join_is_identity",
                    format!(
                        "  u <- DataFrame.head({name}, {rows})\n  u <- DataFrame.with_columns(u, row_id = Expr.rank(Expr.col({})))\n  joined <- DataFrame.select(DataFrame.join(u, u, on = \"row_id\"), {}, \"row_id\")\n  identical(frame_rows(joined, FALSE), frame_rows(u, FALSE))",
                        left[0],
                        left.join(", ")
                    ),
                )
            }
            _ => {
                let (name, _) = self.table(None);
                (
                    "data_frame_round_trip",
                    format!(
                        "  identical(frame_rows(as.DataFrame(as.data.frame({name})), TRUE), frame_rows({name}, TRUE))"
                    ),
                )
            }
        };
        format!(
            "frame_holds(\"{label}\", function() {{\n{}{body}\n}})\n",
            self.tables.concat()
        )
    }

    /// An expression of any cell that gives one value per row.
    fn row_term(&mut self, schema: &FrameSchema, depth: usize) -> Option<Term> {
        match self.rng.below(9) {
            0 | 1 => self.logical_term(schema, depth),
            2 if schema.ordered => {
                let column = self.pick(schema, |values| !values.nan)?;
                let descending =
                    ["", ", descending = TRUE", ", descending = FALSE"][self.rng.below(3)];
                Some(Term {
                    text: format!(
                        "Expr.rank(Expr.col({}){descending})",
                        string_literal(&column.name)
                    ),
                    output: Some(column.name.clone()),
                    reads: vec![column.name],
                    values: Values {
                        cell: Cell::Integer,
                        missing: false,
                        nan: false,
                        infinite: false,
                        big: false,
                    },
                    bounded: false,
                })
            }
            3 => self.window_term(schema),
            4 => self.if_else_term(schema),
            5 if depth > 0 => {
                let column = self.pick(schema, |_| true)?;
                Some(column_term(&column))
            }
            // Only the outermost term of a select or with_columns item (depth 2) is aliased:
            // ry names `Expr.is_null(x |> Expr.alias("p"))` after x's column, and the contract
            // is silent on an alias nested inside another expression.
            6 if depth == 2 => {
                let mut term = self.row_term(schema, depth - 1)?;
                let name = self.fresh_name(&[]);
                term.text = format!("({}) |> Expr.alias({})", term.text, string_literal(&name));
                term.output = Some(name);
                Some(term)
            }
            _ => self.numeric_term(schema, depth),
        }
    }

    fn numeric_term(&mut self, schema: &FrameSchema, depth: usize) -> Option<Term> {
        if depth == 0 || self.rng.chance(1, 3) {
            let column = self.pick(schema, |values| {
                matches!(values.cell, Cell::Integer | Cell::Double)
                    || (values.cell == Cell::Logical && depth == 0)
            })?;
            return Some(column_term(&column));
        }
        if self.rng.chance(1, 8) {
            let inner = self.numeric_term(schema, depth - 1)?;
            return Some(Term {
                text: format!("-{}", parenthesized(&inner.text)),
                values: Values {
                    cell: if inner.values.cell == Cell::Logical {
                        Cell::Integer
                    } else {
                        inner.values.cell
                    },
                    ..inner.values
                },
                ..inner
            });
        }
        let operator = ["+", "-", "*", "/", "^", "%/%", "%%", "%%", "%/%"][self.rng.below(9)];
        let left = self.numeric_term(schema, depth - 1)?;
        let divides = matches!(operator, "%%" | "%/%");
        if divides && !left.bounded {
            return Some(left);
        }
        // R's `^` differs from C's `pow` for a negative infinite base under a fractional power,
        // for a negative zero under a negative one, and for `NA ^ 0`, all of which the contract
        // leaves open, so a power is a positive whole number.
        let right = if operator == "^" {
            literal_term(["2", "3L", "2L"][self.rng.below(3)])
        } else if self.rng.chance(1, 2) {
            let literals: &[&str] = if divides {
                &[
                    "Inf",
                    "-Inf",
                    "0",
                    "0L",
                    "2",
                    "-2",
                    "0.5",
                    "-3L",
                    "7L",
                    "NA_real_",
                    "NA_integer_",
                    "-numeric(1)",
                    "2.5",
                ]
            } else {
                &["1L", "2", "0.5", "-1L", "3", "0L", "Inf", "-2.5"]
            };
            let mut literal = literal_term(literals[self.rng.below(literals.len())]);
            if self.rng.chance(1, 4) {
                literal.text = format!("Expr.lit({})", literal.text);
            }
            literal
        } else if divides {
            column_term(&self.pick(schema, |values| {
                matches!(values.cell, Cell::Integer | Cell::Double) && !values.big
            })?)
        } else {
            self.numeric_term(schema, depth - 1)?
        };
        // GNU R's answer to `NaN op NA` depends on how it was compiled (crates/oracle/AGENTS.md).
        if (left.values.nan && right.values.missing) || (left.values.missing && right.values.nan) {
            return Some(left);
        }
        let (first, second) = if self.rng.chance(1, 5) && right.output.is_none() {
            (right, left)
        } else {
            (left, right)
        };
        let cell = match operator {
            "/" | "^" => Cell::Double,
            _ if first.values.cell == Cell::Double || second.values.cell == Cell::Double => {
                Cell::Double
            }
            _ => Cell::Integer,
        };
        let making_nan = cell == Cell::Double
            && (matches!(operator, "/" | "^" | "%%" | "%/%")
                || first.values.infinite
                || second.values.infinite);
        let values = Values {
            cell,
            missing: first.values.missing
                || second.values.missing
                || (cell == Cell::Integer && (divides || first.values.big || second.values.big)),
            nan: first.values.nan || second.values.nan || making_nan,
            infinite: first.values.infinite
                || second.values.infinite
                || matches!(operator, "/" | "^" | "%/%"),
            big: first.values.big || second.values.big || operator == "^" || operator == "*",
        };
        let mut reads = first.reads.clone();
        reads.extend(second.reads.iter().cloned());
        Some(Term {
            text: format!(
                "{} {operator} {}",
                parenthesized(&first.text),
                parenthesized(&second.text)
            ),
            output: first.output.clone().or_else(|| second.output.clone()),
            reads,
            values,
            bounded: first.bounded && second.bounded,
        })
    }

    fn logical_term(&mut self, schema: &FrameSchema, depth: usize) -> Option<Term> {
        let choice = if depth == 0 { 0 } else { self.rng.below(6) };
        let term = match choice {
            0 | 1 => {
                if self.rng.chance(1, 3)
                    && let Some(column) = self.pick(schema, |values| values.cell == Cell::Character)
                {
                    let operator = ["==", "!=", "<", "<=", ">", ">="][self.rng.below(6)];
                    let value =
                        string_literal(CHARACTER_VALUES[self.rng.below(CHARACTER_VALUES.len())]);
                    let mut term = column_term(&column);
                    term.text = format!("{} {operator} {value}", term.text);
                    term.values = Values {
                        cell: Cell::Logical,
                        nan: false,
                        ..column.values
                    };
                    return Some(term);
                }
                let left = self.numeric_term(schema, depth.saturating_sub(1))?;
                let right = if self.rng.chance(1, 2) {
                    literal_term(["1L", "0", "2.5", "-1", "Inf", "NA", "3L"][self.rng.below(7)])
                } else {
                    self.numeric_term(schema, depth.saturating_sub(1))?
                };
                let operator = ["<", "<=", ">", ">=", "==", "!="][self.rng.below(6)];
                let mut reads = left.reads.clone();
                reads.extend(right.reads.iter().cloned());
                Term {
                    text: format!(
                        "{} {operator} {}",
                        parenthesized(&left.text),
                        parenthesized(&right.text)
                    ),
                    output: left.output.clone().or_else(|| right.output.clone()),
                    reads,
                    values: Values {
                        cell: Cell::Logical,
                        missing: left.values.missing
                            || right.values.missing
                            || left.values.nan
                            || right.values.nan,
                        nan: false,
                        infinite: false,
                        big: false,
                    },
                    bounded: false,
                }
            }
            2 => {
                let inner = self.row_term(schema, depth - 1)?;
                Term {
                    text: format!("Expr.is_null({})", inner.text),
                    values: Values {
                        cell: Cell::Logical,
                        missing: false,
                        nan: false,
                        infinite: false,
                        big: false,
                    },
                    bounded: false,
                    ..inner
                }
            }
            3 => {
                let inner = self.logical_term(schema, depth - 1)?;
                Term {
                    text: format!("!{}", parenthesized(&inner.text)),
                    ..inner
                }
            }
            4 => {
                let left = self.logical_term(schema, depth - 1)?;
                let right = self.logical_term(schema, depth - 1)?;
                let operator = ["&", "|"][self.rng.below(2)];
                let mut reads = left.reads.clone();
                reads.extend(right.reads.iter().cloned());
                Term {
                    text: format!(
                        "{} {operator} {}",
                        parenthesized(&left.text),
                        parenthesized(&right.text)
                    ),
                    output: left.output.clone().or_else(|| right.output.clone()),
                    reads,
                    values: Values {
                        missing: left.values.missing || right.values.missing,
                        ..left.values
                    },
                    bounded: false,
                }
            }
            _ => column_term(&self.pick(schema, |values| values.cell == Cell::Logical)?),
        };
        Some(term)
    }

    /// `Expr.if_else` with a test that is never `NA`, between values of one cell.
    fn if_else_term(&mut self, schema: &FrameSchema) -> Option<Term> {
        let test = self.logical_term(schema, 1)?;
        if test.values.missing {
            return None;
        }
        let yes = column_term(&self.pick(schema, |_| true)?);
        let cell = yes.values.cell;
        let no = if self.rng.chance(1, 2) {
            let literal = match cell {
                Cell::Integer => "0L",
                Cell::Double => "-1.5",
                Cell::Character => "\"none\"",
                Cell::Logical => "FALSE",
            };
            literal_term(literal)
        } else {
            column_term(&self.pick(schema, |values| values.cell == cell)?)
        };
        let mut reads = test.reads.clone();
        reads.extend(yes.reads.iter().cloned());
        reads.extend(no.reads.iter().cloned());
        let (yes, no) = if self.rng.chance(1, 2) {
            (yes, no)
        } else {
            (no, yes)
        };
        Some(Term {
            text: format!("Expr.if_else({}, {}, {})", test.text, yes.text, no.text),
            // ry names it after `yes` or `no`, not the test it reads first, and the contract
            // does not say which argument counts, so it is always named.
            output: None,
            reads,
            values: Values {
                cell,
                missing: yes.values.missing || no.values.missing,
                nan: yes.values.nan || no.values.nan,
                infinite: yes.values.infinite || no.values.infinite,
                big: yes.values.big || no.values.big,
            },
            bounded: false,
        })
    }

    /// `Expr.over` of an aggregation or a rank, within groups of one or two key columns.
    fn window_term(&mut self, schema: &FrameSchema) -> Option<Term> {
        let mut inner = if schema.ordered && self.rng.chance(1, 3) {
            let column = self.pick(schema, |values| !values.nan)?;
            Term {
                text: format!("Expr.rank(Expr.col({}))", string_literal(&column.name)),
                output: Some(column.name.clone()),
                reads: vec![column.name],
                values: Values {
                    cell: Cell::Integer,
                    missing: false,
                    nan: false,
                    infinite: false,
                    big: false,
                },
                bounded: false,
            }
        } else {
            self.aggregate_term(schema)?
        };
        inner.output.as_ref()?;
        let mut keys = Vec::new();
        for _ in 0..1 + self.rng.below(2) {
            let key = self.pick(schema, Values::key)?;
            let text = string_literal(&key.name);
            if !keys.contains(&text) {
                keys.push(text);
                inner.reads.push(key.name);
            }
        }
        let keys: Vec<String> = keys.into_iter().map(|key| self.maybe_named(key)).collect();
        inner.text = format!("Expr.over({}, {})", inner.text, keys.join(", "));
        Some(inner)
    }

    /// An aggregation over a column whose values the contract fixes the result of: no `NA`, no
    /// `NaN`, and no big values, with the order-dependent ones only where the order is fixed.
    fn aggregate_term(&mut self, schema: &FrameSchema) -> Option<Term> {
        let function = [
            "sum", "mean", "median", "min", "max", "first", "last", "std", "var", "len", "cor",
            "sum",
        ][self.rng.below(12)];
        if function == "len" {
            return Some(Term {
                text: "Expr.len()".to_owned(),
                output: None,
                reads: Vec::new(),
                values: Values {
                    cell: Cell::Integer,
                    missing: false,
                    nan: false,
                    infinite: false,
                    big: false,
                },
                bounded: false,
            });
        }
        if matches!(function, "first" | "last") {
            if !schema.ordered {
                return None;
            }
            let column = self.pick(schema, |values| !values.missing && !values.nan)?;
            let mut term = column_term(&column);
            term.text = format!("Expr.{function}({})", term.text);
            return Some(term);
        }
        let accepts = |values: &Values| {
            values.plain()
                && match function {
                    "median" => values.cell == Cell::Double,
                    // Over an infinite value R's `cor`, `var` and `sd` give NaN where ry gives NA
                    // or Inf, and the contract is silent on which one a spread over Inf means.
                    "cor" | "var" | "std" => {
                        matches!(values.cell, Cell::Integer | Cell::Double) && !values.infinite
                    }
                    _ => matches!(values.cell, Cell::Integer | Cell::Double),
                }
        };
        let column = if self.rng.chance(1, 4) {
            let term = self.numeric_term(schema, 1)?;
            if !accepts(&term.values) {
                return None;
            }
            term
        } else {
            column_term(&self.pick(schema, accepts)?)
        };
        if function == "cor" {
            let second = column_term(&self.pick(schema, accepts)?);
            let mut reads = column.reads.clone();
            reads.extend(second.reads.iter().cloned());
            return Some(Term {
                text: format!("Expr.cor({}, {})", column.text, second.text),
                values: Values {
                    cell: Cell::Double,
                    missing: true,
                    nan: true,
                    infinite: false,
                    big: false,
                },
                reads,
                ..column
            });
        }
        let cell = match function {
            "sum" | "min" | "max" => column.values.cell,
            _ => Cell::Double,
        };
        let text = if self.rng.chance(1, 3) {
            format!("{} |> Expr.{function}()", parenthesized(&column.text))
        } else {
            format!("Expr.{function}({})", column.text)
        };
        Some(Term {
            text,
            values: Values {
                cell,
                missing: matches!(function, "std" | "var"),
                nan: column.values.infinite,
                infinite: column.values.infinite,
                big: false,
            },
            bounded: false,
            ..column
        })
    }

    /// A column of `schema` that `accepts` takes, chosen at random.
    fn pick(
        &mut self,
        schema: &FrameSchema,
        accepts: impl Fn(&Values) -> bool,
    ) -> Option<FrameColumn> {
        let candidates: Vec<&FrameColumn> = schema
            .columns
            .iter()
            .filter(|column| accepts(&column.values))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some(candidates[self.rng.below(candidates.len())].clone())
    }

    /// A name from the program's pool that is not `taken`.
    fn fresh_name(&mut self, taken: &[String]) -> String {
        let pool = if self.features.collisions {
            COLLIDING_NAMES
        } else {
            PLAIN_NAMES
        };
        for _ in 0..8 {
            let name = pool[self.rng.below(pool.len())];
            if !taken.iter().any(|taken| taken == name) {
                return name.to_owned();
            }
        }
        (0..)
            .map(|index| format!("col{index}"))
            .find(|name| !taken.contains(name))
            .unwrap_or_default()
    }

    /// An item of `select` or `agg`: the expression alone when its output name is free, or named.
    fn named_item(&mut self, term: &Term, taken: &[String]) -> (String, String) {
        if let Some(output) = &term.output
            && !taken.contains(output)
            && self.rng.chance(1, 2)
        {
            return (term.text.clone(), output.clone());
        }
        let name = self.fresh_name(taken);
        (format!("{} = {}", argument_name(&name), term.text), name)
    }

    /// An argument whose name means nothing to the verb, named after a formal now and then.
    fn maybe_named(&mut self, text: String) -> String {
        if self.features.collisions && self.rng.chance(1, 4) {
            let name =
                ["d", "df", "de", "x", "g", "lf", "desc", "e", "ex", "expr"][self.rng.below(10)];
            format!("{name} = {text}")
        } else {
            text
        }
    }
}

/// A chain over `table` with the steps' `{F}` and `{G}` filled in for an eager or a lazy query.
fn render_chain(table: &str, steps: &[String], lazy: bool) -> String {
    let (frame, group) = if lazy {
        ("LazyFrame", "LazyGroupBy")
    } else {
        ("DataFrame", "GroupBy")
    };
    let mut text = table.to_owned();
    if lazy {
        text.push_str(" |>\n    DataFrame.lazy()");
    }
    for step in steps {
        text.push_str(" |>\n    ");
        text.push_str(&step.replace("{F}", frame).replace("{G}", group));
    }
    if lazy {
        text.push_str(" |>\n    LazyFrame.collect()");
    }
    text
}

fn column_term(column: &FrameColumn) -> Term {
    Term {
        text: format!("Expr.col({})", string_literal(&column.name)),
        output: Some(column.name.clone()),
        reads: vec![column.name.clone()],
        values: column.values.clone(),
        bounded: !column.values.big,
    }
}

fn literal_term(text: &str) -> Term {
    let cell = if text.ends_with('L') || text == "NA_integer_" {
        Cell::Integer
    } else if text == "NA" {
        Cell::Logical
    } else {
        Cell::Double
    };
    Term {
        text: text.to_owned(),
        output: None,
        reads: Vec::new(),
        values: Values {
            cell,
            missing: text.starts_with("NA"),
            nan: false,
            infinite: text.contains("Inf"),
            big: false,
        },
        bounded: true,
    }
}

fn parenthesized(text: &str) -> String {
    if text.starts_with("Expr.") && !text.contains(' ') || !text.contains(' ') {
        text.to_owned()
    } else {
        format!("({text})")
    }
}

/// `text` as an R string literal, with every non-ASCII character escaped.
fn string_literal(text: &str) -> String {
    let mut literal = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            character if character.is_ascii() => literal.push(character),
            character => literal.push_str(&format!("\\u{{{:04x}}}", character as u32)),
        }
    }
    literal.push('"');
    literal
}

fn character_vector(names: &[String]) -> String {
    match names {
        [one] => string_literal(one),
        _ => format!(
            "c({})",
            names
                .iter()
                .map(|name| string_literal(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `name` as an argument name: bare when syntactic, backquoted when ASCII, else a string.
fn argument_name(name: &str) -> String {
    const RESERVED: &[&str] = &[
        "if",
        "else",
        "repeat",
        "while",
        "function",
        "for",
        "in",
        "next",
        "break",
        "TRUE",
        "FALSE",
        "NULL",
        "Inf",
        "NaN",
        "NA",
        "NA_integer_",
        "NA_real_",
        "NA_character_",
    ];
    let mut characters = name.chars();
    let syntactic = match characters.next() {
        Some(first) if first.is_ascii_alphabetic() => true,
        Some('.') => !name[1..].starts_with(|next: char| next.is_ascii_digit()),
        _ => false,
    } && name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '.' || character == '_')
        && !RESERVED.contains(&name);
    if syntactic {
        name.to_owned()
    } else if name.is_ascii() {
        format!("`{name}`")
    } else {
        string_literal(name)
    }
}
