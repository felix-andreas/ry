---
title: Open type-system questions
description: "Type-system design questions that are not yet decided, with the options on the table"
---

This page holds the type-system questions that are not decided. Each entry
states the question, the options on the table, and the stopgap ry uses today.

This page is not the decision log. `.agents/memory/decisions.md` records what
has been settled, and an entry here is a live deliberation. When a question is
resolved, move the decision and its rationale into `decisions.md` and delete
the entry here. The settled contract is always the
[type system reference](/reference/type-system/). Nothing here is contract
until it lands there.

Four questions have already been resolved this way, and `decisions.md` records
them. Generic vector element types became an atomic-element constraint on the
existing constraint mechanism. Ad-hoc overloading became ordered overload sets
with probe-then-rollback, confined to declaration files. A multi-member union
is join-only and annotation-only, and never binds into a unification variable.
The R variable model is mutable slots, with a union at a join read.

## 1. Tags and discriminated unions through a ry standard library

**The question.** How should ry provide Roc-style tags, which are OCaml
polymorphic variants? The shape under discussion is a library the checker
knows specially, exposing tag constructors and a `match` function with
exhaustive-pattern checking.

**The user's direction.** Provide them through a standard library the checker
knows specially, not through new R syntax. Annotated R stays ordinary R. This
comes after the beta.

**An unresolved tension.**
[Inline type syntax](/contributing/design/inline-type-syntax/) proposes the
opposite route for the same capability, which is real syntax in a compiled
dialect. Both routes reach exhaustive case analysis. They differ in whether
the checker blesses particular call shapes or owns a grammar, and in whether a
build step is acceptable. Building both is waste. Settle the fork, which is
section 3 of that page, before either one is implemented, and delete the
losing half.

**What to work out before building.**

- The representation. A tagged value is presumably `list(tag = "Name", value = ...)`
  at run time. Does the type system model it as a union of nominal-like tag
  types, or as a new core form?
- Exhaustiveness. Checking that the named arguments of `match(x, Some = fn, None = fn)`
  cover the union's members needs the checker to know argument names at one
  blessed callee. How special is that call form allowed to be?
- Whether the general unions that are already decided, plus a literal
  discriminant field, are enough, or whether tags need their own type former.
- The interaction with strict mode and with narrowing. A `match` arm should
  see the narrowed member type.

## 2. S3 dispatch

**The question.** How should ry type an S3 generic, such as `print`,
`summary`, `plot`, `format`, or `predict`, whose result depends on the class
of the first argument?

**The options.** One option is a per-class overload set on the generic's stub,
such as `print : fn(x: data.frame) -> data.frame`. That is available today,
and it is the sanctioned form, because it lives in a declaration file. The
other option is a class-hierarchy model with `UseMethod` awareness. That is
heavier, and it is the only option that reaches a user-defined S3 class.
Traits are no longer an option. Section 4 explains why.

**The stopgap.** An S3 generic is `Any` in the corpus, or missing from it. The
operator method tables that do ship, `+.Date` and `Arith.difftime`, show the
shape a per-class answer takes.

## 3. Modeling data frames and matrices

**The question.** How should ry type a data frame at column level, so that
`df$col` and `df[, "col"]` have useful types, and how should it carry a
matrix's dimensionality?

**Notes.** ry ships `data.frame` as an opaque nominal, declared with `@type`
in a `.Rtypes` file. That is honest and shallow. Column typing probably wants
row-polymorphic records over an opaque carrier. A matrix wants an element type
first, with no dimension tracking. Both interact with the semantics of `[` and
`[[`, and with how `x[i, j]` lowers. Design this after the beta semantics
settle.

## 4. Traits and typeclasses, closed and declined

**The question, settled.** Should ry have a general capability mechanism, for
numeric, atomic-element, comparable, and `+`-overloadable or S3 capabilities,
that subsumes the ad-hoc constraint kinds and the overload sets?

**The answer is no.** The type system admits only what is fast to check, which
means Hindley-Milner, and a declaration file carries the one sanctioned
exception. `decisions.md` records this under "The type system is
Hindley-Milner, and stays fast to check". Traits are the textbook correct way
to put ad-hoc polymorphism into Hindley-Milner. That is what type classes were
invented for, and it is why overloading is not Hindley-Milner. Correct is not
the bar here. The bar is checkable at editor speed, with no vocabulary for a
user to learn. Do not reopen this because a third constraint kind appears.
Add the constraint, or accept the imprecision.

