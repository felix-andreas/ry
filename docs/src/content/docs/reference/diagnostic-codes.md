---
title: Diagnostic codes
description: What each finding means, why it is reported, and how to silence it
---

```text
unresolved

  ! I could not resolve `validte_url` in this package, its imports, or builtins. Did you mean `validate_url`?
   --[R/a.R:2:22]
 1 | validate_url <- function(x) TRUE
 2 | check <- function(x) validte_url(x)
   |                      ^^^^^^^^^^^
```

Every finding starts with its code, here `unresolved`. The code stays the same when the wording of
a message changes, so it is what you name in a suppression, in `ry.toml`, and in the JSON output.
`!` marks a warning and `x` an error (`⚠` and `×` in a terminal). The carets underline exactly what the finding is about, such
as one name, not the whole statement.

## Suppressing a finding

```r
total = 1L  # ry: allow(assignment-operator)

# ry: allow(unused, naming-style)
scratchValue <- 1L
```

A suppression covers its own line and the line below, so it works at the end of a line or on the line
above. `allow(all)` covers every code. There is no block or file scope, because a suppression marks
one exception you have reviewed and should not hide findings in code written later. To turn a code
off everywhere, use [`ry.toml`](/reference/configuration), and to turn type checking off for one
file, put `# typing: off` in it.

The marker is found by scanning the line for its first `#`, so `#ry:allow(x)` and
`# roughly: allow(x)` work too, but a `#` inside a string earlier on the line hides it. Findings on
`NAMESPACE`, `ry.toml`, and stub files cannot be suppressed, because they have no R line to carry the
comment.

Some R idioms silence findings on their own:

- `utils::globalVariables(c("a", "b"))` at the top level stops `a` and `b` from being reported as
  unresolved, as it does for `R CMD check`.
- In a file that calls `R6Class()`, `self`, `private`, and `super` resolve.
- A name created with `<<-` anywhere in a file resolves in that file.
- A name that starts with `.` or `_` is never reported as unused.

## On by default

**`syntax-error`** (error): code R would refuse to parse, or an assignment target R would refuse to
run, such as `1 + a <- 2`. A parse error names what is missing, stays on the line that caused it, and
hides every other finding in the broken statement except strict-mode ones, because ry draws no
conclusions from code it could not read.

**`annotation`** (error): a `#:` comment that is malformed, names a type that does not exist, or
annotates nothing, and an unknown `# typing:` value. It is reported even in files with type checking
off, because a broken annotation is a mistake either way.

**`unresolved`** (warning): a name that is defined nowhere ry can see, with a suggestion for a near
miss. It also covers `pkg::name` where `pkg` does not export `name`, and an unknown package.
`pkg:::name` is exempt, because it reaches internal names on purpose. In `NAMESPACE`, an
`importFrom` of a name the package does not export and an `export` of a name you never define are
errors, because R refuses to load such a package. Attaching a package ry knows nothing about turns
off this check for bare names, as [stubs](/type-checking/stubs) explains.

**`unused`** (warning): an assignment nothing reads. In a package, top-level definitions are exempt,
because another file may use them.

**`duplicate`** (warning): a top-level name defined in more than one place in a package, where R
silently keeps whichever is collated last. Both definitions are reported.

**`assignment-operator`** (warning): `=` used for assignment, where `<-` says unambiguously that the
line assigns rather than passes an argument.

**`boolean-shorthand`** (warning): `T` or `F`, which are ordinary variables that any code can
reassign, unlike `TRUE` and `FALSE`.

**`trailing-comma`** (error): a comma after a call's last argument. R reads it as an empty argument,
so `c(1, 2, )` fails when it runs.

**`stub`** (error): a line in your `stubs/*.Rtypes` files that ry could not load, so a broken
declaration is never dropped in silence.

**`config`** (error): invalid TOML or a value of the wrong type in `ry.toml`.

## Opt-in

**`type-mismatch`** (error), with `[check] typing = true` or `# typing: on`: a value of the wrong
type, a call with the wrong arguments, a field that does not exist, and every other rule in the
[type system](/reference/type-system).

**`strict`** (error), with `[check] strict = true` or `# typing: strict`: each place where a value's
type could not be determined, and each name that resolves only because an unknown package was
attached. It also raises `unresolved` from a warning to an error, so a gate on
`--min-severity error` counts it.

**`maybe-undefined`** (warning), with `[check] maybe-undefined = true`: a read that some path reaches
before any write, which R reports as `object 'v' not found` on that path. It is off because it treats
conditions that always agree as independent, so `if (ok) v <- 1` followed by `if (ok) print(v)` is
reported although it is safe.

**`naming-style`** (warning), with `[lint] naming-style = "snake_case"` or `"camelCase"`: a variable
or parameter in the other style. `SCREAMING_SNAKE_CASE` always conforms.

**`unused-parameter`** (off by default): a parameter the body never reads. S3 generics and methods
are exempt, because the generic dictates their parameters.

**`unused-import`** (off by default): an `importFrom` in `NAMESPACE` whose name your code never
mentions. Only `ry check` reports it.

**`shadows-builtin`** and **`shadows-namespace`** (off by default): a top-level name that hides a
function from `base`, or from another package, such as defining your own `filter` while
`stats::filter` exists.

These four take a level in `[lint]`, such as `"warn"` or `"error"`.
