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
`!` marks a warning and `x` an error (`⚠` and `×` in a terminal). The carets underline exactly
what the finding is about, such as one name, not the whole statement.

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

## Fixes

Some findings carry a mechanical fix, which the editor offers as a quick fix and `ry check --fix`
applies. A fix is **safe** when it keeps what the code means, so `--fix`, the editor's fix shortcut
and its "Apply all safe fixes" action apply it without asking. A fix is **unsafe** when it guesses
what you meant or silences a finding without handling it, so it needs `--unsafe-fixes` or a click.

| Code | Fix | Applicability |
| --- | --- | --- |
| `assignment-operator` | `=` to `<-` | safe |
| `boolean-shorthand` | `T` to `TRUE`, `F` to `FALSE` | safe |
| `trailing-comma` | remove the comma | safe |
| `redundant-return` | `return(x)` to `x` | safe |
| `any-is-na` | `any(is.na(x))` to `anyNA(x)` | safe |
| `paste-sep` | `paste(..., sep = "")` to `paste0(...)`, when no comment sits inside the call | safe |
| `literal-coercion` | `as.integer(1)` to `1L`, and the like for other literals | safe |
| `single-quotes` | `'text'` to `"text"`, when the text holds no `"` or `\'` | safe |
| `comparison-negation` | `!(x == y)` to `x != y`, and the like for other comparisons | safe |
| `outer-negation` | `all(!x)` to `!any(x)`, `any(!x)` to `!all(x)` | safe |
| `which-grepl` | `which(grepl(...))` to `grep(...)` | safe |
| `length-levels` | `length(levels(x))` to `nlevels(x)` | safe |
| `numeric-leading-zero` | `.5` to `0.5` | safe |
| `while-true` | `while (TRUE)` to `repeat`, when no comment sits in the header | safe |
| `empty-assignment` | `x <- {}` to `x <- NULL` | safe |
| `unnecessary-concatenation` | `c("a")` to `"a"`, `c()` to `NULL`, when no comment sits inside the call | safe |
| `testthat-expectation` | `expect_equal(x, NULL)` and `expect_true(is.null(x))` to `expect_null(x)` | safe |
| `testthat-expectation` | `expect_equal(x, TRUE)` to `expect_true(x)`, `expect_true(!x)` to `expect_false(x)`, `expect_equal(length(x), n)` to `expect_length(x, n)`, `expect_equal(names(x), n)` to `expect_named(x, n)`, `expect_true(inherits(x, "a"))` to `expect_s3_class(x, "a")` | unsafe |
| `condition-message` | `stop(paste0(a, b))` to `stop(a, b)` | unsafe |
| `fixed-regex` | add `fixed = TRUE` | unsafe |
| `lengths` | `sapply(x, length)` to `lengths(x)` | unsafe |
| `redundant-equals` | `x == TRUE` to `x`, `x == FALSE` to `!x` | unsafe |
| `equals-na` | `x == NA` to `is.na(x)`, `x != NA` to `!is.na(x)` | unsafe |
| `vector-logic` | `&` to `&&`, `|` to `||` | unsafe |
| `length-sequence` | `1:length(x)` to `seq_along(x)`, `1:nrow(x)` to `seq_len(nrow(x))` | unsafe |
| `class-equals` | `class(x) == "a"` to `inherits(x, "a")` | unsafe |
| `length-test` | `length(x == y)` to `length(x) == y` | unsafe |
| `scalar-in` | `x %in% "a"` to `x == "a"` | unsafe |
| `order-subset` | `x[order(x)]` to `sort(x, na.last = TRUE)` | unsafe |
| `matrix-apply` | `apply(x, 1, sum)` to `rowSums(x)`, and `colSums`, `rowMeans`, `colMeans` alike | unsafe |
| `unnecessary-lambda` | `lapply(x, function(y) f(y))` to `lapply(x, f)` | unsafe |
| `yoda-test` | `expect_equal(2, x)` to `expect_equal(x, 2)` | unsafe |
| `nzchar` | `nchar(x) == 0` to `!nzchar(x)`, `nchar(x) > 0` and `!= 0` to `nzchar(x)` | unsafe |
| `sample-int` | `sample(1:n, k)` to `sample.int(n, k)` | unsafe |
| `annotation-mismatch` | change the `#:` comment to the inline type, when ry uses the inline one and it names no type parameter | safe |
| `unreachable-pattern` | remove the arm, when it has its lines to itself | safe |
| `unresolved`, `type-mismatch`, `annotation` | change a misspelled name, argument, field, column or type to the suggestion | unsafe |
| `non-exhaustive-match` | add an arm per missing case that calls `stop("not handled yet")` | unsafe |

