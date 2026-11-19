# Decisions: Syntax and semantics

The parser and syntax tree, the salsa analysis core, name resolution, and the type checker.

## The standard library ships as declaration-only stub files

**Status:** built.

Standard-library types ship as declaration-only `.Rtypes` files, not R files with placeholder bodies
(`length <- function(x) 0L`): a body is unrepresentable, so a stub cannot drift into carrying one, and
the loader parses only declarations. `.Rtypes` avoids R's `.Rd`/`.Rmd`/`.Rda`/`.RData`; `.Rstub` was
rejected. An ad-hoc overload uses an ordered overload set; a genuinely parametric higher-order function
(`lapply`, `Map`, `Reduce`, `identity`) gets a real `<T> fn(...)` type, never `Any`. Stub highlighting
goes through LSP semantic tokens, so one implementation colours both `#:` annotations and stubs in
every client. Contract for the format, the overload rules and the export manifests:
`development/authoring-stubs.md`.

## The beta semantics

**Status:** built.

The core type-system decisions follow. The semantics contract is `reference/type-system.mdx`, updated
before the behavior it describes ships; `backlog.md` holds the work that follows.

## Beta semantics: The R variable model is mutable slots with union joins

**Status:** built.

R assignment mutates the function-scope environment, so a branch or loop write is visible after the
construct. A function scope therefore holds mutable slots: `<-`, `=`, `->` write the slot, `<<-`/`->>`
write the nearest enclosing slot, and a read sees the union of reaching definitions (conditional writes
join with the prior type, loop writes join across iterations). Unused detection falls out: a write no
read reaches is unused, reported at the assignment. Narrowing is a separate later layer on this model.

Rejected: let-shadowing (a fresh binding per `<-`). It was unsound (`x <- 1L; if (f) x <- "two"; x + 1L`
checked clean and crashes) and reported idiomatic conditional updates and loop accumulators as unused.

## Beta semantics: A multi-member union is for joins and annotations, never for a unification variable

**Status:** built.

A union `A | B | C` comes only from a join or an annotation and normalizes flat, deduplicated and
order-insensitive (`T | NULL` is a case of it). It never becomes a constraint on an inference variable,
which keeps inference fast; a variable may be bound to a union by plain substitution. Unification stays
syntactic (a union unifies only with a structurally equal union) and all member-wise directional logic
lives in `compatible`.

## Beta semantics: A type error inside a test assertion stays reported

**Status:** built, except the "testing that something is rejected" section, which the diagnostics page does not have.

A type error inside `expect_error(f("bad type"))` is reported. Suppressing findings inside `expect_error`
and its siblings would hide genuine test mistakes (a misspelled callee) and special-case an open-ended
family (`expect_warning`, `expect_condition`, `expect_snapshot(error = TRUE)`, `tryCatch`, `try`). The
mechanism is the general `# ry: allow(type-mismatch)` on the assertion. Worth revisiting: a
TypeScript-style `@ts-expect-error` that reports when the expected finding is absent, for every code.

## Beta semantics: The type system is Hindley-Milner, and stays fast to check

**Status:** built.

User directive: admit only features that keep inference to unification over the existing constraint
mechanism, decidable and near-linear, with no search and no global solving. Ruled out: type classes and
traits (declined, reasoning in `development/design/open-questions.md`); general subtyping (a different,
slower algorithm; a declared coercion at a named boundary is the shape variance work must take); any
backtracking search over a program-wide constraint set.

The one exception is `.Rtypes` stubs, which may use ad-hoc overloading. It is bounded because the stub
surface is a fixed maintained corpus, and the need comes from R's coercion table: `abs(TRUE)` is an
`integer`, so `<T: numeric> fn(x: T) -> T` is wrong for `logical`, and `min` accepts `character`. Never say
these functions have no principal scheme: a gradual checker that cannot describe the standard library is
not usable. A user's `#:` annotation stays pure Hindley-Milner.

## Beta semantics: ry has no macros

**Status:** built.

User directive: ry adds no macro system, syntactic or procedural, and rejects any proposal that expands
or generates code before checking, including annotation-driven generation inside the checker. A macro
makes a file's meaning depend on code run before checking: every check pays for expansion, an item's
names can no longer be read from its syntax, and a macro edit invalidates every expansion site,
defeating the per-item firewalls. (Vx confines `macro_rules` to a phase before name resolution for this
reason, citing rustc, where expansion and name resolution are still serial; Roc and goose describe
none, and Bend allows only compile-time templates.)

R's own non-standard evaluation (`quote`, `substitute`, `bquote`, `eval`, tidy evaluation, data masks)
keeps working as ordinary calls (`eval()` returns `Any`); ry builds nothing on it. Common macro uses are
replaced: derived printing and equality come from structural types (`print((x = 1, y = 2))`,
`identical()` by value), repeated boilerplate from generics and higher-order functions, code generation
from an external script writing ordinary files (as `scripts/export-manifests.R` does), and precomputed
values from pure top-level constants under strict mode's definitions-only top level.

## Beta semantics: An overload set is a bounded, ordered probe

**Status:** built.

A function whose result type depends on its argument type gets an ordered overload set (the TypeScript
and mypy model). Repeating a name within one `.Rtypes` source appends a candidate; a later source
replaces the whole set. A call tries candidates in declaration order with probe-then-rollback.
Only a name whose declarations come from `.Rtypes` can be overloaded (`overloads` in
`crates/semantics/src/semantics.rs` returns `None` once a script or package binding shadows it); a
project's R code cannot declare one, for the Hindley-Milner reason above, but a project override stub can.

Overloads are the escape hatch, and the corpus is the gauge. Of its 35 sets, about a dozen are
atomic-family promotion (`abs`, `min`, `sum`, `cum*`, a numeric constraint in disguise), about a dozen
are shape dispatch (`head`, `rev`, `Filter`, `lapply`, standing in for a shape-mirroring return), nine
are S3 operator method tables (`+.Date`, `Arith.difftime`, which stay under any design), and two are
genuine two-form functions. A rising count of promotion or shape sets is the signal to build those two
Hindley-Milner-compatible features (`backlog.md`), never traits.

Call-site rules (`try_overloaded_call` in `crates/semantics/src/check.rs`):

- **Arguments are inferred once, before any probe**, because inference writes state the probe snapshot
  does not reverse; a probe runs only instantiation and `match_arguments`.
- **A fact beats a guess.** `collect_unbound_vars` records the caller's open variables first; a candidate
  that fits leaving all of them untouched (same representative, same entry, no tightened constraint) is
  a fact and beats one that fits only by narrowing them, so `function(x) sum(x)` does not over-commit.
  Within a kind, the first declared wins; a lone fit is never a guess. The winner is re-probed to commit.
- **A strict round, then a courtesy round.** Reading a whole-number literal as integer is off first, so
  `sum(1, 2)` picks the double candidate as R does; it is on in a second round only if nothing matched.

Last-fitting-wins was tried and removed: the fact rule already stops the narrow commit (a general `Any`
fallback accepts without binding, so it is a fact and outranks guesses), and last-wins forced
most-general-first declaration (`lapply`, where a lambda makes every candidate a guess), contradicting
the corpus convention and the value-use rule (a non-call use resolves to the last declaration).

