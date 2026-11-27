# Decisions: Type checker

Type inference: overloads, narrowing, unions, nominal types, recursion and strict mode.

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
- `scc_exports` runs Gauss-Seidel iteration in a solve order fixed by the group alone (a depth-first
  post-order of its edges from its first member, so callees come first): all members start at `Unknown`,
  and each member reads the exports its predecessors wrote earlier in the same round. Reading only the
  previous round's table (Jacobi iteration) needs one round per link, so a cycle longer than the cap (a
  chain of 16 small functions through one annotated function) would end at `Unknown` although its types
  are fully determined; in callee-first order such a chain settles in one or two rounds. A group still
  changing at the round cap (`SCHEME_ROUND_CAP`, 16, shared with the backstop) pins all members to
  `Unknown`; a member whose exports keep growing is widened first (see "A recursive group widens an
  export that keeps growing"). Member checks run through `SalsaGlobals`, so no salsa cycle forms.
- `item_check` adopts the canonical exports as a member's scheme, and `global_scheme` reads only
  `item_check`, so there is one source of truth.
- Salsa cycle recovery (`item_check_recover`, `global_scheme_recover`) is a backstop, and reaching it is a
  bug: salsa never backdates a cycle member, so restoring a recovered cycle panics
  (`removing_a_cross_file_cycle_and_restoring_it_revalidates` in `test_incremental.rs`).

Pinned by `cyclic_group_answers_are_forcing_order_independent` in `crates/semantics/tests/test_parallel.rs`
and the fixture `recursion__a_long_recursive_chain_settles_through_its_annotation`.

## Two open comparison operands stay two types, and a ry value checks the pair at the call

**Status:** built.

`function(a, b) a < b` is `<T: comparable or ordered, U: comparable or ordered> fn(a: T, b: U)`
(`eq` under `==`). R compares across types and a function often returns one operand unchanged
(`keep <- function(x, lo) { if (any(x < lo)) warning(); x }`), so unifying the operands was
rejected: it widened `keep(as.Date(...), "2019-01-01")` to `Date | character` and let one operand's
constraint bind the other (`(d + 30) > cutoff` refused a string `cutoff`). ry compares a ry value
only with its own type, so the body records the pair (`FunctionType::compared`, inferred only, not
printed) and `check_compared_pairs` checks a call: a ry value on either side checks the other
side's argument as a parameter of that type (a literal adapts, so `cmp(1i64, 2)` passes), and a pair
the call leaves open passes to the caller's signature. The pending pairs live in the inference
table, so a rollback drops them. A pair the body settles is checked at the item's end (at the
comparison, after default literals settle); a pair with a var of an enclosing function moves to it,
so curried functions and local closures carry it. The pair is part of the function type's identity:
unifying two function types and passing a function value check both sides' pairs. A side settled on a ry value
binds the other side's open type (any statement order); a side settled on an R value constrains the
other to the R half of its constraint (`comparable` under an order), so a ry-only constraint
reports. A declared binder counts as open, so an annotated `<T: ordered, U: ordered>` keeps its pair. The call finds an argument by
its parameter's type identity, never by type equality, so another parameter of that type is never
blamed. A left-out default adapts only when it is a number, string or logical literal (`ComparedPair::left_literal`), and an
annotated function keeps its pairs too. An overload set or a third binder spelling
the relation was rejected as heavier than a side fact the annotation syntax need not express.

A variable bound under a comparison constraint (`Widening::Compared`, `infer.rs`) widens over R's
types, numbers by promotion and others to the union, so a declared `<T: eq>` takes
`Bool.is_eq(1L, "one")`; a ry value never widens. `==` and `!=` beside a literal ask `equatable or
number` and `equatable or Str`, where `equatable` is `comparable` plus `complex` and `raw`, which R
tests for equality but does not order. Pinned by
`two_flexible_comparison_operands_stay_separate_types`,
`a_compared_pair_follows_the_operands_everywhere`,
`a_compared_pair_checks_what_the_body_settles` and
`an_open_value_tested_for_equality_admits_complex`.

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

## Call checking follows R's lazy argument and matching rules

**Status:** built, except the rule that a branch's write counts only when every arm makes it, which "A conditional write of a formal covers the paths that run it" supersedes.

- **Required formals.** A formal is optional unless the body may force it (`check::FormalUses`, in
  evaluation order): a read no write dominates forces; a loop body's or a `&&` operand's write never
  dominates, and `<<-` never assigns the local formal; `missing()`, `substitute()`, `quote()` arguments do
  not force; reads in nested functions do only where the function runs (an inline call, a call
  through its local, the returned value), never when it is passed to a call such as `lapply`, which
  may not call it (httr2 reads `lifespan` only in a callback over a vector that is empty by
  default); a forced default forces what it reads, to a fixpoint.
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

## A computed function that may be `NULL` is called as the function

**Status:** built.

