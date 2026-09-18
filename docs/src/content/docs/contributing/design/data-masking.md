---
title: Data masking
description: "Working draft: checking non-standard evaluation, data.table first"
---

This page holds ideas in progress for checking non-standard evaluation, which
is also called data masking. The settled framing lives in section 7 of
[Open type-system questions](/contributing/design/open-questions/). That
section holds the four-step design ladder and the survey of precedent. This
page is the sketchpad where a concrete design is drafted before it graduates
into that record or into the typing reference. data.table is the pressing
target.

## What ships today

ry recognizes masking and refuses to check inside it. There are no false
positives and there is no checking inside the mask. Four mechanisms do this
work.

- `quiet_reads` in naming. A read inside a masked context is never reported as
  unresolved, and it still keeps its lexical definers alive.
- `masked_subsets` in naming. A `[` bracket that carries an unambiguous
  data.table signature masks its index arguments. The signature is a `by =` or
  `keyby =` argument, a `:=` call, a `.()` call, or a `.SD`-family special,
  which is `.SD`, `.N`, `.I`, `.BY`, `.GRP`, or `.EACHI`. With an unknown
  subject the whole bracket types `Unknown`.
- The stub corpus's `@masked` attribute. It marks a variadic callee whose
  `...` is data-masked, which covers the dplyr verbs. The base masking family,
  which is `with`, `within`, `subset`, and `transform`, is still hardcoded in
  the naming walk instead.
- The result-shape classifier for a data.table bracket, described as idea 1
  below. It has shipped. The typing reference's data-masking section is the
  contract, and the decision record has the shape. A conditional `data.table`
  stub namespace, active only when the project declares or attaches the
  package, gives a value the `data.table` class. A bracket whose subject is
  that nominal masks all its index reads, so no syntactic marker is needed,
  and it classifies its result by the syntax of `j`. The classifier lives in
  the checker, in `infer_index`, next to the rules for `[` on a vector, and it
  is keyed on the `data.table` nominal.

Every idea below must keep the zero-false-positive property. An unknown
masking construct stays silent and is never guessed.

## data.table

### The surface to model

`DT[i, j, by]` is a query language in one bracket.

| Piece | Meaning | Static handle |
| --- | --- | --- |
| `i` | the row filter or join, as in `x > 3`, another table, or `on =` | a masked expression over columns |
| `j` | select, compute, or assign, as in `x`, `.(m = mean(y))`, or `x := …` | decides the result shape |
| `by` and `keyby` | grouping | changes what `.N` and `.SD` mean, not the result class |
| specials | `.SD`, `.N`, `.I`, `.BY`, `.GRP`, `.EACHI`, `.SDcols` | vocabulary that data.table injects itself |
| `:=` | assignment by reference, which mutates `DT` in place | the subject's type evolves across statements |
| `set*()` | `setnames`, `setkey`, `setDT`, `set()` | the same in-place evolution, in ordinary call syntax |
| chaining | `DT[…][…]` | one bracket's result shape feeds the next |

### Idea 1: classify the result shape from `j`, with no column knowledge

Before this shipped, the whole bracket was `Unknown`. The class of the result
is largely decided by the syntax of `j`, before any column is known.

- `j` is absent, as in `DT[i]`. The result has the subject's class, which is
  `data.table`.
- `j` is a `.(…)` or a `list(…)` call. The result is a `data.table`.
- `j` is a `:=` call. The result has the subject's class, returned invisibly,
  plus the in-place evolution noted below.
- `j` is a bare column name, as in `DT[, x]`. The result is a vector, but
  which vector needs the columns, so it is `Unknown`. That is still better
  than nothing, because it is known not to be a data.table. A `nominal-not`
  refinement could carry that once unions support it, and that is probably not
  worth the machinery yet.
- `j` is anything else, such as a call, `with = FALSE`, a character `j`, or
  `..var`. The result is `Unknown`.

This was shippable as a small, sound upgrade. `DT[i]`, `DT[, .(…)]`, and
`DT[, x := …]` type as the `data.table` nominal instead of `Unknown`, and the
rest is refused exactly as before. It makes a chain such as
`DT[a > 1][, .(m = mean(b)), by = g]` keep its class end to end, which is what
downstream code branches on.

### Idea 2: `:=` and the evolution problem