## On by default

**`syntax-error`** (error): code R would refuse to parse, or an assignment target R would refuse to
run, such as `1 + a <- 2`. A parse error names what is missing, stays on the line that caused it, and
hides every other finding in the broken statement, because ry draws no conclusions from code it
could not read. Strict mode can still report an undetermined type there.

**`annotation`** (error): a `#:` comment that is malformed, names a type that does not exist, or
annotates nothing, an inline annotation whose type names something that does not exist or is
malformed in the same ways, and an unknown `# typing:` value. It is reported even in files with type
checking off, because a broken annotation is a mistake either way. A mistake in an inline type's
grammar is a `syntax-error` instead, because it breaks the R statement it sits in.

**`annotation-mismatch`** (warning): a statement whose inline annotation and `#:` comment give the
same slot (the value, a parameter, the rest parameter, or the return) different types. It points at
both types and says which one ry uses, so a stale comment cannot silently contradict the code.

**`unresolved`** (warning): a name that is defined nowhere ry can see, with a suggestion for a near
miss. It also covers `pkg::name` where `pkg` does not export `name`, and an unknown package.
`pkg:::name` is exempt, because it reaches internal names on purpose. Inside a frame query such as
`f[amuont > 1]`, a bare name that is neither a column of `f` nor defined anywhere is reported the
same way, with the closest column as the suggestion. In `NAMESPACE`, an
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
reassign, unlike `TRUE` and `FALSE`. A file that binds `T` or `F` itself, even as a parameter, gets
no finding for that name, because there it may mean the binding.

**`trailing-comma`** (warning): a comma after a call's last argument. R reads it as an empty
argument, which fails only when the callee evaluates it: `c(1, 2, )` stops with "argument 3 is
empty", while `ngettext(2, "a", "b", )` runs, because the empty argument leaves `domain` at its
default.

**`equals-na`** (warning): a comparison with `NA`, such as `x == NA`, which gives `NA` whatever `x`
holds, so it never tests whether a value is missing. The fix to `is.na(x)` is unsafe because the
result changes from `NA` to `TRUE` or `FALSE`, which is the point but changes what the code does.

**`vector-logic`** (warning): `&` or `|` in an `if` or `while` condition, where `&&` and `||` skip the right-hand side once the left decides the answer, and (since R 4.3)
stop with an error on a vector longer than one instead of looking at its first element. The fix
is unsafe because the right-hand side may no longer run. Inside a call such as `any(x & y)` the
operator is left alone, because there it works on vectors on purpose.

**`length-sequence`** (warning): `1:length(x)`, `1:nrow(x)`, `1:ncol(x)`, `1:NROW(x)` or
`1:NCOL(x)`, and the same counts in `seq(length(x))`. When the length is zero, `1:0` is `c(1, 0)`, so a loop over it runs twice instead of
not at all. The fix to `seq_along(x)` or `seq_len(nrow(x))` is unsafe because it changes what an
empty input does.

**`class-equals`** (warning): `class(x) == "name"` or `!=`, which misses an object whose class
vector has several entries: a tibble's class is `c("tbl_df", "tbl", "data.frame")`, so the
comparison gives three values, and since R 4.2 an `if` on it stops with an error. The fix to `inherits(x, "name")` is unsafe because it changes the result for such
objects.

**`unreachable-code`** (warning): statements after `return()`, `stop()`, `break` or `next` in the
same block, which never run. The range covers every statement from the first unreachable one to
the end of the block. A file that defines its own `return` or `stop` gets no finding for it, and
neither does a block inside `quote()`, `bquote()`, `substitute()` or `expression()`.

**`sprintf-arguments`** (warning): a `sprintf()` or `gettextf()` call whose literal format wants a
different number of values than the call passes. Too few values stop R with "too few arguments",
and an extra value is dropped with a warning. `%%` consumes nothing and `*` in a width or precision
consumes a value. A format with `%1$s` positions, a format that is not a literal, a `...` argument,
or a named argument other than `fmt` turns the check off for that call.

```text
! This format wants 2 values but sprintf() gets 1 value, so R stops with "too few arguments"
```

**`length-test`** (warning): a comparison inside `length()`, such as `length(x == 0)`. It counts
the comparison's elements, which is `length(x)` when the right side is one value, so the comparison
was meant for the length. The fix moves it outside, and is unsafe because it changes the result.

```text
! `length(x == 0)` is the length of the comparison, not a test of the length. Did you mean `length(x) == 0`?
```

**`object-system`** (warning): a definition from S4, Reference Classes or R6, which is a call to
`setClass`, `setGeneric`, `setMethod`, `setValidity`, `setRefClass` or `R6Class`. That code still
runs, but ry checks [records and tagged unions](/type-checking/records-and-tags) statically and
cannot see inside these classes, so it recommends those instead:

```text
! `setClass` defines an S4 class. ry recommends a record or a tagged union instead
```

With `[check] object-systems = false`, each definition is an error instead, whatever the `[lint]`
level says:

```text
x S4 classes are turned off in this project (`[check] object-systems = false`), so `setClass` cannot define an S4 class. Use a record or a tagged union instead
```

**`stub`** (error): a line in your `stubs/*.Rtypes` files that ry could not load, so a broken
declaration is never dropped in silence.

**`inline-annotation`** (warning), while `[check] inline-annotations = false`: an inline annotation,
which then types nothing, and which R itself cannot run.

**`ry-syntax`** (warning), while `[check] inline-annotations = false`: a record, tuple, tag or
`match`, which R cannot run either. The value is still typed, but a `match` is not checked for
missing or unreachable cases.

```text
! this record is ry syntax that R cannot run, and inline annotations are turned off (`[check] inline-annotations = false`) to keep code runnable by R. Rewrite it in plain R, or turn inline annotations back on
```

**`config`** (error): invalid TOML or a value of the wrong type in `ry.toml`. The language server
marks the line in `ry.toml`, and `ry check` prints the finding and exits with status 2 without
checking any R.

## Opt-in

**`type-mismatch`** (error), with `[check] typing = true` or `# typing: on`: a value of the wrong
type, a call with the wrong arguments, a field or frame column that does not exist, a join of columns
that cannot be compared, a `:=` that would convert a column's values with loss, and every other rule in the
[type system](/reference/type-system).

**`non-exhaustive-match`** (error), with typing on: a `match` that some value of the scrutinee's
type reaches without a matching arm. The message names up to three missing cases as patterns, and
the range covers `match (x)`. An arm with a guard does not count toward coverage, and a scrutinee
of unknown type is not checked.

```text
x this `match` does not handle `:Empty`: add an arm for it, or a catch-all `_ => ...` arm
```

**`unreachable-pattern`** (warning), with typing on: an arm whose pattern the unguarded arms above
it already cover, so it never runs.

```text
! this arm is unreachable: the arms above it already match every value its pattern matches
```