`infer_ordinary_call` drops `NULL` beside function members of a callee's type unless the callee names a
parameter, and `dispatch_call` checks a union of functions and values against its function members only,
with an `Unknown` result. Per "The checker presumes R code is correct", the `NULL` path of a computed
function (a `switch` without default after `arg_match`, a correlated `if`) is one the code presumably rules
out. A parameter's `NULL` comes from its default or declared type, which the caller supplies, so that call
is still reported. Accepted cost: a `switch` on an unvalidated value loses its true positive.

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
  binds them. A settled ry state is the exception and keeps its hint, and "settled" admits open
  variables the arguments brought (`settled_hint` names each by the argument's own variable, which
  survives the probe's rollback). So `(values = [], n = 0i64)` hints `values: List<a>` with the empty
  list's own `a`, the step function decides it, and the preflight hands the runtime its item type.
  Without the hint the step read an open `acc`, a record update on it tied nothing, and the empty
  list stayed untyped, so the walk ran in the interpreter (quadratic: 8,000 appends took 14 s).

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
(`subset_method_result` in `check.rs`), as S3 dispatch would, before the structural read, so an abstract type
gets typed field reads without exposing its representation
(`a_dollar_method_types_field_reads_on_its_class` in `nominals.R.test`).

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

## An `if` with an `Any` branch is `Any`; a two-path write still unifies

**Status:** built.

`join_branch_values` returns `Any` when either side is `Any`, as unions do; unifying let `f: Any` take the
other branch's signature. `join_written_values` keeps unifying, because the `Any` rule there lost true
positives (NlcOptim's matrix called as a function).

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

## Alias cycles are reported at the declaration, and short names get narrow suggestions

**Status:** built.

`alias_cycle_diagnostics` (`diagnostics.rs`, via `check::alias_cycle_walk`) reports every alias on a cycle
at its declaration, and a use drops the annotation silently, so the mistake is reported once. It runs per
file over that file's declarations, so it adds no project-wide read. A two- or three-character name gets
a suggestion only when exactly one candidate is it with one character inserted (`nam` finds `name`, `df`
never finds `dt`), because substitutions on short names match unrelated names.

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
  one as a branch is `Unknown`, as is one with any `Unknown` branch that is not a recursion
  placeholder (see "An `Unknown` branch makes an `if` `Unknown`").
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

## Strict mode reports where `Unknown` enters, not every type it reaches

**Status:** built.

- **No strict rule tests a finished type for `Unknown`.** Strict mode reports origins (an unsupported construct, an undeclared reference, a loop-widened or recursive binding, unconstrained parameters), and an `Unknown` that flows on from an origin is a consequence, reported once where it entered.
- Measured with `ry debug analysis-stats`, which prints the count. On the ten-package corpus with strict forced on, 1665 of 4036 package items export a type containing `Unknown`, and 1563 of them have a finding in the same item. Every sampled one of the other 102 inherits an `Unknown` that a project callee's own finding reports (renv's `data_frame <- function(...) as_data_frame(list(...))`, knitr's `child_mode` over `opts_knit$get`). The strict typed finbench exports `Unknown` from 0 of 40,746 items. A containment rule would repeat each origin at every item downstream of it.
- **A call to a base function no stub declares is an origin** (`R.home()`, `.Call(...)`), like a package function with no stub. Reading such a name as a value stays unreported, because intrinsics such as `c` and `[[` have no stub and are typed at their calls. `@if-unknown` accepts the result, and the remedy is a stub line.
- `g <- f` keeps `f`'s scheme rather than exporting `fn(p: Unknown) -> Unknown` (`aliased_function_reference_exports_closed`).

## An all-`logical` call of a numeric function keeps `logical`

**Status:** settled.

- `bump <- function(x) x + 1L` is `<T: numeric> fn(x: T) -> T`, so `bump(TRUE)` is `logical` where R gives `2L`. A `logical` argument meets the numeric constraint, and mixed arguments join (`add(1L, 1.5)` is `double`).
- Binding `T` to `integer` for a `logical` argument would mistype `checked <- function(x) { stopifnot(x + 1L > 0L); x }`, which returns the `logical`. Only a type-level `promote(T, integer)` separates the two, and no corpus or benchmark finding depends on the difference, because a `logical` fits wherever an `integer` is expected. Build `promote` when a false report or a typed-ry need shows up, not before.

## A wrapper over a numeric overload set takes the `numeric` bound

**Status:** settled (corpus, 44 CRAN packages and the three benchmark code bases: no new finding).

- **Rule.** In `Checker::overloaded_call`, when the set's `Any` fallback is its only free fit and every specific candidate narrows a caller variable to number atoms (`numeric_operand_parts`), the variable takes `Constraint::Numeric`. A union parameter (`integer[] | logical[]`) bounds rather than binds, so the narrowing reads the bound.
- **Why a fact, not a guess.** Fact-beats-guess keeps a parameter open because candidates disagree. Here they agree on "a number" and differ only in which one, and the constraint is the agreement. A class with an arithmetic method satisfies it, which keeps a user's S3 number-like class working.
- **Number atoms, not the constraint.** `sweep`'s `matrix` candidate is admitted by the numeric constraint (`Arith.matrix`) yet takes a character matrix, so the test asks for atoms, and `max` (which takes `character`) imposes nothing.
- **`NULL` is not a number.** `abs(NULL)` is an error in R. The cumulative functions take `NULL` as an empty vector, so their stubs declare a `NULL` candidate and their wrappers impose nothing; admitting `NULL` in the constraint instead turns a `NULL`-default argument used in arithmetic into an `Unknown` result.
- **Known gap.** A class with only `Math.<class>` or `abs.<class>` methods and no arithmetic is rejected by an `abs` wrapper. `Math.` is not an arithmetic prefix because that set also drives operator dispatch.
- **Not covered.** `sum` has no fallback and its candidates include `difftime` and `data.frame`, so a `sum` wrapper stays `fn(x: Unknown) -> Unknown`.

## Branches decide each other's literal items, and `c()` decides a literal as R's type

**Status:** built.

- A tuple item or record field written with a literal in one branch of an `if` or `match` takes
  the type of the item in the same place of the other branch (`decide_item_literals`): a ry
  scalar unifies with it, an R atomic type decides it as R's own type, and a literal of another
  kind, such as `"a"` beside an `F64`, stays open for a use. A literal the number cannot hold is
  a `type-mismatch` at the literal. A loop pass writes fresh literal variables, so an open item
  would differ on each pass and the loop fixpoint would report that `step` does not stabilize.
- A tag payload's literals are open (`infer_payload` goes through `component`), so a later use
  decides them as it decides a tuple item: `found <- :Ok(1)` returned from `-> Result<I64, Str>`
  is `:Ok(1i64)`. `join_shared_parameters` joins tag arguments part by part (`join_ry_values`),
  so `List.append([:Ok(1)], :Err("x"))` under `-> List<Result<I64, Str>>` keeps both payloads
  open; only payloads that do not join are settled first, and `report_conflicting_tags` settles
  them too, or a conflict would name a type variable. `:Step(0, a)` beside `:Step(q, p)` adapts
  through `adapt_literal`. A ry literal such as `7i64` does not adapt to `F64`.
- A number, string or logical literal passed for a ry function's type parameter (`ry_callee`,
  an unconstrained or open-literal parameter) is a fresh open literal, so the next argument or
  the call's use decides it: `List.walk(words, "", f)` with `f` taking `Str`, and
  `Result.with_default(:Ok(1), 2)` under `-> I64`. Deciding it as R's type at the argument made
  each of these a `type-mismatch` that only an annotated temporary avoided.
- Each loop pass writes fresh variables for the literals inside a value, as in `:Stop(1)`, and
  the loop-back join ties them to the previous pass's variables part by part, so they stay open
  for a later use: `r <- :Err("none"); for (x in xs) r <- :Ok(1); r` under
  `-> Result<I64, Str>` is `:Ok(1i64)` on every pass. `join_writes` treats two writes that
  resolve to the same type as equal.
- An open literal is not a value a caller chooses, so `join_branch_values`' differing-openness
  rule (which unions rather than pins a parameter) ignores literal variables: `(:Stop(1.0), s)`
  beside `(:Go(v), s)` joins to `Tuple{:Go(double) | :Stop(double), I64}`.
- An R integer literal and an R double literal never become one literal (`Constraint::join`),
  because `1L` beside `2.5` would make a `double` that holds `1L`: `:Ok(1L)` and `:Ok(2.5)` join
  to `:Ok(integer | double)`. `Result.with_default(:Ok(1L), 2.5)` is `double`, because the
  integer literal accepts R's widening to double.
- A report names an open literal by the type it defaults to (`Checker::reported`, a probe that
  settles and rolls back), so `r <- :Ok(1); r + 1` says `found `:Ok(double)``, never `:Ok(T)`.
  `type_mismatch` and a callback's explanation report through the same probe, so a report never
  decides a literal for later uses.
- `c` is an R function, so `infer_combine` decides a variable written with a literal as R's type,
  as any other read does, and `bounds <- 1; bounds <- c(bounds, x)` is a `double[]` in strict
  mode.

Fixtures: `typing-strict/strict.R.test`, group `literal_decisions`.

## A loop's `break`s and `next`s join where they land

**Status:** built.

- Each loop pass keeps a `LoopJumps` frame. A `break` or `next` snapshots the slots written so far
  in the pass: a `next`'s state joins the loop head beside the end of the body, and a `break`'s
  state joins the state after the loop. Naming mirrors this with `loop_nexts` beside
  `loop_exits`, so a write read only by the next iteration is not unused.
- A `repeat` leaves only through its `break`s, so the state after it is the union of the break
  states over the converged loop head (`leave_through_breaks`), never the end of the body.
- A nested `for` over an enclosing active loop's variable reuses that variable's slot, because R
  writes the same variable. The checker finds each loop's slot through
  `NamingExtras::for_variables`, keyed by the loop expression, since a reused slot's range is the
  outer loop's.

Fixtures: `typing/control.R.test` group `loops`, `naming/for-bindings.R.test`.

## An error against a fixed type keeps the item's export

**Status:** built.

- A failed item exports `Unknown` so one mistake does not cascade. An error whose value was
  checked against a fixed type, an annotated binding's or a ry record field's (including a field
  the record lacks), leaves the slot holding that fixed type, so `Checker::contained_errors`
  records its range and an item whose every error is contained keeps its export. Later statements
  then still report their own mistakes, such as a field typo after an invalid field write.
- A column update (`f[, y := v]`, `setnames`) writes the table in place and is never an unused
  assignment, at top level either.
- A non-function where a callback goes reports "expected a function that takes `integer`" rather
  than a signature whose undecided types print as variables the reader never wrote.
- A `Module.member` name of the standard library that does not exist says so, and R's size words
  (`length`, `nchar`, `size`, `len`) suggest `List.len` or `Str.count_utf8_bytes`, which no small
  edit reaches. Such a misspelled callee skips the native-argument check, whose complaint would
  only restate the miss.

Fixtures: `typing-scripts/scripts.R.test` (`a_field_write_mismatch_keeps_the_record_type`,
`a_column_update_is_not_a_dead_store`, `a_size_name_from_r_suggests_the_library_spelling`,
`a_number_where_a_callback_goes_names_the_parameter_types`).

## An annotation that misuses the type vocabulary carries no payload

**Status:** built.

- An annotation that names an undeclared type or writes `X[]` over a non-atomic element is
  refused in the checker (`refused_by_vocabulary`, beside the alias-cycle refusal), not in
  lowering, because both judgments need the project's type vocabulary, which lowering does not
  read. The code is then typed as if unannotated, so the one annotation finding is the only one.
  The `[]` finding keeps the `type-mismatch` code so it disappears when typing is off.
- A malformed optional parameter (`[character]` with no `: TYPE`) leaves an empty `ERROR` node, so
  the block is `Unparsed` and refused like any unreadable block.
- `vapply`'s generic candidate fixes `V` from the callback before it reaches `FUN.VALUE`, so the
  mismatch lands on the template as `V[] | V`. The call rewrites that finding to name the callback
  (`TemplateDisagreesWithCallback`) rather than threading which argument fixed a variable through
  the generic argument check.
- Hover on a top-level name renders the item's exported scheme only when the item assigns that
  name; in `m$amount <- 5` the scheme is the value's.

## A declared type parameter keeps its name in messages

**Status:** built

- `TypeRenderer` prints a rigid binder whose name is an identifier as written (`<N: numeric>` gives `N`), so a report on a user's signature uses the user's names. Generated binders are named `#0`, `#R1` (`abstract_vars`, the closer), which no identifier spells, and the renderer letters them `T`, `U`, ... in order of appearance, skipping a name already taken.
- A local `id <- function<T>(...)` instantiates its checked signature one level up, so the binding generalizes to the declared scheme, as an unbound literal instantiates at its use.
- A rigid binder meets a constraint through its declared constraint or its type bound (`InferenceTable::rigid_satisfies`), so `<N: integer | double>` is numeric for an operator.

## A nominal field narrows through a fact, not a rewritten type

**Status:** built

- `is.null(x$field)` narrows a structural record by rewriting the slot's type, but a rewritten `Tree` representation is no longer a `Tree`. For a slot whose remaining members are all nominals naming the field exactly, the false edge establishes `Fact::FieldNarrowed { slot, field, ty }` instead, and `Checker::infer_field` reads `x$field` from it. The slot keeps its nominal type.
- A fact dies with any write to the slot (`Environment::set` forgets it), and a non-diverging `if` retracts its false-edge field facts (`retract_false_facts`), since only a diverging then branch makes the false edge hold afterwards. A closure's read ignores the fact when a later write to the slot exists.

## A type past the size ceiling widens to `Unknown`, never to a recursive nominal

**Status:** settled

- A self-referential value, such as a record whose fields return that record, builds a type whose tree grows exponentially while its hash-consed graph stays small. Building it costs graph nodes only, and every type stores its saturating tree size when built (`types::type_size`), so no cap is needed at construction; `Checker::record` widens a composite at `TYPE_SIZE_CEILING` to `Unknown`, the sound-by-refusal move a loop join makes for a structurally growing variable.
- Folding into a recursive nominal is rejected: it would invent a type name the user never wrote, every message would show it, and two values folded apart would not be compatible. A user who wants the recursive shape declares it (`type Tree = list{left: Tree | NULL, ...}`), which checks precisely.

## A `logical` argument binds a numeric type parameter to `integer`

**Status:** built

- `numeric` admits `integer` and `double`. A `logical` argument still satisfies it, because R promotes it, and binds the parameter to `integer`, the type R's arithmetic reads it as (`check_argument`, `promote_logical`). Leaving it `logical` typed `bump(TRUE)` under `<T: numeric> fn(x: T) -> T` as a flag where R returns `2L`, which a later `logical`-only use would accept unsoundly.
- The cost is a pass-through: `keep <- function(x) { x + 0L; x }` claims `integer` where R returns the `logical`. That is safe, because a `logical` fits wherever an `integer` is expected.
- A rigid whose declared bound admits `logical` does its arithmetic as its bound (`classify_numeric_operand`), so `<N: integer | logical> fn(x: N) -> N` with `x + x` is reported: the sum is `integer`, never `N`.

## An empty list adds no element to `c()` or a loop

**Status:** built

- `list()` is the empty tuple, and its element is the union of zero items, which `union_of` answers as `NULL`. `c()` and a `for` loop over a union skip `NULL` and empty-list members instead (`is_empty_list`), because they contribute no element: `c(list(), f)` is `list[fn]` and `for (f in actions)` over `list{} | list[fn]` binds a function. rmarkdown's exit-action loop was reported as calling a maybe-`NULL`.

## A signature's type parameters are in scope in its body

**Status:** built.

- **Rule.** An annotation in a function's body may name the type parameters of every enclosing signature, outermost first (`annotations::enclosing_type_parameters`), whether the binder is inline (`function<T>`) or in a `#:` block. The body annotation reads the same rigid `T` the signature binds, so `#: T | NULL` widens it and `#: U` where `T` is expected is a mismatch.
- **The root item's block.** An item's syntax window is detached from its own `#:` block, so `expression_annotations` passes that block's binders (`block_type_parameters`) in as the outer scope; a nested function's block is in the tree and is found by the ancestor walk.
- **An inferred signature binds no name**, so a body annotation under an unannotated function still reports an unknown `T`.

## `opaque` makes a type's representation private to its file

**Status:** built.

- **Rule.** `opaque Name = TYPE` (or `#: @opaque Name {TYPE}`) is a nominal type that only its declaring file may build (`@new`) or take apart (`$`, `[`, `[[`, their writes, `for`, a destructuring `match` arm, widening to the representation). Other files, test files included, name and pass it. Without this, `@new` anywhere in the project made a nominal type's invariant a convention.
- **Why the file.** R has no module below the package, and a package's namespace is open to its own code, so the file is the one boundary a reader can see. Gleam's `pub opaque type` and Roc's opaque types draw the same line at the module.
- **One gate.** `TypeDefinitions::get` returns `hidden_type_definition` (body `Unknown`, `Opaque::hidden`) when the definition's home is not the asking file, so every structural path (projection, widening, arithmetic) sees no representation without a check of its own. The explicit `refuse_hidden_representation` calls exist only so access reports the opaque type instead of an abstract nominal's silent `Unknown`.
- **Not refused.** R functions over any value (`length`, `attr<-`, `unclass`) still reach it; they leave the type system for every type, and refusing them would make an opaque value unusable with base R. Nothing changes at run time.
- **Naming.** A stub's bare `@type NAME`, which has no representation, was renamed from "opaque nominal" to "abstract nominal" in the docs, so one word names one thing. The stub attribute `@opaque(...)` on a function's parameters is a separate, stub-only spelling.

## Open records are explicit, with no row variables

**Status:** built.

- **Rule.** `list{a: A, ..}` (`TyKind::OpenRecord`) accepts a record or open record carrying at least the listed fields, paired by name. A closed `list{a: A}` keeps exact field-set equality, so a surplus or misspelled field stays a finding. Implicit width subtyping on every record was rejected for that reason: record typos are the most common record finding.
- **Reads.** `$` and `[[` by name behave as on a closed record (exact, unique prefix, near-miss typo, else `Unknown`). Positional `[[` and `[` read `Unknown`, because an open record's listed fields are not its first ones.
- **No row variables.** Roc's `{ a : A }*` and `{ a : A }r` pass the unlisted fields through a function; ry's `..` cannot, so a function returning its open argument returns only the listed fields. Add a row variable only when a corpus needs that round trip.
- **Scope.** Only the R record shape is open. Frame types list every column, and native `Record{...}` stays closed until typed ry code needs extensible records.

## Recursive types are nominal; an alias never recurses

**Status:** built.

- **Rule.** An `@alias` whose expansion reaches itself stays an error. A recursive type is a nominal `@type`, whose name stops the expansion, so `log$set_threshold("info")$info("hi")` types through `Logger`.
- **Why.** A recursive structural alias is an equi-recursive type: comparing two of them needs coinductive equality over cyclic types, and every structural walk (`map_child_types`, rendering, `type_size`) would need cycle handling. A nominal type is iso-recursive and costs nothing new. Rust, Roc and Gleam make the same choice.
- **Diagnostic.** A cycle through a record or function suggests `@type NAME {...}`; a bare alias chain (`A {B}`, `B {A}`) is a plain mistake and keeps the "write a type in place of one alias" wording.

## S4 is typed through a nominal wrapper, not from `setClass`

**Status:** built.

- **Rule.** `setClass` registers no type: a class name in an annotation is unknown unless a `@type` declares it, and `x@slot` is `Unknown` (a strict origin) except on an `Any` subject, where it is `Any` like `$`. A `setGeneric` generic's call gives `Any`, as an S3 generic's does.
- **Why.** ry discourages S4 (a lint warns on it) and recommends records, so S4 gets the documented route only: a nominal `@type` with a record representation and a constructor wrapping `new(...)` with `@Name(...)`. Reading slot types from `representation()` would duplicate that path for a system users are steered away from, and dispatch over methods registered in any file has no single signature to give a generic.

## A typed environment lists its bindings and never changes type

**Status:** built.

- **Rule.** `environment{count: integer}` (`TyKind::Environment`) types an environment's bindings. A `$<-` or `[[<-` write is checked against the binding and the variable keeps its type (`check_fixed_field_write`, shared with ry's `Record`); an unlisted name is an error on read and write. `new.env()`'s abstract `environment` fits any typed environment and the reverse, and two typed environments must match field for field in both directions, since they alias.
- **Why not a record.** A record write builds a new record type (`x$a <- v` adds or retypes `a`), which is R's copy semantics for lists. An environment is shared by reference, so a write through one name is visible through every other, and only a fixed type is sound.
- **Why closed.** An open environment could not check a write to an unlisted binding, and a typo in a write is the common mistake with environment state.

