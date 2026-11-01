---
title: Why ry
description: One toolchain for R, a type system on top of it, and a path from the R code you have to code an agent can safely change
---

R code bases that are years old are now being changed by agents, and code an agent writes needs to
be checked by something other than a person reading every line. A type checker does that on every
edit. R has none, so ry adds one, and it works on the R code you already have.

ry is one toolchain for R: a checker, a language server, a formatter, a package loader, a console
and a runtime, all reading the same `ry.toml`. Its type system sits on top of R, so plain R stays
valid. On top of that, ry adds features R does not have, such as records, tagged unions and a typed
table, which we call R++. Strict mode then lets you keep only code whose types are known, which we
call R−−.

<figure>
<svg viewBox="0 0 660 400" role="img" aria-labelledby="ry-venn-title ry-venn-desc" xmlns="http://www.w3.org/2000/svg" style="width:100%;max-width:660px;height:auto;font-family:inherit">
  <title id="ry-venn-title">R, ry's additions, and strict mode</title>
  <desc id="ry-venn-desc">Two overlapping circles. The left circle is R: untyped code, S4, R6 and eval run but are not checked. The right circle is what ry adds: records, tags, frames, the standard library and inline types, which run on ry. The overlap is typed R written with #: comments, which is checked and also runs on GNU R. Both circles together are R++, everything ry runs. The right circle alone is strict mode, R−−, where only typed code is allowed.</desc>
  <path d="M 70 50 V 40 H 590 V 50" stroke="currentColor" stroke-opacity="0.45" fill="none"/>
  <text x="330" y="30" text-anchor="middle" fill="currentColor" font-size="15" font-weight="600">R++: everything ry runs</text>
  <circle cx="245" cy="210" r="150" fill="#3b82f6" fill-opacity="0.16" stroke="#3b82f6" stroke-width="2"/>
  <circle cx="415" cy="210" r="150" fill="#a855f7" fill-opacity="0.16" stroke="#a855f7" stroke-width="2" stroke-dasharray="7 5"/>
  <g fill="currentColor" text-anchor="middle">
    <text x="175" y="165" font-size="22" font-weight="700">R</text>
    <text x="175" y="198" font-size="13">untyped code</text>
    <text x="175" y="217" font-size="13">S4, R6, eval</text>
    <text x="175" y="246" font-size="12" fill-opacity="0.7">runs, not checked</text>
    <text x="330" y="165" font-size="15" font-weight="700">typed R</text>
    <text x="330" y="198" font-size="13" font-family="ui-monospace, monospace">#: comments</text>
    <text x="330" y="246" font-size="12" fill-opacity="0.7">checked, and</text>
    <text x="330" y="262" font-size="12" fill-opacity="0.7">runs on GNU R</text>
    <text x="485" y="165" font-size="17" font-weight="700">ry additions</text>
    <text x="485" y="198" font-size="13">records, tags</text>
    <text x="485" y="217" font-size="13">frames, stdlib</text>
    <text x="485" y="236" font-size="13">inline types</text>
    <text x="485" y="262" font-size="12" fill-opacity="0.7">runs on ry</text>
  </g>
  <text x="485" y="385" text-anchor="middle" fill="currentColor" font-size="15" font-weight="600">strict mode (R−−): only this circle</text>
</svg>
</figure>

## From R to typed ry in four steps

Each step is optional, and you can stop after any of them. The docs follow the same four steps.

### 1. It works on your code today

Run `ry check` in a project, with no configuration and nothing to annotate. It reads the source
without running it, so it reports mistakes that R reports only when the line runs:

```r
# R/pricing.R
net_price <- function(price, rate) {
  price * (1 - ratee)
}
```

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/pricing.R:2:16]
 1 | net_price <- function(price, rate) {
 2 |   price * (1 - ratee)
   |                ^^^^^
 3 | }
```

The same analysis drives [your editor](/guides/editors), and `ry fmt`, `ry load` and `ry repl`
cover formatting, loading a package from its sources, and the console.

### 2. Type your code

Set `typing = true` under `[check]` in `ry.toml`, and ry reports type errors. It infers types from
how values are used, and a `#:` comment states the type where you want it fixed:

```r
#: fn(price: double, rate: double) -> double
net_price <- function(price, rate) {
  price * (1 - rate)
}

net_price(100, "0.2")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[R/pricing.R:6:16]
 5 |
 6 | net_price(100, "0.2")
   |                ^^^^^
```

The file is still plain R, so GNU R, `Rscript`, RStudio and every other R tool run it unchanged. If
you want to stay on GNU R, this is where you stop.

### 3. Switch to inline types

Inline annotations write the same types in the code itself, and the checker reports the same
`type-mismatch` for `net_price(100, "0.2")`. They also check at run time, which matters for a value
whose type only shows up when the script runs:

```r
net_price <- function(price: double, rate: double) -> double {
  price * (1 - rate)
}

rate <- eval(parse(text = '"0.2"'))
net_price(100, rate)
```

On ry's runtime, the call fails at the boundary and names the argument:

```text
$ ry run --runtime ry pricing.R
Error in net_price(100, rate) :
  argument `rate` must be `double`, not `character`
```

GNU R fails later, inside the body: `Error in 1 - rate : non-numeric argument to binary operator`.
`ry run` without `--runtime ry` runs the file on GNU R with the annotations erased, but plain
`Rscript` cannot parse them, so a file with inline types needs ry.

### 4. Move to ry's types

R models data with lists, S4, R6 or Reference Classes, which no checker can follow far. ry adds
[records, tuples and tagged unions](/type-checking/records-and-tags) with a `match` that knows every
case, a built-in [table type](/type-checking/frames) whose columns the checker knows, and a
[standard library](/reference/standard-library) of `Result`, `List`, `Dict` and friends:

```r
alias Payment = :Paid(amount: double) | :Refunded(amount: double) | :Pending

balance <- function(payment: Payment) -> double {
  match (payment) {
    :Paid(amount = a) => a
    :Refunded(amount = a) => -a
  }
}
```

```text
non-exhaustive-match

  x this `match` does not handle `:Pending`: add an arm for it, or a catch-all `_ => ...` arm
   --[R/payments.R:4:3]
 3 | balance <- function(payment: Payment) -> double {
 4 |   match (payment) {
   |   ^^^^^^^^^^^^^^^
```

[Strict mode](/type-checking/strict-mode) (`strict = true`) finishes the move: a value ry cannot
type, or a parameter nothing constrains, is an error, and `ry run` refuses to run code that has one.
`object-systems = false` additionally rejects new S4, R6 and Reference Classes:

```text
strict

  x strict mode: parameter `x` has no type. How the function uses it does not determine one, so any value is accepted unchecked. Annotate it, as in `x: TYPE`

object-system

  x R6 classes are turned off in this project (`[check] object-systems = false`), so `R6Class` cannot define an R6 class. Use a record or a tagged union instead
```

## Speed

BENCHMARK

## What it leaves out

Every rule in the type system has to be cheap enough to run on each keystroke, so some things are
left out on purpose:

- **Several signatures for one of your functions.** That would turn every call into a search. A
  parameter can accept either of two types instead, written `integer | character`.
- **Inheritance.** One type extending another makes inference slow and its results hard to predict.
  [Custom types](/type-checking/custom-types) keep values apart without it.
- **What it cannot model.** S4 and R6 objects and data frame columns get a type that is compatible
  with everything, so they never cause a false error, but they are not checked either. Strict mode
  shows where that happens, and [limitations](/type-checking/limitations) lists the rest.

## Status

ry is in beta. The checker, language server, formatter and loader are ready for daily use, and
diagnostic codes, `ry.toml` keys and the JSON output are stable, so CI built on them keeps working.
The type system is still growing, so a new release can report findings an older one did not, which
is why CI should pin the version.

ry's own runtime is under construction. It still reads base R from your GNU R installation, and a
script that uses something it lacks stops with an error that names it. Records, tags, frames and the
standard library run only on it, so for production work today, run plain R and typed R on GNU R.
