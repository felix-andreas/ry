---
title: Data masking
description: "Working draft: checking non-standard evaluation, data.table first"
---

This page is a sketchpad for checking non-standard evaluation, also called data masking, with
data.table as the most pressing target. The framing, a four-step design ladder and a survey of
precedent, is section 7 of [open type-system questions](/contributing/design/open-questions/).

## What ships today

ry recognizes masking and refuses to check inside it, so there are no false positives, and no
checking inside the mask. Four mechanisms do the work:

- **`quiet_reads` in naming.** A read inside a masked context is never reported as unresolved, and
  still keeps its lexical definitions alive.
- **`masked_subsets` in naming.** A `[` bracket carrying an unambiguous data.table signature masks
  its index arguments. The signature is a `by =` or `keyby =` argument, a `:=` call, a `.()` call, or
  one of the specials `.SD`, `.N`, `.I`, `.BY`, `.GRP`, and `.EACHI`. When the subject is unknown,
  the whole bracket types as `Unknown`.
- **The `@masked` stub attribute.** It marks a variadic callee whose `...` is data-masked, which
  covers the dplyr verbs. The base masking family (`with`, `within`, `subset`, and `transform`) is
  still hardcoded in the naming walk instead.
- **The result-shape classifier for data.table brackets.** A conditional `data.table` stub
  namespace, active only when the project declares or attaches the package, gives values the
  `data.table` class. A bracket whose subject has that class masks all its index reads, and its
  result is classified by the syntax of `j` without knowing any column: no `j`, a `.(…)` or
  `list(…)` call, or a `:=` call keeps the `data.table` class, and anything else is `Unknown`. The
  classifier lives in `infer_index`, the typing reference's data-masking section is its contract,
  and the `datatable` group in the typing-imports fixture suite pins it.

Every step below must keep the zero-false-positive property: an unknown masking construct stays
silent and is never guessed at.

## data.table

### The surface to model

`DT[i, j, by]` is a whole query language in one bracket:

| Piece | Meaning | Static handle |
| --- | --- | --- |
| `i` | the row filter or join, as in `x > 3`, another table, or `on =` | a masked expression over columns |
| `j` | select, compute, or assign, as in `x`, `.(m = mean(y))`, or `x := …` | decides the result shape |
| `by` and `keyby` | grouping | changes what `.N` and `.SD` mean, not the result class |
| specials | `.SD`, `.N`, `.I`, `.BY`, `.GRP`, `.EACHI`, `.SDcols` | vocabulary data.table injects itself |
| `:=` | assignment by reference, which mutates `DT` in place | the subject's type evolves across statements |
| `set*()` | `setnames`, `setkey`, `setDT`, `set()` | the same in-place evolution, in ordinary call syntax |
| chaining | `DT[…][…]` | one bracket's result shape feeds the next |

### `:=` and the evolution problem

`DT[, y := x * 2]` adds a column in place. A column-aware checker must treat this like loop-carried
and top-level rebinding: after the statement, the binding's type is the old row type extended with
`y`. Three consequences need working out:

- **Aliasing.** `DT2 <- DT; DT2[, y := 1]` changes `DT` too. One honest option is to refuse any
  column-level claim once an alias escapes. The other is to treat a `data.table`'s column set as a
  lower bound only ("has at least these columns"), which is the compromise pandas-stubs makes and
  probably the right one here: membership checks stay useful, and exactness is never claimed.
- **Deletion.** `DT[, y := NULL]` breaks the monotonicity a lower bound needs. Under the lower-bound
  model, a deletion must widen the whole binding back to "columns unknown", unless exact sets are
  tracked only inside a linear region where no alias escapes.
- **The `set*()` functions** are the same problem in ordinary call syntax. Their stubs can carry the
  same contract once the contract language can express it.

### Where column knowledge comes from

This is step 2 of the ladder, applied to data.table. The sources, cheapest first:

1. **A literal constructor**, such as `data.table(x = 1:3, y = "a")` or `as.data.table(list(...))`,
   gives exact names and exact element types.
2. **A `:=` with a literal left-hand side**, a name or a character vector such as `c("a","b")`,
   extends the set, and so does the functional form `` `:=`(a = …, b = …) ``.
3. **An annotation.** Today that is `#: DT: data.table`. Once question 3 on data frame modeling
   settles a syntax for row types, it becomes something like `#: DT: data.table<x: integer[], …>`.
   This is the escape hatch for `fread()` and friends, which are opaque statically; an F#-style
   schema import at compile time is out of scope.
4. **A derived shape**, from a join, `melt`, or `dcast`. These come late, are hard, and have low
   priority.

With a vocabulary, the first fact that becomes checkable inside the mask is *membership*, and a
misspelled column is the dominant real-world bug in non-standard evaluation. A bare name in `i`, `j`,
or `by` is checked against the column set, with the lexical environment as a fallback, because
data.table looks an unmatched name up lexically. A name is flagged only when it is in neither scope,
which keeps the zero-false-positive property.

### Suggested order for data.table

1. **Declare the bracket semantics and the `set*()` contracts in the stub language**, so they move
   out of the naming walk and into per-package stub metadata. The `@masked` attribute already covers
   the variadic verbs; the bracket is what is still hardcoded.
2. **Add the column vocabulary and membership checks** ([where column knowledge comes
   from](#where-column-knowledge-comes-from)), with `:=` evolution under the lower-bound model
   above. The row-type syntax now exists: ry's built-in `frame{x: integer[], ...}` type has exact
   column sets, checked column references and query result types (the "Frames" section of the
   typing reference). Reusing that machinery for a `data.table` annotation is the remaining step,
   with the lower-bound model replacing exact sets, because a `data.table` updates by reference.

## dplyr

- **What ships.** A conditional `dplyr` stub namespace declares the verbs `@masked`, with
  class-preserving signatures of the form `<T> fn(.data: T, ...) -> T`. A join preserves the left
  class, and `join_by` is a mask with no formals, so every argument is a column reference. The
  tidy-select helpers and the rest of the vocabulary are declared too. The `@masked` contract knows
  about formals: one declared before `...` resolves normally, by position or by name. Together with
  the native-pipe desugaring, `fread(path) |> mutate(r = a / b)` keeps its class end to end.
- **Remaining.** A column membership check will reuse the future column-vocabulary machinery, with
  the `.data$x` and `.env$x` pronouns resolving exactly. Per-verb result shapes over row types come
  with design question 3: `mutate` extends, `summarise` collapses, and `select` projects.
- **Refused indefinitely.** Tidy-eval injection (`!!` and `{{ }}`) stays refused, and `across()` is
  declared `Any` with its arguments masked.

## Builder EDSLs such as ompr: suppression is the answer

There is no data context at the call site. `add_variable(model, x[i], i = 1:10)` declares `x` into
the model object, so the name lives in a value rather than in a frame. Keep the quiet-read
suppression. A package-specific extension could thread the declared names through the builder
chain's return type, but that would be bespoke for each package, and it is not on any near-term path.

## Open questions

- Should a column set be a lower bound, or an exact set inside a linear region? Pick one before
  adding column vocabularies; the current lean is the lower bound.
- The annotation syntax for row types belongs to design question 3. Non-standard evaluation should
  use whatever that question settles, rather than invent a second syntax.