**`strict`** (error), with `[check] strict = true` or `# typing: strict`: each place where a value's
type could not be determined, each parameter nothing constrains, and each name that resolves only
because an unknown package was attached. In a file under `R/`, it also reports each top-level
statement that does more than define something, on the call, loop or assignment that has the effect,
and says where the code belongs ([a package file's top level](/reference/type-system#a-package-files-top-level)):

```text
x strict mode: this calls `cat`, which is not declared pure, so it runs every time the package loads. A file in `R/` may only define things at its top level: move the call into a function, `.onLoad`, or the script `ry run` starts
```

It also raises `unresolved` from a warning to an error, so a gate on `--min-severity error` counts it.

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

**`data-table`** (off by default): a call that creates a data.table, which is `data.table()`,
`fread()`, `as.data.table()` or `setDT()`, bare or as `data.table::`. data.table code runs
unchanged, so this only helps you find the code to move to the typed
[DataFrame](/guides/from-data-table/) when you choose to. A file that defines its own function of
that name is not flagged.

```text
! `fread` creates a data.table. Its typed equivalent is `DataFrame.read_csv()`, see https://ry-lang.org/guides/from-data-table/
```

**`redundant-return`** (off by default): `return(x)` as a function's last expression, including
both branches of a trailing `if`, where `x` alone returns the same value. A file that defines its
own `return` gets no finding.

**`commented-code`** (off by default): a comment that parses as R and calls, assigns, defines,
branches, loops or indexes, such as `# x <- compute(y)`. Prose rarely parses that way, and roxygen
(`#'`), plumber (`#*`), Quarto (`#|`), `#!` and `#>` output comments are skipped. Delete the code
instead: version control keeps it.

**`duplicate-argument`** (off by default): the same argument name twice in one call, which stops a
function with "formal argument matched by multiple actual arguments". It is off because a function
that collects `...` keeps both values on purpose, such as cli's `c("i" = ..., "i" = ...)` bullets or
two HTML `class` attributes, and the lint cannot see which functions do. With type checking on,
a call that gives a known function's parameter twice is a `type-mismatch` anyway, such as
`paste(sep = "", sep = "-")`. `mutate()`, `transmute()`,
`summarise()`, `summarize()` and `reframe()` are exempt, because there a repeated name updates a
column.

**`any-is-na`**, **`paste-sep`**, **`lengths`**, **`literal-coercion`**, **`redundant-equals`** and
**`single-quotes`** (off by default): spellings with a shorter or faster equivalent, listed with
their fixes under [Fixes](#fixes). `redundant-equals` and `lengths` have unsafe fixes, because
`1 == TRUE` is `TRUE` and `sapply()` on an empty list returns a list, not an integer vector.

**`testthat-expectation`** (off by default): a general testthat expectation where a specific one
exists, such as `expect_equal(x, NULL)` for `expect_null(x)`. The specific one fails with a message
that says what went wrong. Most fixes are unsafe because the specific expectation accepts different
values: `expect_false(x)` passes only for `FALSE`, while `expect_true(!x)` also passes for `0`.

**`condition-message`** (off by default): `paste0()`, or `paste()` with `sep = ""`, inside `stop()`,
`warning()`, `message()` or `packageStartupMessage()`, which join their arguments themselves. R
translates a condition message argument by argument, so a pasted message cannot be translated. The
fix is unsafe because `paste0()` recycles vectors element by element where `stop()` joins them end
to end.

**`fixed-regex`** (off by default): a literal pattern with no regular expression syntax in `grepl()`,
`grep()`, `sub()`, `gsub()`, `regexpr()`, `gregexpr()`, `regexec()` or `strsplit()`, which
`fixed = TRUE` matches faster. Calls that already set `fixed`, `perl` or `ignore.case` are skipped,
and so are `sub()` and `gsub()` with a replacement that may hold a backreference. The fix is unsafe
because R can mark the encoding of a fixed match's result differently.

**`browser-call`** (off by default): a `browser()` call, which pauses the code whenever it runs
interactively. A call inside `quote()`, `bquote()`, `substitute()` or `expression()` is skipped,
because there it is code built on purpose.

**`line-length`** (off by default): a line longer than `[lint] max-line-length` characters, 80
unless you set it. The formatter keeps your line breaks and never splits a line, so this lint is
what keeps lines short. It counts characters, not bytes, and reports from the first character past
the limit.

**`implicit-assignment`** (off by default): `<-`, `<<-` or `->` inside an `if` or `while`
condition, such as `if (x <- f())`, where an assignment reads like a comparison. A function defined
inside the condition is skipped. `while ((line <- next_line()) != "")` is a common R idiom, which is
why the lint is opt-in.

**`scalar-in`**, **`order-subset`**, **`matrix-apply`** and **`unnecessary-lambda`** (off by
default): code with a simpler or faster equivalent whose fix can change the result in an edge
case, so each fix is unsafe. `x %in% "a"` is `FALSE` for an `NA` where `x == "a"` is `NA`.
`x[order(x)]` dispatches like `[` where `sort()` dispatches on its own methods. `apply()` turns a
data frame into a matrix before `rowSums()` would, and a function passed by name receives the extra
arguments an apply function forwards, where an anonymous one with a single parameter would reject
them. `matrix-apply` covers `sum` and `mean` over margin 1 or 2, with an optional `na.rm`, and
`unnecessary-lambda` covers `lapply()`, `sapply()`, `vapply()`, `Filter()`, `Map()` and purrr's
`map()` family, `walk()`, `keep()` and `discard()`.

**`yoda-test`** (off by default): `expect_equal()` or `expect_identical()` with a constant first and
the value under test second. testthat reports the first argument as the actual value, so a failure
message reads backwards. The fix swaps them, and is unsafe because `expect_equal()` measures a
numeric tolerance relative to one of its two arguments.

**`nzchar`** and **`sample-int`** (off by default): `nchar(x) == 0`, which counts every string
only to test whether it is empty, and `sample(1:n, k)`, which builds `1:n` only to draw from it.
Both fixes are unsafe: `nchar(NA)` is 2 while `nzchar(NA)` is `TRUE`, and `sample(1:0, 1)` draws
from `c(1, 0)` where `sample.int(0, 1)` fails.

**`while-true`**, **`empty-assignment`** and **`unnecessary-concatenation`** (off by default):
`while (TRUE)`, which `repeat` says directly, `x <- {}`, which assigns `NULL`, and `c()` around a
single constant or around nothing, which returns that constant or `NULL`. All three fixes are safe.

**`comparison-negation`**, **`outer-negation`**, **`which-grepl`**, **`length-levels`** and
**`numeric-leading-zero`** (off by default): more spellings with a shorter equivalent, each with a
safe fix under [Fixes](#fixes). A fix that would change how the surrounding expression parses gets
parentheses, so `all(!x) + 1` becomes `(!any(x)) + 1`.

These lints take a level in `[lint]`, such as `"warn"` or `"error"`. An older `missing-comma` key is
still accepted and does nothing, because `f(1 2)` is now a syntax error, as it is in R.
