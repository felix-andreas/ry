---
title: Why ry
description: Why R needs one fast toolchain with a type checker at its core, and how far along this one is
---

R has good tools, but they are separate tools. Linting, formatting, style, analysis and running the
code are five programs that each parse your source and each build their own partial picture of it.
None of them shares what it learned with the others, so each one starts over.

## Answers without running your code

R's language servers know what your values are because they ask a live R session. That is what makes
completion on a fitted model work in RStudio.

It is also the limit: a session knows only code that has already run, in the state it happens to be
in. It cannot describe the branch you have not taken, the function nobody called, or the file you
just opened. ry answers from the source alone, so the answers exist in a pull request, in CI, and
in a file you have never run:

- a typo in a variable name
- a name you deleted in another file
- an argument in the wrong position, or a call missing a required one
- a value that is sometimes `NULL`, used as though it never is

The first two are reported out of the box. The last two need the type checker, which is one line in
`ry.toml`. Formatting, analysis, editor features and type checking all read your code the same way.

## Speed on large codebases

Latency matters most on large R projects, where a check slow enough to interrupt your work stops
being run at all.

ry is written in Rust, and analysis is incremental: an edit re-checks only what that edit could
have affected, not the project. It is tested against roughly 970,000 lines of real R, which is 69 CRAN
packages plus R's own base library.

`check` and `fmt` never load R and never execute your code, which is what makes them safe in CI and
fast in an editor. The one exception is the [R console](/guides/r-console), which runs R by
definition.

## Types in dynamic languages

Python has type hints, JavaScript got TypeScript, Ruby has RBS, Elixir is adding set-theoretic types.
Each stayed dynamic, kept types optional, and adopted them because finding type errors by running the
program stops scaling long before the codebase does.

R code is full of implicit type expectations, and nothing checks them until the code runs.

ry's approach rests on **inference**: the checker works out types from how values are used,
instead of requiring you to declare them.

```r
scale <- function(x, factor) x * factor
```

`*` is arithmetic, so both parameters are numbers. Nothing was declared, and `scale("a", 2)` is
already an error. This is why most R needs no annotations at all. The ones you do write live in
`#:` comments, so the file stays ordinary R that every other tool reads, and type checking is
opt-in, so you can adopt it one file at a time.

Two things R programmers expect are missing. You cannot define a class hierarchy, and you cannot
give one of your own functions several signatures. Both would make a single expression slow enough
to check that an editor could not keep up.

Some R constructs cannot be described statically at all. Those become `Unknown`, which is compatible
with everything, so a gap means a check was skipped rather than a wrong answer produced.

## Project status

ry is beta software.

**The interfaces are stable.** The diagnostic wording, the diagnostic codes, the `ry.toml` keys,
and the JSON output will not change under you, so CI built on them keeps working.

**The type system is still gaining capability.** A new release may report findings an older one did
not, so pin a version if you gate a build on a clean run.

**Where it runs.** `ry check` reads `.R` files and the R chunks of `.Rmd`, `.qmd`, and `.Rnw`
documents. The editor integration does not handle literate documents yet. Do not point the language
server at one, because it reads the whole file as R and reports the prose. The formatter skips them,
since most of an `.Rmd` is prose the formatter should not rewrite.

**What it does not cover yet.** The largest gaps are data frames, S4, and R6. See
[limitations](/type-checking/limitations) for the full account before deciding how far to trust a
clean run.

## Next

- [Features](/features) lists what you get before you turn anything on
- [Tutorial](/type-checking/tutorial) puts the type checker on real code
