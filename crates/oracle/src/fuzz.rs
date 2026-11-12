//! Random R programs for differential fuzzing, one per seed.
//!
//! A program is a sequence of independent blocks, each a shape the runtime has to get right:
//! hot functions with scalar and vector loops (so `RY_JIT=force` compiles them), element reads
//! and writes, closures, S3 dispatch, `switch`, `repeat`, `Recall`, environments, attributes,
//! NA propagation, integer overflow, recycling and condition handling. Most blocks are
//! well-typed and print what they compute, so a difference shows up in stdout as well as in the
//! last value. A share of the expressions are ill-typed on purpose (a string in arithmetic, a
//! length-two condition, a missing variable, a call with the wrong arity); those sit inside
//! `tryCatch` and print the error message, and a few programs end in an uncaught error.
//!
//! Every program is deterministic under GNU R: nothing reads the clock, addresses, the
//! environment or an unseeded random number, and no environment or closure is ever printed.

use {
    crate::{Engine, EngineError, Outcome, diff},
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
    text.push_str(&generator.ending());
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
        match self.rng.below(28) {
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

/// What one seed's program did under the reference engine and every other engine.
pub struct SeedReport {
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
}

/// Runs `seed`'s program under `reference` and under each of `engines`, in a file written to
/// `directory`, and compares every engine with the reference.
pub fn check_seed(
    seed: u64,
    reference: &dyn Engine,
    engines: &[&dyn Engine],
    directory: &Path,
) -> Result<SeedReport, EngineError> {
    let source = program(seed);
    let path = directory.join(format!("fuzz_{seed}.R"));
    std::fs::write(&path, &source)
        .map_err(|error| EngineError::Failed(format!("{}: {error}", path.display())))?;
    let reference_outcome = reference.run(&path)?;
    let mut divergences = Vec::new();
    let mut unavailable = Vec::new();
    for engine in engines {
        match engine.run(&path) {
            Ok(outcome) => {
                if let Some(detail) = diff(
                    &reference.name(),
                    &reference_outcome,
                    &engine.name(),
                    &outcome,
                ) {
                    divergences.push(Divergence {
                        engine: engine.name(),
                        detail,
                    });
                }
            }
            Err(EngineError::Unavailable(reason)) => {
                unavailable.push(format!("{}: {reason}", engine.name()))
            }
            Err(EngineError::Failed(reason) | EngineError::Crashed(reason)) => {
                divergences.push(Divergence {
                    engine: engine.name(),
                    detail: reason,
                })
            }
        }
    }
    if let Err(error) = std::fs::remove_file(&path) {
        eprintln!("could not remove {}: {error}", path.display());
    }
    Ok(SeedReport {
        seed,
        program: source,
        reference: reference_outcome,
        divergences,
        unavailable,
    })
}

/// The failure report for one seed: the program, every engine's difference, and the commands
/// that reproduce it.
pub fn render_divergences(report: &SeedReport) -> String {
    let engines: Vec<&str> = report
        .divergences
        .iter()
        .map(|divergence| divergence.engine.as_str())
        .collect();
    let mut text = format!(
        "seed {} diverges under {}\nreproduce: cargo run -q -p oracle -- fuzz --seed {} --engines {}\nprogram:   cargo run -q -p oracle -- fuzz --seed {} --print > fuzz_{}.R\n--- program ---\n{}",
        report.seed,
        engines.join(", "),
        report.seed,
        engines.join(","),
        report.seed,
        report.seed,
        report.program
    );
    for divergence in &report.divergences {
        text.push_str(&format!(
            "--- {} ---\n{}\n",
            divergence.engine, divergence.detail
        ));
    }
    text
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
    quit(save = "no")
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
    quit(save = "no")
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