## S3 subsetting dispatches on the declared class first

**Status:** settled.

- `[`, `[[` on a nominal look up `[.class` for the declared class before the representation's,
  because the declared class leads the class vector. An `xts` (`@type xts {zoo}`) took `[.zoo`
  whenever zoo was imported and lost its `Arith.xts` methods.

## A generic's dispatch formal is forced when an argument reaches it

**Status:** settled.

- `FunctionType::dispatch` names the formal an inferred body's `UseMethod` dispatches on: the
  one its `object` argument names, else the first. Any other `object` expression gives none.
  A caller forwarding its own formal there makes that formal required, because `UseMethod`
  evaluates the supplied argument to read its class. A direct call may still omit it (R then
  dispatches on a missing object, and the method decides). Declared signatures carry no dispatch
  formal: an annotation states requiredness itself.

## A nominal over a nominal extends it

**Status:** settled.

- `@type TradeDate {Date}` makes a `TradeDate` a `Date`: compatibility projects a nominal to its
  representation (one source of truth, no separate hierarchy table), and operator and `[`
  dispatch walk that chain (`Checker::class_chain`), as R's class vector `c("TradeDate", "Date")`
  does. The reverse direction stays an error.
- The stubs declare `data.table {data.frame}` and `ggplot {gg}`, which replaces the exact-name
  match that rejected `needs_df(data.table(...))` and accepted the wrong direction.
- Acyclic by construction: a chain stops at a name it has seen.

## `numeric` is closed under arithmetic, `arithmetic` is not

**Status:** settled.

- R defines `Date + 1` and not `Date + Date`, so "the class declares an arithmetic method" does
  not imply "two values of it combine". One constraint for both let `add_them(d1, d2)` under
  `<T: numeric> fn(a: T, b: T)` launder the error the direct `d1 + d2` reports.
- `arithmetic` (loose) admits numbers and any class with an arithmetic method. It is what one
  flexible operand beside a number asks (`x + 1`), and what a numeric-only stub set asks
  (`abs(x)`), so no legal call is rejected.
- `numeric` (closed) admits numbers and a class with a `+.C`, `Arith.C` or `Ops.C` method whose
  first two operands both take the class (`ArithmeticClasses::closed_under_arithmetic`). It is
  what two operands tied to one type ask (`a + b`, `x * x`) and what unary minus asks.
  `numeric` keeps the spelling users write, so a declared `<T: numeric>` means what it reads as.
- `join` treats `arithmetic` as `numeric` against every other constraint and keeps it loose
  unless the other side is `numeric`, so no lattice pair is new.
- Rejected: operator-specific constraints (`T: Add<T>`), which are traits by another name and are
  declined (`development/design/open-questions.md`, section 3).