On total failure, `NoMatchingOverload` (naming the number of signatures tried, with the first
candidate's failure as a hint) is reported only when candidates disagree, as `pick("word")` against
`fn(integer)` and `fn(double)`. When all fail identically, or one gets strictly further (by the index of
the first blamed argument, where a whole-call verdict such as arity counts as no progress), that
candidate's own error is reported at its argument range: a two-parameter callback to `lapply` is the
finding.

## Beta semantics: The boundary for R's object systems is dispatch, not Hindley-Milner

**Status:** built, except point 2: a `setClass` or `R6Class` declaration produces no nominal, so `new(...)` and `$new(...)` are `Unknown`.

S3, S4 and R6 declarations map onto nominals, field projection and ordered overload sets, and S3
operator dispatch on a nominal (`+.Class`, `Arith.Class`, `Ops.Class`) already runs in inference. S4 is
the most declared: `setClass` is a record with literal slot types, `new(...)` a named-argument
constructor, `x@slot` a field projection, `setMethod(signature = ...)` an overload candidate, and a
hand-written `@type` plus a constructor carrying `@new` already checks slots and arity. The gap is a
lowering pass, not type theory. Three things keep dispatch out:

- **Dispatch needs a class known at the call site**; on an open variable the only sound answer is
  `Unknown`.
- **Inheritance is subtyping, and there is none.** `Named` types match by exact name; `contains=` and
  `inherit=` need a nominal-subtyping axis in `compatible` and joins, the one real soundness risk.
- **A generic's method set is global mutable state.** Any file may add `print.foo`; making it an input
  to every call invalidates every call site, a full revalidation at 300,000 lines. It needs a memoized
  per-generic method-set query, so adding a method invalidates only that generic's calls and editing a
  method body invalidates nothing.

Decisions: (1) a false positive (a project's `setGeneric` generic reporting `unresolved`, an unknown `R6`
namespace) is a bug, fixed independently; (2) `setClass` and `R6Class(public = list(...))` may become
nominals with typed slots, catching slot typos and constructor arity with no subtyping or global state;
(3) dispatch and inheritance stay `Unknown` (a strict-mode coverage limit) until a per-generic
method-set query and a nominal-subtyping design exist.

## Beta semantics: `T[]` carries an atomic-element constraint, not a trait

**Status:** built.

The vector's element may be an inference variable, constrained by an atomic-element constraint
(`<T: atomic>`), the same mechanism as `<T: numeric>`; no trait system is needed. A valid element is a
`Scalar(_)`, a constrained `Variable(_)`, or `Any`/`Unknown`. `Constraint` is a lattice merged by `join`
(not `Ord`): numeric meets atomic as `ScalarNumeric` (`<T: scalar numeric>`, defaulting to `double` at a
binding boundary). Annotation lowering records the atomic bound directly on the element variable's entry
rather than via `constrain`, because the element may be a rigid `<T>` binder: the annotation makes the
promise, and a body must not add a bound the annotation never declared.

## Beta semantics: Coercion policy at parameter positions

**Status:** built.

At a parameter position a whole-number `double` literal is accepted for `integer` (as `:` already did),
`integer` widens to `double`, and a vectorized stub declares `T[]` so a scalar coerces up. Without these,
the more precise a stub was the more false positives it made (`seq_len(10)`, `toupper(c("a","b"))`,
`round(x, 2)`), pushing the corpus toward `Any`.

## Beta semantics: Strict mode

**Status:** built.

Under strict mode an `Unknown` origin (including a recursion-induced `Unknown` return), an unannotated
parameter nothing constrains, and an unresolved name are errors. Explicit `Any` is the sanctioned escape.
A `#: @strict` / `#: @strict off` directive at a file's top overrides the default in both pipelines; it
is derived from the parse, so incrementality holds. Refusing an unsupported construct loudly is
acceptable; mistyping one silently is not.

`stop(...)` types as `Any`: there is no bottom type, and `Any` fits every branch without a strict
finding, so an `if (bad) stop(...)` guard is free. `&` and `|` are typed elementwise, but an operand that
is still a variable is not unified with scalar `logical` (as `!`'s is), because that made
`h(c(TRUE, FALSE), TRUE)` an error; the operator is opaque for it instead.

## Beta semantics: Guard narrowing

**Status:** built, except the `&&` limit: the right conjunct of `!is.null(x) && x > 0` sees the refinement.

Narrowing is branch-edge entry refinement on the slot model, not a separate flow analysis.
`recognize_guard` (`crates/semantics/src/check.rs`) accepts `is.null(x)`, an `is.*` family member or a
negation, and `guard_edges` writes each edge's refined type as an ordinary undo-logged environment
write, so rollback and joins need no new machinery. A branch that never falls through (it ends in
`return`, `stop`, `break` or `next`, or a block ending in one) contributes nothing to the join, so the
surviving edge's refinement persists after the `if`.

Deliberate limits, for zero false positives:

- **Only union members are filtered.** A family guard invents no shape for `Any`/`Unknown` (a refined
  `character | character[]` would clash with scalar stub signatures); the one exception is `is.null`'s
  true edge on `Any`/`Unknown`, which becomes `NULL`.
- **A statically undecidable member stays on both edges** (a variable, a flexible-element vector, an
  opaque nominal: `is.list(data.frame)` is true).
- **Only a local slot narrows** (a parameter, function local or script local), not a package global,
  which keeps winner semantics, or `x$field`.
- The guarded key resolves exactly as a read resolves, so fixtures and production share one path.

## Beta semantics: The expanded parameter directive is `@param name {TYPE}`

**Status:** built.

The parameter directive is `@param name {TYPE}` (or `@param [name] {TYPE}`), not JSDoc's
`@param {TYPE} name`: a multi-line type continues cleanly when it comes last, every directive shares the
shape (`@type Name {TYPE}`, `@alias Name<T> {TYPE}`), and the name is one identifier token right after the
directive, which removes ambiguity and leaves room for a trailing description. The JSDoc order gets an
error naming the new form. `@return {TYPE}` and `@forall` are unchanged.

## A syntax error does not erase a file

**Status:** built.

Lowering is error-tolerant at statement granularity (as in rust-analyzer), not file granularity. A
whole-file cutoff on the tree's error bit made one keystroke drop the file's exports, refold the package
index and flood dependents with unresolved names.

- A well-formed statement always lowers; a broken one contributes no names, reads or cascading
  diagnostics, only its syntax error (contract: the syntax-errors section of `reference/type-system.mdx`).
- Inside a broken region, an error node's well-formed `Assign` children lower, and a broken assignment
  with an intact name side keeps its definition with an `ExpressionKind::Missing` value. A well-formed
  non-assignment fragment sharing a line with a following error sibling is dropped as the split half of
  the broken statement.
- `Missing` and `Unsupported` both type `Unknown`, but only `Unsupported` is a strict origin (the syntax
  error already covers `Missing`).
- An annotated definition with a `Missing` value binds its declared type unchecked, so callers keep the
  contract mid-edit and exports stay byte-equal (early cutoff, no downstream invalidation).

Pinned by `crates/semantics/tests/lowering/error-tolerance.R.test` and `error_tolerant_lowering` in
`crates/semantics/tests/typing/project-values.R.test`.

## `is.null` shapes an unconstrained inference variable

**Status:** built, except the finding on unguarded use after the test: `if (is.null(x)) 0L else 1L; x + 1L` is accepted.

The type model never invents a union for a variable (that keeps inference fast); this rule is the one
place a union is introduced, and only for a variable nothing else constrains.

In `guard_edges`, when the predicate is `is.null` and the guarded local slot is a completely
unconstrained inference variable `A` (entry `Unbound`, constraint `Unconstrained`), `A` is bound to
`T | NULL` for a fresh `T`, and ordinary member filtering narrows the edges. The coalesce idiom
`if (is.null(x)) fallback else x` then generalizes to `<T> fn(value: T | NULL, fallback: T) -> T`, the
same scheme as the annotated form, instead of unifying `x` with `fallback` (which rejected
`or_else(NULL, "text")`). It reuses the `@param x {T | NULL}` shape: no new type form, no deferred
union. A negated guard works through the edge swap. A constrained variable is never reshaped (a numeric
bound contradicts `NULL`), nor is a rigid or declared parameter (the annotation is the contract).

## Data-masked resolution for data.table and the with-family

**Status:** built, except the mask-depth counter, which "A masked argument is its own scope, and the base masking family is typed" supersedes.

data.table evaluates `i`, `j` and `by` in the data's frame, so bare column names must not warn. A mask is
recognized structurally in naming, and the diagnostic is suppressed at the emission edge.

- A `[` call is a data.table mask only with an unambiguous signature: `by =`/`keyby =`, a `:=` or `.()`
  call, or `.SD`, `.N`, `.I`, `.BY`, `.GRP`, `.EACHI`. None occur in base indexing, so `m[i, j]` is
  untouched. The base family `with`, `within`, `subset`, `transform` (callee not shadowed) masks the
  arguments after the data.
- A masked read that fails lexical resolution goes to `ItemNaming.quiet_reads` as well as `non_locals`,
  so it still resolves stubs (`sum` in `j` keeps its scheme); unresolved emitters skip quiet reads.
  Keeping masked reads out of `non_locals` was tried and lost stub typing.
- The recognized bracket (`NamingExtras.masked_subsets`) types as a silent `Unknown` before the base
  index-arity rules, which must not judge `[.data.table` shapes.
- `:=` lowers as an ordinary binary call, so its operands stay visible to naming, hover and the mask.

Not silenced: anything that resolves (locals, stubs, globals) keeps full checking inside a mask, and
base indexing keeps lexical resolution and its warnings. Strict mode stays silent on a masked column
read, a recognized construct rather than an unmodelled hole.

## Recursion in a function-valued assignment is monomorphic

**Status:** built, except the limit on mutual recursion between two local closures, which resolves without a finding.

A function-valued assignment uses the `let rec` rule: before the right-hand side is inferred, the target
slot is pre-bound to a fresh variable at the binding's generalization level, recursive reads unify with
it, and it then unifies with the inferred function type. Recursion is monomorphic, because polymorphic
recursion is undecidable in general.

Trap: the top-level package-winner path must overwrite the placeholder in the per-site local entry as
well as the global one, or a cross-file consumer reads a dangling variable as `Unknown`.

Limits: recursion through `<<-` stays a silent `Unknown` (the enclosing-slot join makes a placeholder
ambiguous); letrec visibility is per binding, not per block (a block letrec is a possible extension).
The rule runs on the local-assignment path in both context and context-less inference, and the fixture
driver pre-binds the global slot. Top-level mutual recursion resolves through
the interface fixpoint, which pins a genuinely cyclic scheme to `Unknown`.

## Bare-name resolution is not gated on NAMESPACE imports

**Status:** built.

A bare name that resolves only through the shipped stub corpus (`median` against stats) resolves
unconditionally, without `importFrom` or `library()`. The shipped namespaces (base, stats, utils,
methods, graphics, grDevices) are exactly what R attaches in every default session, and a project
`.Rtypes` stub already declares the names it uses, so gating would make the user say it twice. Per-file
visibility for non-default packages, masking warnings and `library()` attach order need the full import
model, and gating falls out of it. Until then the NAMESPACE surface is import validation (typos against
the corpus) and the opt-in `unused-import` lint.

## The shipping stack is a hand-written parser, a lossless tree, and salsa

**Status:** built.

`development/architecture.md` describes the architecture; the entries below hold the reasoning and the
risks.

## Shipping stack: Why the previous stack was replaced rather than tuned

**Status:** built.

The tree-sitter stack had four structural limits, each already mitigated in place: tree-sitter capped
error quality (opaque ERROR nodes), cost about 8 µs per line (about 2.5 s of a 3.7 s cold pass at 302k
lines was parsing), built trees about sixty times the source (forcing rope-only input and a parse LRU),
and could not see `#:` annotations (forcing a re-lexing subsystem); the analysis unit was the whole file,
making whole-file re-inference the keystroke floor and file-granular edges manufacture cluster SCCs;
`CoreType` was a deep-cloned enum whose churn capped inference near 6 µs per line;
and the engine was single-threaded. They rank in that order of impact after granularity: file-granular
analysis, type churn, parse cost and size, threads (only a division by core count). The ultimate ceiling
is R's dynamic semantics.

## Shipping stack: Why a lossless tree, and why ry's own

**Status:** built.

The parser is hand-written and emits a lossless concrete syntax tree, not a span-carrying AST:

- **Losslessness.** `#:` comments are type syntax, so the formatter and annotation tooling need no side
  tables.
- **Error resilience.** Error nodes are local to the break.
- **Position independence.** An item's subtree compares with item-relative offsets, so it stays equal after
  an edit elsewhere; absolute spans would kill sub-file incrementality.
- **Cheap splicing.** A statement-splice reparse copies the untouched top-level runs of the previous tree
  and parses only the edited middle.

Cutoffs use value equality (kinds, subtree ends, item-relative offsets), never pointer identity, as
rust-analyzer's `ItemTree` and `AstIdMap` barriers do. The checker runs on per-item HIR, never the
dynamically kinded tree, which serves the IDE, formatter and refactorings; parse is a pure per-file
query and correctness never depends on splicing. Layout: "The syntax tree is ry's own flat preorder arena".

## Shipping stack: Why salsa rather than an in-house engine

**Status:** built.

salsa provides what would otherwise be hand-rolled: parallel snapshot reads with write cancellation, and
first-class fixpoint cycles (proven in rust-analyzer and Astral's `ty`). Managed risks:

- **Pin the version and upgrade deliberately**; the API breaks several times a year. Vendoring or forking
  is the exit.
- **Fixpoint non-convergence panics at 200 iterations.** The pin-to-`Unknown` rule lives in the cycle
  function; reaching the cap is a bug, never a fallback.
- **Interned-value GC is young** (rust-analyzer used about four times the memory until it tuned `lru`,
  and `ty` hit multi-gigabyte blowups).
  Per-query `lru` and interned GC are the first levers when memory regresses.
- **Parallel iteration over a fixpoint had hang bugs** upstream; a parallel-cycle stress test covers it.

## Shipping stack: Item granularity and item identity

**Status:** planned. Built so far: top-level items with insertion-stable identity; no nested definition is an item.

The analysis unit is the item, and a nested definition (a field or method in `R6Class`, `setRefClass` or
an S4 block, a function inside a function) should be one too, or R's giant single-statement
object-oriented files degenerate to whole-file granularity. Item identity hashes kind and name plus the
parent with an index disambiguator, never a bare position, so an insertion does not shift unrelated
identities (rust-analyzer's `AstIdMap` design; its index-based predecessor shifted).

## Shipping stack: An annotation is one concept with pluggable spellings

**Status:** built.

Both spellings lower through `crates/semantics/src/annotations.rs` into one `Annotation`. `#:` is
structured trivia, and a `#:`-annotated file must always remain valid R; the inline spelling is the
superset dialect (next two entries).

## Shipping stack: The inline annotation syntax is a superset of R

**Status:** built, except `declare(type(...))` as an annotation spelling: the checker does not read it.

The mandate (inline syntax on by default, a warning when off) only makes sense for a syntax GNU R cannot
parse, so the inline form is a superset of R (costed in `development/design/inline-type-syntax.md`), and
typed files keep the `.R` extension because ry executes them directly. The cost is that other R tools fail
on a typed file, so code that must run on GNU R goes through a strip step that moves inline types into
`#:` comments (`development/design/r-runtime.md`, section 8).

## Shipping stack: How inline annotations are built

**Status:** built.

Syntax and docs: `reference/type-system.mdx#inline-annotation-syntax`. Every `TYPE` is parsed by the `#:`
type grammar's own functions on R's token stream, so there is one type grammar.

- **Binding recognition.** `name: TYPE <- value` is recognized only at a statement start, by a shallow
  scan for `<-` or `=` outside brackets where only `(`, `[`, `{` carry past a line break (`<`, `>` are
  comparisons), so `x:y`, `x:y < -1` and arguments stay ranges. Rejected: `declare(type(...))` (a runtime
  call per binding, no place for parameter or return types), `let x: T` (a keyword), `x <- value :: T`
  (`::` is the namespace operator), `function(x): T` (`->` mirrors `fn(...) -> T`).
- **Erasure.** Deleting the annotation nodes gives plain R (`syntax::erase_inline_annotations`); the
  tree-sitter oracle judges the erased text, and `check_parse_invariants` pins clean, idempotent erasure.
- **Trivia.** `Parser::deferred_trivia` keeps trailing whitespace and comments outside the type node.
- **One meaning.** An inline site lowers to the same `Annotation` as a `#:` block
  (`lower_inline_annotation`); signature resolution is shared with the runtime (see "A function has one
  signature, resolved in `syntax::annotation`"). A signature attaches to the statement assigning its
  function (merging with its `#:` block, exporting a declared scheme at an item root), or else to the
  function expression, which is how a lambda argument is checked; a binding's own `: TYPE` is checked
  against the function's result (`check_item_with_annotation`).
- **Both spellings on one statement** merge per slot (value, parameter, rest, return), type parameters
  pairing by position, the inline side winning; a coercion (`@trust`, `@if-unknown`, `@new`) governs
  the value wherever it is written, the inline one winning if both have one (`merge_annotations`).
  Disagreement is judged separately in `annotation_agreement_diagnostics`, on file-absolute ranges after
  alias expansion and union sorting, so the warning points at both written types.
- **Turning it off** is a salsa input (`AnalysisSettings`), because it changes what types mean; it is read
  only where an inline site exists (`test_incremental.rs`). Off, sites type nothing and each warns
  (`inline-annotation`), and hosts warn once per load.
- **Strict mode reports untyped parameters** (`noImplicitAny`-style): an unannotated parameter typed
  `Unknown`, or a fresh unconstrained variable of its own function's level that nothing else mentions (a
  rule on `Unknown` alone never fires, since unifying with `Unknown` binds nothing). The level check keeps
  a closure parameter tied to an enclosing function clean; lambda arguments are skipped because the call
  types them only after inference. Returns and bindings get no finding of their own, since each
  undetermined one traces to an origin, a type error or a definition ("one undetermined value, one
  finding").
- **Optionality** of an inline-only parameter comes from the formals (`Annotation::inline_parameters`),
  since inline has no `[name]`; a parameter a `#:` block names keeps the comment's `[name]`.
- **Known gap.** A generic lambda argument keeps its binders rigid, so
  `lapply(xs, function<T>(v: T) -> T v)` is reported (`backlog.md`).

## Shipping stack: Inline type definitions, coercions and nominal constructors

**Status:** built.

Syntax and docs: `reference/type-system.mdx#inline-annotation-syntax` (`type Name<T: c> = TYPE`,
`alias Name = TYPE`, `x: @trust T <- v`, `@Name(value)`).

- **Unambiguous.** Two names in a row and a leading `@` are not valid R. `type` and `alias` are
  contextual (recognized only at a statement start with the defined name on the same line), so
  `type <- 1` and `alias(type)` work. At a line start `@Name` is a constructor only before `(` or `<`, so
  `obj` newline `@slot` keeps R's error; after an operator a bare `@Name` is one, so completion works
  while typing. A nested definition is an `annotation` error, because a type's scope is the file. Rejected: `@type Name = T` (collides with the
  constructor) and `newtype` (a second keyword for no new meaning).
- **Leading `|`.** A union may open with `|`, and inline a union continues onto a line starting with `|`;
  no R statement starts with `|`.
- **One meaning.** A definition lowers to the same `NamedDefinition` as its `#:` block; consumers read
  `file_type_definitions`. A `#:` definition directly above a same-named inline one is one definition
  written twice (`paired_comment_definition`): inline on, the inline one wins and any disagreement warns
  `annotation-mismatch`; inline off, the comment applies, and an unpaired inline definition declares its
  name with an `Unknown` body, so its uses do not add unknown-type errors to the `inline-annotation`
  warning.
- **Erasure.** A definition erases line for line to its `#:` comment, so it must end its line; a paired
  one erases to nothing (two copies would be a duplicate). `@Name(value)` erases to `(value)` and a
  coercing binding's `: @trust T` is dropped. The runtime skips definitions; a nominal has no run-time
  representation.

## Shipping stack: Testing doctrine for `syntax`

**Status:** built.

The parser is the foundation, so all seven layers apply and duplicated coverage is never pruned:
tree-sitter-r's corpus as fixtures; a real-world corpus (base R sources plus top CRAN packages) for round
trip and acceptance parity; exhaustive per-construct suites with golden trees and error messages;
property tests (tokens cover input, ranges nest, reprint equals input); fuzzing (random and
structure-aware, never-panic and always-lossless, bounded pass in the crate's tests); statement-reparse
equivalence against a from-scratch parse; and acceptance cross-check against R's parser where R exists.

## Shipping stack: Cases where R parsers get subtle

**Status:** built.

Cover exhaustively: raw strings (`r"(...)"`, `R"[...]"`); escapes; `%op%`; backtick names; multi-line
`#:` blocks stitching into one region; newline-versus-operator continuation; `]]` against `] ]` in
`x[[y[1]]]`; top-level `else` after a newline (legal in braces, an error at top level); `->` and `->>`;
`=` as assignment against named argument; `-2^2` is `-(2^2)`; hex, `L` and `i` literals; `\(x)` lambdas.

## Shipping stack: Corpus mechanics

**Status:** built, except "CI has no R", which "CI runs `just gate` against GNU R 4.6.2, and nothing ignored" supersedes.

Fetch tree-sitter-r's `test/corpus/` (MIT) from its GitHub repository, not the crates.io package, which
may omit tests. The real-world corpus (base R sources plus about the top 100 CRAN packages) lives in a
gitignored directory with a committed manifest and fetch script in `scripts/`; the fetch needs outbound
network. The acceptance-divergence allowlist is adjudicated against R's `parse()` and committed.

## Shipping stack: Better syntax errors are a goal, not a side effect

**Status:** built.

Recursive descent knows what it was parsing, so the bar is expected-token sets ("expected `)` or `,`"),
paired-delimiter pointers carrying both spans ("unclosed `(` opened here"), statement-anchored recovery,
and real type-syntax errors with exact spans inside `#:` comments ("expected a type after `|`"). The
golden error-message suite pins the wording.

## Deep resolve cuts a cycle to `Unknown` and resolves a shared subtree once

**Status:** built. The table of finished subtrees lives for one call, as "Types and symbols are four-byte ids into append-only columns" describes.

`InferenceTable::resolve` (`infer.rs`) walks the type graph with explicit cycle detection and resolves a
shared subtree once per call. A depth-capped recursive walk was rejected: it re-resolved shared subtrees
per occurrence (397 million inner steps on a 507k-line corpus, against 4.2 million for the graph walk)
and silently left types past depth 64 unexpanded, a position-dependent semantics no cache can sit on.

- **A cycle cuts to `Unknown`.** A `visiting` stack of variables under expansion detects an infinite type,
  matching the pin-to-`Unknown` rule of the fixpoint cap and loop widening. Alias expansion keeps a depth
  guard purely as a resource backstop.
- **Only a clean subtree is kept**, keyed by interned type, once a walk has taken 64 steps, and only if no
  cycle cut lies beneath it (a node containing a variable under expansion resolves differently at top
  level). Steps are counted, not read from the type's size, because a variable has size one whatever it
  binds: variables each binding a tuple holding the next twice walk 2^depth steps without the table
  (`infer::tests::a_subtree_shared_through_variables_resolves_once`). Most calls end within a few steps,
  where a table costs more than the walk.
- **Nothing outlives the call**, since bindings change between calls and a longer-lived table needs an
  invalidation rule. A type whose id says it holds no variable and applies no alias returns without a
  walk, which covers most repeated calls.

`RESOLVE_CALLS` stays as an instrument: a near-linear step count is an invariant the performance harness
watches.

## The interface fixpoint is canonical per group, so a cyclic scheme does not depend on forcing order

**Status:** built, except `interface_sccs` and the per-item `interface_group` walk: groups come from one walk per file ("Every host resolves names through one path, and trees live in the host's store"), and an item reads its edges from `item_links`.

With dynamic salsa cycle recovery alone, whichever member was queried first became the cycle head, so
which items pinned to `Unknown` at the round cap depended on query order (hover-then-check and
check-then-hover disagreed; on the corpus, pre-forcing per-file phases gave 64852 findings against 64835
for direct forcing, stable across thread counts, so it was query-order semantics, not a race). A cyclic
interface group therefore resolves through an explicit canonical fixpoint in
`crates/semantics/src/semantics.rs`:

- The graph has exactly the edges a check follows, resolved by the check's own binder (nearest earlier
  writer, a script's last writer, the test layer, the package winner, conditional writers up to the join
  cap), condensed in canonical order (file order, then item order); only cyclic groups (more than one
  member, or a self-edge) are recorded. A winner-only graph that missed statement items, earlier writers,
  script and test files and conditional writers was the bug that surfaced this rule.
- `scc_exports` runs Jacobi iteration: all members start at `Unknown` and each round re-checks every member
  against the previous round's table, so order cannot matter. A group still changing at the round cap
  (`SCHEME_ROUND_CAP`, 16, shared with the backstop) pins all members to `Unknown`. Member checks run
  through `SalsaGlobals`, so no salsa cycle forms.
- `item_check` adopts the canonical exports as a member's scheme, and `global_scheme` reads only
  `item_check`, so there is one source of truth.
- Salsa cycle recovery (`item_check_recover`, `global_scheme_recover`) is a backstop, and reaching it is a
  bug: salsa never backdates a cycle member, so restoring a recovered cycle panics
  (`removing_a_cross_file_cycle_and_restoring_it_revalidates` in `test_incremental.rs`).

Pinned by `cyclic_group_answers_are_forcing_order_independent` in `crates/semantics/tests/test_parallel.rs`.

## There is no third constraint kind, so two flexible comparison operands stay unconstrained

**Status:** built, except the same-family error on two concrete operands, which "Comparisons coerce across atomic types" supersedes, and the two-kind limit: `Constraint` has `Key`, `Number`, `Eq` and other kinds.

Two flexible operands of a comparison stay unconstrained: `function(a, b) a < b` infers
`<T, U> fn(a: T, b: U) -> logical`, and a cross-family call is accepted. R coerces across atomic families
(`1 < "2"` compares as characters), so tying the operands to a family or to each other rejects legal
programs. A "comparable" constraint was rejected: it would be nearly vacuous (every atomic family is
comparable) and is the traits tripwire. Pinned by `two_flexible_comparison_stays_unconstrained`.

## Union compatibility commits a flexible argument at first use, in program order

**Status:** built.

A flexible argument checked against a union parameter binds to the whole union at that use, as
unification would: after `f(v)` with `f : fn(x: integer | character)`, a later `g(v)` with
`g : fn(x: logical | character)` errors at its own site, although `character` would satisfy both. The
escape is an annotation naming the intended member. Order-free intersection solving was rejected: it needs
an intersection constraint former (the traits frontier), and the order only decides which site is blamed,
never whether an inconsistent pair errors. Program order is R's evaluation order, so the blamed site is the
first that would misbehave. Pinned by `flexible_argument_commits_to_the_union_at_first_use` and
`union_commit_blames_the_later_conflicting_use`.

## Strict mode attributes a recursive binding the fixpoint cannot fully type

**Status:** built.

The interface fixpoint types converging recursion precisely (`fact` exports `fn(n: integer) -> integer`).
A growing self-reference pinned at the round cap already surfaces through the undetermined-reference
origin at the recursive read. A cycle that converges with `Unknown` embedded (`f <- function() f()` gives
`fn() -> Unknown`) fires no origin, because the read sees a function type. So after `item_check` adopts the
group scheme, a `StrictOriginKind::RecursiveUnknown` origin is recorded on the binding when the body
produced no errors or other strict origins and the scheme still `contains_unknown`. The clean-body gate
keeps one attribution per undetermined value. Accepted over-report: a clean member whose `Unknown` comes
from a sibling's origin is still attributed, since detecting it needs group-wide origin bookkeeping and
annotating the member closes its export anyway.

## Script frame semantics

**Status:** built, except a conditional top-level slot typing `Unknown`, which "Conditional slots type, export edges generalize, and `missing()` flows through the environment" supersedes.

A script's top level is one frame run top-down, with one rule per kind of read:

- **An immediate read** resolves to the nearest earlier top-level binding, before package globals and
  stubs. A use before every definition is unresolved, including `x <- x + 1L` with no earlier `x`.
- **A deferred read** (inside a nested function) resolves against the whole document, because the closure
  runs after the frame settled: the last binding wins (the `deferred` flag on the checker's lookups),
  including the enclosing statement's own, so self-recursion types through the cycle fixpoint
  (`a <- function() a()` exports `fn() -> Unknown`).
- **A conditional top-level write** (in a top-level `if`, `for`, `while`, `repeat`) creates the document's
  slot, and a later read resolves to it.
- **A quiet read** (data masking, an opaque operator) is never reported unresolved but counts as a use and
  navigates, because at run time it falls back to the enclosing binding.
- **Unused check.** A deferred read keeps every binding alive; an immediate read marks definers backward
  through conditional ones, since a conditional rebinding does not end an earlier binding's liveness. A
  loop that reads its carried variable keeps both its own write and the earlier binding alive, because
  the first iteration reads the outer one.

A duplicate `@type` or `@alias` name errors at every site. The per-file slot order is
`frame_slot_positions` in `crates/semantics/src/diagnostics.rs`.

## An undeclared type name errors once at the reference and compares like `Unknown`

**Status:** built.

- **Recording.** `annotations::lower_annotation` records every `Named` mint with its token range in
  `Annotation::nominal_references`; primitives and in-scope binders are resolved first, so binder scoping
  stays single-sourced.
- **Reporting.** `unknown_type_diagnostics` checks references against the project's `@type`/`@alias`
  declarations (plus the file's own, for a script) and the stub corpus's nominals, and errors at the token
  with a nearest-name hint. Forward references are legal; the vocabulary is position-independent.
- **Tolerance.** An undeclared nominal compares like `Unknown`: `unify`, `compatible` and the operator
  checks' `structural()` all consult one `undeclared_nominal` predicate, so the typo is reported exactly
  once and never cascades.

The declared-annotation check gates on a negative list (`Unknown` and `Any` only), not a positive list:
the positive list silently skipped `Named`, `Record` and `Tuple`, letting `#: Point` mint a nominal without
`@new`.

## An annotation shape violation refuses the whole block

**Status:** built.

- **One refusal semantics.** A block with any shape violation (directive order, duplicate or unknown type
  parameters, applied binders, `@new` payload shape, nesting caps, vector-element atomicity, attachment)
  keeps only its errors (`Annotation::errors`, `typing_errors`); `lower_annotation` drops the declared
  type, definitions, `@new`, `@strict` and nominal references, so one mistake gives one error. Consumers
  observe the absent payload, not a validity flag: an inlay hint gates on a surviving declared type,
  `@new` or trust, so a refused binding hints its inferred type again.
- **Attachment is single-sourced.** `statement_annotations` (`crates/syntax/src/annotation.rs`) classifies
  each block as attached, blank-line-separated or dangling; `item_annotation_syntax` and the
  dangling-annotation diagnostics both read it. A blank line or interposed comment detaches.
- **Two classes gate differently.** Past 160 nesting levels the shape is refused and always reported; past
  128 the type is refused for checking, a typing-class finding that `# typing: off` hides. Vector-element
  atomicity is judged later with the project vocabulary (`Annotation::vector_elements`: an alias expands,
  a nominal refuses, an undeclared name is left to the unknown-type error) in the typing class at the use
  site, and does not strip the payload, because lowering lacks the vocabulary. The accepted difference is
  visible only in exported schemes.
- **A definition is top-level only.** A nested `@type`/`@alias` errors and does not enter the vocabulary
  (`file_type_definitions` reads top-level children only).

## A statement-level annotation attaches at any depth and applies where the expression infers

**Status:** built.

- **Association.** `statement_annotations(parent)` runs over any statement sequence, file root or braced
  block, so root and nested attachment and dangling diagnostics share one rule. `expression_annotations`
  maps each block in an item to the annotated expression's HIR id by exact range; it is a plain function,
  not a tracked query, because `Annotation` carries text ranges and its callers already depend on
  `item_syntax` and `item_hir`.
- **Application.** One checker seam, `apply_expression_annotation`: an assignment applies it before the
  slot write; any other expression where it infers; a non-assignment item root routes through it too.
  `@new` reuses `check_new_nominal`, a checked type enforces directional `compatible`, `@trust` overrides
  unchecked.
- **IDE.** Hover and goto-type-definition read the recorded expression types; an inlay hint skips an
  annotated nested binding.

## Capture liveness is frame-scoped

**Status:** built.

A read inside a nested function keeps every write of the name alive in the frame the read resolves to,
and in no other, which is R's environment semantics: sequential rebindings in one frame are one variable,
and a shadowed outer binding is not what the closure reads, so it can be reported dead. A frame has a
stable `Scope::id` minted per defining expression (a loop re-walk reuses it); each write records its
owning frame, and the capture sweep filters on frame and name. A write after the read is covered by the
per-slot `captured_slots` marking.

- `environment()` and the rest of `EXPLICIT_NAME_ACCESSORS` read every name of their frame.
- The `duplicate` rule drops a pair when the earlier value is read eagerly between them or a `setGeneric`
  call sits between them (`earlier_value_read`); a deferred read does not count.
- Implicit reads that exist only for the unused check (`tail_assignments`, `escaped_frames`,
  `mark_formals_read`) stay in naming's slot reads and never enter `captured_slots`, which typing uses.

## Conditional slots type, export edges generalize, and `missing()` flows through the environment

**Status:** built.

- **A conditional top-level slot types.** `ItemCheck::top_level_bindings` carries the settled,
  export-closed scheme of every name an item's top-level frame binds; `statement_binding_scheme(item,
  name)` projects it per binding as a value-equality firewall with cycle recovery (a statement item can
  read its own conditional write). Readers consult it after `package_definitions`, joined across writers,
  and in the script sequential search; an unconditional definition still shadows the slot.
- **The export edge generalizes a constrained residual.** `close_scheme` turns an unbound variable carrying
  a constraint into a fresh scheme binder and erases only an unconstrained one to `Unknown`; erasing all
  of them lost bounds such as `invoke(mirror)`'s numeric one. Instantiation is per reader, matching R's
  call-by-call semantics for immutable closures.
- **`missing()` flows through the environment** as `EnvEntry::MissingFormal` on the true edge of
  `missing(name)` for a formal with no default, riding the existing branch mark, rollback and join
  discipline rather than a parallel liveness structure: reading it errors, any write supplies it, and at a join possibly-missing reads as the
  supplied type, so only a definite failure reports.

## NAMESPACE and DESCRIPTION metadata feed resolution

**Status:** built.

`metadata::PackageMetadata` is a singleton input of normalized, sorted, deduplicated import pairs
`(namespace, Option<name>)` plus the DESCRIPTION dependency set, installed beside `StubSources`. The
NAMESPACE parser lives in `semantics::metadata` as the single source of truth; hosts render problems. The
server refreshes it on NAMESPACE buffer syncs and NAMESPACE/DESCRIPTION watcher events, diffing parsed
facts so a formatting edit invalidates nothing.

Consumption is predicates at the diagnostic edges only: `imported_by_name` and `imports_every_name` quiet
an unresolved bare read, `declared_dependency` quiets the unknown-namespace warning on `pkg::`. They run
only after every cheaper skip fails, so only on a genuinely unresolved name. Typing is
untouched (an imported but undescribed read is `Unknown` with the usual strict origin). An `import(pkg)` of
a namespace without stubs quiets every unresolved bare read in the package, because its export set is
unknowable and guessing breaks the zero-false-positive mandate; stubs for `pkg` restore exact gating.
Metadata only widens the resolved universe. Pinned by the `typing-imports` fixture suite.

## Data.table awareness

**Status:** built, except "column knowledge is deliberately not modelled" for a `data.table` whose columns are known: `data.table{...}` tracks them.

- **The stub namespace is conditional.** `types/data.table.Rtypes` carries the `@type data.table` nominal
  and the high-traffic declarations, and joins `stub_library`'s fold only when
  `metadata::namespace_active` says the project uses the package: a DESCRIPTION dependency, a NAMESPACE
  import, or a literal `library()`/`require()`/`requireNamespace()`/`loadNamespace()` in any file
  (`metadata::file_attached_namespaces`, a per-file syntax scan hosts union into
  `PackageMetadata.attached`: the CLI once after load, the server per synced file plus the idle prime,
  never a per-keystroke sweep). Gating at assembly gives every consumer (bare resolution, `pkg::`
  validation, nominals, completion, shadow lints, typo suggestions, masked verbs) one universe; while
  inactive, data.table is an undescribed package, so its names cannot steal a typo warning. Activation
  keys only on inputs, never on a naming or item query, which breaks the naming-to-stubs-to-naming cycle.
  A flip rebuilds the stub library.
- **A result-shape classifier in `infer_index`.** A bracket whose subject is the `data.table` nominal
  (shipped or project `@type`) keeps the subject's class for a missing or empty `j`, a `:=`, `.()` or
  `list()` call, or any `j` with `by =`/`keyby =`; another `j` shape is a sound refusal (`Unknown` with a
  strict origin). The result-class table is the contract in `reference/type-system.mdx`.
- **Masking uses the typed subject.** That classification records reads under the index arguments
  (nested closures included, since they are created in the data's frame) in `ItemCheck::masked_reads`,
  which the unresolved renderer skips, so `DT[speed > 20]` goes quiet exactly when the subject's class is
  known. Naming's syntactic recognition still handles an unknown subject.

## The native pipe desugars at lowering

**Status:** built.

R's parser rewrites `x |> f(y)` to `f(x, y)`, so desugaring is R's definition, not an approximation.
`hir::lower_pipe` intercepts `PIPE_GREATER`: a call on the right lowers with the piped value as first
positional argument, or, when `_` is the whole value of exactly one named argument (`pipe_shape`, a
syntax scan), substituted there; `_` never lowers. Everything R rejects (a non-call right-hand side, a
positional, repeated or nested `_`, `_` as a tag) keeps the opaque-operator lowering, sound silence rather
than a guess. Naming, typing, strict mode and the IDE inherit the real call; blame on a bad piped value
lands on the left-hand range. magrittr's `%>%` is opaque: it is a real function with dot substitution,
not parse-time sugar.

## `@masked` is formal-aware, and dplyr is a conditional namespace

**Status:** built.

`StubLibrary::masked` maps each masked verb to its formals declared before `...` (from the lowered
`FunctionType`). Naming resolves an argument normally when it matches a leading formal by position or
name and masks what `...` absorbs; an empty formal list masks everything (`join_by(x == y)`). The base
family keeps one data argument through its real formals `data`/`x`. Hardcoding the first positional
argument as the data broke zero-formal masks and named data arguments.

`dplyr.Rtypes` is in `CONDITIONAL_NAMESPACES`, with class-preserving verbs (`<T> fn(.data: T, ...) -> T`;
a join preserves the left class) and the tidy-select helpers and verb vocabulary (`n()`, `row_number()`,
`if_else`) declared so they resolve inside a mask. Where a dplyr name collides with an attached stub name
(`filter`, `lag` in stats), source order makes dplyr win exactly when it is active, matching R's attach
shadowing.

## A vendored export manifest carries the name-level truth beside the typed stubs

**Status:** built.

Every namespace R ships pairs with a generated `types/<ns>.exports` manifest of its full export list
(`scripts/export-manifests.R`, run in a live R session; the header records the R version), because
chasing completeness with typed declarations does not scale: the typed corpus had about 530 declarations
against about 2,500 real exports (base alone exports about 1,400). `datasets` uses the search-path
listing, because its objects are lazy data, not namespace exports. Manifest names join
`exports_by_namespace` (so `pkg::name` validation and shadow lints see them) and `known_exports`
(consulted by `package_scheme_exists_in` after schemes and nominals); they resolve everywhere a typed name
does, including completion and typo suggestions, but type `Unknown`. A non-syntactic name is excluded
from bare completion and backtick-quoted after `pkg::`.

Three tiers mirror R: default-attached namespaces (including `datasets`) are bare-visible
unconditionally; shipped but unattached ones (`QUALIFIED_ONLY_NAMESPACES`: tools, parallel, compiler,
grid, splines, stats4, tcltk) validate `::` but gate bare visibility on an attach or declaration; a
conditional CRAN namespace gates both, with its stubs. A unit test asserts every `.Rtypes` value
declaration is a real export of its namespace (`@type` nominals exempt, and a conditional namespace may
override a base name, as data.table's class-preserving `merge` does). An agent container can install
real R (apt plus the CRAN repository, minutes), so manifest regeneration runs in-container.

## One source of truth for where an item spells its name

**Status:** built.

`item_spans` classifies each top-level node through `top_level_definition`, which returns the item's
kind, name and the range spelling the name (the assignment target, or the `setGeneric` name inside its
quotes). `semantics::item_name_range(db, item)` re-reads that range off `item_node` at the rendering edge,
so item identity stays position-free; the IDE, duplicate-definition and unused checks all read it instead
of their own heuristics (which highlighted whole statements for `x <<- 1` or `setGeneric`). `:=` is not a
binding spelling: neither R nor the HIR (a call to `:=`) defines a name. A project-defined generic is one
global symbol, so its `setGeneric`, `setMethod` and `standardGeneric` strings and calls navigate,
reference and rename together.

## A check reads per-name firewalls, never project-wide maps

**Status:** built, except `interface_group` and the per-name lookups for names an item spells, which "Every host resolves names through one path, and trees live in the host's store" replaces with `file_interface_groups` and `item_links`. `package_name`, `test_name` and `frame_binder` serve the names a check constructs.

`item_check` never reads a project-wide map (`package_definitions`, every winner), because a new or
renamed definition anywhere would re-run every check. Each cross-item read goes through a small tracked
projection that backdates when its answer is unchanged: `package_name(files, name)` (winner plus
conditional writers) and `frame_binder(item, name, deferred)` (the file-local binder an immediate or
script read sees; a package function body never consults it). `SalsaGlobals` holds no map. `item_check`
grows its stack on demand through `stacker`, because demand recursion is as deep as the longest chain of
definitions that read the next. `crates/semantics/tests/test_incremental.rs` pins that a new definition
re-checks only its readers and an inserted statement only the reads it intercepts; the keystroke probe
cannot see this loss, because it edits one file and demands only that file. Cost: resident memory 3.5%
higher at 794K lines (1,270 to 1,314 bytes per line), cold time unchanged. Open gap (`backlog.md`): a salsa-level cycle through statement items
re-iterates on every structural edit.

## Cold-pass memory and time: interned names, compact HIR, parallel setup

**Status:** built, except the per-thread rowan token cache and the boxed-slice HIR lists, which "The syntax tree is ry's own flat preorder arena" and "HIR as struct-of-arrays, lowered from the green tree" supersede.

- `hir::Symbol` is a process-wide interned name, a `Copy` 4-byte index, carried by HIR, naming and the
  check output, because a name like `x` appears hundreds of thousands of times.
- `ItemCheck.expression_types` is a dense table indexed by `ExprId`, since ids are dense within an item.
- `ry check` forgets the database instead of dropping it (`std::mem::forget` in `crates/ry/src/cli.rs`):
  the drop only walks every allocation before exit.
- The binary uses mimalloc as a default feature; massif sees only the system allocator, so profile memory
  with `--no-default-features`.

## Syntax trees are evicted, and only small syntax facts are memoized

**Status:** built, except tree eviction and `RootDatabase::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes. Kept for the rule that tree readers are memoized queries with small results.

- Every tree reader is a memoized query with a small result (`file_error_ranges`,
  `file_annotation_references`, `agreement_candidates`, `lints::file_syntax_lints`, `item_annotations`),
  so no consumer holds a tree for a few facts.
- A fact that also needs naming or project declarations splits into a naming-free memoized syntax half
  and a cheap join (`SyntaxLints.unless_redefined`, `SlotPair`'s `same_type` step), so the syntax half can
  be computed before naming.
- `diagnostics::derive_from_syntax` lists every tree-derived fact a later pass reads; a new tree reader
  must be added there, or the cold check re-parses (the one-parse test in `test_incremental.rs` catches it).
- Source text lives once, in `SourceFile`; the CLI reads it through `ProjectAnalysis::source`.
- The server enters workspace files at `Durability::MEDIUM` and lowers a file to `LOW` on its first edit
  (restored on close), so memos over untouched files verify without walking their dependencies; settled
  re-check time had been two thirds salsa memo verification.

## A call's result drops unconstrained type-parameter members of a union

**Status:** built.

For `Result.and_then : <A, B, E, F> ... -> Result<B, E | F>`, a step that never fails leaves `F` open, so
the result would render `:Err(:InvalidNumStr | ?)` and could later bind to anything.
`Checker::callback_outputs` picks the instantiated variables that occur in parameter types only as a
callback's return; `Checker::drop_unconstrained_members` removes those still unbound after the call that no
older variable reaches. A union of only such members stays. Dropping every open instantiated variable was
wrong: it typed `Find` in an unannotated function and every return-only generic (`<T> fn() -> T | NULL`)
as `NULL` (fixtures `an_open_parameter_filled_by_an_argument_is_kept`, `a_return_only_parameter_is_kept`
in `standard-library.R.test`). Contract: `type-system.mdx`, "Union normalization".

## The cold check searches green trees and caches globals per item check

**Status:** built.

- `syntax::matching_nodes` filters with borrowed `NodeRef` cursors and makes owned nodes only for matches;
  `NodeRef::tokens` reads token text without owned cursors (`testing::check_parse_invariants` pins both to
  the owned walks). With the flat tree an owned cursor is only a refcount bump, so the gain is small.
  `map_child_types` returns the input type when no child changed.
- `SalsaGlobals` caches global lookups per `item_check` execution in `RefCell` maps. This is safe for salsa
  because the first read in a query records the edge and repeats only dedupe; the cache never outlives or
  leaves one execution.
- mimalloc's eager arena commit made the kernel zero transparent huge pages the process never used;
  `crates/ry/src/main.rs` turns it off from an `.init_array` constructor before the first allocation
  (peak RSS about 790 to 520 MiB).
- `infer::RESOLVE_CALLS` is thread-local: a shared atomic was a contended cache line.

## Call checking follows R's lazy argument and matching rules

**Status:** built, except the rule that a branch's write counts only when every arm makes it, which "A conditional write of a formal covers the paths that run it" supersedes.

- **Required formals.** A formal is optional unless the body may force it (`check::FormalUses`, in
  evaluation order): a read no write dominates forces; a loop body's or a `&&` operand's write never
  dominates, and `<<-` never assigns the local formal; `missing()`, `substitute()`, `quote()` arguments do
  not force; reads in nested functions do; a forced default forces what it reads, to a fixpoint.
  Requiring every defaultless formal rejected valid R (`f(1)` for `function(x, unused) x`). Inferred and
  declared types share the walk, so a declared `[name]` is legal over an unforced formal, but laziness
  only relaxes inferred signatures: `fn(a: integer, b: character)` over `function(a, b) a` keeps `b`
  required, because the annotation is the contract. The message names the missing formals, not a count.
- **Partial matching.** `argument_targets` runs R's middle pass: an unclaimed name that prefixes exactly
  one unclaimed formal before `...` claims it. Case-sensitive, so `x =` goes to `lapply`'s `...`.
- **`match.fun` strings.** A literal string at a `check::MATCH_FUN_CALLERS` base callee (never a project
  function of that name) checks as the named function via `CallArgument::named_function`, only where a
  function is expected; elsewhere it reads as `character`.
- **Variadic actuals.** `infer::pair_parameters` lets a variadic function serve a fixed interface, so base
  generics declaring `...` (`mean`, `as.numeric`) pass as callbacks and accept method arguments.
- **Forwarded `...`** to a callback parameter adds no parameter to the demanded type (it may be empty).
- **`trailing-comma` is a warning**: R fails on the empty argument only when the callee forces it.

## Called names are looked up as functions

**Status:** built.

A call head follows R's `findFun`: `Checker::function_lookup` skips a binding whose type is definitely no
function and continues through enclosing slots, the project namespace (or `GlobalEnv::enclosing_scheme`
past a test file's or script's top level) and the library, so `names <- toupper(names(x))` and cli's
`.Call <- NULL` work. An untyped binding the body only calls is a callback and stays the function. An
untyped binding also read as a value (`dir <- path; dir(dir)`) next to an outer function of its name types
the call `Unknown`. A unification probe was rejected: at the head the binding is still an open variable
any function unifies with, and the conflict appears only at a later value use. A binding typed by a type parameter counts as untyped, because skipping it
produces schemes the annotation grammar cannot write (`test_roundtrip.rs`). The reached binding is stored
once in `ItemCheck.callee_targets` for the IDE. `pkg::name` never uses this lookup: a foreign package reads
its stubs, the project's own its namespace (`GlobalEnv::namespace_scheme`).

## testthat files mirror testthat's environments

**Status:** built.

`DocumentKind::{Test, TestHelper}`: helper and setup files fill a shared test namespace
(`test_definitions`), each test file keeps its own bindings, and a closure in a test file prefers the
binder its own top-level statement writes, because each `test_that()` block is its own environment.
Treating tests as package files leaked bindings into the namespace; pooling all test files let one resolve
another's names. The kind is derived from the path once per surface (`cli::document_kind`,
`semantics::test_document_kind`). `test_name` is a per-name projection like `package_name`, and typo
suggestions come from the per-name `project_typo_suggestion` firewall, so a test-file edit invalidates no
package item (`test_incremental.rs`).

## useDynLib native symbols resolve only as a native routine's name

**Status:** built.

`metadata::NativeSymbols` parses `useDynLib` (explicit names, `.registration = TRUE` with its `.fixes`
prefix). Explicit names and `.fixes`-prefixed names resolve anywhere, since `useDynLib` binds them. A
registering `useDynLib` binds routines ry cannot see, so any other bare name resolves only as the `.NAME`
(or first positional) argument of `.Call`, `.External`, `.External2`, `.C` or `.Fortran`
(`naming.native_routine_reads`); a typo elsewhere still reports.

## Comparisons coerce across atomic types

**Status:** built.

A comparison accepts any mix of `logical`, `integer`, `double` and `character`, because R coerces both
sides to a common type; `complex` and `raw` are unsupported, and an open operand is never constrained
(`comparison_operand_shape` in `check.rs`). Requiring one family rejected ordinary R
(`Sys.getenv("RSTUDIO") == 1`, `x > 0` on a flag), and constraining an open operand to `numeric` leaked
through exported schemes into callers. Accepted cost: `while (n && out)` pins a counter `n` to `logical`,
because no comparison says "numeric" any more.

## A test-guarded write to a formal normalizes it; inferred types admit the default

**Status:** built.

- **Normalizing write.** In `infer_if`, a non-`NULL` write to a slot whose pre-if type is an open formal
  (`is_open_parameter`), under a condition reading the same slot (`reads_slot`), normalizes: the slot takes
  the written type after the `if` and the formal's variable is not unified. Unification pinned
  `if (is.logical(type)) type <- "q"` to `character`, and a union join cascaded `T | logical` everywhere.
  Other writes go through `join_writes`. Accepted unsoundness: the body after the test sees only the
  written type, because code after such a test assumes it.
- **Default widening.** A defaulted formal whose settled type does not admit its default's type widens to
  `settled | default` (probed with `compatible` under a snapshot).
- **Defaulted flags.** A condition or `!` on an unannotated defaulted formal does not constrain it to
  `logical` (`reads_open_defaulted_formal`): flags are passed `1`/`0` as often as `TRUE`.

## An open `T | NULL` parameter binds `T` to the argument's non-`NULL` part

**Status:** built.

In `compatible_inner` (`infer.rs`), when the expected type is exactly a two-member union of an open
variable and `NULL` and the argument is a variable or a union with no other tag, the variable member
unifies with the argument minus its `NULL` members, under a snapshot; otherwise a caller's open argument
became `U | NULL` and every later use was reported. The rule is deliberately this narrow: any union with a
variable member produced `logical[][]` cascades.

## `switch(typeof(x), ...)` narrows `x` per arm

**Status:** built.

`infer_switch` narrows the subject slot before each named arm to the types its labels and fall-through
labels name (`narrow_to_typeof_labels`, `typeof_label_membership`): a union keeps matching members, an open
variable gets a fresh one so one arm does not pin the parameter for the others. The narrowing is a
refinement, not a write, so it does not join back. `NULL = ...` is a legal label naming the string
`"NULL"` (`hir.rs`).

## Nested replacement rewrites the spine

**Status:** built.

`replacement_written_type` walks the accessor spine of `x[[1]]$a <- v`: each intermediate container is
projected (`project_replacement`), the innermost takes `v`, and each enclosing container takes the
rewritten child through the single-level `write_replacement_step`. An undescribable intermediate projects
as `Unknown`; a `[` or `@` step leaves its container unchanged.

## A strict library file's top level holds only definitions, and purity is declared

**Status:** built.

- **Rule.** `semantics::top_level::item_top_level` classifies each top-level item of a library file
  (`DocumentKind::Package`, under `R/`) as a definition, a constant or an effect, and `strict` reports the
  first effect per statement (`reference/type-system.mdx`, "A package file's top level"). Scripts, tests,
  helpers and the console are unrestricted. Class definition forms come from `object_system_function`, one
  table shared with the lint and the runtime refusal.
- **Purity is declared, never inferred.** Roc-style effect inference was rejected: R's higher-order
  functions take pure and effectful callbacks under one name, and an inferred effect would let one added
  `cat()` change every transitive caller's interface. `item_declared_pure` is a per-item `bool` from
  `#: @pure` or inline `@pure` (`PURE_MARKER`), so a body edit backdates it. The marker is trusted and
  means "no effect whatever the arguments". A project definition shadows the
  stub, and a name with a conditional writer (`CalleeOrigin::Conditional`) is never pure. A bare stub call
  takes purity from its winning declaration's namespace (`StubLibrary::declarations`).
- **Higher-order purity is a per-parameter stub marker.** `@pure(FUN)` (`StubLibrary::pure_by_namespace`
  keeps the names, empty for plain `@pure`) means pure when the functions passed at those parameters are,
  so `lapply` and purrr's `map` family accept a pure callback without inferring effects through R's
  higher-order functions. Arguments match by `check::argument_targets` against the first candidate,
  and a call whose winning stub comes from another namespace than its purity, or is no function, is an
  effect (fail closed). A callback is pure when it is a function literal whose defaults and body have no
  effect, or names a pure function; a computed callback is an effect. purrr's string, number and formula
  `.f` are extractors or bodies, judged by the caller's namespace. Inside `local()` purity is inferred for
  a function-literal binding, because no other item can call it and the read stays inside the item. The
  call, not the binding, is judged: `LocalScope::functions` records the literal and the depth of the
  environment that bound it, and a call walks the literal against that environment as it stands then
  (`LocalScope::outer` snapshots), which is lexical scoping with later rebindings seen. Any other write
  to the name (loop, destructuring, replacement) clears it. A recursion guard and a per-item walk budget
  (`LOCAL_CALL_BUDGET`, past which a call is an effect) bound the walk. `#: @pure` above anything but a top-level name binding is an
  `annotation` warning, since nothing reads it there.
- **Lazy constants.** In a strict file a single-writer constant is bound lazily
  (`load_index::package_index`) only when every package name its initializer mentions has one earlier
  writer in collation order, or a late evaluation could read a different value; `loader::diff` rebinds it
  when anything it reaches changes, and checks every name (not only dirty items') for a laziness flip,
  since laziness depends on other files. A `<<-` in any package function (`item_super_assigned_names`)
  counts as a writer after every top-level statement, because a locked namespace can only take it during
  `.onLoad` (over-approximating that call graph without building it). A replacement inside `local()` of an
  outer name is accepted when the outer binding is a literal or `c()`/`list()` of literals
  (`item_binds_plain_value`), since reading its inferred type would put inference on the load path.
- **Strictness reaches the loader** through `Loader::set_strict`, because the loader cannot read
  `ry.toml`: GNU R hosts read the planned root's config, and ry's runtime passes the `RUN_FLAG_STRICT` bit
  of `ry_run`, because libR links its own copy of `loader`. The loader installs the stubs and
  `ProjectFiles` only for a strict package, so non-strict loads never parse stubs. The conditional-writers
  half of classification skips `name <- function(...)` items without naming them, so strict planning is
  faster than a non-strict load (finance benchmark: about 0.8 s against 0.9 s).
- **Lazy R6 generators.** `name <- R6Class(...)` is bound lazily in any file when its arguments are
  literals, names, functions, and `list()`/`c()` of them (`top_level::TopLevel::Generator`). The rule is
  syntactic because non-strict loads install no stubs, so a purity rule would differ between `ry load` and
  `ry check`; a purity rule for non-strict constants was rejected because even `toupper` reads the locale.
  A generator counts only its immediate reads (`IndexedItem::reads`), not every mentioned name as a strict
  constant does: its methods read the namespace when they run, and R6 evaluates `inherit` late. On the
  finance code base all 199 generators stay promises, and an index-cache-hit load drops from about 0.4 s
  to about 0.29 s.
- `test_incremental.rs` pins that adding `cat()` to a pure function re-derives only it, and removing a
  marker re-classifies only its callers.

## Cold-check constant costs: one green walk per file, immortal interning, high durability

**Status:** built, except the salsa-interned `Name` and `Ty`, which "Types and names are hash-consed outside salsa" supersedes, and `item_frame_binders`, which `item_links` replaces ("Every host resolves names through one path, and trees live in the host's store").

- `file_syntax_sites` collects every per-file site in one pass, and `lints::syntax_lints` scans in preorder
  with borrowed cursors, making an owned node only where `may_lint` says a lint can fire.
- The duplicate-type map is read once per file, not once per annotation.
- Interned names and types are never garbage-collected: free in a one-shot check, and in the server the
  tables grow only with distinct structural types and names, which repeat heavily.
- The CLI creates every input at `Durability::HIGH` except `PackageMetadata.attached`, the one input it
  sets after analysis starts; low durability made every syntax-pass memo deep-verify.

## Types and names are hash-consed outside salsa

**Status:** built, with the storage superseded: a type is an id into columns, not a pointer to an entry in a sharded table ("Types and symbols are four-byte ids into append-only columns"). Hash-consing outside salsa and the reasoning for never sweeping stand.

Types and names are hash-consed in a process-global table outside salsa. salsa interning (a shard lock and
an id-to-slot lookup per construction and per `kind` read; `Ty::intern_id` alone was 4% of samples) was
the largest constant cost of item checking and bought nothing: types carry no revision-dependent state.
Two databases in one process share the table, which is harmless because entries are immutable and
structural. A tracked function keyed by one name takes
`QueryName`, a salsa-interned wrapper, because salsa needs a salsa struct as a single key.

**Never swept**, because the type universe saturates under real editing: `TyKind` has no literal variant,
so changing a literal builds no type. Over 5,000 server body edits on the finance benchmark RSS levels
off at about 373 MB; only edits that keep inventing names grow memory, by about 240 B per new name (about
25 MB over 100,000 such edits). Add revision-scoped sweeping only if a workload
shows growth past that.

## The checker presumes R code is correct where R's semantics leave the question open

**Status:** built.

Each rule asks what R does at run time and refuses only what R itself fails on, or a near-certain typo
(treating every use as a demand gave 3,112 errors on the GNU R corpus of CRAN-checked code, essentially
all false; these rules cut it to 624):

- A formal is required only when every path forces it (`collect_branch_uses` returns the forced set,
  `join_forced` intersects branches); a same-name argument to a project function
  (`inner(finally = finally)`) leaves the decision to the callee.
- A name a record-like list lacks reads `Unknown` with a strict origin, because R lists are open and code
  adds fields later; a near-miss field is still an error. An empty `list()` reads `Unknown`, or contributes
  `NULL` inside a union (the field-by-field accumulator idiom). `NULL` was rejected for absent fields: it is
  precise only when the literal is the whole value, and otherwise spreads into false errors.
- Logical widens under the numeric constraint (`widen_numeric_binding`), and a vector partner leaves the
  flexible operand open with a `double[]` result. A callback called only with forwarded `...` is
  `fn(...: Any) -> T`. `pkg::name` on a shipped namespace is accepted unattached, since `::` loads.
- A call to an inferred function narrows a union parameter its return echoes to the argument's member
  (`narrow_echoed_parameter_unions`); a remaining member that is a variable of the callee alone (a
  getter-or-setter's setter value) makes the return `Unknown` rather than leak. A list union passed to a
  generic callback consumer joins element-wise, skipping `list()` members (`joined_list_union`).
- Arithmetic on an open operand fits any class with arithmetic methods (`Arith.matrix`, `Ops.Date`),
  except a method taking `Any` (`Arith.data.frame`) and inside an overload probe, where it would pick
  `sum`'s `difftime` candidate; untyped arithmetic does not fit an operator method's parameter
  (`operator_method_depth`), or `day - 2 * x` would take `-.Date`'s `difftime` candidate.
- A `<<-` write to a top-level variable is read back only in the rest of the writing body, never in other
  items. The path that skips the write holds the top-level binding (`materialize_pre_state`), except a
  top-level `NULL`, which is a lazily filled cache's placeholder.

Accepted gaps: fewer true positives on literal-only lists and on qualified typos of inactive namespaces.
Remaining corpus findings: `backlog.md`, "what `ry check` still reports on the GNU R corpus".

## A computed function that may be `NULL` is called as the function

**Status:** built.

`infer_ordinary_call` drops `NULL` beside function members of a callee's type unless the callee names a
parameter, and `dispatch_call` checks a union of functions and values against its function members only,
with an `Unknown` result. Per "The checker presumes R code is correct", the `NULL` path of a computed
function (a `switch` without default after `arg_match`, a correlated `if`) is one the code presumably rules
out. A parameter's `NULL` comes from its default or declared type, which the caller supplies, so that call
is still reported. Accepted cost: a `switch` on an unvalidated value loses its true positive.

## A file's check names only what its interface reaches

**Status:** built, except the per-item `interface_group` walk and `item_interface_reads`: one walk per file finds the groups and `item_links` holds the edges ("Every host resolves names through one path, and trees live in the host's store").

Group membership is a property of what an item reaches, so it must not come from a whole-project graph;
otherwise the first check of one file names every item of the project (on dsbench, `item_naming` ran
56,546 times instead of 318, a one-file check took 2.3 s instead of about 0.25 s, a file opened at server
startup settled in 490 to 620 ms instead of 170 to 240 ms, and a keystroke after a cold prime took 240 to
290 ms instead of 60 to 74 ms). Every member of an item's strongly
connected component lies in its reachable set, so walking only what it reaches is exact, and members come
back in canonical order (`scc_exports` keyed by the first). `file_conditional_writers` screens items on the
file's tree (`binds_only_own_name`): only an assignment operator, `for`, `%<-%`/`%->%`, or a
`setnames`/`setcolorder` call outside a nested function can write a top-level name, so an item binding only
its own name needs no naming query. The screen may overcount, never undercount
(`testing::check_write_screen` asserts it in the fuzz battery). `file_global_variable_declarations` checks
the text for `globalVariables` before walking. The server's warm-up derives only per-file folds
(`warm_project_folds`) and names nothing. `file_conditional_writers` reads the file's tree, so it re-runs on
any edit to that file (one walk, backdating when the writers are unchanged).

## Every host keeps every tree

**Status:** built for the server. A command keeps a bounded number of trees in its `TreeStore` ("Every host resolves names through one path, and trees live in the host's store"). Kept for the measurement that salsa LRU capacities serialize parallel reads.

No salsa query has an LRU capacity. salsa records each read of an LRU query under one mutex per query, and
on four cores the cold check's reads of `item_syntax` and `parse` queued on those mutexes (the largest
single wait in a gdb sample); dropping only the item-level capacities removed the saving too, because item
subtrees are most of a file's tree. The cold check's three passes (derive from syntax, name, report) are
plain parallel loops (`in_parallel` in `crates/ry/src/cli.rs`), and tree readers stay memoized queries with
small results, so a check never walks another file's tree. Trade-off measured: cold `ry check` went from
about 2.2-2.5 s to 1.6-1.8 s while peak RSS rose from about 485 to 565 MB; mimalloc's remaining time is
fresh-page setup, and its purge and huge-page options made no difference. Remaining waits are salsa claims on
project-wide queries (`duplicate_binding_map`, `project_arithmetic_classes`, `project_type_definitions`)
that one thread computes while others need them.

## `ry check` releases its trees once, and memoized records are compact

**Status:** built, except the tree release (`release_trees`) and `item_interface_reads`, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- **Strictness has one decider.** `AnalysisSettings` carries `[check] strict` and `semantics::file_is_strict`
  decides a file's mode (the loader's `load_index::file_is_strict` reads the package default). The checker
  records strict origins only when strict, since non-strict projects never read them (about 14 MiB on
  the benchmark code bases).
- **Empty lists cost one word.** Most per-item lists are empty, and an empty `Vec` is 24 bytes, a boxed
  slice 16, which made `ItemNaming` 320 bytes and `ItemCheck` 248 across about 56,000 items. Frozen per-item tables are `SortedMap`/`SortedSet` (`crates/semantics/src/sorted.rs`) over a
  `ThinVec`, one pointer wide and allocation-free when empty; the same for check and naming lists and HIR
  side arrays.
- `ExpressionTypes` is a palette of the item's distinct types plus one `u16` slot per expression (`u32`
  past 65,535 distinct types).
- Interned types carry content flags (`HAS_VAR`, `HAS_RIGID`, `HAS_UNKNOWN`, `HAS_NAMED`) computed at intern
  time, so a walk skips a type with nothing to rewrite.
- `file_binders` is keyed by `Symbol`. Together these took peak RSS from 536-544 to 417-419 MiB.
  mimalloc's v2 build (489 MiB) and its purge, reclaim, retention and huge-page options, and a forced
  `mi_collect` at worker exit, measured no saving.

## HIR as struct-of-arrays, lowered from the green tree

**Status:** built, except `Module` owning one vector per array: it holds every array in a single word buffer.

`crates/semantics/src/hir.rs`:

- An `ExprId` indexes separate `kinds` and `ranges` arrays, because inference reads kinds and mostly only
  diagnostics read ranges; `Module::expressions` zips them for IDE scans needing both.
- A kind holds a `Span<T>` into one side array per list type, and literal text a `Text` span, so every kind
  is `Copy` (owned lists made readers clone to release a borrow). Children are visited with
  `Module::for_each_child` and `Module::any_child`, not a per-kind child vector. Literal text is not a `Symbol`, because the interner never frees and would keep every
  intermediate spelling typed in the editor.
- Lowering reads through borrowed `syntax::NodeRef` cursors, collects each list on a scratch stack, and
  runs in a per-thread scratch module that keeps its capacity; the memoized module gets exact-size copies
  ("scratch arena, owned compact result"). Red-cursor walking had been about 60% of `item_hir`'s samples.
- Names at their own site (argument names, fields after `$`/`@`, loop variables, tags) are a `NameId` into
  `Module::name_sites`, read through `Module::name`, `Module::name_range` and `Module::argument_name`. Ids
  are `NonZeroU32`, so an `Option<ExprId>` is four bytes; an expression kind is 16 bytes and an argument 8.
- `Symbol` hashes by a spelling hash computed at minting and orders by spelling, never by its index, which
  depends on which thread met a spelling first and would make output order vary between runs.
- `ExprId` allocation order is fixed, so ids, fixtures and downstream maps are stable. `Module` compares by
  value, so salsa backdating works. The checker records expression types in a vector indexed by `ExprId`
  (`RecordedTypes` in `check.rs`), not a hash map.

## A function literal in a call argument is typed in context

**Status:** built.

Spec: "Function literals in call arguments" in `reference/type-system.mdx`.
`infer_call_arguments_against` (`check.rs`) infers the other arguments, probes each candidate in a
rolled-back snapshot, and gives the literal the expected parameter types (`parameter_hints`). The literal
is inferred exactly once, because a probe snapshot does not reverse environment or recorded-type writes.

- **All or nothing.** Hints apply only when every defaultless formal gets one: a partial hint broke
  `Reduce(function(a, b) a + b, xs)`, since `a + b` no longer tied the two together.
- **Overloads.** A hint survives only where every candidate accepting the other arguments agrees, so the
  list and Array overloads of `List.map` each hint their own item type.
- **Findings.** An operand error on a hinted formal becomes `ContextualParameter`, reported on the use.
- **Accumulators get no hint.** A formal whose expected type shares a variable with the expected result
  (`S` in `fn(S, T) -> S`) is a fold accumulator, but only when another argument is that bare variable (the
  initial state); otherwise `List.update`'s `fn(T) -> T` lost its item hint. Hinting it pinned
  `List.walk(xs, list(), ...)` to `list{}`. `parameter_hints_for` reads the variables before the probe
  binds them.

## Shipped stubs are whatever `types/` holds

**Status:** built.

`crates/semantics/build.rs` scans `types/`, embeds every file, orders stubs (R's packages in search-path
order, CRAN namespaces alphabetically, `ry` last, so later sources override earlier ones as R's search path
does) and derives `CONDITIONAL_NAMESPACES` as every namespace except R's own and `ry`. Hand lists drifted:
a stub never shipped, or was always active and resolved in projects that never use it.
`QUALIFIED_ONLY_NAMESPACES` stubs are gated by activation too, so an inactive package's `pkg::name` stays a
manifest lookup.

## Override-stub problems are computed after `PackageMetadata` exists

**Status:** built.

`stub_library` is a HIGH-durability memo; computed before the `PackageMetadata` input existed, it saw no
active namespaces and was never re-verified, so a project with any `stubs/*.Rtypes` lost every conditional
namespace. `cli.rs` creates the metadata input first
(`check_keeps_declared_packages_active_beside_override_stubs` in `test_cli.rs`).

## Strict mode implies type errors

**Status:** built.

`[check] strict = true` turns typing on (`diagnostics.rs`), and `typing = false` beside it does not turn
type errors off: strict code is fully typed by definition, and strict findings without type errors made a
project look clean while its calls did not type.

## An all-`Any` overload does not veto agreement among unchecked fits

**Status:** built.

- With an unchecked (`Unknown` or `Any`) argument every candidate fits, and the call is `Unknown` unless the
  fits agree. `overloaded_call` (`check.rs`) drops all-`Any` fits (such as `rep`'s `fn(...) -> Any`) when a
  more specific fit exists and every specific fit still fits with each unchecked argument replaced by a
  witness that only `Any` or an unconstrained type parameter accepts
  (`unchecked_arguments_decide_nothing`); without the witness, the unknown argument chose the candidate
  (`levels(unknown)` became `character[]`, `rep(unknown, 2)` a vector). The always-disagreeing catch-all
  had been the largest strict friction. Rejected: the union of the fits' results, which turns silence
  into false operand errors (operators do not dispatch per union member).
- When fits still disagree and no unchecked argument is an inferred `Unknown` (each is `Any` or a
  forwarded `...`), the call is `Any`, as in mypy: opting out of an argument opts out of the call. Strict
  mode reports only `Unknown`, so `#: fn(x: Any) -> double` over `sum(x)` and `utils::head(...)` are clean,
  matching a single-signature stub's silent `Any` result (`nrow`). An unannotated parameter is a type parameter, so `t(m)` on untyped `m` is
  `Any`; strict mode reports the parameter itself.
- Cost: an `Any` result hides a mismatch every candidate shares (`#: fn(x: Any) -> character` over
  `sum(x)` is clean). Joining disagreeing results (`integer | double`) was rejected because it fails an
  honest `-> double`. Picking a common family without a union is the refinement if hidden mismatches matter.

## A formula is the nominal `formula`, and its operands are not judged

**Status:** built.

`~` types as `formula` (`infer_formula` in `check.rs`). Its operands are inferred for hover and
goto-definition, but their errors and strict origins are dropped: they name model variables (columns,
`.`, `I(x^2)`), not bindings in scope, and checking them would report every column as unresolved.

## `$`, `[` and `[[` dispatch to a class's subset method

**Status:** built.

A stub or project method `$.class`, `[.class` or `[[.class` types the access on a value of nominal `class`
(`subset_method_result` in `check.rs`), as S3 dispatch would, before the structural read, so an opaque type
gets typed field reads without exposing its representation
(`a_dollar_method_types_field_reads_on_its_class` in `nominals.R.test`).

## A new top-level name costs one project fold

**Status:** built, except that `item_interface_edges` is part of `item_links`, which reads the project name maps once per item ("Every host resolves names through one path, and trees live in the host's store").

A name being typed is a new top-level definition on every keystroke, so the project fold must happen once.
`package_definition_index` folds once into winners and duplicated names; `package_definitions` borrows its
winners and `duplicate_binding_map` is a tracked copy of its duplicates (empty in a healthy project, which
keeps the per-file duplicate check firewalled). Map keys are `Symbol`, hashed by the spelling's FxHash
computed once at interning, not by index, because index order depends on which thread interned first.
`Symbol` therefore does not implement `Borrow<str>`: a lookup by text interns first (`Symbol::new`). The
symbol table stores each spelling's character count, which the typo-suggestion scan filters on before
reading any spelling. A new-name edit costs about 4 to 6 ms on dsbench and finbench (23 to 27 ms with two
folds and per-item edges read from project maps). Pinned by
`a_new_definition_reprojects_the_name_maps_and_checks_only_the_new_item` in `test_incremental.rs`.
Measurement trap: a keystroke probe that edits a digit inside an identifier renames an item, and stubs
entered at `LOW` durability deep-verify every memo (about 51,000 per keystroke).

## Typed checking costs per name, not per file

**Status:** built.

- `TypeDefinitions` (`semantics.rs`) is a `Copy` view that looks a type name up in the file (scripts, or no
  `ProjectFiles`), then in the per-name projection `project_type_definition(files, name)`, then in the
  stubs; nothing merges the sets, because a merged map per file made typed analysis cost files times types
  (typed finbench: diagnostics 2.5 s against 0.2 s untyped; with the per-name view a typed check takes
  about 1.9 s / 644 MiB against 1.65 s / 533 MiB untyped).
  The unknown-type check builds the full known set only to rank suggestions after a miss. An edit to one
  type definition re-verifies only that name's readers.
- `file_comment_annotations` lowers each `#:` block once per file, keyed by start offset;
  `comment_annotation` finds one by binary search.
- `file_unresolved_non_locals` (`diagnostics.rs`) memoizes the non-local read classification per item, so
  the ordinary and strict checks share it.
- `ry load` reuses `ry check`'s record for its strict verdict: see "The strict load check shares the check
  record" (`libr-rcmd.md`).

## A conditional write of a formal covers the paths that run it

**Status:** built.

`FormalUses::assigned` holds the names some path wrote before forcing them, joined by union; a write of a
name its path already forced is not recorded. A later read forces the formal only when no path wrote it
first, matching the spec's "required only when every path forces it" and the `missing()` narrowing. Loops
and lazy operands keep the writes of the paths that run them; `local()` discards its writes. Accepted cost
of a path-insensitive analysis that must not report correct calls: a call omitting the formal and taking
the non-writing path is not reported.

## `<<-` writes flow forward, and a NULL-initialized cache reads its own first write

**Status:** built.

The writing body reads the value it just wrote (`write_target`, Super branch); the enclosing scope still
joins every write through `pending_enclosing_writes`, an undetermined write joining as a union member
(`join_closure_write`). Joining in place made `cache <<- f(); cache` read `NULL | T` and pinned a closure's
generic return. On entry to a body that superassigns a slot whose enclosing type has a `NULL` member, the
slot reads `existing | seed`, a fresh variable the body's first write binds (`superassign_seeds`), modelling
a closure that runs many times: `if (is.null(cached)) cached <<- dir("css"); cached` reads `character[]`.
Only NULL-initialized slots are seeded, so `count <<- count + 1L` reads the concrete count. A seed the body
never binds is unified with `Unknown` so it cannot generalize. The seed lives in the inference table, per
body.

## The overload probe's unchecked-argument witness is a nominal no candidate declares

**Status:** built.

The probe's stand-in for an unchecked argument is a `Named` type with one reserved name,
`UNCHECKED_WITNESS` in `infer.rs`, which no annotation can spell and which `undeclared_nominal` excludes.
Otherwise it counted as an undeclared nominal, compatible with everything, and `months(unknown)` picked the
`Date | POSIXct` candidate. Only `Any` and an unconstrained variable accept it.

## A call on a parameter whose arity the body tests is unchecked, and a later sibling does not force

**Status:** built.

- When the body reads `formals()` of a parameter, a later call of it with another argument count is
  unchecked (`arity_tested_by_formals`), because such code picks the call by the callback's arity; without
  the `formals()` read an extra argument is an error.
- `collect_formal_uses` runs before locals are typed. A bare formal passed to a local whose first write
  comes after the call (a sibling defined further down a `local()` block) is not forced
  (`optional_argument_targets`); an earlier-defined local still forces.
- An unresolved inferred `integer` slot accepts a `double` (`fits_inferred_scalar`), since R's
  integer-taking functions truncate; a string still fails. A slot already bound (a default lambda the join
  binds) is not covered and still errors.
- A `NULL` read is a placeholder only while no ordinary write gave the slot a non-`NULL` type
  (`valued_slots`), so a local narrowed or overwritten to `NULL` is checked. A `<<-` write does not count,
  because sibling closures read the `NULL` until the defining frame joins the write, and a top-level
  variable read from a function has no local slot, so it stays a placeholder.
- A read before a body's first `<<-` can bind the superassignment seed; a first write that then fails to
  unify is reported, except an occurs failure, which is the accumulator `acc <<- c(acc, list(item))`.

## Corpus-driven stub and overload fixes

**Status:** built.

- A name the item writes with `<<-` reads as `Unknown` unless the project defines it (`read_non_local`, via
  `top_level::super_assigned_names` over the item's own HIR); otherwise a Reference Class field
  `t <<- t[-inds]` resolved to base `t`.
- `grepl`, `grep`, `regexpr` and `gregexpr` take `pattern: character[]` as a second overload, not a union,
  because a union parameter makes an untyped caller's parameter infer the union, which then fails at
  `paste0(collapse = p)`. Accepted cost: `grepl(1L, x)` reports "no overload matches" over the whole call.
  `sub` and `gsub` keep `character`, since no corpus code needed more.
- Overload resolution lets a scalar narrowing beat the same atom's vector. A general "most specific by
  `compatible`" rule was rejected: it picked list shapes later reads rejected (ten more corpus reports).
- `menu`'s `choices` is one union including `list[Any]`; split overloads made untyped callers `Unknown`.

## Strict-friction rules for matrices, unions, `...: Any` and `$` members

**Status:** built.

- `m[i, j]` on a `matrix` (`matrix_index`): elements are `Any`, two vector or empty slots give `matrix`, a
  scalar slot gives `Any` (R drops dimensions), `drop = FALSE` keeps `matrix`; any other named argument
  falls back to the general rules.
- An operator on a union containing a class dispatches per member, a classless member taking the ordinary
  rules (`ordinary_operator_result`), committing only if that adds no error, so plain unions keep their
  messages.
- An expected `fn(...: Any)` pads a lambda's parameter hints with `Any`, because R forwards anything through
  `...` and `Unknown` would make every plumber or shiny callback a strict report.
- An annotation covers strict origins inside its value except inside a nested function literal or braced
  block (`annotation_covers`), which have their own bindings.
- A stub declares an environment member as `Class$member : T` (`is_stub_name`, `infer_field`), looked up
  before `$.class` dispatch (how polars types `frame$select(...)`), keyed by class name in the global
  scheme map, with no separate table.

## Inferred annotations follow the project's annotation style

**Status:** built.

- `ide::inferred_annotations` is the one source of edits for the code actions and `ry check --annotate`.
- With inline annotations on, types go inline; a right assignment, quoted name, generic non-function value,
  or a formal optional only because the body may not read it falls back to `#:` (inline spells optionality
  only as a default, so the inline form would make a caller omitting the argument an error).
- Whole-file annotation writes top-level bindings only, because locals carry no interface (the
  per-binding action still offers a local); `--annotate` is separate from `--fix` because it
  rewrites every signature rather than repairing a finding.

## The cold check's per-call work: a bump arena, borrowed schemes, a token-only green builder

**Status:** built, except the token-sharing green builder, which "The syntax tree is ry's own flat preorder arena" supersedes.

- Argument matching allocates in a `bumpalo::Bump` per item check; the checker borrows `ItemCheck`s and stub
  schemes instead of cloning, and fetches the stub library once per check.
- Lookups take `hir::Symbol`, and `types::Name::new` accepts `IntoSymbol`, so no caller round-trips through
  a string. `hir::Symbol` orders by a cached 8-byte big-endian prefix before comparing text, so sorted
  symbol sets rarely touch the spelling.
- The lexer has ASCII fast paths; keyword lookup rejects by length and first byte.
- `ry check` reads and renders in parallel, prints in path order, and hashes sources only for the cache.
- `TypeDefinitions::of`, `read_context` and `SalsaGlobals::new` fetch stub library, project files and file
  kind up front: a few extra dependencies on rarely changing inputs, for one fetch per check instead of
  per lookup.
- The 1 s cold-check target is not met, and the remaining profile is flat (`backlog.md`, "Check cache
  follow-ups").

## The item check's working maps live in a pooled bump arena

**Status:** built, except the reads of a green tree, which "The syntax tree is ry's own flat preorder arena" supersedes.

- `ArenaMap`/`ArenaSet` (`check.rs`) are `hashbrown` maps in a `bumpalo::Bump` through an adapter for
  std's `Allocator`. Bumpalo's `allocator-api2` impl fails because the JIT enables hashbrown's `nightly`
  feature workspace-wide; bumpalo's `allocator_api` feature breaks Cranelift's register allocator.
- The per-thread arena pool is a stack, because an item check can start a nested check while its arena is
  live; arenas are reset, not freed.
- S3 method names are spelled in a per-thread buffer before interning (`method_symbol`).
  `substitute_rigid` takes a slice of bindings and memoizes only types larger than 64 nodes
  (`UNMEMOIZED_SUBSTITUTION_SIZE`), where sharing actually repeats work.
- Yes-or-no syntax scans build no owned nodes; the full argument list is built only when a name repeats.
- The pooled arenas cost about 24 MiB of committed memory on the finance bench. The remaining allocations
  are salsa's memo bookkeeping and scattered `Vec` growth (`backlog.md`, "Check cache follow-ups").

## `$` fields resolve through the values that built the receiver

**Status:** built.

- `ide::fields` follows the receiver back to its constructions (constructor calls, records, R6 classes
  including `inherit` and `self$`/`private$`, `Class$new(...)`, pass-through calls, project returns, `if`
  branches, `receiver$name <- value`), capped at `MAX_DEPTH`.
- An unfollowable receiver falls back to every same-named declaration: references list them all and rename
  is refused, because a guess must never edit code. A precise target's references are a closed set (a read
  reaching one declaration brings in every other it reaches, such as both `if` branches); rename is refused
  while any same-named read is untraced, and likewise for a method parameter, whose calls are the field's
  reads.
- Constructor parameters (`CONSTRUCTOR_PARAMETERS`) and tag payload records are not fields. A library
  function's return has its fields declared by that function.
- A named argument to a project function forwarding `...` into a constructor declares that field, unless it
  names one of the function's parameters. `object$method(arg = 1)` navigates `arg` to the held function.
- `Resolver::sources` is memoized per resolution with a cycle placeholder, so a variable reassigned from
  itself costs one pass per assignment, not a power of them. Spellings come from `field_index` over
  per-file `file_fields`, so references never scan every file. `goto_definition` returns several locations because an `if`
  can build the value two ways.

## Packages without stubs are read from the R library by the host, lazily

**Status:** built.

- Analysis returns `DefinitionTarget::Package` with candidates in R's lookup order and never touches the R
  library, so it stays pure and machine-independent.
- The host (`crates/ry/src/installed.rs`) resolves it through `rdb` (without R), writing the namespace as R
  source under `<cache>/ry/packages/<pkg>/<version>/<pkg>.R`; the server ignores that file's notifications
  so it never becomes project code. A package is read once, on first use. Primitives' formals come from
  `.ArgsEnv`/`.GenericArgsEnv`.
- No stub is generated; this is navigation only. The LSP simulator runs with `installed_packages: false`,
  because the machine's library would make it nondeterministic.
- `ide::Installed` feeds signature help, completion and the import quick fix, which searches only packages
  the project already depends on, attaches or reads with `::`. `()` answers nothing; the fixture harness
  answers for one fake package. Exports are the `NAMESPACE` `export` names plus the `.rdx` names an
  `exportPattern` matches.
- Completion ranks a local ahead of library names at equal match quality unless `(` follows the cursor,
  since most locals hold data. This raised httr2's two-character variable completion from 91% to 95%;
  ranking locals first unconditionally lost call heads (`req |> req_url(...)` to the local `req`).

## One-shot checks record coarse dependencies

**Status:** superseded by "Every host resolves names through one path, and trees live in the host's store", which removes `Db::one_shot`. Kept for two measurements that still shape the code.

- A walk recording one salsa edge per project item leaves its handle's reused query-stack frame sized for
  the project, and every later query at that depth clears it (about 0.75 G instructions on the finance
  bench), so a project-wide name map is built on a throwaway database handle.
- The global allocator calls `mi_malloc` directly when alignment is at most 16 and no larger than the size;
  the `mimalloc` crate always took the aligned path.

## Inferred signatures carry body facts that a call reads

**Status:** built.

`types::FunctionType` has inferred-only fields (empty on declared and stub signatures), because R decides
these at the call:

- `flag_return`: a literal-logical-default formal the tail `if` tests, plus both branch types;
  `flag_selected_return` picks a branch when the flag is omitted or literal. It needs a literal default
  and the tail as the only return. A forwarded `...` is assumed not to carry the flag, since the opposite
  reports DistributionUtils' ordinary `pDist(densFn, q, ...)`.
- `quoted`: formals read only through `substitute()` (a bare forward to another call counts as an
  evaluation); `quoted_arguments` types the argument `Unknown` without inferring it.
- `default_reads`: an optional formal another forced formal's default forces; `match_arguments` reports a
  formal missing when both it and the formal whose default reads it are left out.
- `missing_tested`: formals tested with `missing()`, which the callee may leave unread even with a default.
- `condition_only`: formals read only as a whole `if`/`while` condition, the only ones a logical-looking
  string fits.
- `null_unless`, `reduces`, `from_calls`: see the `Reduce` entry below.

They live on the type because they belong to the function value, which flows through bindings, lists and
callbacks; a per-item side table would serve only direct calls and need its own invalidation.
Annotations cannot spell them, so a written-back annotation is less precise (`test_roundtrip.rs`).

An apply-family element gets the inferred-scalar leniency a direct call gives. At a `match.fun` caller a
bare formal also read as a value is accepted unpinned (`check::reads_as_value`), because `match.fun` looks
a bare name up as a function; a formal used only as a function still infers as one
(`function(xs, f) lapply(xs, f)` keeps `f: fn(T) -> U`).

## `Reduce` folds a named function instead of matching `fn(U, T) -> U`

**Status:** built.

- `infer_reduce_fold` calls a directly named function (or an `if` between two names) on
  `(accumulator, element)` until the joined accumulator stops growing (at most four rounds), because R
  carries a wider value forward than the start type. Only the last round's findings are kept, unless a
  round fails. Closures over enclosing type variables and lambda
  literals keep the stub path, since repeated calls would pin shared variables.
- `FunctionType.reduces` records a body ending in `Reduce(f, x, init =)` over its formals (possibly through
  a local `function(a, b) formal(a, b, ...)`), with no early return;
  `forwarded_reduce` folds the caller's callback via `fold_reduce`, so purrr-style wrappers get the fold.
- **Facts on edges.** `check::Environment` carries `facts` (`Fact::Holds(slot, predicate)`,
  `Fact::NullTogether(a, b)`) in the entries' undo log, so forks and joins treat them as refinements. Any
  write to a slot drops its facts, including in `join_writes` when the joined entry is unchanged (a
  same-typed write is a different value), and a loop counts that as a change so its body is rechecked. A
  `NULL` guard on one slot also refines its null partners. `FunctionType.null_unless` records a callee
  whose only `NULL` return is an opening `if (!pred(formal)) return(NULL)` on a formal it never writes (an
  open, `Any` or `Unknown` member of another return counts as possibly `NULL`); `null_excluded_return`
  strips `NULL` where the argument holds `pred`. Facts are about variables, not values, so they are not on the type.
- `FunctionType.from_calls` marks a callback type synthesized from calls, whose later calls may omit an
  argument. Making its parameters optional was rejected: an expected callback's optional parameter requires
  the actual callback to have a default.
- Bare `NA` returns (`bare_na_returns`) take the other returns' atomic kind, else `logical`; only inferred
  returns are lenient: a declared `-> character` still reports `return(NA)`, so the round-trip test lists
  the case.
- Branch joins keep a bounded variable open when the closed side is not a plain scalar or vector (`rho +
  rho0` against `L %*% rho`); a bound such as `numeric` does not make a matrix the parameter's type.
- An `exists("name")` probe for a function an attached package ships and the project does not define makes
  the `else` diverge: the fallback serves older R versions ry never presents.

## References are checked against an independent resolver, and a replacement copy is its source

**Status:** built.

- The oracle (`crates/ry/src/scope_oracle.rs`) shares no code with the analysis, because checking against
  the analysis's own resolver would agree with every bug. It applies R's lexical rules: locals visible from
  the end of their assignment, enclosing functions seeing all their locals, `<<-` falling through to
  package level. Sites scoping cannot decide (formulas, quoted code, `:=` targets, calls with no visible
  function literal, `<<-` targets nothing lexical defines) are left out.
- Each `for` loop variable is its own binding, an assignment after a loop joins that loop's binding, and a
  read links to the nearest preceding definition; reassignments with no loop between stay one binding.
- `the$x <- v` in a function mints a local copy (R: `the <- \`$<-\`(the, ...)`);
  `ItemNaming::replacement_copies` maps it to a `CopySource`, and `ide::target_at`, hover and
  `occurrences` follow `copy_origin`, so renaming the global `the` reaches it. The checker types the copy
  as its own variable.
- Refused renames are not misses to fix: a parameter of a function stored in a field whose reads cannot all
  be traced (an R6 `private$` method, `list(set = function(token))`) is refused, because a named argument
  at an untraced call would break.
- `ry debug analysis-stats <path>` ("editor coverage") on R6, purrr, httr2 and finbench: hover and
  definition at or above 90% per role, references 100%, rename 95.5% to 100% (the rest are those
  refusals), call hierarchy and workspace symbols 99% or more. Remaining misses are receivers or callees
  nothing in the project declares (`.Call` symbols, a parameter's fields, generator `<<-` targets).
- `project_declarations` indexes `declaring_item`, whose scan of every file on a miss was a references p99
  regression; the probe's timing table catches such a regression.

## `if` in NAMESPACE is evaluated by the host, then resolved in text

**Status:** built.

- R evaluates, Rust parses. `.ry_namespace_conditions` (`crates/loader/src/runtime.R`) walks each `if` as
  `parseNamespaceFile` does (top level or inside a top-level `{`, only the taken branch) and encodes the
  values as one string (`loader::parse_namespace_conditions`), because `.Call` routines take strings;
  `Loader::read` substitutes taken directives (`metadata::resolve_namespace_conditionals`), keeping
  directive order, before anything parses `NAMESPACE`, so the spec, diff, index cache key and analysis
  all see the NAMESPACE R would build.
- A directive without values fails the read with `LoadError::Conditionals` before any stamp is taken (a
  stamp for a discarded read would hide the edit), and R evaluates again, repeating while the plan names
  new directives (values are keyed by directive source text). The first request of a load sends
  no values, since stale ones could run conditions the file no longer holds; such a package plans twice.
- The checker cannot evaluate R, so it reads both branches, which avoids false unresolved reports for
  platform-conditional imports.

## Reference Class and R6 method bodies resolve object members in naming

**Status:** built.

- `naming::ref_class_definition` recognizes `setRefClass`, `$methods`, `$fields`, `R6Class` and
  `$set("public", ...)` syntactically; `resolve_item` takes an `is_member(RefClassKey, Symbol)` oracle, and
  a method-body read it accepts records nothing (the checker reads `Unknown`). R6 methods answer only
  `self`, `private`, `super`. Otherwise a sibling method sharing a top-level function's name typed as it.
- Membership is a per-name projection (`file_ref_classes`, `project_ref_classes`,
  `project_ref_class_binds`) that reads `item_hir` only, never naming, so there is no cycle, and a class
  edit rechecks only methods whose answers change (`test_incremental`,
  `a_reference_class_edit_rechecks_only_methods_whose_member_answers_change`).
- An undefined superclass or generator hides the members, so every name binds rather than guessing.
  `file_defines_r6_class` covers helpers placed in an R6 list by name.

## A literal `source()` path is followed for resolution

**Status:** built.

- `file_source_calls` collects literal `source`/`sys.source` paths in non-package files; `sourced_files`
  resolves them against the sourcing file's directory, then the project root, transitively.
  `sourced_definition(file, name)` is a per-name projection after the file's own scope and before the test
  and package layers (last sourced binding wins), so a helper edit rechecks only reads whose answer
  changes (`test_incremental`, `a_sourced_helper_edit_rechecks_only_the_script_reads_it_changes`). A sourced
  file reports no unused assignments (`file_is_sourced`). Position is ignored: a nested `source()` runs at
  an unknown time.
- The server otherwise holds only `R/` and open documents, so `Worker::sync_sourced_files` loads from disk
  exactly the open documents plus what they and the package transitively source, dropping helpers nobody
  sources, so a session equals a fresh start; its read retries on cancellation because the set must be complete.
- A package file's `source()` is ignored: a namespace is its `R/` files.

## A masked argument is its own scope, and the base masking family is typed

**Status:** built.

- Naming pushes a `ScopeKind::Mask` scope around each non-data argument of a masking call and pre-mints its
  assignments there (`syntactic_masking_family`); resolving them in the caller's scope made
  `within(d, c <- a * 2)` shadow `base::c`. Mask writes are never unused (`within` copies them back).
  `top_level` walks a base-family masked argument with fresh locals, as for `local()`. The stubs are
  `@pure`, since arguments are walked for effects.
- An overloaded call left ambiguous only by an already-`Unknown` argument returns `Unknown` without a new
  strict origin (`typing-strict/unknown-origins.R.test`).

## `pkg::name` on an inactive shipped namespace checks the shipped export list

**Status:** built.

`stubs::dormant_namespace_exports` reads an inactive namespace's shipped manifest and stub declarations
without activating it, and `namespace_read_message` reports a missing name as "not exported". A package
with stubs but no manifest is not checked (partial stubs would flag real exports); `:::` is unchecked.

## A name inside a string's `{...}` is a read for liveness

**Status:** built.

Naming lexes each string literal's `{...}` segments and marks the bindings they resolve to as read
(`interpolated_names`), so a glue or cli template local is not unused (about 280 of the corpus's 3,180
unused warnings were such locals). It records no resolution (the
string may be plain text), so rename and hover ignore it. It applies to every string, because wrappers
forwarding `parent.frame()` are common and a false read only misses an unused warning. `{{`, `.`-led
(cli style), `?`-led (pluralization) segments and names after `$`, `@`, `::` are skipped.

## An `if` with an `Any` branch is `Any`; a two-path write still unifies

**Status:** built.

`join_branch_values` returns `Any` when either side is `Any`, as unions do; unifying let `f: Any` take the
other branch's signature. `join_written_values` keeps unifying, because the `Any` rule there lost true
positives (NlcOptim's matrix called as a function).

## tidyr and tibble ship masked-verb stubs beside their manifests

**Status:** built.

`types/tidyr.Rtypes` and `types/tibble.Rtypes` declare only the `@masked` verbs, because masking is keyed by
stub declarations; other exports come from the manifests. Reshaping verbs return `Any` because their
columns depend on the data; row-preserving ones (`drop_na`, `fill`, `complete`) keep `<T>`. tidyr
re-exports `tibble`, so its stub declares `tibble` too.

## `length(x)` guards drop `NULL`

**Status:** built.

`recognize_guard` treats `length(x)` and `length(x) <op> literal` as a test whose zero-excluding edge is
non-`NULL` (`non_null_edge`). It only drops `NULL` from a union, never shapes an open variable, because a
length test says nothing else about the value.

## A numeric binding yields to a matrix or a generic argument

**Status:** built.

Arithmetic ties operands to one variable, so the first argument bound it. `widen_numeric_binding` lets a
`matrix` absorb a numeric binding (the element type is not tracked, so a character matrix passes); an open
or numeric rigid argument meeting a `double`-bound variable is only constrained numeric, so the caller
stays `<T: numeric>`. Redirecting the bound variable was rejected: it dropped the literal's `double`
(`outer(1L)` typed `integer`) and moved a variable between levels, so an inner function generalized an
outer one's variable.

## A splice reparse refuses a prefix with an unclosed delimiter

**Status:** built.

`reparse::try_splice` does a full parse when the old tree reports an unclosed delimiter, unterminated
string or "to close" error before the edit: recovery ended that group early, so a closer the edit adds
reshapes the tree. The "to close" phrase catches a `[[` closed by one `]`, which never says "unclosed";
it also matches other recoveries (a type argument list's `>`), which only costs a full parse. Such files
are already broken, so the fallback is cheap.

## Alias cycles are reported at the declaration, and short names get narrow suggestions

**Status:** built.

`alias_cycle_diagnostics` (`diagnostics.rs`, via `check::alias_cycle_walk`) reports every alias on a cycle
at its declaration, and a use drops the annotation silently, so the mistake is reported once. It runs per
file over that file's declarations, so it adds no project-wide read. A two- or three-character name gets
a suggestion only when exactly one candidate is it with one character inserted (`nam` finds `name`, `df`
never finds `dt`), because substitutions on short names match unrelated names.

## The syntax tree is ry's own flat preorder arena

**Status:** built.

- `syntax::Tree` is struct-of-arrays columns in preorder (`kinds`, absolute `starts`, `ends` as the index
  one past the subtree, `parents`) plus the text, with dense `u32` indices; the next sibling is `ends[i]`.
  The builder records postorder events and `finish` scatters them into preorder, so a file costs a few
  `Vec` pushes. It replaced rowan, whose per-node allocations and transient red nodes forced a second
  traversal API beside the red one.
- `SyntaxNode`/`SyntaxToken` (an `Arc<Tree>` plus an index) mirror rowan's API; `NodeRef`/`TokenRef` are
  borrowed `Copy` cursors for hot walks, and `TokenRef` reads kind, range and text lazily (eager reads cost
  about 1 G instructions per cold check). Token sharing across files and rowan's node cache were given up.
- An item's syntax is a window (`Tree::subtree`) over the file's shared columns, so nothing is copied;
  copying item columns duplicated every file's columns (about 114 MiB, 80 MiB more peak on the finance
  bench). The windows' offset arithmetic costs about 0.85 G instructions but measured faster than the
  copies' memory traffic. Equality compares kinds,
  ends and item-relative offsets, so per-item cutoffs are position independent. A window keeps its file's
  columns alive, which is fine because an edit replaces that file's item trees too.
- `tree::splice` is cross-checked against a full parse (parents included, which equality omits) under
  `enable_invariants()`, and `syntax::testing` checks every navigation primitive against a naive reference
  on fuzzed trees and nested windows.

## Overload probes reject by call shape first

**Status:** built.

`shape_verdict` (`check.rs`) decides arity, overflow and missing required arguments from argument targets
alone; `match_arguments` uses it for its own arity findings, so the two never disagree, and
`probe_overload_candidate` runs it before instantiating a candidate (the finance bench ran about 155 k
full probes for 17 k overloaded calls without it). A call forwarding `...`, or naming an
unmatched argument without overflowing, gets no verdict and falls through to full matching.

## `[[` by name on a named atomic vector is the element, not `T | NULL`

**Status:** built.

`extract_result` gives `T` for `T[named]`: R stops with "subscript out of bounds" on an absent name in an
atomic vector, so `NULL` only produced false reports. Lists keep `T | NULL`, which R returns. Spec:
`type-system.mdx`, "`[[` on vectors".

## Correlated-path allowances added for the fresh sample

**Status:** built.

Each trades a narrow class of missed errors for a class of false reports seen on real packages.

- A flag-selected return also accepts `return()` branches and an earlier `if (flag) return(value)` when
  those are the body's only early returns.
- `isTRUE()`/`isFALSE()` narrow like `is.logical()`, deliberately loose in the false branch.
- When the project defines any `$` method, `$` on an atomic vector is `Unknown`, via the per-file
  projection `project_defines_dollar_method`.
- In arithmetic, a list member beside a numeric one is skipped, and a list or `NULL` operand beside an
  `Unknown` one is not reported.
- A range endpoint read through a list field drops `NULL`; a local or directly indexed value keeps it
  (`typing/control.R.test`, `a_length_test_that_admits_zero_keeps_null`).
- A guard on `is.x(name <- value)` narrows `name`; a bare `NA` local a closure fills with `<<-` is a
  placeholder like `NULL`.
- `seq`'s integer and double overloads precede Date and POSIXct, so an unresolved numeric `from` does not
  pick a date.

## A cold check holds one parse tree per worker

**Status:** built, except `with_held_tree`, `parse_memo`, the per-item syntax memos and `Db::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes: trees live in the host's `TreeStore`, and `RootDatabase::for_command(editor_facts)` replaces both one-shot constructors.

- `file_item_annotations` stores detached subtrees (`syntax::Tree::detach`), so it never pins a file's tree.
- `Db::editor_facts` off leaves editor-only facts (expression and pattern types, literal expectations, loop
  variables, selected overloads, callee targets) empty; it is its own switch because `ry migrate` reads
  expression types. `match_findings` reads the inference table, so exhaustiveness does not need them.
- `sourced_definition` returns early for a file that sources nothing (tracked part:
  `sourced_name_definition`). `TypeScheme::binders` is a `ThinVec` (one per item for the whole run).
- mimalloc's purge delay is 10 ms (`crates/ry/src/main.rs`), so trees freed mid-run return to the OS before
  the late peak.

## The one-shot check scans kind columns and overlaps its serial phases

**Status:** built, except the one-shot frame-binder path, `project_interface_groups` and the `SalsaGlobals::layer` memo, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- `NodeRef::descendants_of_kind`, `elements_of_kind` and `tokens_outside` scan the kind column and skip
  unwanted subtrees; lints, `write_tokens`, `collect_used_tokens` and `file_syntax_sites` use them.
- Item HIR is one exactly sized allocation; one `item_syntax_flags` query answers per-item syntax
  questions; rarely set `ItemNaming`/`ItemCheck` fields sit behind one optional box.
- The lexer uses `memchr2` for comment ends and string delimiters; `TreeBuilder::finish` is one backward loop.
- `ReadContext` fetches the file's package definition and conditional-slot maps once;
  `package_scheme_exists_in` takes a probe closure so both paths share one rule.
- `cold_check_memory_budget` (`crates/ry/tests/test_perf.rs`) fails a release cold check above 256 MiB.
- The profile is flat: the parser and tree builder (about 1.4 G instructions on finance), the lexer
  (about 0.65 G), spread salsa fetches and `infer_ordinary_call` remain.

## The checker's per-item facts are columns and bitsets indexed by dense id

**Status:** built.

Item-local ids are dense from zero, so hashing them wasted work on the hottest tables
(`Environment.entries` is probed on every read). `crates/semantics/src/dense.rs` has `Column<K, V>` and
`Bitset<K>` in the check arena, unallocated until the first write (most stay empty). Iteration is in id
order, so no result depends on hash order.

## Every host resolves names through one path, and trees live in the host's store

**Status:** built.

The server and `ry check` had separate implementations of name lookup, frame binders and cyclic groups, so
they could disagree, and trees had three homes.

- **`TreeStore` (`Db::trees`)** is the one home of trees; `parse` reads it and splices on changed text.
  Capacity is the host's only choice: every tree for the server, four per core for a command
  (`RootDatabase::for_command`). A command worker holds the file whose first pass it runs
  (`TreeStore::hold`); held trees are never dropped, the rest keep the most recently read. Holds alone were
  rejected: a read of an unheld file would reparse it every time.
- **`item_links(item)`** resolves every name the item's naming lists against the project name maps, and
  holds its interface edges and whether its file is in the project. It is the item's projection of the
  project-wide maps and backdates unless one of its names moved. It reads the project's file list but no
  fold over the project's names, so a name a test or script adds re-runs no package item's links.
- **Constructed names** (S3 methods like `+.Date`) go through `package_name`, `test_name` and
  `frame_binder`. A project package item skips `frame_binder` for a name the package layer does not bind,
  since every top-level name a package file binds is a definition or a listed conditional write; helper
  files' `test_name` layer works the same. Tests and scripts ask `frame_binder`.
- **`file_interface_groups(file)`** is one Tarjan walk from the file's items; groups list items, not
  positions, so they stay equal when a statement is added. `SalsaGlobals` keeps one column of resolved
  schemes by link position, because resolving copies a scheme.
- **Rejected:** a project-wide bound-name set (it lowered and named every file before the first check
  finished: one package file's cold diagnostics ran `file_binders` 1,006 times and `item_hir` 9,905
  times on the finance bench; and it is a fold over the project); a per-file name set (any new name
  re-checks the file); a `frame_binder` memo per constructed name (about 100,000 memos, 18 MB, nearly all
  for unbound names); edges in
  their own query (a second memo from the same inputs); one per-file links query (loses the per-item
  firewall).
- **Pinned:** `enable_invariants()` compares linked lookups with per-name answers, unbound verdicts with
  `frame_binder`, groups with a walk from the item. `test_incremental.rs` covers every way a package or
  helper file binds a name its check constructs (definition, conditional, chained, call-argument and
  in-loop writes, each failing if the verdict ignores the winner or conditional writers), query counts
  (a package file's check lowers no other file's item), and that a diagnostics-only host reports what an
  editor reports.

## Types and symbols are four-byte ids into append-only columns

**Status:** built, except the per-item store for types that contain inference variables, which
is rejected below.

Interner lookups took a shard mutex that checking threads contended for, hidden behind four caches.

- `crates/semantics/src/intern.rs`: `Column<T, CHUNKS>` is append-only in fixed chunks of 4,096 that
  never move, so a read is a shift, a load and an offset;
  `IdTable<W>` is an open-addressing hash-to-id table whose slots fill once and that grows by replacement,
  so lookup is lock-free and only storing takes a lock.
- A `Ty` is a `NonZeroU32`: low 27 bits index the kind and facts columns, upper bits say whether it is or
  contains a variable, named type, rigid parameter or `Unknown`. `TyKind` is `Copy`, its child slices in a
  bump arena behind the writer lock. The 15 leaf kinds and the first 4,081 inference variables fill the
  first chunk as constants. A `Symbol` is an id into a column of `{ text, hash, first eight bytes, length }`,
  hashes as its stored spelling hash and orders by spelling; `Symbol::lookup` probes without interning.
- `resolve` and `substitute_rigid` return on the id bits when nothing can change, and memoize subtrees for
  one call only for large work (`resolve` after 64 steps, `substitute_rigid` above 64 nodes).
  `InferenceTable::named_types` is two inline lists searched front to back, not a hash map: 91% of 1.08
  million definition lookups on the finance bench repeat a name the item already asked about, and an
  uncached answer is two salsa fetches and a scan.
- **Rejected:** a per-item store for types with variables (they leave the item via `EditorFacts`,
  `TypeErrorKind` and diagnostics, so every exit would copy, and only 696 of the 2,589 distinct types a
  finance check builds contain one). Never freed (see "Types and names are
  hash-consed outside salsa"); a full store (2^24 types, 2^28 spellings) panics.
- **Invariant:** id order differs between parallel runs, so nothing reaching output may walk ids in order;
  that is why `Symbol` orders by spelling.
- A finance check executes 18.06 G instructions against 19.07 G with pointer types.

## An item's syntax has a stamp, so an edit lowers only the items it changed

**Status:** built.

Reading an item's range from `item_spans` made every edit re-lower every item of the file.

- The store keeps a `SyntaxStamp` per top-level node. On reparse, nodes at the file's start and end whose
  subtrees equal the old ones (an exact comparison) keep their stamps, others get fresh ones from a
  store-wide counter; equal stamps imply equal subtrees.
- `item_stamp(item)` backdates outside the edited region; `item_syntax` reads only the stamp and finds the
  node through a per-file index, falling back to a fresh parse when a bounded store dropped the tree.
- `item_annotation_syntax` and `item_name_range` read per-file memos (`file_item_annotations`,
  `item_spans`); the detached annotation subtrees compare by content, so they backdate while no `#:` block
  changes. An edit to any `#:` block re-runs `item_annotations` for every item of the file (each backdates
  unless its own block changed).
- `TreeStore` has 16 shards; a parse runs outside every lock, and a second reader waits for it.
- **Rejected:** the stamp in `item_syntax_flags` (other items read those flags, so they would re-run on a
  body edit); a content hash (a collision keeps a stale lowering unnoticed); matching moved nodes anywhere
  (one edit changes one region, so the linear prefix and suffix comparison suffices).
- Known cost: edits at both ends of a file between two reads make one region spanning the file, so the
  items between re-lower and `item_hir` backdates. The stamp queries take 3.3 MB at the finance heap peak
  (50,815 items).
- **Pinned:** `enable_invariants()` checks each stamp read; `test_incremental.rs` pins query counts for an
  edit inside an item, a new item above the others, a removed item, the second of two equal statements
  removed, and edits at both ends.

## The analysis core's per-item and per-file tables after the type store

**Status:** built.

- `item_links` has one memo per item for the whole run (50,815 on the finance bench), so `ItemLinks` is two
  thin vectors (`words`: header, names, one word per binding holding its name's position above its kind;
  `items`: binding items, then edges), two pointers wide, and allocates nothing when empty. Its memos hold
  10.5 MB at the finance heap peak, and it is 4.6% of a check's instructions, mostly resolving every listed
  name (`backlog.md` has it). Edges sort by file, spelling, kind and disambiguator, not `Symbol::index()`, which is not
  reproducible.
- `file_follows_strict` is the one answer to "is this file strict"; `item_tree_positions` the one
  item-to-position map; only an item binding more than its own name keeps `item_bound_names`. `Item.name`
  is a `Symbol`, and the stub library's collections are `SymbolMap`/`SymbolSet`.
- The server derives the attached set from `file_attached_namespaces` scans rather than a hand-kept copy.
  `ide::spelled_names` lets tag completion and searches open only matching files.
- **Trap:** every tracked read on the server worker must run inside `Worker::cancellable`; one that did not
  let a queued cancellation kill the worker (about one run in ten under load, in
  `burst_of_edits_settles_on_the_final_text`), so the worker's exit message names an escaped cancellation.

## A function has one signature, resolved in `syntax::annotation`

**Status:** built.

- **Decision (on the user's mandate that the annotation forms are equal).** A function whose signature is in
  a `#:` comment or on its binding runs exactly like its inline twin: same boundary checks, same native
  compilation.
- **Rule.** `syntax::annotation::signature(function, inline)` resolves each slot's type node from syntax:
  the literal's type wins, else the binding's function type (replacing a `#:` block whole), else the
  attached `#:` block. Refused blocks, `misplaced_directives` and non-function types declare nothing; a
  coercion (`@trust`, `@if-unknown`, `@new`) leaves no signature; `inline = false` reads only `#:`. The
  runtime (`interp::annotations::register_definition`) and the checker share `attached_annotation`,
  `block_declaration` and `pair_formals` (a formal takes the declared parameter of its name, else the next
  unnamed one, and `...` the rest type); the checker lowers types itself.
- **Why syntax.** The runtime lowers code the checker never sees (`eval(parse())`, installed packages, lazy
  definitions), and `interp` does not depend on `semantics`; handing it the checker's scheme would need a
  serialized signature and a checker run before every lowering. The gap is a signature that is only an
  alias name: the checker peels it (`check::declared_function`), the runtime cannot, and
  `native::Uncompiled::AliasedSignature` warns only when an alias was peeled and the types are incomplete,
  so an incomplete signature with another cause (a parameter name the function lacks) is not blamed on it.
- **Difference.** The checker drops a block on any lowering error (`Lowering::finish`), including ones only
  it sees, such as an unknown type name; the runtime checks the types it recognizes. Such blocks carry an
  error, and strict mode refuses to run the file.
- **Consequence.** A `#:` comment is enforced at run time, where GNU R ignores it, `ry check --annotate`
  output included; the check is first-order and fails only on a value that really lacks the type.
- **Lowering needs the statement.** `loader::Code` carries `statement_start`; `loader_backend::lower_code`
  parses from the statement's line plus the `#:` lines above, masking the prefix with spaces so offsets
  match; a parse error inside a `#:` line does not fail the lowering, since R reads it as a comment. The
  console's `syntax::input_is_unfinished` waits after a trailing `#:` block until the next line, and a
  blank line ends the wait. `typed::lower::resolve_function` forces a lazy definition (`ryLoaderForce`),
  which is unobservable because the loader binds only function definitions and pure constants of strict
  files that way.
- **Pinned by** `typing-strict/native.R.test` (`signature_written_in_a_comment`,
  `signature_from_a_type_alias`, `signature_alias_is_blamed_only_when_an_alias_is_the_cause`,
  `signature_in_a_comment_reports_what_blocks_it`),
  `crates/ry/tests/native/blocked-aliased-signature.R`, `crates/syntax/tests/test_annotation.rs`,
  `a_console_reads_on_only_when_the_input_ended_too_early`, and the scenario `typed-comment-signatures`.

## A splice reparse shares what the parser's recorded lookahead proves untouched

**Status:** built.

- **Previous source of truth.** `syntax::reparse::try_splice` guessed whether a line break was a safe
  seam from the result of the parse: it matched error messages ("unclosed", "to close", "unterminated"),
  counted open delimiters in the token stream, and refused a middle whose last error was incomplete.
  Nothing said what a statement had read, so the guess was wrong wherever the grammar looks past a line
  break without leaving an error: an inline union that continues on a line starting with `|`, a function
  return type whose body is on the next line, a record type closed on a later line, recovery that asks
  whether the next line starts a statement, and an f-string whose `{}` spans lines. On about 32,700
  simulated edits (small edits and typed statements over the program corpus and the fixture sources) the
  guess produced 95 trees or error lists that differ from a full parse.
- **Target shape.** The parser records what it read. `Parse::lookahead` holds one `Lookahead { statement,
  end }` for each top-level statement, and each `#:` annotation between two statements, that read a token
  past the one it stops on or read the end of the text. Every token read at or past the cursor goes
  through `Parser::kind_at`, `token_str` or `adjacent`, which keep the furthest index in `examined`; an
  f-string's opening token counts as a read of its closing quote, because the lexer reads the f-string as
  one unit. A splice shares the children before a line break no old statement read across, parses a
  middle that includes its closing line break, requires that no middle statement read past that break
  when a suffix follows, and shares the suffix only after a break no old statement read across. A spliced
  parse composes its lookahead from the three parts, so the next splice starts from the same facts as
  after a full parse.
- **What stays lexical.** The lexer has two reads the parser cannot see, and each keeps its guard in
  `try_splice`: an unterminated quote ends at its line break only because no closing quote follows
  anywhere, and a lone `\r` joins a following `\n` into one line break.
- **Impact.** Correctness: 0 differences on the same edits, and on 30,000 seeds each of the edit-stream
  and small-edit arms. Splice rate while typing a statement: 34% before, 44% after, because the rule
  refuses only what a statement read. Cost: one `Cell<usize>` maximum per token read, and one record per
  statement that looked ahead (none for a statement that stops on the line break it read last).
  Incremental analysis is unchanged: the tree store is invisible to queries.
- **Check.** `reparse` compares every splice with a full parse under `enable_invariants()`, lookahead
  included; `SPLICE_REGRESSIONS` in `crates/syntax/tests/test_fuzz.rs` pins each shape above.

## One syntax verdict, in the `syntax` crate, for `ry check` and the runtime

**Status:** built.

- **Decision.** A text is a syntax error for the runtime exactly when `syntax::parse` reports an error
  outside a `#:` comment. `ry check` therefore reports every text `ry run` stops on before running a
  line, and nothing the runtime runs. One exception: a byte outside UTF-8, which only the runtime
  reads (as a stand-in character) and refuses where GNU R's lexer does.
- **Previous source of truth.** The parser held GNU R's grammar, and `interp/src/lower.rs` held the checks
  GNU R makes on text its grammar accepts, each on a lowered value: string escapes, a parameter declared
  twice, the pipe's right side and its placeholder, an empty statement between semicolons, an empty name.
  `ry check` was silent on all of them, and the escape decoder existed twice (lexer text, lowering bytes).
- **Target shape.** `syntax::lexer` decodes a quoted token once (`unescape_quoted`; the runtime reads its
  pieces) and reports a bad escape, an empty backquoted name and a NUL byte in a string (lowering cuts
  a name at one, as GNU R does). An error the lexer raises inside a `#:` comment is an annotation
  error, as the parser's are, and a string in such a comment ends at an escaped line break too. An
  f-string is not R, so its bad escape is a ry rule (`error_rule`) with ry's message.
  `syntax::validation` walks the finished tree for the rules that need structure (repeated parameters,
  pipes and placeholders, empty statements, an empty string as a function or argument name). Each such
  error carries a class (`Unexpected::Classed`), so the runtime words it as GNU R does and tools read
  ry's message.
- **Which error the runtime reports.** GNU R stops at the first error its parser meets, and a rule it
  checks in a grammar action fires when the construct closes. `lower::error_order` gives every error the
  offset of the last token GNU R has read by then and a rank (lexer, action, grammar, a construct the end
  of input cuts short), and the smallest wins. An action runs only for a construct that is whole when the
  grammar refuses a token, which `error_order` decides by parsing the construct's text up to that token
  alone (`c("" = -)` is the grammar's error, `c("" = (1) 2)` the action's). `ry check` reports all of them.
  GNU R refuses a NUL in a string once it has read the closing quote, so an escape error anywhere in
  that string comes first (oracle case `parse__nul_bytes`).
- **When a console reads on.** GNU R's console waits for the rest of a command although an action has
  refused part of it (`f(x |> y,`, `function(a, a`, `c("" = 1,`), and reports the action's error with the
  whole command. A lexer error (`"\q`, ``` `` ```) and a grammar error end the wait at once. So
  `syntax::input_is_unfinished` sets the action classes aside before it asks whether every error says
  the input ended too early (`a_console_reads_on_only_when_the_input_ended_too_early`).
- **Impact.** Correctness: `ry check` finds `"\q"`, `function(x, x) 1`, `1 |> sum(_)`, `1;;2` and ``` `` ```
  before the program runs. Simplicity: lowering no longer validates. Performance: one pass over
  every file's tokens, which stops at functions, pipes, placeholders and strings. Incremental analysis:
  the errors are part of the parse result, so the splice reparse re-validates the statements it parsed.
- **Check.** `interp::testing::check_parse_verdict` (`fuzz_parse_verdict_is_shared`, `cargo fuzz`
  `parse_verdict`), which also puts `#: ` before every line and requires both to accept the result,
  since it is all comments to R; the golden cases in `crates/syntax/tests/errors`; the oracle's
  `parser` kind for GNU R's side.
- **Open.** `semantics/src/hir.rs` recognizes a pipe's shape with rules of its own (`pipe_shape`).

## Corpus false reports: the narrowing and overload rules that cleared them

**Status:** built.

- **A local written from a literal fills a vector parameter.** A literal constraint lasts until the
  local's first use, and only scalar-bounded variables filled a vector parameter as its element, so
  `p <- "a"; sprintf(p, x)` failed (about 150 corpus reports). `compatible` treats the literal bound
  like the scalar one.
- **A missing-path function is a default.** `if (missing(f)) f <- function(n) ...` types as a default,
  so the formal stays open. An explicit `NULL` to a formal whose default is `NULL` is the omitted call.
  A default naming a function ry cannot type reads as `Unknown` in the body, because the body's calls
  do not say what a callback must accept.
- **A scalar bound does not spread through arithmetic.** R recycles the shorter operand, so `sigma^r`
  with `r` scalar-bounded by `2:r` leaves `sigma` open instead of unifying the operands. Two distinct
  rigid parameters (`a / b` under `<T: numeric, U: numeric>`) give `double` instead of a mismatch.
- **An `if` over an undecided library call is `Unknown`.** `overloaded_call` marks a call whose
  unchecked arguments left candidates disagreeing (`Checker::undecided_calls`), and a value `if` with
  one as a branch is `Unknown`. A plain `Unknown` branch still joins as the other one, because a
  recursive call's placeholder is `Unknown` during the group fixpoint; the mark is the provenance.
- **An unchecked argument beside open caller variables still decides.** Without this, the
  narrowings agreed and declaration order picked `vapply`'s first candidate for an `Unknown`
  `FUN.VALUE`. Disagreeing narrowings become `Unknown`; agreed ones commit by re-probing the first fit,
  since a probe's narrowings name rolled-back variables and cannot be unified directly.
- **`identical` guards.** `identical(f, g)` with `g` a function narrows the local `f` to `g`'s type on
  the true edge (roxygen2's `format_name`); a generic `g` narrows to its scheme
  (`SlotRefinement::NarrowScheme`), so each read instantiates it afresh. `identical(x$field, literal)` rules `NULL` out of `x`.
- **Correlated branch writes.** After `if (case == "a") x <- f(x)`, `Fact::Correlated` keeps `x`'s
  value on both edges keyed on `case`; a later test of `case` whose edge implies the earlier test (or
  its negation) narrows `x` to that value (`KeyTest::implies` over literal sets, `==`, `!=`, `%in%`,
  a bare local read as `== TRUE`). Only literals of one kind compare, since R coerces before it
  compares (`"1" == 1`), and a string with a backslash is no key, since a raw string keeps its
  escapes literally. A write to either local drops the fact through `Environment::forget`. Two
  tests that only jointly cover an enclosing test are not combined (snakecase `to_any_case` 286).
- **Union echo.** An inferred `list[T] | T[]` parameter returned as-is gives back what a call passed,
  a union argument included, when every member is accepted.
- **No sound rule for a vector of unknown length at a scalar parameter.** `p[file.exists(p)]` has one
  element only by the program's logic, so devtools `test.R` 72 and knitr `citation.R` 163 stay
  reported; relaxing scalar parameters for vectors would hide true length bugs.
- **A union-bounded variable** resolves renv `python.R` 109 ("A type variable may be bounded by a union" below).

## `@opaque` parameters accept ry values

**Status:** built.

- **A stub marks a parameter the callee only evaluates or quotes** with `@opaque(name)` (`stubs.rs`, `DeclarationMarkers.opaque`), and `check_native_arguments` skips the R-boundary check for it. testthat's expectations mark `object` (and `expect_invisible` its `call`), so `expect_error(g(-1L), "neg")` with a record-returning `g` passes: the value is evaluated and dropped, never handed to R code. A marker rather than a hard-coded list keeps the knowledge in the stubs, where package authors extend it.

## The stats d/p/q functions follow the scalar claim

**Status:** built.

- **`dnorm`, `pnorm`, `qnorm`, `pchisq` and their 17 siblings return `double`** in `types/stats.Rtypes`, like base's arithmetic, so `function(q: double) -> double stats::pchisq(q, df = 1)` checks. Measured on the GNU R corpus, the scalar claim adds no false report, because callers that pass vectors read the result as `double`, which a `double[]` slot accepts.

## A `#:` block in an argument list types the argument after it

**Status:** built.

- **An argument is an attachment target**: `syntax::annotation::annotation_target` takes the argument's value (`argument_value`, the expression after `=` when the argument is named), and `attached_annotation` anchors on the whole argument, so a block above `FUN = function(v) ...` types the lambda. This is how a callback whose callee types it `Any` (`apply`, `uniroot`) gets a signature without becoming its own binding. The blank-line rule holds inside the list, because the site walk visits `ARGUMENT_LIST` as an annotated sequence.

## A function literal default takes its parameter's declared function type

**Status:** built.

- **`check_declared_function` hints a lambda default from the declared parameter type** (`hint_from_declared_function`), as a call hints the lambda it is passed, so `#: fn([connector]: fn(dsn: character) -> character)` over `connector = function(dsn) toupper(dsn)` types `dsn` without a second annotation. A finding the hint causes names its source (`LambdaContext::Default`). Each default is inferred once, after the formals are bound, because R evaluates a default in the function's frame; the `FunctionType` is built after that loop.

## A type variable may be bounded by a union

**Status:** built.

- **An open variable that meets a union takes it as a bound instead of binding to it** (`Entry::Unbound.bound` in `infer.rs`), so `normalize(path)` over `map_lgl(path, ...)` keeps the caller's own type: `<T, U: list[T] | T[]> fn(path: U) -> U`. Binding to the union lost the member the caller passed, and every caller saw the whole union back (renv `python.R` 109).
- A later binding must fit the bound. Two bounds meet by keeping the narrower one, else the members of the existing bound that fit the other, both when a variable meets a union and when two bounded variables unify; no fitting member is a mismatch blamed on the later use, and a single fitting member binds the variable, so no `<T: logical>` is ever printed.
- **A variable holds a constraint or a bound, never both, and a bound never names its own variable.** A constraint (`numeric`, from `x + 1`) or a unification that would put the variable inside its own bound first settles the variable to its bound and then retries, so the bound's members are checked as types (`TypeTable::settle_bound`, `unify_settled`). Keeping both silently dropped the bound, and a self-referencing bound looped while printing.
- At generalization a bounded variable in one signature slot is replaced by its bound (`settle_lone_bounds`), so a binder appears only when the bound links a parameter to the return or to another parameter. A rigid binder with a bound fits wherever its bound fits.
- `join_types` unions two distinct bounded variables instead of unifying them, since one is not the other, and a mismatch report shows the bound, never the bare variable.
- **Syntax**: `function<U: list[T] | T[]>` and `@forall U: list[T] | T[]`. After `:`, a run of plain words up to `,`, `>` or the line end is a constraint list (`T: numeric`); anything else is a type. `TypeScheme.binders` holds `Binder { name, bound: Bound::Constraint | Bound::Type }`, one list, so a binder cannot carry both.

## A value whose code a later top-level write replaces is a template

**Status:** built.

- **`f <- g(...)` followed at top level by `body(f) <-` builds `f` from a template**, so the call building `f` does not check its function arguments against `g`'s parameter types (`Checker.template_call`). diffobj `diff.R` 654 builds a one-parameter function, then gives it the parameters the interface names.
- The check probes `file_code_rewrites`, a per-file list of rewritten names that backdates, before it asks for the name's writer (`frame_binder`). Asking `frame_binder` for every value item tied each one to the file's binder list, so inserting a statement rechecked every value before it.
- Only `body<-` (also as `base::body<-` or with `->`) qualifies, because it is the one replacement that discards the code `g` built. `formals(f) <-` keeps that body, which still runs on the new parameters, so the call is checked; `attr(f, "x") <-` changes nothing a call does.

## A key test reads within the tests that enclose it

**Status:** built.

- **Each edge of an `if` on a local records the domain it shows** (`Fact::KeyDomain`), and a later test of the same local is intersected with the enclosing domains (`KeyTest::within`) before it is compared with earlier correlated writes. Inside `if (case %in% c("a", "b", "c"))`, the false edge of `if (case == "c")` is `case %in% c("a", "b")`, which implies an earlier `if (case %in% c("a", "b"))`, so the earlier write is the only value on that edge (snakecase `to_any_case.R` 286, 295, 304).
- When both branches of an `if` continue, the false edge's domain is retracted after the join, because code after the `if` runs on both edges. Mixed literal kinds keep the test unchanged, because R coerces before it compares.


## Strict mode reports where `Unknown` enters, not every type it reaches

**Status:** built.

- **No strict rule tests a finished type for `Unknown`.** Strict mode reports origins (an unsupported construct, an undeclared reference, a loop-widened or recursive binding, unconstrained parameters), and an `Unknown` that flows on from an origin is a consequence, reported once where it entered.
- Measured with `ry debug analysis-stats`, which prints the count. On the ten-package corpus with strict forced on, 1665 of 4036 package items export a type containing `Unknown`, and 1563 of them have a finding in the same item. Every sampled one of the other 102 inherits an `Unknown` that a project callee's own finding reports (renv's `data_frame <- function(...) as_data_frame(list(...))`, knitr's `child_mode` over `opts_knit$get`). The strict typed finbench exports `Unknown` from 0 of 40,746 items. A containment rule would repeat each origin at every item downstream of it.
- **A call to a base function no stub declares is an origin** (`R.home()`, `.Call(...)`), as a package function with no stub already was. Reading such a name as a value stays unreported, because intrinsics such as `c` and `[[` have no stub and are typed at their calls. `@if-unknown` accepts the result, and the remedy is a stub line.
- The backlog's premise that `g <- f` exports `fn(p: Unknown) -> Unknown` no longer holds: an alias keeps the scheme (`aliased_function_reference_exports_closed`).

## An all-`logical` call of a numeric function keeps `logical`

**Status:** settled.

- `bump <- function(x) x + 1L` is `<T: numeric> fn(x: T) -> T`, so `bump(TRUE)` is `logical` where R gives `2L`. The two refusals this once caused are gone: a `logical` argument meets the numeric constraint, and mixed arguments join (`add(1L, 1.5)` is `double`).
- Binding `T` to `integer` for a `logical` argument would mistype `checked <- function(x) { stopifnot(x + 1L > 0L); x }`, which returns the `logical`. Only a type-level `promote(T, integer)` separates the two, and no corpus or benchmark finding depends on the difference, because a `logical` fits wherever an `integer` is expected. Build `promote` when a false report or a typed-ry need shows up, not before.