`DT[, y := x * 2]` adds a column in place. A column-aware checker must treat
this the way it treats loop-carried and top-level rebinding. The binding's
type after the statement is the old row type extended with `y`. Three
consequences need drafting.

- Aliasing. `DT2 <- DT; DT2[, y := 1]` also changes `DT`. There are two honest
  options. One is to refuse a column-level claim after an alias escapes. The
  other is to treat a `data.table` column set as a lower bound only, meaning
  "has at least these columns". The lower bound is the compromise
  pandas-stubs makes, and it is probably right here. A membership check stays
  useful, and exactness is never claimed.
- Deletion. `DT[, y := NULL]` breaks the monotonicity a lower bound needs.
  Under the lower-bound model, a delete must widen the whole binding back to
  column-unknown, or exact sets must be tracked only in a linear region, where
  no alias escapes.
- The `set*()` functions are the same problem in ordinary call syntax. Their
  stubs can carry the same contract once the contract language can express it.

### Idea 3: where column knowledge comes from

This is ladder step 2 applied to data.table. The sources are listed
cheapest first.

1. A literal constructor, such as `data.table(x = 1:3, y = "a")` or
   `as.data.table(list(...))`. It gives exact names and exact element types.
2. A `:=` with a literal left-hand side, which is a name or a character vector
   such as `c("a","b")`. It extends the set. The functional form,
   `` `:=`(a = …, b = …) ``, does too.
3. An annotation. Today that is `#: DT: data.table`. Once question 3, which
   covers data frame modeling, settles a row-type syntax, it becomes
   `#: DT: data.table<x: integer[], …>`. This is the escape hatch for
   `fread()` and similar functions, which are opaque statically. An F#-style
   compile-time schema import is out of scope.
4. A derived shape, from a join, `melt`, or `dcast`. These are late, hard, and
   low priority.

With a vocabulary, the first checkable fact inside the mask is membership. A
column typo is the dominant real-world bug in non-standard evaluation. A bare
name in `i`, `j`, or `by` is checked against the column set, with the lexical
environment as a fallback, because data.table looks an unmatched name up
lexically. A name is flagged only when it is in neither scope, which preserves
the zero-false-positive property.

### Suggested sequencing for data.table

1. The result-shape classifier, idea 1, is done. It includes the conditional
   stub namespace and the typed-subject masking it needed to be usable. The
   `datatable` fixture group in the typing-imports suite pins the behavior.
2. Declare the bracket semantics and the `set*()` contracts in the stub
   language, so they move out of the naming walk and into per-package stub
   metadata. The `@masked` attribute already covers the variadic verbs, and
   the bracket is the part still hardcoded.
3. Add the column vocabulary and the membership checks, idea 3, gated on
   question 3's row-type design, with `:=` evolution under the lower-bound
   model from idea 2.

## dplyr, where the verb level has shipped

- **Shipped.** A conditional `dplyr` stub namespace declares the verb set
  `@masked`, with class-preserving signatures of the form
  `<T> fn(.data: T, ...) -> T`. A join preserves the left class, and
  `join_by` is a mask with no formals, so every argument is a column
  reference. The tidy-select and verb vocabulary is declared too. The
  `@masked` contract is formal-aware, because a formal declared before `...`
  resolves normally, by position or by name. With the native-pipe desugar,
  `fread(path) |> mutate(r = a / b)` keeps its class end to end.
- **Remaining.** A column membership check reuses the future
  column-vocabulary machinery, with the `.data$x` and `.env$x` pronouns
  resolving exactly. A per-verb result shape over row types comes with design
  question 3. `mutate` extends, `summarise` collapses, and `select` projects.
- **Refused indefinitely.** Tidy-eval injection, which is `!!` and `{{ }}`,
  stays refused. `across()` is declared `Any` and its arguments stay masked.

## Builder EDSLs such as ompr, where suppression is the answer

No data context exists at the call site. `add_variable(model, x[i], i = 1:10)`
declares `x` into the model object, so the name lives in a value rather than
in a frame. Keep the quiet-read suppression. A package-specific extension
could thread the declared names through the builder chain's return type, but
that is bespoke per package and is not on any near-term path.

## Open questions

- Should a column set be a lower bound, or an exact set inside a linear
  region? Pick one before idea 3. The lower bound is the current lean.
- The annotation syntax for row types is owned by design question 3.
  Non-standard evaluation should consume whatever that question settles rather
  than invent a second syntax.
