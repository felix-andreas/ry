---
title: Why ry
description: Why R needs one fast toolchain with a type checker at its core, and how far along this one is
---

R has good tools, but they are separate tools. Linting, formatting, style checks, analysis, and
running the code are five programs, and each one parses your source and builds its own partial
picture of it. None of them shares what it learned with the others, so each one starts from
scratch. ry does all of these jobs in one tool, and they all read your code the same way.

## Answers without running your code

R's language servers know what your values are because they ask a live R session. That is what makes
completion on a fitted model work in RStudio.

It is also the limit. A session knows only the code that has already run, in whatever state it
happens to be in, so it cannot describe the branch you have not taken, the function nobody has
called yet, or the file you just opened. ry answers from the source alone, which means its answers
are there in a pull request, in CI, and in a file you have never run:

- a typo in a variable name
- a name you deleted in another file
- an argument in the wrong position, or a call missing a required one
- a value that is sometimes `NULL`, used as though it never is

The first two are reported out of the box. The last two need the type checker, which is one line in
`ry.toml`.

## Speed on large codebases

Latency matters most on large R projects, where a check slow enough to interrupt your work stops
being run at all.

ry is written in Rust, and its analysis is incremental: an edit re-checks only what that edit could
have affected, not the whole project. It is tested against roughly 970,000 lines of real R: 69 CRAN
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

`*` is arithmetic, so both parameters must be numbers. Nothing was declared, yet `scale("a", 2)` is
already an error. That is why most R needs no annotations at all. The ones you do write live in `#:`
comments, so the file stays ordinary R that every other tool can read. And type checking is opt-in,
so you can adopt it one file at a time.

Two things R programmers might expect are missing: you cannot define a class hierarchy, and you
cannot give one of your own functions several signatures. Either would make a single expression slow
enough to check that an editor could no longer keep up.

Some R constructs cannot be described statically at all. Those become `Unknown`, which is compatible
with everything, so a gap means a check was skipped rather than a wrong answer produced.

## Project status

ry is beta software.

**The interfaces are stable.** The diagnostic wording, the diagnostic codes, the `ry.toml` keys,
and the JSON output will not change under you, so CI you build on them keeps working.

**The type system is still gaining capability.** A new release may report findings an older one did
not, so pin a version if you gate a build on a clean run.

**Where it runs.** `ry check` reads `.R` files and the R chunks of `.Rmd`, `.qmd`, and `.Rnw`
documents. The editor integration does not handle literate documents yet: the language server would
read the whole file as R and report the prose, so do not point it at one. The formatter skips them,
since most of an `.Rmd` is prose it should not rewrite.

**What it does not cover yet.** The largest gaps are data frames, S4, and R6. See
[limitations](/type-checking/limitations) for the full account before deciding how far to trust a
clean run.

## Next

- [Features](/features) lists what you get before you turn anything on
- [Tutorial](/type-checking/tutorial) puts the type checker on real code
