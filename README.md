<div align="center">

<img src="docs/public/logo.svg" alt="" height="96">

# ry

**A type checker and a fast runtime for R, written in Rust.**

[**Docs**](https://ry-lang.org) ·
[**Releases**](https://github.com/felix-andreas/ry/releases) ·
[**VS Code**](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) ·
[**Zed**](https://github.com/felix-andreas/ry/tree/main/editors/zed)

</div>

Untyped R on ry's runtime is faster than GNU R on tight loops and recursion and slower on call-heavy
code. The two benchmarks, side by side:

<!-- bench-summary -->
| untyped R on ry | what it measures | programs | speed vs GNU R (geometric mean) |
| --- | --- | ---: | ---: |
| Real-world workloads | call-heavy R: closures, S3 and S4 dispatch, conditions, environments, strings, lists | 7 | 0.90x |
| Synthetic kernels | tight loops, recursion and vector kernels, where the JIT applies | 17 | 2.70x |
<!-- /bench-summary -->

The synthetic kernels give seconds per program and the peak memory of the whole process, R's
startup included:

<!-- bench-why -->
| program | GNU R | ry | ry, typed and strict | speed-up | peak memory, GNU R / ry / typed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fib` | 0.559 s | 0.007 s | 0.004 s | 79.9x | 52 / 49 / 50 MiB |
| `drawdown` | 0.200 s | 0.029 s | 0.007 s | 6.9x | 71 / 146 / 134 MiB |
| `vector_arith` | 0.111 s | 0.091 s | 0.053 s | 1.2x | 107 / 114 / 77 MiB |
| `log_rank` | 0.071 s | 0.046 s | 1.426 s | 1.5x | 105 / 64 / 108 MiB |
<!-- /bench-why -->

The typed column runs the same program with type annotations in strict mode. The real-world workloads
are in [Benchmarks](https://ry-lang.org/benchmarks#untyped-r-on-call-heavy-real-world-workloads).
[Benchmarks](https://ry-lang.org/benchmarks) has the code of each program and the method.

## Installation

[Install ry](https://ry-lang.org/installation), then run it inside an R project or package:

```sh
ry check    # report problems
ry fmt      # format in place (--check for CI)
ry run      # run a script, or open a console that reloads your package as you edit it
```

Checking and formatting never run your code. `ry run` needs no GNU R on Linux, because ry carries base R 4.6.2 inside the binary. The macOS and Windows
builds (0.4.1-beta) read base R's own R code from an installed GNU R.

## Features

### Type checking

ry infers types from plain R, so it finds the mistake below without a single annotation. Type
errors are opt-in: turn them on in `ry.toml` at the root of your project:

```toml
[check]
typing = true
```

```r
apply_discount <- function(price, rate) price * (1 - rate)
apply_discount(100, "0.2")
```

`ry check` reports the string passed as `rate`:

```
type-mismatch

  x expected a numeric value (`integer` or `double`), found `character`
   --[discount.R:2:21]
 1 | apply_discount <- function(price, rate) price * (1 - rate)
 2 | apply_discount(100, "0.2")
   |                     ^^^^^
```

Where inference is not enough, you state a type yourself: in a `#:` comment, such as
`#: fn(price: double, rate: double) -> double` above the function, which keeps the file plain R, or
inline, as in `function(price: double, rate: double) -> double`.
[Type checking](https://ry-lang.org/tutorial/types) explains both.

### Performance

ry's runtime has a JIT compiler, which turns the functions and loops that run most into machine
code. It also implements R's C API, so packages with compiled code, such as jsonlite, load into it.

Inside a package, `ry run` opens a console that reloads exactly the definitions you edited before
each command, so you never call `load_all()` or restart R. On a 326,000-line package, the command
after an edit runs within 10 to 30 ms, where `devtools::load_all()` takes 8.6 s.

Some parts of R are missing from the runtime, and a script that needs one stops with an error that
names it. `ry R` runs the same script on the GNU R installed on your machine instead.
[Running and reloading](https://ry-lang.org/tutorial/runtime) has the details.

### Exhaustive `match`

When you add a case to a tagged union, ry points you at every `match` that does not handle it:

```r
alias Payment =
  | :Paid(amount: double)
  | :Refunded(amount: double)
  | :Pending

balance <- function(payment: Payment) -> double {
  match (payment) {
    :Paid(amount = a) => a
    :Refunded(amount = a) => -a
  }
}
```

`ry check` reports the case the `match` misses:

```
non-exhaustive-match

  x this `match` does not handle `:Pending`: add an arm for it, or a catch-all `_ => ...` arm
   --[payment.R:7:3]
 6 | balance <- function(payment: Payment) -> double {
 7 |   match (payment) {
   |   ^^^^^^^^^^^^^^^
 8 |     :Paid(amount = a) => a
  help: Add an arm for `:Pending` (`ry check --fix --unsafe-fixes` applies it)
```

Tagged unions and `match` are ry syntax, not R, so code that uses them runs only on ry's runtime.
[Records and tags](https://ry-lang.org/tutorial/runtime#records-tags-and-match) covers them.

## FAQ

### Why would I need type checking?

When AI agents write much of the code, you need tools that verify a codebase without a person
reading every line, and type checking is one of them. It checks every line on every change,
including the lines no test reaches.

### Who is ry for?

Teams with R codebases of more than 100,000 lines that cannot easily move to another language.

### My R codebase isn't huge. Should I still use ry?

No. Let Claude rewrite your code in Rust.

### Why is it called ry?

It started as "the roughly good enough language server": all it offered was go-to-definition
based on regular expressions, but it was fast. It grew into a proper language server built on
tree-sitter, and people called it just Roughly. With its first type checker it was rewritten from
scratch with its own recursive-descent parser, and renamed ry.

## License

The `ry` program is GPL, like GNU R, because parts of its runtime are translated from R's own C
code. The analysis libraries (the parser, type checker, formatter and language-server features) are
UPL-1.0, so any project can reuse them. The C headers that packages compile against are
LGPL-2.1-or-later, as R's own are, so a compiled package can choose its own licence. The editor
extensions are MIT. [Licensing](https://ry-lang.org/reference/licensing/) gives the exact version
for every part.
