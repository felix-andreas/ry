# ry for VS Code

**A type checker and a fast runtime for R, written in Rust.**

The extension bundles the [ry](https://ry-lang.org) binary, so you open an R project and the editor
features work, with no configuration and no R session behind them.

> **Note**
> The bundled binary covers **Linux x86_64, macOS on Apple silicon and Windows x86_64**. On another
> platform, [install `ry`](https://ry-lang.org/installation) yourself and set `ry.path`.

## What it does

- **Diagnostics as you type**: misspelled names, unused variables, lint findings and, once you turn
  them on, type errors, the same findings as `ry check`.
- **Hover**: the inferred type of any expression, parameter or loop variable, and the help page of a
  function from an installed package.
- **Completion**: variables, list fields after `$`, `pkg::` exports and argument names, with the
  names whose type fits where you are typing ranked first.
- **Signature help**: each parameter's type and default while you fill in a call.
- **Inlay hints**: the inferred types of assignments, loop variables and function parameters, and
  parameter names in calls.
- **Go to definition and find references**: across files and into the R code of installed packages.
- **Rename**: a name changes everywhere it is used.
- **Quick fixes and refactorings**: a misspelled name or a lint corrected in one step, a variable or
  a function extracted, and types moved between `#:` comments and inline syntax.
- **Formatting**: the rules of `ry fmt`, for the document, a selection, or as you type.
- **A console that reloads your package**: `Ctrl+Enter` sends the current line or selection to a
  console running `ry run`, which reloads the definitions you edited before each command. Plots
  appear in an "R Plot" panel beside the editor, and `View()` opens a data viewer.
- **Running and debugging**: "Run file" and "Run test" lenses run a script or a test, and `F5` or
  "Debug test" starts the debugger, with breakpoints, stepping and each frame's variables.

Running code needs GNU R installed, because ry's runtime reads base R's own R code from it. On
Windows, the console has no connection to the editor, so the plot panel, `View()` and attaching the
debugger to the console are not available there. R Markdown, Quarto and Sweave chunks get the same
features. The [editor guide](https://ry-lang.org/guides/editors) lists everything.

## Type checking

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

The editor underlines `"0.2"`, and `ry check` reports the same finding:

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

## Exhaustive `match`

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

## Speed

On ry's runtime, loops and recursion run several times faster than on GNU R, and vectorized code
runs about as fast. The table gives seconds per program and the peak memory of the whole process,
R's startup included:

<!-- bench-why -->
| program | GNU R | ry | ry, typed and strict | speed-up | peak memory, GNU R / ry / typed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fib` | 1.312 s | 0.010 s | 0.010 s | 131.2x | 54 / 43 / 43 MiB |
| `drawdown` | 0.335 s | 0.074 s | 0.079 s | 4.5x | 73 / 133 / 141 MiB |
| `vector_arith` | 0.290 s | 0.242 s | 0.249 s | 1.2x | 109 / 157 / 164 MiB |
| `log_rank` | 0.128 s | 0.137 s | 0.136 s | 0.9x | 107 / 56 / 58 MiB |
<!-- /bench-why -->

The typed column runs the same program with type annotations in strict mode.
[Benchmarks](https://ry-lang.org/benchmarks) has the code of each program and the method.

## Settings

```jsonc
{
  // Use another ry binary instead of the bundled one
  "ry.path": "/path/to/ry",
  // Arguments for the binary; the default is ["server"]
  "ry.args": ["server"]
}
```

Project settings, such as turning on type errors or choosing lints, live in `ry.toml`
([configuration](https://ry-lang.org/reference/configuration)).

## License

The extension's own code is MIT, and the bundled `ry` binary is GPL, like GNU R
([licensing](https://ry-lang.org/reference/licensing/)).
