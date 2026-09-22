---
title: Open type-system questions
description: "Type-system design questions that are not yet decided, with the options on the table"
---

This page collects the type-system questions that are still open. For each one it states the
question, the options on the table, and the stopgap ry uses in the meantime.

It is not the decision log. `.agents/memory/decisions.md` records what has been settled, while an
entry here is a live deliberation. When a question is resolved, move the decision and its rationale
into `decisions.md` and delete the entry here. Nothing on this page is contract until it lands in the
[type system reference](/reference/type-system/).

Four questions have already made that journey:

- Generic vector element types became an atomic-element constraint on the existing constraint
  mechanism.
- Ad-hoc overloading became ordered overload sets with probe-then-rollback, confined to declaration
  files.
- Multi-member unions are join-only and annotation-only, and never bind into a unification variable.
- R's variable model is mutable slots, with a union wherever a read sees several writes.

## 1. Tags and discriminated unions through a ry standard library

**The question.** How should ry provide Roc-style tags, the same idea as OCaml's polymorphic
variants? The shape under discussion is a library the checker knows about specially, exposing tag
constructors and a `match` function with exhaustiveness checking.

**The maintainer's direction.** Provide them through that specially known library, not through new
R syntax, so that annotated R stays ordinary R. This comes after the beta.

**An unresolved tension.** [Inline type syntax](/contributing/design/inline-type-syntax/) proposes
the opposite route to the same capability: real syntax, in a compiled dialect. Both routes reach
exhaustive case analysis. They differ in whether the checker blesses particular call shapes or owns a
grammar, and in whether a build step is acceptable. Building both would be waste, so settle the fork
(section 3 of that page) before implementing either, and delete the losing half.

**What to work out before building:**

- **Representation.** At run time a tagged value is presumably `list(tag = "Name", value = ...)`.
  Does the type system model it as a union of nominal-like tag types, or as a new core form?
- **Exhaustiveness.** Checking that the named arguments of `match(x, Some = fn, None = fn)` cover the
  union's members requires the checker to know argument names at one blessed callee. How special is
  that call form allowed to be?
- **A separate type former?** Perhaps the general unions already decided, plus a literal
  discriminant field, are enough, and tags need nothing of their own.
- **Interaction with strict mode and narrowing.** A `match` arm should see the narrowed member type.

## 2. S3 dispatch

**The question.** How should ry type an S3 generic such as `print`, `summary`, `plot`, `format`, or
`predict`, whose result depends on the class of its first argument?

**The options.** One is a per-class overload set on the generic's stub, such as
`print : fn(x: data.frame) -> data.frame`. That is available today, and it is the sanctioned form,
because it lives in a declaration file. The other is a model of the class hierarchy that understands
`UseMethod`. That is heavier, but it is the only option that reaches user-defined S3 classes. Traits
are no longer an option, for the reasons in section 4.

**The stopgap.** S3 generics are `Any` in the corpus, or missing from it. The operator method tables
that do ship, such as `+.Date` and `Arith.difftime`, show what a per-class answer looks like.

## 3. Modeling data frames and matrices

**The question.** How should ry type a data frame down to its columns, so that `df$col` and
`df[, "col"]` have useful types, and how should it carry a matrix's dimensions?

**Notes.** ry ships `data.frame` as an opaque nominal, declared with `@type` in a `.Rtypes` file,
which is honest but shallow. Column typing probably wants row-polymorphic records over an opaque
carrier, while a matrix wants an element type first, with no dimension tracking. Both interact with
the semantics of `[` and `[[`, and with how `x[i, j]` lowers. Design this after the beta semantics
settle.

## 4. Traits and typeclasses: closed, and declined

**The question, now settled.** Should ry have a general capability mechanism (numeric, atomic
element, comparable, overloadable with `+`, S3) that subsumes the ad-hoc constraint kinds and the
overload sets?

**The answer is no.** The type system admits only what is fast to check, which means Hindley-Milner,
with declaration files carrying the one sanctioned exception; `decisions.md` records this under "The
type system is Hindley-Milner, and stays fast to check". Traits are the textbook-correct way to add
ad-hoc polymorphism to Hindley-Milner. That is exactly what type classes were invented for, and why
overloading is not Hindley-Milner. But correctness in the textbook sense is not the bar here. The bar
is checking at editor speed, with no new vocabulary for users to learn. Do not reopen this because a
third constraint kind turns up: add the constraint, or accept the imprecision.

Three pressures were examined, and none changed the answer:

- A "comparable" kind for two flexible operands is nearly vacuous, because R compares across atomic
  families.
