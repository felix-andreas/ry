# Decisions: Syntax and semantics

The parser and syntax tree, name resolution, stubs, and the foundations of the type system.

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
mechanism is the general `# ry: allow(type-mismatch)` on the assertion, or `# ry: expect(type-mismatch)`,
which also reports once the finding is gone (`ide-server.md`, "`# ry: expect(code)` reports when its finding is gone").

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
  abstract nominal: `is.list(data.frame)` is true).
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

**Status:** built. `declare(type(...))` is not a spelling (see "Plain R comes from `ry build --plain-r`").

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

## Inferred annotations follow the project's annotation style

**Status:** built.

- `ide::inferred_annotations` is the one source of edits for the code actions and `ry check --annotate`.
- With inline annotations on, types go inline; a right assignment, quoted name, generic non-function value,
  or a formal optional only because the body may not read it falls back to `#:` (inline spells optionality
  only as a default, so the inline form would make a caller omitting the argument an error).
- Whole-file annotation writes top-level bindings only, because locals carry no interface (the
  per-binding action still offers a local); `--annotate` is separate from `--fix` because it
  rewrites every signature rather than repairing a finding.

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

## tidyr and tibble ship masked-verb stubs beside their manifests

**Status:** built.

`types/tidyr.Rtypes` and `types/tibble.Rtypes` declare only the `@masked` verbs, because masking is keyed by
stub declarations; other exports come from the manifests. Reshaping verbs return `Any` because their
columns depend on the data; row-preserving ones (`drop_na`, `fill`, `complete`) keep `<T>`. tidyr
re-exports `tibble`, so its stub declares `tibble` too.

## A splice reparse refuses a prefix with an unclosed delimiter

**Status:** built.

