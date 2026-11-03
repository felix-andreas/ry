# ry for VS Code

R support from [ry](https://ry-lang.org), a type checker, linter, formatter and runtime for R. Open an
R project and it works, with no configuration and no R session needed for the editor features.

> **Note**
> The extension bundles the `ry` binary for **Linux x86_64, macOS aarch64 and Windows x86_64**. On
> another platform, install `ry` yourself and set `ry.path`.

## What you get

- **Diagnostics as you type**: syntax errors that say what is missing, unresolved names, unused
  variables, lints, and type errors once you turn them on, the same findings as `ry check`
- **Hover** with the inferred type of any expression, parameter or loop variable, and the help page
  of a function from an installed package
- **Completion** of variables, list fields after `$`, `pkg::` exports and argument names, ranked
  by the type the code expects
- **Navigation**: go to definition, find references, rename, call hierarchy, document and
  workspace symbols, also into installed packages
- **Signature help** and **inlay hints** for inferred types and parameter names
- **Quick fixes** for misspelled names and lints, and refactorings: extract variable or function,
  inline, and convert between `#:` comments and inline types
- **Formatting** of the document, a selection, or as you type, with the rules of `ry fmt`
- **Semantic highlighting**, folding and an outline with RStudio-style sections
- **Running and debugging**: "Run file", "Run test" and "Debug test" lenses, a console that reloads
  your package as you edit it (`Ctrl+Enter` sends the line or selection), breakpoints and stepping,
  a data viewer for `View()`, and an "R Plot" panel for plots drawn in the console

R Markdown, Quarto and Sweave chunks get the same features. The
[editor guide](https://ry-lang.org/guides/editors) lists everything.

## Type checking for plain R

ry infers types from how values are used, so it finds this mistake in unannotated code before
anything runs:

```r
apply_discount <- function(price, rate) price * (1 - rate)
apply_discount(100, "0.2")
```

```
type-mismatch

  x expected a numeric value (`integer` or `double`), found `character`
   --[R/discount.R:2:21]
 1 | apply_discount <- function(price, rate) price * (1 - rate)
 2 | apply_discount(100, "0.2")
   |                     ^^^^^
```

Type errors are opt-in: add a `ry.toml` with `typing = true` under `[check]` at the project root.

## Fast R

ry's own runtime compiles hot code to machine code. Seconds per program, and peak memory of the
whole process, R's startup included:

<!-- bench-why -->
| program | GNU R | ry | ry, typed and strict | peak memory, GNU R / ry / typed |
| --- | ---: | ---: | ---: | ---: |
| `fib` | 1.312 s | 0.010 s (131.2x) | 0.010 s | 54 / 43 / 43 MiB |
| `drawdown` | 0.335 s | 0.074 s (4.5x) | 0.079 s | 73 / 133 / 140 MiB |
| `vector_arith` | 0.290 s | 0.242 s (1.2x) | 0.249 s | 109 / 157 / 164 MiB |
| `log_rank` | 0.128 s | 0.137 s (0.9x) | 0.136 s | 107 / 56 / 58 MiB |
<!-- /bench-why -->

[Benchmarks](https://ry-lang.org/benchmarks) has every program, its code, and how it was measured.

## Tagged unions and `match`

ry adds records, tuples and tagged unions, and a `match` that knows every case:

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

```
non-exhaustive-match

  x this `match` does not handle `:Pending`: add an arm for it, or a catch-all `_ => ...` arm
```

## Settings

```jsonc
{
  // Use a custom binary instead of the bundled one
  "ry.path": "/path/to/ry",
  // Arguments for the binary; the default is ["server"]
  "ry.args": ["server"]
}
```

Project settings, such as turning on type errors or choosing lints, live in `ry.toml`
([configuration](https://ry-lang.org/reference/configuration)).

## Links

- [Documentation](https://ry-lang.org)
- [Source code](https://github.com/felix-andreas/ry)

## License

The extension's own code is MIT. The bundled `ry` binary is GPL-2.0-or-later, the same terms as
GNU R; [licensing](https://ry-lang.org/reference/licensing/) has the details.
