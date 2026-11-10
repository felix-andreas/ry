<div align="center">

<img src="docs/public/logo.svg" alt="" height="96">

# ry

**A type checker and a fast runtime for R, written in Rust.**

[**Docs**](https://ry-lang.org) ·
[**Releases**](https://github.com/felix-andreas/ry/releases) ·
[**VS Code**](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) ·
[**Zed**](https://github.com/felix-andreas/ry/tree/main/editors/zed)

</div>

ry checks, formats and runs the R code you already have, and adds static types and its own data
types on top. Plain R stays valid, and the checker never runs your code.

```sh
ry check    # report problems
ry fmt      # format in place
ry run      # run a script on ry's runtime, or open a console that reloads your package
```

## Example

A typo and an argument of the wrong type. The `#:` comment is an optional annotation that keeps the
file plain R:

```r
#: fn(price: double, rate: double) -> double
net_price <- function(price, rate) {
  price * (1 - ratee)
}

net_price(100, "0.2")
```

```
$ ry check
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[price.R:3:16]
 2 | net_price <- function(price, rate) {
 3 |   price * (1 - ratee)
   |                ^^^^^
 4 | }
  help: Change to `rate` (`ry check --fix --unsafe-fixes` applies it)

type-mismatch

  x expected `double`, found `character`
   --[price.R:6:16]
 5 |
 6 | net_price(100, "0.2")
   |                ^^^^^
```

Type errors need `typing = true` in `ry.toml`; names, syntax and lints are checked without any
configuration.

## Speed

ry's runtime against GNU R, as the geometric mean of each suite's speed-ups:

<!-- bench-summary -->
| benchmark | programs | ry vs GNU R |
| --- | ---: | ---: |
| Real-world workloads (call-heavy R) | 7 | 0.90x |
| Synthetic kernels (loops, recursion, vectors) | 17 | 2.70x |
<!-- /bench-summary -->

A few of the synthetic kernels, as speed-ups over GNU R, untyped and typed in strict mode:

<!-- bench-why -->
| program | ry | ry, typed |
| --- | ---: | ---: |
| `fib` | 80x | 140x |
| `drawdown` | 6.90x | 29x |
| `vector_arith` | 1.22x | 2.09x |
| `log_rank` | 1.54x | 0.05x |
<!-- /bench-why -->

Loops and recursion gain the most. Call-heavy code is slower than on GNU R, and typed code built on
records runs mostly in the interpreter. [Benchmarks](https://ry-lang.org/benchmarks) has every
program and the method.

## Features

- **Checker:** unresolved names, syntax errors, lints, and type errors inferred from plain R.
- **Types:** `#:` comments or inline syntax, generics, unions, records, tagged unions, strict mode.
- **Formatter:** `ry fmt`, one style for every file.
- **Editor:** a language server for any LSP editor, and a VS Code extension with a debugger.
- **Loader:** reloads a 326,000-line package after an edit in about 80 ms, where `load_all()`
  takes 8.6 s.
- **Runtime:** a JIT, R's C API for compiled packages, and `DataFrame`, a typed table on Polars.

ry is in beta. The checker, formatter, language server and loader are ready for daily use. The
runtime is incomplete: a script that needs something it lacks stops with an error that names it, and
`ry R` runs it on GNU R instead. [Install ry](https://ry-lang.org/installation), then follow
[Adopting ry](https://ry-lang.org/guide) or take the [language tour](https://ry-lang.org/ry).

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
