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
        match self.rng.below(26) {
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
        let (allocate, write) = match self.rng.below(5) {
            0 => ("numeric(n)", format!("v[i] <- {element}")),
            1 => ("integer(n)", "v[i] <- i * 2L".to_owned()),
            2 => (
                "numeric(0)",
                format!("v[i] <- v[max(1L, i - 1L)] + {element}"),
            ),
            3 => ("logical(n)", "v[[i]] <- i %% 2L == 0L".to_owned()),
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
        let shown = self.show(&format!("w{k}"));
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
            Err(EngineError::Failed(reason)) => divergences.push(Divergence {
                engine: engine.name(),
                detail: reason,
            }),
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