- An intersection constraint for a conflict between union commitments is unnecessary, because
  committing at first use, plus an annotation, is the specification.
- The `T[]` element type became the atomic-element constraint on the existing mechanism.

The pattern across all three is the useful lesson: what looked as if it needed traits was better
served by one more constraint on machinery that already existed.

## 5. Bridging variadic inference

**The question.** What type does `...` have inside the body of a `function(x, ...)`? That covers
`list(...)`, `..1`, and forwarding to another variadic function. Inference first needs to lower a
trailing `...` as a rest parameter, and `backlog.md` records that decided direction.

**The stopgap.** A use of `...` inside a body stays `Unknown`; only the rest parameter in the
signature is bridged. The full semantics of `...`, including compatibility when forwarding and
`..N` access, is open.

## 6. The NAMESPACE and import model

**The question.** How should library scoping work: what `library(pkg)` attaches, what `importFrom`
in a `NAMESPACE` file imports, the order of the search path, and masking warnings?

**Notes.** ry ships `pkg::name` resolution against namespace-partitioned stubs, but the attach and
import model is not designed yet. It decides when an unresolved name is really unresolved, so it
directly decides how useful strict mode is on a real project.

## 7. Non-standard evaluation

**The question.** How far can a *masked* expression be checked statically, and what machinery does
each step need? A masked expression is one whose names resolve against a data context the callee
builds at run time, such as `mutate(df, y = x * 2)`,
`DT[x > 3, .(m = mean(y)), by = z]`, or `add_variable(model, x[i], i = 1:10)`.

**What ships today.** ry recognizes masking and refuses to check inside it, which means no false
positives and no checking inside the mask. [Data masking](/contributing/design/data-masking/)
describes the mechanisms in detail.

**The design ladder.** Each step can ship on its own:

1. **Masking contracts in the stub language.** A signature declares that its `...` is data-masked,
   and which of its formals resolve normally. The `@masked` attribute in `.Rtypes` does this today,
   so this step is mostly built. What remains is moving the base masking family (`with`, `within`,
   `subset`, and `transform`) out of the naming walk and into the corpus, and declaring the
   data.table bracket semantics the same way.
2. **A column vocabulary, not column types.** Once a data frame carries column-level structure
   (question 3), a masked name can be checked for membership in the data argument's columns and the
   lexical environment. That catches a misspelled column, the most common bug in non-standard
   evaluation, while a masked expression still types as `Unknown`. The `.data$x` and `.env$x`
   pronouns resolve exactly.
3. **Typed masked expressions.** Check the masked expression in an environment extended with the
   columns' types. The hard part is the result type. `mutate` extends the row type, so its return
   needs record extension, applied in argument order. A grouped operation, such as `summarise` or a
   data.table `by=`, changes the frame's shape. And tidy-eval injection (`!!` and `{{ var }}`) needs
   a column-reference kind in the type system. That is the expensive tail, and it will probably stay
   behind an explicit annotation for good.
4. **Builder EDSLs, such as ompr.** Here there is no data context at all: a name is declared by an
   earlier builder call, such as `add_variable`, and lives only in the model object. Generic checking
   is unrealistic, and the honest options are the quiet-read suppression ry does today or a
   package-specific extension.

**Precedent:**

- **R itself.** `R CMD check`'s "no visible binding for global variable" note is the oldest collision
  between a static checker and non-standard evaluation. The ecosystem answered it with
  `utils::globalVariables()` and the `.data` pronoun, which validates both the suppression baseline
  and explicit pronouns as the bridge to checkability.
- **TypeScript query builders**, such as Prisma, Kysely, and Drizzle, encode a column set as an
  object type and check masked names through `keyof` and mapped types. They are the direct analogue
  of steps 2 and 3, and proof that the ladder works at ecosystem scale.
- **F# type providers**, such as FSharp.Data, import a schema at compile time and generate typed
  accessors. That is the strongest form of "know the columns", at the cost of a compile-time
  dependency on the data.
- **Python.** mypy and pyright deliberately do not type pandas columns: pandas-stubs types the
  operations, columns stay stringly typed, and schema checking lives in runtime validators such as
  pandera. This mainstream punt shows how steeply the cost curve bends. It is also where ry can stand
  out, because steps 1 and 2 fit its existing architecture of stub contracts and structural records
  with no new inference machinery.

**The recommendation.** Do steps 1 and 2 once data frame columns land, which question 3 gates. Do
step 3 only for the pronoun forms and the explicit forms, and leave step 4 suppressed. Never give up
the zero-false-positive property: at every step, an unknown masking construct must stay silent
rather than guessed at.