Three pressures were examined and none of them changed the answer. A
"comparable" kind for two flexible operands is nearly vacuous, because R
compares across atomic families. An intersection constraint for a
union-commitment conflict is unnecessary, because first-use commitment plus an
annotation is the specification. The `T[]` element type became the
atomic-element constraint on the existing mechanism. The pattern across all
three is the useful one. What looked like it needed traits was better served
by one more constraint on machinery that already existed.

## 5. Bridging variadic inference

**The question.** What type does `...` have inside the body of a
`function(x, ...)`? This covers `list(...)`, `..1`, and forwarding to another
variadic function. Inference needs to lower a trailing `...` as a rest
parameter first, and `backlog.md` records that decided direction.

**The stopgap.** A use of `...` inside a body stays `Unknown`. Only the
signature-level rest parameter is bridged. The full semantics of `...`, which
covers forwarding compatibility and `..N` access, is open.

## 6. The NAMESPACE and import model

**The question.** How should library scoping work? This covers what
`library(pkg)` attaches, what `importFrom` in a NAMESPACE file imports, the
order of the search path, and masking warnings.

**Notes.** ry ships `pkg::name` resolution against namespace-partitioned
stubs. The attach and import model is undesigned. It decides when an
unresolved name is really unresolved, so it directly decides how useful strict
mode is on a real project.

## 7. Non-standard evaluation

**The question.** How far can a masked expression be checked statically, and
what machinery does each step need? A masked expression is one whose names
resolve against a data context that the callee constructs at run time.
Examples are `mutate(df, y = x * 2)`, `DT[x > 3, .(m = mean(y)), by = z]`, and
`add_variable(model, x[i], i = 1:10)`.

**What ships today.** ry recognizes masking and refuses to check inside it.
There are no false positives and no checking inside the mask.
[Data masking](/contributing/design/data-masking/) describes the mechanisms in
detail.

**The design ladder.** Each step is independently shippable.

1. **Masking contracts in the stub language.** A signature declares that its
   `...` is data-masked, and which of its formals resolve normally. The
   `@masked` attribute in `.Rtypes` does this today, so this step is mostly
   built. What remains is moving the base masking family, which is `with`,
   `within`, `subset`, and `transform`, out of the naming walk and into the
   corpus, and declaring the data.table bracket semantics the same way.
2. **A column vocabulary, not column types.** Once a data frame carries
   column-level structure, which is question 3, a masked name can be checked
   for membership in the data argument's column set and in the lexical
   environment. That catches a column typo, which is the most common bug in
   non-standard evaluation, while a masked expression still types `Unknown`.
   The `.data$x` and `.env$x` pronouns resolve exactly.
3. **Typed masked expressions.** Check the masked expression in an environment
   extended with the columns' types. The gate is the result type. `mutate`
   extends the row type, so the return needs record extension, applied
   sequentially in argument order. A grouped operation, such as `summarise` or
   a data.table `by=`, changes the frame's shape. Tidy-eval injection, which
   is `!!` and `{{ var }}`, needs a column-reference kind in the type system.
   That is the expensive tail, and it will probably stay behind an explicit
   annotation for good.
4. **Builder EDSLs, such as ompr.** No data context exists. A name is declared
   by a prior builder call, such as `add_variable`, and it lives only in the
   model object. Generic checking is unrealistic. The honest options are the
   quiet-read suppression ry does today, or a package-specific extension.

**Precedent.**

- R itself. The "no visible binding for global variable" note from
  `R CMD check` is the oldest collision between a static checker and
  non-standard evaluation. The ecosystem's answers are
  `utils::globalVariables()` suppression and the `.data` pronoun. They
  validate both the suppression baseline and explicit pronouns as the bridge
  to checkability.
- TypeScript query builders, such as Prisma, Kysely, and Drizzle. They encode
  a column set as an object type and check a masked name through `keyof` and
  mapped types. This is the direct analogue of steps 2 and 3, and it is proof
  that the ladder works at ecosystem scale.
- F# type providers, such as FSharp.Data. A schema imported at compile time
  generates typed accessors. This is the strongest form of "know the columns",
  and it costs a compile-time dependency on the data.
- Python. mypy and pyright deliberately do not type a pandas column.
  pandas-stubs types the operations, and a column stays stringly typed. Schema
  checking lives in a runtime validator, such as pandera. This mainstream
  punt marks how far the cost curve bends. It is also where ry can
  differentiate, because steps 1 and 2 fit its architecture, which already has
  stub contracts and structural records, with no new inference machinery.

**The recommendation.** Do steps 1 and 2 after data frame columns land, which
question 3 gates. Do step 3 only for the pronoun forms and the explicit forms.
Leave step 4 suppressed. Never regress the zero-false-positive property. Every
step must keep an unknown masking construct silent rather than guessed.
