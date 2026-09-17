---
title: Features
description: What ry gives you with no configuration, and the one setting that turns on type errors
---

Everything below works with no configuration and no annotations. The last section needs one line of
configuration.

## Navigation and completion

ry is a standard language server, so go-to-definition, find-references, completion and hover work
in VS Code, Zed, Neovim and Helix alike. It reads your whole project, so a name defined in one file
completes in another:

```r
# R/utils.R
normalise_region <- function(x) tolower(trimws(x))
```

```r
# R/report.R
normalise_re      # completes to normalise_region
```

## Syntax errors

ry parses R with its own hand-written parser rather than calling R or reusing a grammar. That
is what makes these messages possible. Forget a comma in a list:

```r
config <- list(
  title = "Revenue"
  subtitle = "by quarter",
  width = 800
)
```

R reports the token it could not accept:

```text
Error: unexpected symbol in:
"  title = "Revenue"
  subtitle"
```

ry reports what is missing, and points at the place it should go:

```text
syntax-error

  x missing `,` between these arguments
   --[a.R:2:20]
 1 | config <- list(
 2 |   title = "Revenue"
   |                    ^
 3 |   subtitle = "by quarter",
 4 |   width = 800
```

The same comparison with a parenthesis left open:

```r
if (x > 1 {
  y
}
```

```text
Error: unexpected '{' in "if (x > 1 {"
```

```text
syntax-error

  x unclosed `(` in the `if` condition; expected a matching `)`
   --[a.R:1:4]
 1 | if (x > 1 {
   |    ^
 2 |   y
```

R's parser stops at the first error, because its job is to run your code. A parser written for
tooling carries on, keeps the rest of the file analyzable, and can say which construct was left
open.

## Formatting

`ry fmt` fixes spacing, indentation and bracing:

```r
x<-c(1,2,3)
if(x>1){y<-2}
```

```r
x <- c(1, 2, 3)
if (x > 1) { y <- 2 }
```

It does not decide where your line breaks go. Both of these are already formatted, and both stay
exactly as written:

```r
totals <- summarise(data, by = region)

breakdown <- summarise(
  data,
  by = region
)
```

You chose one call on a line and the other spread over four; the formatter keeps both. A formatter
that reflows to a column limit would rewrite one of them, and the next diff would show line breaks
instead of the change you made. The style is fixed apart from indent width and line endings; the
reasoning is in [formatting rules](/reference/formatting-rules#philosophy).

## Rename

Rename does not search and replace. It runs the same analysis that answers go-to-definition: every
occurrence is resolved to the binding it refers to, and only the occurrences bound to the one you
picked are edited.

This matters because the same word is not the same thing twice. A local `total` inside one function
and a global `total` used by another are different bindings. A parameter shadows the global of the
same name for the length of the function. The word appearing inside a string is not a binding at
all. Renaming one of them must leave the others alone, across every file in the project.

## The type checker

Everything above comes from one model of your project: which names exist, which binding each
occurrence refers to, and where each is visible. That model is what separates these features from
text search, and building it is most of the work.

The type checker goes one step further. It knows not only which binding a name refers to, but what
kind of value that binding holds. Completion therefore knows what is inside a value, not just which
names exist:

```r
account <- list(holder = "ada", balance = 120.5)
account$        # balance, holder — with their types
```

Nothing declared those fields. Hovering a function you never annotated gives you its full type:

```r
make <- function(x) list(x, 1L)
```

```text
make: <T> fn(x: T) -> list{T, integer}
```

It takes a value of any type `T` and returns a two-element list of that value and an integer.
Inference produced that, and it runs whether or not type errors are reported.

Reporting the places where those types disagree is what is off by default:

```toml
# ry.toml
[check]
typing = true
```

```r
parse_count <- function(raw) {
  count <- 0L
  if (raw == "unknown") {
    count <- "?"
  }
  count + 1L
}

print(parse_count("12"))
```

```text
type-mismatch

  x expected a numeric value (`integer` or `double`), found `integer | character`
   --[parse.R:6:3]
 5 |   }
 6 |   count + 1L
   |   ^^^^^
 7 | }
```

That took one line of configuration, no annotations, and no change to the code.

Type checking a whole project is affordable because analysis is incremental: an edit re-checks only
what that edit could have affected, not the project. ry is tested against roughly 970,000 lines of
real R — 69 CRAN packages plus R's base library.

- [Tutorial](/type-checking/tutorial) — put it on real code
- [Concepts](/type-checking/concepts) — how it works out what it knows
- [Adopting an existing codebase](/guides/adopting) — turning it on a piece at a time