- `a - b` on two tied operands asks only `arithmetic`: R defines `-.Date` and `-.POSIXct` for
  two values of the class, and `abs(x - y) < tol` over times is common (testthat's compare). The
  tie's result stays `T`, so such a helper over dates claims `Date` where R gives a `difftime`;
  that imprecision is the cost of having no operator-specific constraint.


## `new()` is checked against the slots `setClass` writes out

**Status:** settled

- `s4.rs` reads each top-level `setClass` call as written: `representation(...)` (named entries are slots, unnamed strings parents), `slots =` (named or bare strings) and `contains =`. `class_slots` is a per-(file, class) projection over the package's definitions, so a `new()` check never reads the project-wide map directly.
- Inheritance is followed, because a subclass legitimately takes its parent's slots. Anything the call does not settle (a computed slot list, a parent outside the project such as `numeric`, a class defined twice, an `initialize` method, which may take any argument) makes the check silent: a false positive on correct S4 code costs more than a missed typo.
- The finding is `type-mismatch` at the argument name, like a nominal's missing field. Slot types stay `Unknown`; typing them is a separate step.

## `sapply`, `mapply` and `Map` take their shape from the callback's return, after the stub call

**Status:** built (`simplified_apply` in `crates/semantics/src/check.rs`).

- The stubs return `Any`: the shape depends on the callback's return and the `simplify` flag, and an overload set cannot discriminate on either, because both leave every argument type the same.
- The checker types the call through the stub first (so the callback is checked against the elements), then refines: a scalar-like return gives `T[]`, a literal `simplify = FALSE` gives `list[T]`, anything else keeps `Any`. Names follow R (a named input, or a character input unless `USE.NAMES = FALSE`).
- The scalar-like claim is trusted as length one, the same trust the vector rules already extend, so a callback typed scalar that returns several elements is a missed matrix rather than a false error.
- A generic callback (a parameter, read before the call bound it, holds a variable) is trusted only when every element of `X` is scalar: generic arithmetic claims `double` before the parameter meets an element, so over a list of vectors it would type a matrix as `double[]`. A return holding a variable or a rigid (`identity`) keeps `Any`.
- `sapply` keeps a named input's names whatever `USE.NAMES` says; `mapply` and `Map` name the result only under `USE.NAMES`. A project or local definition of the name wins (`Checker::calls_base`).
- `tapply` stays `Any`: its result is an array shaped by the groups, which needs the matrix-shape design.

## In strict mode, a checked annotation over an `Unknown` value is reported

**Status:** built (`StrictOriginKind::UncheckedAnnotation` in `crates/semantics/src/check.rs`).

- `Unknown` satisfies every type, so `#: double[]` over `df$price` checks nothing, yet it covered the value's strict origins and left no trace. On data-frame code that made annotations look protective when they were not.
- The annotation still covers the origins inside the value (one finding, not one per `Unknown` read), and is reported in their place at the value's range. `@if-unknown` is the explicit "accept unchecked" form, so it, `@trust` and `@new` stay silent, and so does a checked annotation whose declared type is `Any` or `Unknown`.
- Only the value's own type counts: `#: double[]` over `as.numeric(df$age)` checks a known `double[]` and covers `df$age` as before.
- The strict messages for an undetermined binding and an undeclared reference name `#: @if-unknown TYPE` as the fix, because a plain annotation would now be reported in turn.

## An `Unknown` branch makes an `if` `Unknown`, unless it read a recursion placeholder

**Status:** built (`MemberExports::pending`, `GlobalEnv::pending_reads` and `Checker::placeholder_reads`).

- `Unknown` served two roles: the gradual top and the seed of the recursion fixpoint (a group member's export in round 0, a local function's read of itself before its write). Joining an `Unknown` branch away let recursion converge, but also typed `if (flag) 1L else df$count` as `integer`, a false type on untyped data.
- A separate bottom type would need a new `TyKind` handled at every `Unknown` site. Instead the placeholder is tracked by provenance: a group export is `pending` when it is the seed, or when its member read a pending export and still exports an `Unknown`. A read of a captured slot before any write is a placeholder too. `infer_if` compares the read counts before and after each branch.
- An `Unknown` branch that read no placeholder makes the `if` `Unknown`; one that did joins as the other branch. Later rounds read computed exports, so a genuine `Unknown` still wins at the fixpoint (`a_cycle_with_a_genuine_unknown_stays_unknown`), while `pending` keeps mutual recursion from oscillating between rounds.
- A branch that mixes a genuine `Unknown` with a placeholder read keeps the old join. That is imprecise but never a false error.

## A project's top-level `%op%` is checked as a call unless it quotes an operand

**Status:** built (`infer_declared_operator` in `crates/semantics/src/check.rs`).

- `a %op% b` is the call `` `%op%`(a, b) `` in R. A project operator used to give `Unknown` (a strict-mode origin) because one may quote its right operand, as magrittr's `%>%` does.
- The function type already records quoted formals (`FunctionType::quoted`, from `substitute()` reads) for ordinary calls, so the operator takes the same path: checked and typed as the call, unless one of its formals is quoted, which keeps it opaque with its operands' findings discarded.
- An operator defined inside a function stays opaque: it is not a global, and a local binding's scheme is not a `GlobalEnv` read. Lowering `%op%` into a `Call` node was rejected, because the HIR keeps `Binary` for the destructuring forms (`%<-%`) and the `%in%` key tests read it.

## A tied difference is `double`, and only a class closed under arithmetic negates

**Status:** built (`infer_binary_numeric`, `infer_unary_minus` in `crates/semantics/src/check.rs`).

- `function(a, b) a - b` asks `arithmetic`, so two dates pass, and returns `double`, not `T`: R subtracts two dates to a `difftime`, so `T` was a wrong `Date`. `double` is the representation every arithmetic class shares, the claim the other unfixed-atomic cases make. A tied `-` no longer ties a later operand, so `(x - mu) / sigma` leaves `sigma` its own variable.
- Unary `-` on a class needs arithmetic between two of its values (the `numeric` test), as R stops on `-day`; `difftime` and `matrix` negate to themselves.
- An `arithmetic` violation names "or a class with arithmetic" only when the value found is a class, because the plain `half("4")` message is the one most users see.
- An annotated function keeps the formals its body captures with `substitute()` (`FunctionType::quoted`), so annotating a quoting `%op%` does not make its operands checked.

## Two independent open branches join by union

**Status:** built (the branch join in `crates/semantics/src/check.rs`; `typing/calls.R.test`).

- A project `%||%` typed `<T> fn(x: T | NULL, y: T) -> T` tied the fallback to the value, so `opts$flag %||% FALSE` beside `opts$extra %||% list()` was a false report. Two branches open in variables of their own join by union instead of unifying.
- A call drops a type parameter no supplied argument filled, so `NULL %||% "x"` is `character`, not `character | T`.
- A callback's parameters keep tying, because the callee decides how its arguments relate.

## The fresh sample's false-report classes, and what each rule claims

**Status:** built (`crates/semantics/src/check.rs`; fixtures named per bullet). Measured on 99 packages outside the benchmark closure (`tools/corpus/fresh-sample.txt`).

- **A block diverges when any statement does**, so a definition after `stop()` never binds and a later read warns as maybe-undefined (`a_definition_after_stop_never_binds`).
- **`match.fun` passes over a local that is not a function** (`apply(m, 1, prod)` beside a local list `prod`); a local string is the function's name.
- **A stubbed base generic with a project S3 method is unchecked** when the first argument is not a plain atomic, `NULL` or function, because the stub describes only the default method (`a_registered_method_of_a_stubbed_generic_leaves_its_result_unchecked`). A registration alone, not a defined `str.Logger`, decides, matching R's dispatch table.
- **`vapply(NULL, ...)` is an empty result**, as R maps `NULL` to `list()`.
- **A formal used as a position of a two-or-more-position `[` is optional**: R reads a missing formal there as the empty index. A one-position `x[i]` stays forcing, because the checker types it as one element and would otherwise lose that claim; `[[` has no empty index.
- **A formal passed to a callee read from a field or `[[` is optional**: the checker cannot see whether that function reads it.
- **A class-dispatch chain ending in `NULL`** (`if (is.a(x)) ... else if (is.b(x)) ... else NULL`, each test a one-argument project predicate) excludes `NULL` only for an argument that cannot carry a class, so `chain_count(obj)` over an untyped object is not reported, and `chain_count("a")` is.
- **`inherits(x, "cls")` narrows the true edge** to members that may carry a class, dropping atomic, `NULL` and function members. A class R gives unclassed values implicitly (`"integer"`, `"list"`, `"function"` and the rest) narrows nothing, and the false edge narrows nothing because an object may lack the class.

## `@elementwise(x)` gives a scalar result the shape of its argument

**Status:** built (`Library::elementwise` in `crates/semantics/src/stubs.rs`, `elementwise_result`, `elementwise_formals` and `elementwise_return` in `crates/semantics/src/check.rs`; `base-functions.R.test`, `vectorised_math_keeps_the_shape_of_its_argument`).

- `sqrt`, `exp`, `log`, the trigonometric and rounding functions, `nchar`, `toupper`, `tolower`, `trimws`, `chartr`, `is.na` and its siblings declared one scalar result for any input, so `#: fn(x: double[]) -> double` over `sqrt(x)` checked clean and `#: fn(rates: matrix) -> matrix` over `exp(-rates)` failed.
- An overload set per shape was tried and dropped: an unannotated `function(x) sqrt(x)` then matches candidates that disagree, and the overload rules give it `Unknown` or a `numeric` bound across the typing fixtures.
- Target shape: one declaration, `sqrt : @pure @elementwise(x) fn(x: Any) -> double`. After the call types, its scalar result takes the shape of the first marked argument that is a vector (`double[]`), a named vector (`double[named]`, since these functions keep names) or a `matrix` (which records no element type). A union lifts member by member. Any other argument, a scalar, `Any` or an open variable among them, keeps the declared scalar.
- No `array` type: `array()` stays `Any`, which keeps the scalar claim, as before. A base `array` type would need `dim` tracking that no fixture asks for yet.
- An unannotated wrapper takes the shape too. Its signature reads `<T> fn(x: T) -> double`, and the inferred `FunctionType` lists the formals every returned value follows elementwise (`elementwise`, inferred only): a marked call over the formal, an elementwise project function over it, or `+ - * / ^ %%` joining such a value with the formal or a number. A call lifts the scalar result to the argument's shape as a direct call does (`base-functions.R.test`, `an_unannotated_wrapper_of_vectorised_math_takes_its_arguments_shape`). A shape variable was the alternative; it would add a type form every renderer, parser and unifier must carry, for one fact that only calls read. A formal the body rebinds (an assignment, a replacement such as `dim(x) <- NULL`, a `for` variable, or any `assign`, `rm` or `eval` call), an early `return`, a formal callee (`function(x, f = wrap) f(x)`), or another formal in the arithmetic (`sqrt(x) + y`) keeps the scalar claim. Unification ignores the list, so a join of two functions whose lists differ drops it. Limit: a wrapper reached through an elided return (`#: fn(double[])` on `g <- wrap`) keeps the scalar result.
- `as.integer` and the other `as.*` drop names and `dim`, so they are not marked; they keep the scalar claim.

## The fresh sample's false-report classes, second pass

**Status:** built (`crates/semantics/src/check.rs`, `crates/semantics/src/infer.rs`; the fixtures named per bullet).

- **A top-level `formals(name) <- ...` that is the file's last write of `name`** makes the function accept any argument and keep its body's result (`a_computed_formals_rewrite_accepts_any_argument`): the new parameter list is computed, so no type describes it.
- **An unbound variable in a union takes a bound the union meets.** `character | U` passed to `list[T] | T[] | NULL` binds `U` instead of failing, and a `NULL` default joined with a number (`T | double`) passed to arithmetic makes `T` a number (`a_null_default_joined_with_a_number_passes_a_numeric_parameter`). `constraint_rejects` admits a variable whose own bound implies the required one, and `bind` gives each open member that bound first.
- **`tryCatch` handlers take a `condition`**, not `Any`, so `function(e) conditionMessage(e)` and `e$message` type. `inherits(e, "error")` or `"condition"` narrows the false edge too, because inside a handler the value is a condition; for a handler of another class this is an approximation that only drops `condition`.
- **A union of functions fits a function parameter** when each member fits with its own return and the join of those returns fits the parameter's return (`function_union_fits`). A local holding one of two functions passes `lapply`. Forwarded arguments reach each member of such a union (`a_local_holding_one_of_two_functions_takes_forwarded_arguments`).
- **A flag guard block** (`if (!flag) { ...; return(a) }`) selects returns as the one-line guard does, and a negated flag is the same flag (`a_negated_flag_guard_block_selects_its_returns`).
- **Two formulas compare by their text** (`a_formula_compares_by_its_text`), as R's `==` deparses them.
- **Arithmetic registered through S4 or S7** (`setMethod("+", ...)`, `method(\`+\`, ...)`) counts as arithmetic for a class no type declares, like a project `+.cls` (`registers_arithmetic_method`), so an environment operand may dispatch there and gives `Unknown` (`arithmetic_registered_through_s4_may_dispatch_on_an_environment`). A class-dispatch error then needs a mismatch on a class the operand declares itself.
- **An inferred callback bound to a scalar by a forwarded call takes a vector later** (`a_callback_bound_to_a_scalar_by_a_forwarded_call_takes_a_vector_later`): `uniroot` calls `f` with one number, and the same `f` over a grid is R's ordinary vectorised use. Only a parameter variable bound by inference relaxes; an annotation stays exact.

## The fresh sample's false-report classes, third pass

**Status:** built (`crates/semantics/src/check.rs`, `crates/semantics/src/naming.rs`; the fixtures named per bullet).

- **Element writes grow a tuple** (`a_list_grown_by_element_writes_holds_what_was_written`): `d[[2]] <- v` on a one-item tuple appends, and a computed `d[[i]] <- v` turns the tuple into `list[A | V]`. Leaving the tuple unchanged read the seed item's type after a fill loop.
- **An open callback passed to a forwarding library function becomes variadic** (`a_callback_parameter_takes_the_arguments_a_library_forwards_to_it`), because `apply(v, 2, FUN = fun, na.rm = na.rm)` calls `fun(x, na.rm = na.rm)`.
- **A literal-defaulted formal is skipped by the call lookup** (`a_parameter_with_a_string_default_is_skipped_by_the_call_lookup`): `cor = "cor"` holds no function unless a caller passes one, and R skips non-functions.
- **A `matrix` and a named vector are conditions** where a vector is; `@elementwise` lifts a `matrix` argument only for a `double` result, because the element-less `matrix` would lose `is.finite(m)`'s `logical`.
- **A computed-name `assign`, `delayedAssign` or `list2env` into the running frame makes later free reads quiet** (`naming/opaque.R.test`). The frame argument must be absent, `environment()`, or a local assigned from it; any other environment leaves reads ordinary. A called name keeps its function lookup, because the package's functions are what such code calls, and the checker types a read of a top-level name there as `Unknown` (`a_name_a_computed_assign_may_bind_is_not_the_package_function`).
- **A direct call of an inferred helper gets the length tolerance but keeps its `NULL`** (`a_call_of_an_unannotated_helper_passes_one_string_on`). Dropping the `NULL` too removed pinned reports (`1:chain_count("a")`, the `unguarded` cases in `control.R.test`), whose contract is that only a variable can be narrowed.
- **A callback passed by name drops its inferred return's `NULL`** against a template that admits none (`an_unannotated_callback_that_may_return_null_fits_a_vapply_template`); a lambda keeps it, since its `NULL` is written where the caller can see it.
- **`sapply` over an `@elementwise` function makes no length-one claim** unless the elements are known scalars (`an_elementwise_function_applied_over_unknown_elements_is_no_length_one_claim`): each element may be a column, and `sapply` then stacks a matrix.
- **A `:` endpoint drops the placeholder `NULL`** of a computed variable or an undeclared formal, as an argument does (`a_sequence_bound_from_a_null_defaulted_formal_drops_the_placeholder`).
- **A vector passes an inferred `scalar numeric` bound** (`a_vector_index_passes_to_a_parameter_the_body_reads_as_a_sequence_bound`), like any inferred scalar, except a literal `c()` of several elements (`vector_endpoint_through_a_polymorphic_call_is_rejected`).
- **A `[` write into `NULL` starts a vector of the written type** (`an_element_write_into_null_starts_a_vector`), because `NULL` is R's empty vector; kableExtra fills `new_contents <- c()` in a loop. An element `x[i]` of a computed variable passed as an argument drops the placeholder `NULL` (`indexes_computed_value`), but a `:` endpoint keeps it, since `length(range) < 3` admits zero (`a_length_test_that_admits_zero_keeps_null`).
- **`sub` and `gsub` keep two overloads, with the general `replacement` last** (`a_choice_between_sub_and_gsub_takes_a_string_replacement`). A name passed as a value takes the last declaration, so `if (all) gsub else sub` must see the general one; the `character` overload first keeps an open `replacement` binding to `character`. Merging them into one signature lost the per-element overload path of `lapply(words, gsub, ...)`.
- **Code reading a variable that holds its own name is captured code** at an unknown library callee (`a_library_call_quotes_code_that_reads_a_name_placeholder`): ggpubr's `.x <- ".x"` before `scales::math_format(2^.x)`. The slot fact is `name_placeholder_slots`, set by the latest write.
- **tidyselect's `-"name"` is a column exclusion in a `@masked` verb's data-mask arguments** (`naming::NamingExtras::tidyselect_arguments`; `a_masked_verb_reads_a_string_exclusion_as_tidyselect`). Base masking functions (`with`, `subset`, `transform`) evaluate it, so it is reported there.
- **`length(x) == 1` narrows each `T[]` member of `x`'s union to `T`** and drops `NULL` on the edge where it holds (`a_length_of_one_test_reads_a_vector_as_its_scalar`). A union with no vector member takes the plain non-`NULL` edge, which keeps facts such as two slots being `NULL` together.
- **Every guard of the selecting flag returns the same branch** (`a_second_guard_of_the_same_flag_selects_the_same_returns`): `flag_return` collects each `if (flag) return(...)` statement with the first guard's flag and negation. A guard of another flag, or of the same flag negated, leaves an early return outside the selection, so the function returns the union.
- **A local read whose unassigned path reaches a function is reported by the opt-in `maybe-undefined` rule, and typed as the local alone** (`a_read_that_skips_the_write_may_reach_a_function`; `ItemCheck::function_fallback_reads`). Joining the function into the type would make `if (trace) t2 <- ...; if (trace) use(t2)` a type error by default, so the finding stays with the rule that already treats correlated tests as independent.
- **A declared `T | NULL` parameter keeps its `NULL`; only an inferred `NULL` default is a placeholder** (the round-trip test lists `a_null_default_is_a_placeholder_at_a_numeric_parameter` as unwritable). A declaration states that `NULL` is a value the function handles, so passing it on unchecked is an error; the placeholder reading is a guess about undeclared intent. Marking it in rendered signatures would need syntax nothing else earns, so the round trip stays inexact there, as it does for flag-selected returns.
- **A value written on a formal's `missing()` path is its default** (`a_value_written_on_the_missing_path_is_a_default`; `missing_path_defaults`, `missing_path_values`). The branch join keeps the formal's open type, so a later `m * tol` bounds callers, and the written type becomes `Parameter::default`, which a call that omits the argument unifies with. A forwarder `pseudoinverse(m, tol) fast_svd(m, tol)` whose `tol` stays optional takes the callee's default type the same way, since R's `missing()` in the callee sees the forwarded missing argument. Joining to `Unknown` instead let any argument through. Functions written there keep their generalized scheme and record no default type.
- **A forwarded missing formal takes the callee's default type, declared or written** (`a_value_written_on_the_missing_path_is_a_default`). Only a value written on the callee's `missing()` path is what R reaches; with a declared default R stops at the callee's read, so that type describes only a failing call. Telling the two apart would need a flag on `Parameter` that nothing else reads. A default recorded from a call is kept only free of inference variables, because a call matched inside a probe is rolled back and `missing_path_values` is not.
- **`inherits(x, "error")` drops a `condition` member on its false edge although a `warning` handler also returns one**: every condition has the one type `condition`, and the `error = function(e) e` idiom is far more common than returning a warning. Class-level condition types would remove the approximation.

## The fresh sample's false-report classes, fourth pass

**Status:** built (`crates/semantics/src/check.rs`, `crates/ide/src/ide.rs`; the fixtures named per bullet).

- **A missing formal passed on to a parameter the callee may leave unread is no read** (`a_missing_formal_passed_to_a_parameter_that_tests_missing_is_not_read`; `Checker::unforced_missing_reads`). ggplot2's `if (missing(x)) stop_input_type(x, ...)` hands `x` to a function that tests `missing(x)`, and R forces nothing. Only a callee whose body the checker inferred qualifies, through the same `leaves_unread` test as optional arguments, so a callee that reads the parameter still reports, and so does any library function (`a_missing_formal_passed_to_a_library_function_is_read`), because a stub's optional parameter says nothing about whether it is read.
- **A formal with no declared default is optional one forward further up when its callee's matching parameter has a default** (`forwarding_a_forwarded_missing_path_default_leaves_it_to_the_callee`), because R's `missing()` in the callee sees the forwarded missing argument.
- **rlang's `expr`, `exprs`, `quo` and `quos` quote their arguments** (`rlang_exprs_quotes_its_arguments`) when rlang exports the name and no project binding resolves it, as `quote()` does.
- **Arithmetic on a plain environment may dispatch when the project registers `+` for an untracked class** (`arithmetic_registered_through_s7_may_dispatch_on_a_returned_environment`): ggplot2's plot is an environment its S7 `+` method handles.
- **An open formal the body also calls, passed to a declared function with `...`, is variadic even where that call forwards nothing** (`a_callback_parameter_is_variadic_even_where_the_library_call_forwards_nothing`). raster's `apply(x, 1, fun)` came before `fun(x, na.rm = na.rm)`, and the first call fixed the arity. A formal the body only passes on keeps the exact shape (`each <- function(f) lapply(list(1, 2), f)` is `fn(f: fn(double) -> T)`), since nothing calls it with more. The callee must be declared (a stub or an annotated function), since an inferred function's `...` may go elsewhere; a declared project function whose `...` does go elsewhere widens the callback too.
- **Completion offers every name R creates in the global environment as a side effect** (`a_top_level_loop_variable_completes_after_the_loop`, `a_global_written_by_superassignment_completes`): `global_names` reads `super_globals`, the same set unresolved-name checking reads, so a top-level loop's variable and a `<<-` target with no enclosing binding complete like any global.
- **Each name of a zeallot pattern is a declaration for references** (`a_zeallot_pattern_name_is_a_declaration`; `slot_occurrences` reads `destructure_leaves`), as the left side of `<-` is.

## `ifelse` types as the union of its branches, and an all-`NA` logical arrives as `character`

**Status:** built (`Checker::ifelse_type` in `crates/semantics/src/check.rs`, `converted` in `crates/interp/src/annotations.rs`; `stdlib-base.R.test` group `ifelse`, `test_records.rs`).

- R builds the result from `test` (an empty test gives `logical(0)`) and drops attributes, so a stub `Any` let `-> character[]` over `ifelse(x > 0, "a", "b")` check clean and fail at run time.
- Typing: the union of the branches' element types, each in the shape of the test: a vector for a vector test, a scalar only when the test is made of literals, and a vector for a test of unknown shape. Any other test that types as a scalar gives `Any`, because a comparison on a matrix and `is.na` of one type as a scalar (`Compare.matrix`) and a variable may hold one, while R keeps the matrix's dimensions. A bare `NA` branch fits the other one. Not a widening to the wider type, because R keeps each element's own branch type only when both agree; a union is what a reader can rely on. A branch that is not a plain atomic vector (a factor, a list, raw) keeps `Any`, because a factor becomes its codes.
- Run time: the checker lets `NA` and an all-`NA` logical stand for any atomic type, so the boundary agrees. `integer`, `double` and `complex` masks admit logical storage; a non-`Array` `character` annotation converts a logical vector of only `NA` (empty included) with `coerce_vector`, keeping attributes, in the same `converted` step as the `I64` conversion. Rejected: admitting the logical unchanged, because a compiled body or a later `-> character` caller would see logical storage under a `character` type. The same rule makes `f(NA)` for `x: character` run, as `ry check` accepts it.
- The compilers' storage predicates (`storage_admits`) refuse logical for `character`, so a compiled call with such a value falls back to the interpreter, which converts.

## A qualified call reads a dormant namespace's declarations

**Status:** built (`dormant_namespace_schemes` in `crates/semantics/src/stubs.rs`, `Checker::qualified_schemes` and `validated_namespace_name` in `crates/semantics/src/check.rs`; `stdlib-base.R.test` group `dormant_namespaces`; `reference/type-system.mdx`, "Conditional stub namespaces").

- R loads a namespace for `::` whether or not the project declares it, so `data.table::fifelse(...)` in a script without a `DESCRIPTION` types from the package's declarations.
- A per-namespace tracked query lowers that namespace's declarations on demand, read only through `pkg::name`. Bare names stay out of scope, so the rule that dormant names never mask a typo holds, and stub assembly parses no extra source for projects that never write `pkg::`.
- Left out, so they stay `Unknown`: a declaration that names a type any dormant namespace defines (`as.data.table -> data.table`, or `PerformanceAnalytics::Return.calculate -> xts` while `xts` is inactive), because a type definition and its supertypes join only with their namespace and a bare nominal would fail every `data.frame` parameter; and one with `@masked`, `@opaque` or `@elementwise`, which the checker reads from the active library by bare name.
- `warning` is declared (the stub grammar accepts the trailing-dot `[call.]`) and returns `character`. `stop` is `Any` because the checker has no `Never`.

## A recursive group widens an export that keeps growing

**Status:** built (`WIDENING_ROUND`, `Widening`, `widen` and `exports_grew` in `crates/semantics/src/semantics.rs`; `strict.R.test` cases `recursion__a_group_whose_results_nest_each_other_widens_the_growing_result` and `recursion__a_finite_result_reached_late_in_a_group_is_not_widened`).

- Functions in one recursion group that return a record holding the group's own results (`list(value = ..., parts = list(a, b))`) nest the record one level deeper per round, about tenfold in size, until `TYPE_SIZE_CEILING` cuts it to `Unknown`, after which the readers shrink and grow again. Such a group never settles: 400 members ran all 16 rounds on types near the ceiling (18.6 s on one core) and ended at `Unknown` anyway.
- From round 2 on (`WIDENING_ROUND`), a member whose exports grew is widened and stays widened for the rest of the solve, so the group cannot oscillate. The first widening sets a function's result to `Unknown` and keeps its parameters; from then on the whole function is measured, so parameters that keep growing widen every export to `Unknown`. The same 400 members check in 0.35 s.
- Growth compares `type_size` of a function's result (or a binding's type) with the previous round's, and counts only when the previous one was larger than a leaf. A finite result can arrive late, after several back edges, and arriving from the seed's `Unknown` (size 1) is not growth; a self-nesting result grows from one built type to a larger one every round. A structural nesting test does not work, because a generic result is instantiated afresh each round.
- Strict mode reports a widened member as defined recursively, the same finding as a self-nesting binding, and an annotation on it fixes the type.
- Rejected: lowering `TYPE_SIZE_CEILING` (it bounds one type, not the rounds, and still oscillates) and recursive types (the checker has no `μ` types; a widened `Unknown` is the honest answer).