`reparse::try_splice` does a full parse when the old tree reports an unclosed delimiter, unterminated
string or "to close" error before the edit: recovery ended that group early, so a closer the edit adds
reshapes the tree. The "to close" phrase catches a `[[` closed by one `]`, which never says "unclosed";
it also matches other recoveries (a type argument list's `>`), which only costs a full parse. Such files
are already broken, so the fallback is cheap.

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

## A `#:` block in an argument list types the argument after it

**Status:** built.

- **An argument is an attachment target**: `syntax::annotation::annotation_target` takes the argument's value (`argument_value`, the expression after `=` when the argument is named), and `attached_annotation` anchors on the whole argument, so a block above `FUN = function(v) ...` types the lambda. This is how a callback whose callee types it `Any` (`apply`, `uniroot`) gets a signature without becoming its own binding. The blank-line rule holds inside the list, because the site walk visits `ARGUMENT_LIST` as an annotated sequence.

## A value whose code a later top-level write replaces is a template

**Status:** built.

- **`f <- g(...)` followed at top level by `body(f) <-` builds `f` from a template**, so the call building `f` does not check its function arguments against `g`'s parameter types (`Checker.template_call`). diffobj `diff.R` 654 builds a one-parameter function, then gives it the parameters the interface names.
- The check probes `file_code_rewrites`, a per-file list of rewritten names that backdates, before it asks for the name's writer (`frame_binder`). Asking `frame_binder` for every value item tied each one to the file's binder list, so inserting a statement rechecked every value before it.
- Only `body<-` (also as `base::body<-` or with `->`) qualifies, because it is the one replacement that discards the code `g` built. `formals(f) <-` keeps that body, which still runs on the new parameters, so the call is checked; `attr(f, "x") <-` changes nothing a call does.

## A key test reads within the tests that enclose it

**Status:** built.

- **Each edge of an `if` on a local records the domain it shows** (`Fact::KeyDomain`), and a later test of the same local is intersected with the enclosing domains (`KeyTest::within`) before it is compared with earlier correlated writes. Inside `if (case %in% c("a", "b", "c"))`, the false edge of `if (case == "c")` is `case %in% c("a", "b")`, which implies an earlier `if (case %in% c("a", "b"))`, so the earlier write is the only value on that edge (snakecase `to_any_case.R` 286, 295, 304).
- When both branches of an `if` continue, the false edge's domain is retracted after the join, because code after the `if` runs on both edges. Mixed literal kinds keep the test unchanged, because R coerces before it compares.

## A package's own `pkg::name` reads are checked against `NAMESPACE` and `data/`

**Status:** settled.

- The export set is `export()` and `exportMethods()` from `NAMESPACE` plus, under `LazyData: true`,
  the dataset names in `data/` (`rdb::dataset_names`: every object of an `.rda`/`.RData`, the
  `Rdata.rdx` index, the stem of `.csv`/`.tab`/`.txt`). `getExportedValue` falls back to the
  namespace's lazydata environment, which `R CMD INSTALL` fills only under `LazyData`.
- It is a `PackageMetadata` field (`qualified_names`), so an edit to a source file never touches it,
  and a check asks the per-name `own_package_reaches`, so a `NAMESPACE` edit re-checks only the reads
  whose answer moved. Naming a saved image's objects unserializes it, so the server reads `data/`
  at start-up and when `DESCRIPTION` or a `data/` file changes on disk, never per `NAMESPACE`
  keystroke.
- The project's bindings are the wrong set. Checked against them, every report on the CRAN corpus
  was false: re-exports, S4 generics, datasets and `.onLoad` bindings are exported without a
  definition. `NAMESPACE` plus `data/` gives no report on 44 CRAN packages, and a
  planted typo, an internal read and `survival::lung` (one object of `cancer.rda`) behave as R does.
- Unknowable means silent: no `NAMESPACE`, an `exportPattern()`, or an `.R` file under a lazy-loading
  package's `data/` makes the set `None`. The loader passes `None` because it reports no findings.
- A name the package defines gets the "fails once installed, export it or write `:::`" message
  rather than a respelling, because the intent is plainly that function. A name defined nowhere,
  with no export near it, gets the plain "not exported".

## A `[` on a query's result is a query

**Status:** built.

A data.table query that gives a table dispatches `[` on its result to the same method, so every
later link of a chain such as `dt[, .(total = sum(amount)), by = group][order(-total)]` evaluates
in the table's frame. Naming marks a `[` as a masked subset when its target is one that gives a
table (`query_gives_table`: it groups, has no `j`, passes `with`, or its `j` is a `.()`, `list()`,
`:=` or `lapply` call or reads `.SD`), with no data.table special case. A `j` such as `.N` or a
column gives a vector, so `dt[, .N][idx]` indexes it in the caller's scope and an undefined `idx`
is reported. A lone `dt[order(-total)]` without query syntax is base indexing, where `total` must
be a variable. The untyped data-science benchmark has one such chain per generated file, about
1,000 column references in all.
Fixtures: `naming/closures.R.test` and `typing/project-values.R.test`.

## An unassigned path reads the name outside the function

**Status:** built.

- R looks a name up in the enclosing frames when the function's own frame has no binding yet, so a
  read that some path reaches before any local write (`if (ok) limit <- 5L; limit`) reads the
  outer `limit` on that path. Naming (`mark_slot_reached`) returns what that path finds:
  nothing, or an enclosing function's slot, which it records in `enclosing_fallbacks` and marks
  read. A name bound outside the item (another file, an import, base R) is judged where the
  project's names are known: `maybe-undefined` asks `classify_name_read` as a deferred read, and
  the unused check counts the read as a deferred read of the top-level name.
- The checker unions the fallback's type into the read (`unassigned_path_read`), because either
  value reaches it. Reporting only the local writes' type would blame correct code.
- An annotation on a value that is not a function literal (a trusted coercion, `#: fn(integer)`
  on `g <- f`) has no body to infer an elided `->` from, so the return comes from the value's own
  function type, called with the written parameter types (`elided_return_from_value`). Parameters
  pair by position, as a call binds them, because the written signature names no formals.

## A closure's read sees the writes after its definition

**Status:** built

- Naming marks every read that crosses a function boundary (`NamingExtras::captured_reads`). Where such a read finds an environment entry, the checker joins it with each write to the slot positioned after the read (`Checker::capture_writes`, by source offset), so a narrowing at the definition does not survive a later `x <- NULL`, whether from the frame or from a sibling closure's `<<-`.
- The reading body's own `<<-` writes are left to its superassignment seeds. A later R literal is skipped when the read is an open literal with the same default, or a non-literal open variable, or a ry scalar its constraint admits, so `step <- 2` after `step <- 1` stays `I64` in typed code while `x <- 2.5` after `x <- 1L` joins `double`.
- Each captured read carries a horizon, the last offset whose write it can see (`naming::Horizon`). An inline call and a literal passed to an eager caller (`EAGER_CALLERS`: the apply family, `do.call`, `tryCatch`, purrr's `map` family) run before the call returns, so their horizon is the call's end. A literal bound with `name <- function` reaches its latest own-frame call through the slot, extended to the end of the outermost enclosing loop; any other read of the slot makes it escape, and an escaping closure sees every later write. Without horizons a vapply callback or a closure called once before a reset was blamed for writes it never sees. A write on the other `if` arm lies after the arm's last call, so position alone leaves it out. Signature help (`bound_function`) picks the callee bound by the same horizon.
- A newly seen later write records the read's position in `late_triggers`; the function body whose range holds it re-checks once, so a write in a nested closure re-checks the frame that defined the reading closure. Late writes join with `join_closure_write`, which keeps a callback's open result generic (`memoize` returns `T | logical` after a `cache <<- TRUE` reset).

## Feature probes, Windows-only exports and callback strings

**Status:** built

- A `pkg::name` read that is the whole expression of `try()` or of a `tryCatch()` with an `error` or `condition` handler, or that expression's callee, is a probe of the running R (`NamespaceRead::probed`), so a missing name is not reported there. Any other read in the expression stays checked, because code that wraps a whole body in `tryCatch()` (the tryCatchLog style) would otherwise lose export checking.
- `scripts/export-manifests.R` adds the Windows-only exports of base, utils and grDevices (from R's own NAMESPACE files) on every platform, because packages reach them behind a platform test and the manifests are generated on Linux. Per-platform manifests were rejected: a check does not know which platform the code will run on. The names therefore also resolve unqualified (`shell()` in a Windows branch) and appear in completion.
- A character argument at a function-typed parameter of a function that takes ry values (`Checker::ry_callee`) is a type error with a "pass the function, not its name" hint, since ry functions call their callback directly and never `match.fun` it. R functions keep accepting a name.

## Shipping stack: Plain R comes from `ry build --plain-r`

**Status:** built.

Code that must run on GNU R (a CRAN submission) goes through `ry build --plain-r`, which writes the tarball's
R files through `syntax::inline_annotations_to_comments`: each statement's inline annotations become a `#:`
block above it, the rest erases as `erase_inline_annotations` does, and each annotation no block can carry
(a lambda argument's signature, a `@Name(value)` inside an expression, an inline type on a statement that
already has a block) is a warning. A flag, not the default, because the default tarball is the one ry's
runtime installs, with its rewrites for the runtime; a plain tarball has none. ry code with no R spelling
(records, tags, suffixed numbers, native functions) refuses the build. Pinned by
`crates/syntax/tests/comments` and `build-plain-r.scenario`, and checked by hand to report the same findings
on both spellings.

`declare(type(...))` is not built as a third spelling. The `#:` comment is already the spelling that keeps a
file valid R, and it covers every position (parameters, returns, binders, directives), which a call inside
`declare()` cannot reach without inventing a grammar inside R call syntax. A third spelling would add a
lowering path and a third way for two annotations to disagree, for no case `#:` misses.

## A top-level `if (FALSE)` body binds no package name

**Status:** settled.

- Packages keep example code in `if (FALSE) { ... }` at the top level (ff's `ordermerge.R`). As
  package globals, its writes joined other items' SCCs, the fixpoint failed to converge and pinned
  real functions to `Unknown`, which hid true reports in the same file.
- `naming.rs` resolves the body in its own `Local` scope with a stable id, marked escaped so its
  stores never report unused, and `<<-` inside stops at that scope. Only the literal `FALSE`
  counts: any other condition may run.
- Its calls are still checked, because the example is usually how the package means to be called.

## Diagnostic messages are lowercase and end without a full stop

**Status:** settled.

- Every finding's message and related label starts lowercase and ends without a full stop, in the
  style of rustc. A message may hold several sentences: the later ones are capitalized and keep
  their full stops, and only the last loses it (`this comment is code. Delete it, since version
  control keeps the history`).
- A message keeps a capital when its first word is `I`, `R`, code, an acronym or a proper noun
  (`I could not resolve ...`, `R's $ operator ...`, `S4 classes ...`, `Reference Classes ...`).
- A hint appended to a message carries its own separator (`". Did you mean `x`?"`), so the base
  message never ends in a full stop that an empty hint would leave behind.
- Quick-fix titles are not messages: they are capitalized imperatives (`Change to `TRUE``), as
  editors show them in a menu.
- `semantics::testing::assert_message_style` checks every message the typing and lint fixtures
  render, so a new message in another style fails its suite.
- Rejected: two shapes (sentence-cased with a full stop for sentences, lowercase without for
  fragments). Which messages count as sentences is a judgment per message, so no test can hold
  it, and the corpus had drifted under it. Lowercase was already the majority (229 of 355
  rendered messages).

## A called name inside a table's mask resolves loudly

**Status:** settled.

- A masked argument's unresolved reads stay quiet because a bare name may be a column. A called
  name may not: R looks it up as a function, skipping non-functions, and a table's column is a
  vector. So inside `subset`, `transform`, an `@masked` verb or a table `[`, an unresolved callee
  reports (`mutate(d, b = scale_it(a))`).
- `with` and `within` stay fully quiet, because their data is any list or environment and often
  holds functions (`with(options, real_callback(x))`, found on the CRAN corpus).
- A quiet level from anything else (a formula, a quotation, `%op%`) keeps the callee quiet too:
  `mask_depth` in `naming.rs` counts only table masks, and the callee is loud only when every quiet
  level is one.
- `MASK_ONLY_CALLEES` names what a mask provides without an import: data.table's `.()`, `J()`,
  `:=`, `let()` and `patterns()`, and tidyselect's selection helpers, which tidyselect puts in
  every selection's mask, so `where()` works under tidyr with nothing attached.

## S3 methods are held to their package generic's formals, by `R CMD check`'s rule

**Status:** built (`s3_method_finding` in `crates/semantics/src/diagnostics.rs`).

- A package function `a.b` is a method when the package's winner for a prefix `a` calls `UseMethod("a")`; the longest such prefix wins. The generic is reached through `package_name`, so a method's check depends on its generic's name alone.
- The rule is `tools:::checkS3methods`'s: the generic's parameters up to its first `...` (cut at the method's own `...`) match by position, every generic parameter appears in the method unless the method has `...`, and a method parameter outside the generic needs `...` on the generic. One finding per method, the first rule it breaks.
- NAMESPACE registrations refine the name rule: when the package registers any method of a generic with `S3method()`, only registered functions are its methods (a dotted helper is not), and `S3method(g, cls, fn)` makes `fn` a method of `g`. Read through the per-name `registered_methods` and `registered_generic`, from `PackageMetadata::s3_methods`.
- Generics from other packages are not checked: stub formals are written for typing and need not be the real ones, so a stub generic would turn a stub's spelling into a false report.


## A stub never sees the project's own types

**Status:** built (the messages in `stub_source_problems`, `crates/semantics/src/stubs.rs`).

- A `.Rtypes` stub describes code ry cannot read, and the stub set is a set-once input. Letting it read the project's `@type` declarations would make stub loading depend on every project file, so one edit would re-verify every stub-derived memo, and a type would have two homes.
- A type the project's R code defines takes its operator methods as annotated R functions in that code, which is what an S3 class author writes anyway. The stub problem for an unknown type says so, and a backtick-quoted stub name is told to drop the backticks rather than reported as an invalid name.