## Strict mode reports a frame of unknown columns where a type lists columns

**Status:** built (`Checker::note_unlisted_columns`, `StrictOriginKind::UnlistedColumns` in `crates/semantics/src/check.rs`; `dataframes.R.test` group `strict_columns`; `reference/type-system.mdx`, "Tables in typed code" just above).

- A frame with unknown columns (`Frame(_, None)`) is gradual and compatible with every frame type, so `load <- function(frame: data.frame) -> Rows as.DataFrame(frame)` would return frames of wrong types, missing columns or extra columns as `Rows` without a finding.
- Strict mode records an origin wherever such a frame meets a frame type that lists columns, at a declared return (each early return and tail leaf), an argument (`check_argument`) and an annotated binding. The finding names the variable the frame was converted from, looking through table verbs such as `as.DataFrame(frame)` (but not into a verb's subject whose columns are known: in a join, the unknown columns came from the other side, so the finding underlines the frame and names no variable), and suggests listing its columns. A union expected type (`Rows | NULL`) checks against its frame member, unless another member is itself a frame of unknown columns (`Rows | DataFrame`), which admits the value. Outside strict mode the flow stays gradual, as every bare frame does.
- Not done: a run-time check of a `DataFrame{...}` annotation. Its mask is 0 because the table lives behind an external pointer in the `frames` crate, which `interp` does not link, and an alias such as `Rows` is not checked at run time anyway. Extra columns and column order are width subtyping, which the open-table-type design decides.

## An overload probe rolls back the types it recorded, and an undecided literal takes its default's candidate

**Status:** built (`RecordedTypes`, `Checker::begin_probe`, `end_probe` and `literal_defaults` in `crates/semantics/src/check.rs`; `native-syntax.R.test` case `conversions__an_unsuffixed_list_literal_converts_through_the_overloads`).

- Overload resolution probes each candidate and rolls the inference table back, on the rule that matching is a pure function of the table. Types recorded on expressions during a probe broke that rule: a number literal adapted in one probe kept a type naming a variable the rollback discarded, and the next probe read it (`List.to_r([1, 2])` indexed past the table). While a probe runs, every write of a recorded type or a recorded expectation is logged, and rolling the probe back restores both with the table; the winner is re-probed to commit, so nothing a probe recorded is lost.
- A call whose open argument is an undecided literal picks the candidate the literal's default would pick, so `List.to_r([1, 2])` is `integer[] | double[]` like `counts <- [1, 2]; List.to_r(counts)`. Every other caller variable must stay open in that candidate. The usual rule, the first candidate leaving the caller's variables open, would pick the generic `List<T> -> list[T]`, which a guess about R code needs but a literal does not: its default is known.

## A nominal record updates with `..` and keeps its nominal type

**Status:** built (`Checker::infer_record_update` in `crates/semantics/src/check.rs`; `native-syntax.R.test` group `record_update`; `typed-ry-values.scenario`; `reference/type-system.mdx`, "Record update").

- `(..point, y = 3)` on `point: Point` with `type Point = Record{x: F64, y: F64}` checks against the representation and has type `Point`, the same rule as a field write on a nominal record. A replaced field keeps its declared type, so `(..point, y = "far")` is a `type-mismatch` and a field the representation lacks is one too. Giving the copy a structural type instead would silently drop the nominal type at every update, which makes a nominal record useless in a loop that updates it.
- An opaque type outside its home file is refused like any other look through it. A nominal tuple is not updatable, as no tuple is.
- The runtime needs no change: a nominal value is erased to its representation, so the interpreter's `.ry_update` and the typed core's record update already produce the right value.

## `==` on ry's records, tuples and tags is structural, and a crossing names the R side's conversion

**Status:** built (`Checker::infer_ry_value_equality`, `r_operand` and the `NumberOperands` text in `crates/semantics`; `comparison` in `crates/interp/src/arith.rs`; `ry_box_equal` and the `OBJECT` arm of `compare` in `crates/typed/src/values.rs`; `native-syntax.R.test` group `ry_value_equality`; `typed-ry-values.scenario`; `reference/type-system.mdx`, "Equality of records, tuples and tags").

- `==` and `!=` on two records, tuples or tags (nominal ones included) give `Bool` and compare structurally: one shape and equal fields, recursively. A `List` of them compares item by item. Ordering operators stay undefined on them. A tag such as `kind == :Bond` is the common case, and without equality every such test needed a `match`.
- The two sides must be able to hold one value: either side's type fits the other (a literal adapts first), so `:Bond | :Cash` beside `:Other`, a tag beside `I64`, or two record types with different fields are a `type-mismatch` saying the values are never equal. A check that only compares shapes would accept comparisons that are always `FALSE`.
- A number field is equal as `identical` decides it (`NaN` equals `NaN`, `NA` does not equal `NaN`), unlike `F64 ==`. The interpreter already had `identical` on these values, and the typed core's `object_equal` follows it, so every tier gives one answer, and a value compared with itself is equal whatever the pointer test says.
- An R value never equals a ry value. The report names the conversion for the R side (`r_side` on `NumberOperands`): `Str.from_r()` for `character`, `Bool.from_r()` for `logical`, `Num.from_r()` for a number, `List.from_r()` for a vector. Beside a record, tuple or tag no conversion fits, so the report says an R value never equals one.
- At run time `==` and `!=` accept ry values (`ACCEPTS_NATIVE`). In untyped R code a record beside an R value is `FALSE` rather than a boundary error, since the checker reports that comparison where types are known.

## ry's immutable containers are covariant

**Status:** built.

- `List<T>`, `Dict<K, V>` and `Set<T>` are abstract (`@type` without a body), which the variance rule checks invariantly, and invariance would reject `[:Cash]` at a `List<:Bond | :Cash>` parameter. They are immutable values, so a wider item type can never be written back into a narrower list: `parameter_variances` (`crates/semantics/src/infer.rs`) makes every parameter of an abstract type the standard library declares (`stubs::is_ry_type`) covariant. Other abstract types stay invariant.
- A lone `..` (every tag) normalizes to `OtherTags` rather than a union, so `compatible_probe` accepts a tag, or an open union, against it directly. As a source, a lone `..` is an open union that lists no tag, so it fits only an open target that lists none either; otherwise a function `fn(x: ..) -> :A(I64) | ..` would launder `:A("text")` into the narrower payload.

## A `Record{...}` or `DataFrame{...}` type may end in a row variable

**Status:** built (`frame_with_row` and `native_record_with_row` in `crates/semantics/src/types.rs`; `unify_open` and `open_compatible` in `infer.rs`; `lower_row` in `annotations.rs`; `refine_dataframe_call`, `join_type` and `adapt_open_record` in `check.rs`; `records-and-tags.R.test` group `open_records`, `dataframes.R.test` group `open_tables`; `reference/type-system.mdx`, "Rows").

- A test user's package works on "any table with a `code` column" and returns it with one more column. A closed `DataFrame{...}` cannot type that, and `list{a: x, ..}` forgets the other fields, so `..R` (Roc's extension variable) names them: `<R> fn(d: DataFrame{code: character[], ..R}) -> DataFrame{code: character[], id: integer[], ..R}`.
- The row is an `Option<Ty>` on `TyKind::NativeRecord` and `TyKind::Frame`, only ever a `Var`, a `Rigid` or `None` (closed). A row bound to a record or frame merges into it in the normalizing constructors (listed names win, its own row carries on), so no type ever nests a resolved row. `for_each_child` visits the row, so substitution, instantiation and the id flags need no row-specific code.
- Unification splits each side into the names both list (unified), the names one side lacks (pushed into the other side's row as a record or frame with a fresh shared tail when both sides have extras), and a closed side that would receive names is a mismatch. There is no "lacks" constraint: a listed field shadows a row field of the same name, which matches `with_columns` replacing a column.
- `R` must be a declared binder, so it is `Rigid` in the body and a fresh var at each call. Inside the body an unlisted read, a positional `[[` and a record pattern without `..` are errors, because the row's names are unknown and may come before the listed ones; iterating an open table gives `Unknown` columns.
- A binder used as a row and also as a type of its own, or as the row of both a record and a table, is an annotation error (`Lowering::finish`), because no binding fits every use. A bare binder passed to a generic is not a use of its own; the diagnostics check an alias argument that fills a row against the definition (`row_parameter_shape`), because a wrong-shaped row would otherwise collapse to `Unknown` in `native_record_with_row`. The solver also treats a row bound to the wrong shape as a mismatch.
- Unifying a row with names that the same row must also absorb from the other side is a mismatch, not a fresh tail, or the binding recurses forever. An inferred value whose row is still a variable takes the names an expected open type adds (`open_compatible`), so a parameter two typed calls read different names of gets both.
- Rows are refused on `list{...}` (which has its bare `..`), `Tuple{...}`, tag payloads and the R classes `data.frame{...}`/`data.table{...}`, whose verbs build closed results. Polars verbs that keep every column keep the row, and so do `d[i]` and a `:=` update; `select` and `GroupBy.agg` drop it; a join with an open right side and `concat` with an open table give a plain `DataFrame`.
- A record literal against an open expected type adapts its listed fields' literals and then must still fit, so `(a = 1,)` passed to `Record{a: I64, ..R}` is `Record{a: I64}` while returning `(a = 1L,)` as `Record{a: I64, ..R}` is an error.
- `GroupBy{...}` takes a row too, because `group_by` keeps it and every printed type must parse back.
- Not done: the "no column" message on an open frame lists only the listed columns, without naming the row. Tag unions take no row and a record update adds no field, by decision ("Tag unions take no row, and a record update adds no field").

## Tag unions take no row, and a record update adds no field

**Status:** settled.

- A record update `(..r, b = 1)` changes listed fields only (Roc's rule), so a function cannot return "an open record plus one field". Adding one needs a shadowing rule for a row that already holds `b` and a native copy of fields the compiled code cannot name. A table verb covers the common case (a column added to any table with a `code` column), and no typed port or test user has needed it on records.
- Tag unions are structural and join by union, so `:A` already fits `:A | :B`, and an `:A | :B` value fits a parameter typed `:A | :B | :C`. A row (`[A, ..R]`) would only add pass-through of unknown tags, which Roc uses to accumulate error tags across calls; ry's `Result` errors are closed unions and none of the typed ports needs more.
- Either becomes worth building when a real program needs it, as typed ry adds features only when they earn their place.

## Literal variables stay open through joins, tail reads and `List` positions

**Status:** built (`default_join_literals`, `open_number_literal`, the tail reads in `check_declared_function` and the `List` position in `infer_plain_index`, all in `check.rs`; `native-syntax.R.test` group `conversions`).

- A join of two paths ties their open literals when R gives them the same type, so `x <- 0` and a branch's `x <- x + 1.5` stay one literal that a later use decides. Literals of different R types (`1L` and `2.5`) take R's types at the join, because plain R code keeps them apart as `integer | double`. Beside a ry number on the other path, the literal is that number.
- A number literal beside an open literal variable of the same R type is open too, so `x <- 0; x + 1.5` is undecided, as arithmetic on two literals is. A literal of another R type (`i <- 1L; i + 1.5`) stays closed, or the joined constraint would turn `i` into a `double`. A quotient of open `1L`-style integers leaves them integers (`n <- 10L; n / 2L`), because R's `/` gives `double` whatever its operands; other quotients tie their operands to the fraction, so `/` still asks for `F64`.
- A variable that a function declared with a ry number result ends with is read as that number, as a tail literal is. Only declared ry number results do this, because against an R type the default already is that type.
- A `[[` position into a ry `List` decides the literal as `I64`, so `lookup[[j]]` before `lookup[[i]] <- j` keeps `j` an `I64`.
- A nominal constructor's representation is an expected type for its literals (`check_new_nominal` adapts them), so `@Tree(:Leaf(1))` against `:Leaf(I64)` holds.
- A recursive `alias` is refused, with a short message that names `type` as the recursive form: an alias is its expansion, so a recursive one has no finite type, while a nominal `type` gives the recursion a name to stop at.
- An `if` with `else` or a `match` whose branches are all number literals of one R type is one open literal, bound to a variable or beside one (`literal_leaves`), so `m <- if (c) 1 else 2; m + n` with `n: I64` makes `m` an `I64`. A nominal constructor in a branch (`@UserId(5L)`) stops it, because the constructor gives its own type.

## A record pattern's `..` takes no name

**Status:** built (`Parser::pattern_list`; golden case `records_and_tags__named_rest_in_record_pattern`).

- `(x = a, ..others)` is a syntax error. A list pattern's `..rest` binds a list of the remaining items, but a record's remaining fields would need a record type made by removing fields from the scrutinee's, which no other construct needs, and an empty rest has no value because a record has at least one field. Roc's and Elm's record patterns take no rest name either.
- Reading the other fields from the matched value (`p as whole`, then `whole$y`) or building a changed copy with `(..p, x = 1)` covers what a named rest would.

## ry values join covariantly, and a walk's tag state widens

**Status:** built (`join_ry_values` and `widen_accumulators` in `crates/semantics/src/check.rs`; `with_tags`, `fits_widening` and `joined` in `crates/typed/src/lower.rs`; `native-syntax.R.test` case `tags_widen_through_walk_states_loops_and_branches`; `crates/ry/tests/native/clean-widening-tags.R`).

- A join that fails to unify does not take the union of the two values: the union would make `if (c) [:A] else [:B]` a `List<:A> | List<:B>`, and a loop's `acc <- List.append(acc, :B)` over `acc <- [:A]` would fail at the next `List.append`. ry's values are immutable, so `List`, `Dict`, `Set`, native records, native tuples and tag payloads join part by part (`join_ry_values`, before `wider_record` and the union). A part that neither unifies nor joins (numbers never widen) falls back to the union of the whole values.
- A `List.walk` step function may return more tags than its initial state. The accumulator formal takes the settled initial state as its hint, and after inferring the step function `widen_accumulators` joins its result into the state, rolls back that pass (environment, unification table, findings, pending enclosing writes, as the capture repass does) and infers the step again under the join, at most `ACCUMULATOR_PASS_CAP` times. The join must be closed, because the rollback drops the pass's bindings. The state argument's type becomes the join, an upcast. A callback that returns the state as every tag's payload (`List.walk_until`'s `:Continue(S) | :Break(S)`) widens by the join of the payloads.
- Only a single candidate callee widens, because the hints of several candidates must agree. With several candidates, or a callback that returns the state any other way, a tag-bearing state gives the formal no hint, so the formal stays open. The hint side asks `accumulator_states` itself, so a kept hint always widens.
- The typed core derives types from values, so it widens by relowering, as for a variable: `List.append`, `List.prepend` and `List.set` of a tag outside the list's item union return the wider list (its items are boxes either way), the branches of an `if` and a function's returns join tag lists (`joined`), an assignment of a list of more tags widens the variable, and a walk's state widens keyed by its step function's `Sexp`. `Dict.insert` does not widen, because the interpreter checks the value against the dictionary's value type.

## Unannotated arithmetic infers a constraint per number family and a family-dependent result

**Status:** built (`Constraint::{Num, ArithmeticOrNumber, ArithmeticOrF64, NumericOrF64}` and `TyKind::Family` with `types::family_result` in `types.rs`; family reduction and `constraint_rejects` in `infer.rs`; the operand rules in `Checker::infer_binary_numeric_operands`, `r_side_of_family` and `settle_unchosen_numbers` in `check.rs`; `native-syntax.R.test` group `unannotated_arithmetic`).

- Previous shape: only arithmetic whose result is the operands' type in both families (`a + b` on one type) asked `num`; everything else asked an R-only constraint and typed its result as R's `double`, so `scale <- function(x) x * 2` rejected an `I64` though both tiers run it, and `a > 0L` claimed `logical` for a `Bool`.
- A joined constraint pairs an R half and a ry half (`Constraint::families`): `num` (numeric, number), `arithmetic or number`, `arithmetic or F64` (a fractional literal or `/`, which ry defines on `F64` alone) and `numeric or F64` (two operands under `/`). Each admits exactly what the operator accepts in each family, as Roc's `Num a` and `Frac a` do.
- A result that differs by family is `Promoted<T>` (the operand type for a ry number, `double` for an R value) or `Compared<T>` (`Bool` or `logical`). It is a type, not an overload pair, so one signature serves both families and reduces as soon as `T` resolves. A whole-number literal keeps `T` under integer-preserving arithmetic (`x + 1L`).
- Where only R values fit (an overload set's argument, a union operand, a condition, an operand beside another open value) a family narrows to its R outcome, because those rules type R's semantics.
- A family operand variable that reaches no parameter defaults to `double` at generalization, so no unsolvable family leaks into an export.
- A function return `Promoted<T>` (alone or in a union with `T`) checked against `T` itself (a fold callback against `fn(U, T) -> U`) settles `T` to a fixed point of the family (`InferenceTable::family_fixed_point`): R's outcome when the constraint admits it, else the ry half (`number`, or `Bool` for `Compared<T>`). A settled `logical` takes `Widening::Joined`, so a later init of another R type joins it as a union instead of rebinding it to a number, which would break `Compared<T> = T`. Only function returns settle this way, because a branch join (`if (flag) x <- x + 1`) must keep `T | Promoted<T>` polymorphic. Both halves admitted picks R's `double`, the R result of the untyped callbacks this comes from; keeping both would need a constraint of exactly `double` or a ry number, which no `Constraint` spells.
- `num` meets a ry constraint as that constraint, a `List` item as `number`, and anything R as `numeric`; a number literal stays the literal. A condition narrows an open `num` value to `numeric` (`Checker::narrow_condition_number`). `has_r_bound` counts every joined constraint, so such a signature is a boundary function.

## A comparison of an open operand asks what both families compare

**Status:** built (`Constraint::{Comparable, Ordered, ComparableOrNumber, ComparableOrOrdered}` in `types.rs`; admission in `infer.rs` `constraint_rejects`; `Checker::infer_compare` in `check.rs`; `native-syntax.R.test` case `a_comparison_asks_what_both_families_compare`).

- Previous shape: `Compared<T>` asked nothing of `T`, so `pos <- function(x) x > 0` accepted a `Str` or a record and typed the call `Bool`, while `s > 0` written directly is a `type-mismatch`.
- The constraint follows the other side, because each family compares a different set: beside a number literal `comparable or number` (R compares any atomic value with a number, ry only a number); a value with itself under `==`/`!=` `eq`; under an ordering operator `comparable or ordered` (`I64`, `F64`, `Str` on the ry side).
- `comparable` is the R half: an atomic type other than `complex` or `raw` in any shape, a list, or a class (R's `Ops` methods). A ry nominal unfolds to its body for the two comparison families, so `type Count = I64` compares with a number.
- `Compared<T>` over a ry record, tuple, tag or nominal is `Bool`, and over a ry `List` `List<Bool>`, since a `List` compares item by item (`InferenceTable::family`); a `List` meets the comparison constraints through its item type.
- `key` admits ry values, so it meets a comparison family as an R atomic scalar (`AtomicElement`), as it meets an arithmetic one; otherwise a key compared with a number would let a `Str` through.
- Two different open operands record a compared pair (the entry above); one beside a string literal asks `comparable or Str`.

## Record and tag payload fields display in sorted order

**Status:** settled (type rendering in `types.rs`).

- A record type and a tag payload show their fields sorted by name, in diagnostics as in hovers, so `:Rect(width: double, height: double)` displays as `:Rect(height: double, width: double)`.
- The sorted order is the semantics: a positional index counts the sorted fields (`type-system.mdx`), and a structural type has no declaration order to recover, since two spellings of one record are the same interned type. Roc sorts record fields the same way.

## A fold's accumulator rebinds to the type its init gives under arithmetic

**Status:** built (`Widening::Folded` in `infer.rs`; `inference.R.test` case `a_fold_accumulator_takes_the_type_its_init_gives_under_arithmetic`).

- Previous shape: a callback doing arithmetic on its accumulator (`function(acc, x) acc * 2`) settled `T = Promoted<T>` as `double` with a fixed binding, so `Reduce(f, list(1:3, 4:6), c(1L, 2L, 3L))` and `Reduce(function(acc, x) acc - 0.5, 1:3, Sys.Date())` were false errors blamed on the init.
- The binding keeps the variable's constraint from before the fixed point, and a later init it admits rebinds it to what R's fold returns: a number joins `double` by `numeric_join` (`integer[]` gives `double[]`, a scalar `integer` stays `double`), and a class with arithmetic (`Date`, `matrix`) or a ry number keeps its own type.
- The constraint is the guard, so `acc * 1.5` with an `I64` init is still refused, and a `character` init still fails.
