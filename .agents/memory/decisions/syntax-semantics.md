# Decisions: Syntax and semantics

The parser and syntax tree, the salsa analysis core, name resolution, and the type checker.

## The standard library ships as declaration-only stub files

**Status:** built.

Standard-library type information ships as dedicated declaration-only files with
the extension `.Rtypes`, rather than as ordinary R files.

The retired approach shipped stubs as R files carrying a placeholder body, such
as `length <- function(x) 0L`, and harvested the `#:` annotation while ignoring
the body. That let a stub carry a meaningless, unreachable function body, and it
required a full parse and lowering just to reach the annotation. In a
declaration-only file a body is unrepresentable, so a stub cannot drift into
carrying one, and the loader parses only declarations.

The extension avoids colliding with R's own family, which is `.Rd` for
documentation, plus `.Rmd`, `.Rda`, and `.RData`. `.Rstub` was considered and
rejected.

An ad-hoc overload uses an ordered overload set. A genuinely parametric
higher-order function, such as `lapply`, `Map`, `Reduce`, or `identity`, gets a
real generic type through the existing `<T> fn(...)` polymorphism, and is never
widened to `Any`.

Highlighting a stub file goes through LSP semantic tokens. One server
implementation then colours both an inline `#:` annotation and a stub file, in
every LSP client, reusing spans the server already computes.

`docs/src/content/docs/development/authoring-stubs.md` is the contract for the
format, the overload rules, and the export manifests.

## The beta semantics

This record holds the core type-system decisions. `backlog.md` holds the prioritized work that
follows from them. The semantics contract is `reference/type-system.mdx`, and it is updated before
the behavior it describes ships.

## Beta semantics: The R variable model is mutable slots with union joins

**Status:** built.

R assignment mutates the current function-scope environment. A branch or loop assignment is
therefore visible after the construct. A model that creates a fresh binding per `<-`, which is
let-shadowing, does not describe R, so it is not used. The model is this.

- A function scope holds mutable variable slots. Each `<-`, `=` and `->` writes the slot. Each
  `<<-` and `->>` walks the lexical chain to the nearest enclosing slot.
- A read sees the union of the reaching definitions at that point. A conditional write joins with
  the prior type. A loop-body write joins across iterations.
- Unused detection falls out of the model. A write that no read reaches is unused, so the report
  names an assignment rather than a binding.

Flow-sensitive narrowing is a later layer on the same model. It is not part of this decision.

The let-shadowing model was unsound. `x <- 1L; if (f) x <- "two"; x + 1L` typechecked clean and
crashes at run time. It also reported the conditional update and the loop accumulator as unused,
and both are idiomatic R. No fix inside the lint alone was possible.

## Beta semantics: A multi-member union is for joins and annotations, never for a unification variable

**Status:** built.

A general union `A | B | C` comes from a join or from an annotation. It normalizes flat,
deduplicated and order-insensitive, and `T | NULL` is a special case of it.

A union imposes no union constraint on an inference variable. That is what keeps inference fast.
Binding a variable to a union value by plain substitution is allowed, like any other type.
Unification stays syntactic, so a union unifies only with a structurally equal union. All
member-wise directional logic lives in `compatible`. Inference therefore stays decidable
and fast, and the split follows the existing rule that unification is the invariant floor.

## Beta semantics: A type error inside a test assertion stays reported

**Status:** built, except the "testing that something is rejected" section, which the diagnostics page does not have.

`expect_error(f("bad type"))` is how an author tests that a function refuses bad input. The call
inside it really is type-incorrect, so the finding is true. The open question was whether an
expectation that asserts a condition should suppress type findings in its payload.

It should not. Suppressing everything inside `expect_error` and its siblings would also silence a
genuine mistake in the test, such as a misspelled callee or an argument passed by accident. Test
code deserves the same checking as everything else. It would also be a special case for one
function family with an open-ended tail: `expect_warning`, `expect_condition`,
`expect_snapshot(error = TRUE)`, `tryCatch` and `try`.

The answer is the general mechanism that already exists, which is `# ry: allow(type-mismatch)` on
the assertion. It is explicit, it is local, and it says what it means. The diagnostics page
documents it under "testing that something is rejected".

One stronger form is worth revisiting. TypeScript's `@ts-expect-error` reports when the expected
finding does not appear, which turns "ignore this" into "assert a finding here". That is a feature
rather than a special case, and it would apply to every code.

## Beta semantics: The type system is Hindley-Milner, and stays fast to check

**Status:** built.

This is a user directive. Admit only a feature that is fast to check, which means Hindley-Milner.
Ask one question of any proposed addition. Does it keep inference to unification over the existing
constraint mechanism, decidable and close to linear, with no search and no global solving? If it
does not, it does not go in, however useful it looks. Expressiveness never buys soundness or speed.

Three things are ruled out.

- Type classes and traits are declined rather than deferred.
  `development/design/open-questions.md` records the reasoning.
- General subtyping is out, because subtype inference is a different and slower algorithm than
  unification. A declared coercion at a named boundary is Hindley-Milner, and that is the shape any
  variance work must take.
- Any construct that requires backtracking search over a program-wide constraint set is out.

A declaration file is the one sanctioned exception. A `.Rtypes` stub may do something a user's own
annotated code may not, and today that is ad-hoc overloading. The exception is bounded on purpose.
The cost of a non-principal feature is proportional to how much code it applies to, and the stub
surface is a fixed corpus the project maintains. The need there is real, and it comes from R's coercion
table rather than from a gap in the declaration language. `abs(TRUE)` is an `integer`, so a
type-preserving `<T: numeric> fn(x: T) -> T` would be wrong for a `logical` argument, and `min`
accepts `character`, which no numeric binder covers. Never say these functions have no principal
scheme. A gradual checker that cannot describe the standard library is not usable. A user's `#:` annotation stays pure
Hindley-Milner, which is what keeps the user-facing promise honest.

## Beta semantics: ry has no macros

**Status:** built.

This is a user directive: compile time and type-checking speed rank highest, so ry adds no macro
system, syntactic or procedural.

- **Why.** A macro makes what a file means depend on code that runs before checking. Every check
  then pays for expansion, an item's names can no longer be read from its syntax, and an edit to a
  macro invalidates every site that expands it, which defeats the per-item firewalls. Vx, which has
  `macro_rules`, confines expansion to its own phase before name resolution so that it cannot break
  parallel per-body checking, and its notes cite rustc, where macro expansion and name resolution
  are still serial (`docs/language_rules_for_a_parallel_compiler.md` in vx-lang/Vx).
- **R's own non-standard evaluation stays.** `quote`, `substitute`, `bquote`, `eval`, tidy
  evaluation and data masks keep working, because existing R code must run unchanged. The checker
  treats them as ordinary R calls (the recognized data masks read bare names as columns, and `eval()`
  returns `Any`), and ry builds nothing on top of them.
- **Prior art.** Roc's language reference describes no macros; it evaluates top-level constants at
  compile time instead. Bend lists "no macros beyond compile-time templates" among its limitations.
  goose's reference describes no macros; its `const for` copies a fragment once per field, variant
  or type at specialization time, as its form of reflection. Vx has `macro_rules`, restricted as
  above.
- **What replaces common macro uses.**
  - Derived behavior, such as Rust's `derive` for printing, equality or serialization, comes from
    structural types: with no declaration, `print((x = 1, y = 2))` prints `(x = 1, y = 2)`, and
    `identical()` compares two records by value.
  - Boilerplate repeated over types is written once with generics (`<T> fn(...)`) and the standard
    library's higher-order functions.
  - Code generation happens outside the language, in a script that writes ordinary files the checker
    reads, as `scripts/export-manifests.R` writes the export manifests; `.Rtypes` stubs declare what
    R code cannot express.
  - Values computed ahead of time are pure top-level constants, which strict mode's planned
    definitions-only top level makes possible (`backlog.md`, "strict mode restricts what a file's
    top level runs").
- **Consequence.** Reject any proposal that expands or generates code before checking, including
  generation driven by annotations inside the checker.

## Beta semantics: An overload set is a bounded, ordered probe

**Status:** built.

A function whose result type depends on the argument type gets an ordered overload set, on the stub
surface first. Repeating a name within one `.Rtypes` source appends a candidate. A later source
replaces a name's whole set. A call site tries the schemes in declaration order using the existing
probe-then-rollback machinery, and the first compatible match wins. Principal-type purity is
relaxed at overload sites only, because declaration order carries meaning there. This is the
TypeScript and mypy model.

The scope is enforced rather than conventional. Only a plain or namespace-qualified name whose
declarations come from a `.Rtypes` source can be overloaded. `GlobalEnv::overloads` reads the stub
library and returns `None` as soon as a script or package binding shadows the name. A project's own
R code cannot declare an overload set, and should not gain the ability, for the Hindley-Milner
reason above. A project override stub can, because a `.Rtypes` file is a declaration file for
foreign code either way.

Overloads are the escape hatch, not the mechanism, and the corpus is the pressure gauge. Of the 35
sets the corpus declares:

- About a dozen are atomic-family promotion: `abs`, `min`, `sum` and the `cum*` family. Each is a
  numeric constraint in disguise.
- About a dozen are shape dispatch: `head`, `rev`, `Filter` and `lapply`. Each stands in for a
  shape-mirroring return. This also explains why `abs` needs one candidate per shape and per
  family.
- Nine are S3 operator method tables, such as `+.Date` and `Arith.difftime`. Those are dispatch.
  They need a multi-entry table under any design and would survive untouched.
- Two are genuinely two-form functions.

Two thirds are therefore workarounds for two absent features. Both features are
Hindley-Milner-compatible and both are in `backlog.md`. Watch that ratio. A rising count of
promotion sets or shape sets is the signal to build those two features, never to design traits.

Three call-site rules keep selection sound. `try_overloaded_call` implements them.

- **Arguments are inferred once, before any probe.** Expression inference writes environment and
  recorded-type state that the probe snapshot does not reverse. A probe therefore runs
  instantiation and argument matching only. The signature-matching half of function-call inference
  is split out as `match_arguments` for this reason.
- **A fit is either a fact or a guess.** The caller's open inference variables are recorded before
  probing, by `collect_unbound_vars`. A candidate that fits while leaving every one of them
  untouched was chosen by the concrete arguments, so it is a fact. Untouched means the same
  representative and the same entry, so binding, redirecting and tightening a constraint all count
  as touching. A fact beats a candidate that fits only by narrowing those variables, which would
  over-commit a wrapper such as `function(x) sum(x)`. Among fits of the same kind, declaration
  order decides and the first wins. That keeps one reading of order everywhere: first match at a
  call site, most specific first in the corpus, and the last declaration for a value use of the
  name. A lone fit is never a guess, because it is the only signature that accepts the call.
  Probing rolls back, so the winner is re-probed to commit. Matching is a pure function of the
  table, and the table is back in its pre-probe state, so the fit repeats.
- **Selection runs a strict round, then a courtesy round.** The courtesy that reads a whole-number
  literal as an integer is off in the first round, so `sum(1, 2)` picks the double candidate, which
  is what R computes. It is on in a second round that runs only when nothing matched strictly, so a
  name whose only fitting candidate wants `integer` still accepts `foo(1)`. An exact match outranks
  a conversion.

A last-fitting-wins tiebreak was tried and removed. Its purpose was to stop `function(x) sum(x)`
from committing to a narrow candidate, and the fact rule already does that. A general fallback
taking `Any` accepts without binding, so it is a fact and outranks every guess above it. Last-wins
also forced any set whose candidates differ only in a sequence shape to be declared
most-general-first. `lapply` is such a set, where a named list in gives a named list out, and a
lambda callback makes every candidate a guess. That order contradicts both the corpus convention
and the value-use rule, which resolves a non-call use of the name to the last declaration and would
then hand out the narrower contract.

When every candidate fails, name the set only when the candidates disagree about what is wrong.
`NoMatchingOverload` says that no overload of `f` matches, gives the number of declared signatures
tried, and adds the first candidate's failure as a hint. That is right when the call could have
meant several shapes and each one rejects it elsewhere or for a different reason. `pick("word")`
against `fn(integer)` and `fn(double)` is refused at the same argument by both, and neither reason
is the answer.

Two failure shapes have a single answer, and the wrapper buries it along with the argument's own
range, because the wrapper blames the whole call. The first shape is every candidate failing for
the identical reason. The second is one candidate getting strictly further into the call than any
other, measured as the index of the first argument each one blames, where a whole-call verdict such
as an arity error or an unknown name counts as no progress. The deeper candidate is the signature
the caller meant. A two-parameter callback handed to `lapply` is refused at the callback by the
candidate that accepted the sequence, and at the sequence by the candidate that wanted a named
list, so the callback is the finding.

## Beta semantics: The boundary for R's object systems is dispatch, not Hindley-Milner

**Status:** built, except point 2: a `setClass` or `R6Class` declaration produces no nominal, so `new(...)` and `$new(...)` are `Unknown`.

A recurring claim is that S3, S4 and R6 support is fundamentally at odds with a Hindley-Milner
core, and that supporting it therefore costs soundness or speed. That is the wrong diagnosis, and
it is recorded here so nobody re-derives it.

The declarative core of all three systems maps onto machinery the checker already has and relies
on: a nominal type with a checked representation, written `@type` plus `@new`, record field
projection on a nominal, and declaration-ordered overload sets. The existence proof is that S3
dispatch already runs inside the inference core. An operator on a nominal is dispatched statically
and soundly today, through `+.Class`, `Arith.Class` or `Ops.Class`. S4 inverts the claim hardest.
`setClass` is a record declaration with slot types written literally in source, `new(...)` is a
named-argument constructor call, `x@slot` is a field projection, and `setMethod(signature = ...)`
is an overload candidate. S4 is therefore more statically declared than the `#:` annotations the
checker already consumes. Hand-writing the equivalent, which is `@type` plus a wrapper constructor
carrying `@new`, already checks slot types and constructor arity with no strain on inference. The
gap is a lowering pass, not type theory.

Three things genuinely do keep dispatch out, and they are the real reasons.

- **Dispatch needs a class known at the call site.** Inside an unannotated `function(x) speak(x)`
  the argument is an open inference variable, so there is nothing to dispatch on and the only sound
  answer is `Unknown`. R code is most dynamic exactly where dispatch matters most, so generic
  dispatch structurally underdelivers where it would be used. This is the same shape as overload
  selection with a flexible argument: never guess, fall back.
- **Inheritance is subtyping, and there is none.** `TyKind::Named` matches by exact name, type
  arguments aside. No hierarchy exists anywhere in the compatibility relation. S4's `contains=` and
  R6's `inherit=` require one. Nominal subtyping is a new axis in `compatible` and in the union and
  join rules, and it is the one place where a soundness regression is a real risk.
- **A generic's method set is global mutable state, which fights the interface firewall.** Any file
  may add `print.foo`. If the method set of a generic is an input to every call of that generic,
  one new method invalidates every call site in the workspace. Incremental support needs a
  separately memoized method-set query per generic, so that adding a method invalidates only calls
  to that generic and editing a method body invalidates nothing. Getting this wrong turns a single
  edit into a full revalidation at 300,000 lines of code.

One decision therefore splits into three, decided separately.

1. **A false positive is a defect, not a deferred feature.** `setGeneric("f", ...)` not defining
   `f`, so that every call to a project's own S4 generic reports `unresolved`, is a bug. So is the
   absent R6 stub, which makes `R6::R6Class` report an unknown namespace. Neither costs anything in
   soundness or speed, and both are fixed independently of any object-system ambition.
2. **A declaration may become a nominal, and this is the cheap win.** Recognizing `setClass` and
   `R6Class(public = list(...))` as class declarations that produce a nominal with typed slots or
   fields catches the mistakes users actually make, which are slot and field typos and constructor
   arity. It needs no subtyping, no new global state, and it firewalls per file like any other
   item.
3. **Dispatch and inheritance stay unmodelled**, on the three grounds above. An unmodelled
   construct stays `Unknown`, which strict mode reports as a coverage limit rather than a guess.
   Revisit only with a memoized per-generic method-set query and a decided nominal-subtyping
   design.

## Beta semantics: `T[]` carries an atomic-element constraint, not a trait

**Status:** built.

The core vector generalizes to carry an element type, so it can hold a variable. That needs a new
atomic-element constraint kind on an inference variable. It is the same mechanism as the existing
numeric constraint `<T: numeric>`, and it renders as `<T: atomic>`. A trait system is not justified
by this need alone, because the constraint mechanism is already built, proven and fast.

A valid vector element is a `Scalar(_)`, a constrained `Variable(_)`, or `Any` or `Unknown` for an
element the checker does not track. `Constraint` is a lattice merged by `join` rather than by
`Ord`. Numeric meets atomic-element as `ScalarNumeric`, which is a scalar `integer` or `double`,
renders as `<T: scalar numeric>`, and defaults to `double` at a binding boundary like plain
numeric. Annotation lowering records the atomic-element bound directly on the element variable's
entry instead of going through `constrain`, because the element may be a rigid `<T>` binder.
The annotation itself makes the promise there, while a function body must not add a bound the
annotation never declared.

## Beta semantics: Coercion policy at parameter positions

**Status:** built.

Three verified false-positive factories are fixed at the compatibility level.

- A whole-number `double` literal is accepted at an `integer` parameter. This generalizes the rule
  `:` already had.
- `integer` widens to `double` at a parameter position.
- A vectorized stdlib stub declares `T[]` parameters, so a scalar coerces up. A scalar parameter
  would otherwise reject a vector.

The old policy punished precision. The more precise a stub was, the more false positives it
produced. `seq_len(10)`, `toupper(c("a","b"))` and `round(x, 2)` all errored, which pressured the
corpus toward `Any`.

## Beta semantics: Strict mode

**Status:** built.

Under strict mode an `Unknown` origin is an error, an unannotated parameter nothing constrains is an
error (see the inline-annotations record), and an unresolved-name reference is an error rather than
a plain naming warning. A recursion-induced `Unknown` return is an origin. Explicit
`Any` remains the sanctioned escape hatch, so an allowed unknown is expressed by annotating it and
never silently.

A `#: @strict` or `#: @strict off` directive at the top of a file overrides the configured default
in both pipelines. The gates are already per file, and the directive is derived from the parse so
incrementality holds.

Refusing an unsupported construct loudly is acceptable policy. Mistyping one silently is not.

`stop(...)` types as `Any`, not `NULL` or `Unknown`. There is no bottom type, and `Any` is the one
type that fits every branch and return position without strict mode reporting it, so a
`if (bad) stop(...)` guard costs strict users nothing. `&` and `|` are typed elementwise (logical
scalar or vector) rather than opaque, so their operands resolve normally in naming. An operand
whose type is still a variable is not unified with scalar `logical`, as `!`'s operand is, because
`&` is mostly used on vectors and the unification made `h(c(TRUE, FALSE), TRUE)` an error; the
operator is opaque for it instead.

## Beta semantics: Guard narrowing

**Status:** built, except the `&&` limit: the right conjunct of `!is.null(x) && x > 0` sees the refinement.

Flow-sensitive narrowing is branch-edge entry refinement on the slot model, not a separate flow
analysis. `recognize_guard` accepts `is.null(x)`, a member of the `is.*` family, or a negation of
one, and `guard_edges` computes refined types for the guarded slot's two edges. Each
refinement is an ordinary undo-logged environment write inside the branch region. A branch write
replaces it, the region rollback reverts it, and the branch join sees final values, so no new
machinery is needed.

Early-exit persistence falls out of divergence-aware joins. A branch that never falls through
contributes neither a value nor state, so the surviving edge's refinement applies after the `if`,
and only that edge's. A branch never falls through when it ends in `return`, `stop`, `break` or
`next`, or in a block ending in one of them.

The limits below are deliberate, for soundness and for zero false positives.

- **Only union members are filtered.** A family guard does not invent a shape for `Any` or
  `Unknown`, because a refined `character | character[]` would false-positive against a stub
  signature that claims a scalar. The one non-union refinement is the true edge of `is.null` on
  `Any` or `Unknown`, which becomes `NULL`.
- **A statically undecidable member stays on both edges.** That covers an inference variable, a
  flexible-element vector and an opaque nominal. `is.list(data.frame)` is true at run time.
- **Only a local slot narrows.** That is a parameter, a function local or a script local. A package
  global keeps winner semantics, and a guarded expression such as `is.null(x$field)` is not
  tracked.
- **`&&` and `||` are not decomposed**, and narrowing does not apply inside the condition. The
  right conjunct of `!is.null(x) && x > 0` does not yet see the refinement. Both are recorded
  follow-ups.
- The guarded key resolves exactly as a read resolves. That is the local slot under a naming
  context, or the flat global entry in a context-less fixture state, so a fixture and production
  share one path.

## Beta semantics: The expanded parameter directive is `@param name {TYPE}`

**Status:** built.

The expanded parameter directive writes the name first and the braced type second, as
`@param name {TYPE}` or `@param [name] {TYPE}`. JSDoc writes the type first, as
`@param {TYPE} name`, and that order is rejected. There are three reasons.

- The wrapping payload is the type, so putting it last lets a multi-line type continue cleanly
  under the directive instead of leaving the name dangling after the closing brace.
- It gives one shape across all directives: `@type Name {TYPE}`, `@alias Name<T> {TYPE}` and
  `@param name {TYPE}`.
- The grammar becomes unambiguous. The name is a single identifier token right after the directive.
  That retires the ambiguity of swallowing the tail as a name, and it leaves room for an optional
  trailing description later.

The JSDoc order is rejected with an error that names the new form rather than with a generic parse
failure. `@return {TYPE}` and `@forall` are unchanged.

## A syntax error does not erase a file

**Status:** built.

A single error node anywhere in a tree used to short-circuit lowering to an empty module. One
half-typed keystroke therefore dropped the file's whole export set. The package symbol
index re-folded on every keystroke inside a broken window, dependents flooded with unresolved-name
errors, and diagnostics, hover and completion went dark for the rest of the file. The single source
of truth for what exists mid-edit was the parse tree's error bit, and it applied at file
granularity.

The shape now is statement granularity, as rust-analyzer does it.

- A well-formed statement always lowers. A broken file keeps every export whose statement parsed.
- A broken statement contributes nothing: no names, no reads, and no cascading diagnostics. A
  broken region reports its syntax error and nothing else. The contract is the syntax-errors
  section of the typing reference.
- Two salvage shapes apply inside a broken region. At sequence level, an error node's well-formed
  assignment children lower normally. Fragments are filtered by kind, so `ExpressionKind::Assign`
  is kept and everything else is dropped, and a well-formed non-assignment fragment that shares a
  line with a following error sibling is dropped as the split half of the broken statement. At
  expression level, a broken assignment with an intact name side keeps its definition and degrades
  the value to `ExpressionKind::Missing`.
- `Missing` is a distinct HIR kind from `Unsupported`. Both type as `Unknown`. `Missing` records no
  strict origin, because the syntax error already covers the region. `Unsupported` stays
  strict-relevant, because it marks a complete construct the checker cannot model.
- A checked annotation on a `Missing`-valued definition binds its declared type unchecked. A hole
  proves nothing, so demanding proof would be a guaranteed false mismatch. Annotated definitions
  therefore keep their contract for callers mid-edit, which means zero downstream invalidation.

Correctness: half-typed code produces no false unresolved-name or type errors. The
`lowering/error-tolerance` fixtures and the `error_tolerant_lowering` project fixtures pin it.
Simplicity: one policy sentence governs every case. Performance: the malformed-flip engine witness
went from two index refolds plus a referrer recheck to no refolds and a recheck of the edited file
only, because exports stay byte-equal across the flip and early cutoff does the rest. Incremental
analysis: typing inside a construct is the highest-churn edit state, and it now has the smallest
blast radius.

## `is.null` shapes an unconstrained inference variable

**Status:** built, except the finding on unguarded use after the test: `if (is.null(x)) 0L else 1L; x + 1L` is accepted.

At an `if (is.null(x)) fallback else x` join where `x` and `fallback` are both unbound inference
variables, the join used to unify the two variables. The type model deliberately never invents a
union for a variable, which is a long-standing decision that keeps inference fast. The call
`or_else(NULL, "text")` then bound the single variable to `NULL` from the first argument and
rejected the second.
That is a false positive on the coalesce idiom, and it was recorded as a structural design tension
whose workaround was to annotate.

The guard itself carries the missing information, so the fix is to consume it. In `guard_edges`,
when the recognized predicate is `is.null` and the guarded local slot resolves to a completely
unconstrained inference variable `A`, meaning entry `Unbound` and constraint `Unconstrained`, bind
`A := T | NULL` for a fresh `T`. The model already supports that
union shape from annotations, because `@param x {T | NULL}` produces exactly it, and the existing
member filtering then narrows the edges. There is no new type form, no deferred union and no join
special case. The coalesce body joins `fallback` with the narrowed `T` and generalizes to
`<T> fn(value: T | NULL, fallback: T) -> T`, which is byte-identical to the verified-clean
annotated form. A negated guard works through the existing edge swap. A constrained variable is
never reshaped, because a numeric bound contradicts a `NULL` member, and neither is a rigid or
declared parameter, because there the annotation is the contract.

One consequence is deliberate and pinned as a fixture. Testing a parameter for `NULL` and then
using it unguarded afterwards is now a genuine finding, so `if (is.null(x)) 0L else 1L; x + 1L`
errors. The test declared `NULL` a possible inhabitant, and the annotated form of the same code
already behaved this way, so the model is now consistent rather than lenient when unannotated.

Correctness: this removes the last recorded idiom false positive from the sweep, with no
regressions across any fixture suite or the real-world corpus, and no expectation changed anywhere
else. Simplicity: about twenty lines in one function, reusing the union machinery end to end.
Performance: one extra variable and union per shaped guard, which is negligible. Incremental
analysis: unaffected, because this is a per-file inference detail.

## Data-masked resolution for data.table and the with-family

**Status:** built, except the mask-depth counter, which "A masked argument is its own scope, and the base masking family is typed" supersedes.

Every bare name in `DT[region == "west", .(total = sum(amount)), by = product]` used to fail
lexical resolution and produce a could-not-resolve warning, plus type errors from the base
index-arity rules. data.table evaluates `i`, `j` and `by` in the data's own frame. This is the same
false-positive class that R CMD check and lintr hit on code that uses non-standard evaluation. `:=`
lowered to `Unsupported`, which hid its operands from the IDE entirely. A user reported the pain
directly, because data.table code drowned in could-not-resolve warnings.

The shape now recognizes a mask structurally in the naming walk and suppresses the diagnostic at
the emission edge.

- Two recognizers set a mask depth during resolution. The first is a `[` bracket whose arguments
  carry an unambiguous data.table signature: a `by =` or `keyby =` argument name, a `:=` or `.()`
  call, or one of the `.SD`, `.N`, `.I`, `.BY`, `.GRP` and `.EACHI` symbols. None of those occur in
  base indexing, so `m[i, j]` is untouched. The second is the base masking family `with`, `within`,
  `subset` and `transform`, whose callee must not be locally shadowed, and which masks the
  arguments after the data.
- A read that fails lexical resolution inside a mask is recorded in `ItemNaming.quiet_reads` in
  addition to `non_locals`. A masked name still resolves stubs and package globals normally, so a
  `sum` in `j` keeps its scheme. The first design kept masked reads out of `non_locals` and lost
  stub typing, so it was revised. Both could-not-resolve emitters skip masked ids, which are the
  analysis package pass and the engine's per-file query. The typecheck fallthrough for an
  unresolved non-local was already a silent `Unknown` with no strict origin.
- The recognized bracket itself lands in `NamingExtras.masked_subsets` and types as a silent
  `Unknown` before the base index-arity rules run, because `[.data.table` returns shapes those
  rules must not judge.
- `:=` lowers as an ordinary binary call, so its operands stay visible to naming, to hover and to
  the mask.

Some things are deliberately not silenced. Anything that resolves keeps full checking inside a
mask, which covers locals, stubs and globals. Base indexing keeps lexical resolution and its
warnings. Strict mode stays silent on a masked column read, because non-standard evaluation is a
recognized dynamic construct with intended semantics rather than an unmodelled hole. The backlog
records the extension: honor `utils::globalVariables()`, and generalize the mask marker into
`.Rtypes` stub syntax so a dplyr verb can declare a masked parameter.

Correctness: this removes the dominant non-standard-evaluation false-positive class with no
regressions across the suites, and base-R control fixtures pin the non-masked behavior. Simplicity:
one mask-depth integer and two sets on the existing naming result, with suppression at the existing
emission edges. Performance: a shallow per-bracket marker scan, which is negligible. Incremental
analysis: unaffected, because these are per-file naming facts.

## Recursion in a function-valued assignment is monomorphic

**Status:** built, except the limit on mutual recursion between two local closures, which resolves without a finding.

The recursive read inside `fact <- function(k) ... fact(k - 1L) ...` resolved to a slot with no
environment entry yet, so it typed as a silent `Unknown`. The recursion contributed nothing to the
function's scheme, and a bad recursive signature went unnoticed. The naming half, where a closure
right-hand side sees its own target binding, landed separately. This record is the typing half.

The shape is the classic `let rec` rule, scoped to a function-valued assignment. Before the
right-hand side is inferred, the target slot is pre-bound to a fresh inference variable created
inside the binding's generalization level. The body's recursive reads unify against it, and the
variable then unifies with the inferred function type. Recursion is monomorphic, so every recursive
use shares one instantiation. Polymorphic recursion is undecidable in general and is not supported.
The rule applies on the local-assignment path in both context and context-less inference, and a
fixture driver pre-binds the global slot.

One subtlety matters. The top-level package-winner path writes the final generalized scheme to the
global entry, while `exported_value_schemes` reads the per-site local entry first. The pre-bound
placeholder must be overwritten in the local entry too, or a cross-file consumer sees a dangling
variable as `Unknown`. The project-suite fixtures caught this, and the winner path now writes both
entries.

Three scope limits are deliberate and pinned by fixtures. Recursion defined with `<<-` keeps the
silent `Unknown` behavior, because it is rare and the enclosing-slot join semantics make a
placeholder ambiguous. Mutual recursion between two local closures stays a loud unresolved
reference on the earlier-defined one, because letrec visibility is per binding and not per block. A
full block letrec is a recorded possible extension. Top-level mutual recursion already resolves
through the package interface fixed point, whose oscillation guard pins a genuinely cyclic scheme
to `Unknown`.

Strict attribution for top-level recursion whose converged scheme retains `Unknown` is the open
part, and the backlog records it. It needs an origin on the binding.

Correctness: a recursive helper gets a real scheme, and a violation of a recursively inferred
signature is caught, so `countdown("not an integer")` errors where it was previously silent. The
polymorphic-identity and cross-file suites pin that nothing regressed. Simplicity: one pre-bind and
one unify on the existing assignment path. Performance: one extra variable per function-valued
assignment, which is negligible.

## Bare-name resolution is not gated on NAMESPACE imports

**Status:** built.

The fork was this. Should a bare name that resolves only through the stub corpus, such as `median`
against the stats stub, require the package to import it with `importFrom(stats, median)` or
`library(stats)`, or should it resolve unconditionally?

Unconditional resolution against the shipped corpus is correct, and it is not a shortcut. The
shipped namespaces are base, stats, utils, methods, graphics and grDevices. R attaches exactly
those in every default session, so a bare `median` genuinely resolves at run time in the
environments R code actually runs in. A project `.Rtypes` stub is user-authored, and writing one
already declares that the project uses those names. Gating on NAMESPACE entries would make the user
say the same thing twice.

What NAMESPACE gating would really buy is per-file visibility for a non-default package, masking
warnings, and `library()` attach ordering. All three need the full import model, which stays after
the beta. Gating falls out of that model naturally, so bolting it on first would be wasted work.
Until then the NAMESPACE surface stays what it is today: import validation, which detects a typo
against the corpus, and the opt-in `unused-import` lint.

This record changes no behavior. It closes the fork by ratifying the current shape and pointing the
future work at the import model.

## The shipping stack is a hand-written parser, a lossless tree, and salsa

`docs/src/content/docs/development/architecture.md` describes the architecture as it stands. This
record holds the reasoning behind it and the risks that come with it, so that no session
re-derives either.

## Shipping stack: Why the previous stack was replaced rather than tuned

**Status:** built.

Four limits were structural, and each had already been mitigated in place. A mitigation around an
architecture is not an architecture.

- tree-sitter capped syntax-error quality, because an error is an opaque ERROR node. It cost about
  8 microseconds per line, derived from a cold pass at 302k lines of code where about 2.5 s of 3.7 s
  was parsing. Its trees were about sixty times the source size, measured through `analysis-stats`,
  which is what forced the rope-only input and the parse LRU. It also cannot see a `#:` annotation,
  which forced a re-lexing subsystem over a reconstructed buffer.
- The analysis unit was the whole file. That makes whole-file re-inference the keystroke floor, and
  file-granular interface edges manufacture cluster SCCs.
- `CoreType` was a deep-cloned enum. Allocation churn capped inference near a measured 6
  microseconds per line.
- The engine was single-threaded by design.

The limiting factors rank in this order: file-granular analysis, type-representation churn, parse
cost and tree size, and single-threadedness. The last one is a division by the core count, while
the first three are asymptotic or large per-operation wins. The ultimate ceiling is R's dynamic
semantics, which is a semantics budget rather than an infrastructure one.

## Shipping stack: Why a lossless tree, and why ry's own

**Status:** built.

The parser is hand-written, and it emits a lossless concrete syntax tree rather than a typed AST of
structs with spans, for four reasons.

- **Losslessness.** Every byte lives in the tree, trivia included, and reprints exactly. A `#:`
  comment is type syntax here, so the formatter, the byte-exact round trip and the annotation
  tooling all come from the representation instead of from side tables.
- **Error resilience.** Every parse yields a tree whose error nodes are local to the break, and the
  typed AST layer returns `Option`, so a consumer never carries a parallel data model for broken
  code.
- **Position independence.** An item is analyzed as its own subtree, rebased to offset zero, so an
  untouched item stays equal after an edit elsewhere in the file. That is the property per-item
  cutoffs are built on. An AST that carries absolute spans shifts every span after any edit, which
  kills sub-file incrementality at the root.
- **Cheap splicing.** A statement-splice reparse copies the untouched top-level runs of the previous
  tree and parses only the edited middle.

The tree was rowan's green and red design until it was replaced by ry's own flat preorder tree
(`crates/syntax/src/tree.rs`; see "The syntax tree is ry's own flat preorder arena" below for why).
The cost both share is that traversal is dynamically kinded, which is why inference never walks the
tree: the checker runs on per-item HIR, and the tree serves the fidelity layers, which are the IDE,
the formatter and refactorings.

Value equality is the mechanism for cutoffs, not pointer identity. An item's subtree compares
structurally (kinds, subtree ends and item-relative offsets), and rust-analyzer's barriers are likewise value
equality on its `ItemTree` and `AstIdMap`. Parse stays a pure per-file query and correctness never
depends on splicing.

## Shipping stack: Why salsa rather than an in-house engine

**Status:** built.

Two things would otherwise have to be hand-rolled: parallel snapshot reads with write
cancellation, and first-class fixpoint cycles. Both are proven in rust-analyzer and in Astral's
`ty`, whose type inference uses salsa fixpoints. A concurrent red-green memo core is the one
component not worth building in-house. The query decomposition from the in-house engine is the
asset that transferred: per-symbol firewalls, names-only cutoffs, and the durable and open fold
split.

Four salsa risks are recorded so they stay managed.

- **Pin the version and upgrade deliberately.** The public API churns hard and often, with several
  breaking releases a year. Vendoring or forking is the exit hatch.
- **Fixpoint non-convergence is a hard panic at 200 iterations.** The pin-to-`Unknown` rule lives
  inside the cycle function. Reaching salsa's cap is a bug, never a fallback.
- **Interned-value garbage collection is young.** rust-analyzer used about four times the memory on
  its salsa migration until it tuned per-query `lru`, and `ty` hit multi-gigabyte blowups. Per-query
  `lru` and interned GC are the first levers when memory regresses.
- **Parallel iteration over a fixpoint had real hang bugs**, fixed upstream. A parallel-cycle
  stress test covers it.

## Shipping stack: Item granularity and item identity

**Status:** planned. Built so far: top-level items with insertion-stable identity; no nested definition is an item.

The analysis unit is the item, not the top-level statement. A nested definition is an item too.
That covers a field or method inside a class-constructor call such as `R6Class`, `setRefClass` or
an S4 block, and a function defined inside a function body. Without that, R's common giant
single-statement object-oriented files degenerate straight back to whole-file granularity.

An item's identity hashes its kind and name, plus its parent with an index disambiguator. It never
hashes a bare position or index, so inserting an item does not shift an unrelated item's identity.
This follows rust-analyzer's current `AstIdMap` design. Its earlier index-based design had exactly
the shifting problem.

## Shipping stack: An annotation is one concept with pluggable spellings

**Status:** built.

Annotations are one internal concept, and the surface spelling is pluggable: both spellings lower
through `annotations.rs` into one `Annotation`. `#:` is structured trivia, and a `#:` file must
always remain valid ordinary R. The inline spelling is the superset dialect this seam kept open,
chosen by the mandate in `AGENTS.md` (the next two records).

## Shipping stack: The inline annotation syntax is a superset of R

**Status:** built, except `declare(type(...))` as an annotation spelling: the checker does not read it.

The previous position was that a superset dialect "stays a product decision" (the entry above),
with `declare()` as the valid-R route. The mandate in `AGENTS.md` makes that decision: the inline
syntax is on by default and ry warns when it is turned off, which only makes sense for a syntax that
GNU R cannot parse. So the inline form is the superset that `development/design/inline-type-syntax.md`
costed (generic binder, parameter, return and binding positions), typed files keep the `.R`
extension because ry executes them directly, and `declare(type(...))` stays a recognized spelling for
code that must remain valid R. The cost is that other R tools fail on a typed file, so code that must
run on GNU R goes through a strip step that moves inline types into `#:` comments. The reasoning is in
`development/design/r-runtime.md`, section 8.

## Shipping stack: How inline annotations are built

**Status:** built.

**Syntax.** `name: TYPE <- value` (or `= value`) for a binding, `name: TYPE` (and `...: TYPE`) in a
parameter list, `-> TYPE` between the parameter list and the body, and `function<T: numeric>(...)`
(or `\<T>(...)`) for type parameters. Every `TYPE` is the `#:` type grammar, run on R's token stream
by the same parser functions, so there is one type grammar. A function header reads like the compact
annotation it means: `function(x: integer) -> integer` is `fn(x: integer) -> integer` with formals.
Docs: `reference/type-system.mdx#inline-annotation-syntax`.

**Why this shape.** The record above gives the product reasons. The binding form is recognized only where a statement starts
(top level, directly inside `{}`), by a shallow token scan for a `<-` or `=` outside every bracket, where only `(`, `[` and `{` carry
the scan past a line break (`<` and `>` are also comparisons), so `x:y`, `x:y < -1` and every argument stay ranges. Rejected: `declare(type(...))` calls (valid
R, but a runtime call per binding and no natural place for parameter or return types), `let x: T`
(new keyword), `x <- value :: T` (`::` is R's namespace operator), and `function(x): T` (`->`
mirrors the existing `fn(...) -> T`).

**Erasure.** Deleting `TYPE_ASCRIPTION`, `RETURN_TYPE`, and a `FUNCTION_DEF`'s `TYPE_BINDER_LIST`
gives the plain R program (`syntax::erase_inline_annotations`). The tree-sitter acceptance oracle
judges the erased text, and `check_parse_invariants` pins that erasing a clean parse yields a clean,
annotation-free, idempotent result.

**Trivia.** Inline type grammar defers the trivia it skips and emits it only when the type continues
(`Parser::deferred_trivia`), so whitespace and comments after a type sit outside its node. Without it
every range, the erasure, and the formatter had to strip absorbed trailing trivia.

**One meaning.** An inline site lowers to exactly the `Annotation` the equivalent `#:` block would
(`lower_inline_annotation`). A signature attaches to the statement assigning its function, so it
merges with that statement's `#:` block and exports a declared scheme at an item root, or else to
the function expression itself, which is how a lambda passed as an argument is checked. When a
binding also has a `: TYPE`, the function's own signature types it and the binding type is checked
against the result (`check_item_with_annotation` skips the declared-function path when the value
carries its own annotation).

**Both spellings on one statement** merge per slot (value, parameter, rest, return), and type
parameters pair by position (the comment's are renamed to the inline names first), with the inline
side winning, except that a coercion (`@trust`, `@if-unknown`, `@new`) governs the value wherever it
is written, and the inline one wins when both sides carry one (`merge_annotations`). Disagreement is judged separately in `annotation_agreement_diagnostics`, on
file-absolute ranges, comparing types after alias expansion and union-member sorting, so the warning
can point at both written types. Positional comment parameter types pair with formals the way
`check_declared_function` pairs them.

**Turning it off** is a salsa input (`AnalysisSettings`), not host-side filtering, because it changes
what types mean. It is read only where an inline site exists, which `test_incremental.rs` pins. Off,
sites type nothing and each warns (`inline-annotation`), and hosts warn once per load.

**Strict mode reports untyped parameters**, by a `noImplicitAny`-style rule: an unannotated parameter
whose type is `Unknown`, or a fresh unconstrained variable of its own function's level that no other
parameter and not the return mentions. The level check keeps a closure parameter tied to an
enclosing function clean, and a lambda passed as an argument is skipped, because the call types its
parameters only after the lambda is inferred. Unification with `Unknown` never binds a variable, so a rule on `Unknown`
alone would never fire. Returns and bindings get no finding of their own: every undetermined one
traces to an origin, a type error, or a project binding reported at its definition, and a separate
finding would break "one undetermined value, one finding".

**Optionality** comes from the formals alone for a parameter typed only inline
(`Annotation::inline_parameters` skips the restatement checks in `check_declared_function`), because
the inline form has no `[name]`: a defaultless formal the body tests with `missing()` stays optional.
A parameter a `#:` block also names keeps the comment's `[name]`, which is checked as written.

**Known gap.** A generic lambda passed as an argument keeps its binders rigid, so
`lapply(xs, function<T>(v: T) -> T v)` is reported. `backlog.md` tracks it.

## Shipping stack: Inline type definitions, coercions and nominal constructors

**Status:** built.

**Syntax.** `type Name<T: c> = TYPE` declares a nominal type and `alias Name = TYPE` an alias, as
statements at the top level of a file; `x: @trust T <- v` (also `@if-unknown`, `@new`) is a coercing
binding; and `@Name(value)` or `@Name<Args>(value)` constructs a nominal value, the expression form of
`#: @new Name`. Docs: `reference/type-system.mdx#inline-annotation-syntax`.

**Why it is unambiguous.** Two names in a row are not valid R, and neither is `@` at the start of an
expression, so none of these forms gives a valid R program a new reading. `type` and `alias` are
contextual: the definition is recognized only at a statement start with the defined name on the same
line, so `type <- 1`, `alias(type)` and `type` as a parameter keep working. At a line start
`@Name` is a constructor only before `(` or `<`, so `obj` newline `@slot` keeps R's own error;
after an operator a bare `@Name` is a constructor, so completion works while it is typed. A nested definition is an
`annotation` error rather than a silent new scope, because a type's scope is the file. Rejected:
`@type Name = T` (reads as a decorator, and `@` at statement start would collide with the constructor),
and `newtype` (a second keyword for no new meaning).

**Leading `|`.** A union may open with `|`, in `#:` comments too, and inline a union continues onto a
line that starts with `|`. No R statement starts with `|`, so this never swallows the next statement,
and it lets a tagged union list one member per line.

**One meaning.** A definition lowers to the same `NamedDefinition` as its `#:` block, and every
consumer reads definitions through `file_type_definitions`. A `#:` definition directly above an
inline one of the same name is one definition written twice (`paired_comment_definition`): with
inline annotations on, the inline one is used and any disagreement in kind, parameters, constraints
or body is an `annotation-mismatch` warning; with them off, the comment applies alone. An unpaired
inline definition with inline annotations off still declares its name, with an `Unknown` body, so
uses of the name do not turn into unknown-type errors on top of the `inline-annotation` warning.

**Erasure and run time.** Erasure turns a definition into its `#: @type`/`#: @alias` comment, line
for line and keeping comments, so the erased program still declares the type. A paired definition
erases to nothing, since its partner already declares it (two copies would be a duplicate). Because
the comment would swallow the rest of its line, a definition must end its line, which the parser
enforces. Erasure also drops the `@Name` of a constructor, leaving `(value)`, and drops
a coercing binding's `: @trust T`. The runtime skips definitions and lowers `@Name(value)` to its
parenthesized value; a nominal type has no run-time representation.

## Shipping stack: Testing doctrine for `syntax`

**Status:** built.

The parser is the foundation of everything, so it must be extremely well tested. More is better,
and duplicated coverage is welcome and never pruned for elegance. All seven layers apply, not a
selection.

1. tree-sitter-r's parser corpus, imported wholesale and converted into the fixture-harness format.
   The suite is at least tree-sitter-r's, expressed as fixtures.
2. A real-world parse corpus, which is R's base library sources plus top CRAN packages, checked for
   lossless round trip and acceptance parity.
3. Exhaustive hand-written per-construct suites with golden trees and golden error messages. That
   covers every operator, precedence pair, call form, literal form, string, raw string and escape
   variant, every `#:` annotation form, and every error-recovery scenario.
4. Property tests: the tokens cover the input, node ranges nest, and reprinting equals the input.
5. Fuzzing, both random bytes and structure-aware mutations, with never-panic and always-lossless
   invariants. It ran against every parser increment from the first one, and a bounded pass runs in
   the `syntax` crate's own test suite.
6. Statement-reparse equivalence, so an incremental result tree equals a from-scratch tree for a
   randomized edit.
7. Acceptance cross-check against R's own parser where an R installation exists. This is
   local-only, like every test that requires R.

Redundancy across these layers is the point. The same construct covered five ways is deliberate.

## Shipping stack: Cases where R parsers get subtle

**Status:** built.

Cover all of these exhaustively: raw strings in the `r"(...)"` and `R"[...]"` forms, where the
formatter has a byte-for-byte rule for a reason; escapes; `%op%` operators; backtick names;
multi-line `#:` blocks, where consecutive `#:` lines stitch into one annotation region;
statement-boundary and newline sensitivity, which is R's newline-versus-operator continuation rule;
`]]` against `] ]` in nested indexing such as `x[[y[1]]]`; the top-level `else` after a newline,
which is legal inside braces and a parse error at top level; `->` and `->>` assignment; `=` as
assignment against `=` as a named argument, which is context-dependent; unary-minus precedence,
where `-2^2` is `-(2^2)`; the hex, `L` integer and `i` complex literal forms; and `\(x)` lambdas
from R 4.1.

## Shipping stack: Corpus mechanics

**Status:** built, except "CI has no R", which "CI runs `just gate` against GNU R 4.6.2, and nothing ignored" supersedes.

tree-sitter-r's parser corpus lives in its GitHub repository under `test/corpus/`, MIT-licensed.
Fetch it from the repository rather than from the crates.io package, which may omit tests. The
real-world corpus is R's base library sources plus roughly the top 100 CRAN packages. It lives in a
gitignored corpus directory, with a committed manifest and fetch script in `scripts/`. The fetch
needs outbound network, so run it where that exists. The acceptance cross-check against R's
`parse()` needs a local R installation. CI has no R, so the acceptance-divergence allowlist is
adjudicated against R locally once and then committed.

## Shipping stack: Better syntax errors are a goal, not a side effect

**Status:** built.

Dramatically better error messages are an explicit goal of the parser, for R syntax generally and
for `#:` type annotations specifically. Recursive descent knows what it was parsing at every point,
so the bar is: expected-token sets such as "expected `)` or `,`"; paired-delimiter pointers such as
"unclosed `(` opened here" carrying both spans; statement-anchored recovery, so one broken
construct never poisons the file; and, because annotations are first-class grammar, real type-syntax
errors with exact token spans inside a `#:` comment, such as "expected a type after `|`". The
golden error-message suite pins the wording, and the diagnostics goal in `AGENTS.md` sets the bar.

## Deep resolve cuts a cycle to `Unknown` and resolves a shared subtree once

**Status:** built. The table of finished subtrees lives for one call, as "Types
and symbols are four-byte ids into append-only columns" describes. It replaced a table kept across
calls and cleared by a binding epoch.

`InferenceTable::resolve`, the deep resolver in `crates/semantics/src/infer.rs`, walked the interned
type structure recursively with only a depth-64 cap as protection. Interned types form a directed
acyclic graph, so a shared subtree appears once in memory but was re-resolved once per occurrence.
A self-referential binding, meaning a variable whose binding transitively contains itself or a
self-referential alias, expanded as a tree up to the cap. On the real-file corpus of about 507k
lines this measured 397 million inner resolve steps, and resolve alone cost more wall time than the
previous stack's entire pipeline. The depth cap also truncated meaning. Past depth 64 a type silently
stayed unexpanded, which is a position-dependent semantics that no cache can be layered onto.

Deep resolve is a walk over the interned graph with explicit cycle detection that resolves a
shared subtree once per call.

- **A cycle cuts to `Unknown`.** The walk carries a `visiting` stack of the variables under
  expansion. Re-encountering one means an infinite type, and it resolves to `Unknown`. That matches
  the pin-to-`Unknown` doctrine used everywhere self-reference grows, including the fixpoint cap and
  loop widening. Alias expansion keeps a depth guard as a pure resource backstop, not as a
  semantics.
- **Only a clean subtree is kept.** A walk that has taken 64 steps keeps the subtrees it
  finishes from then on, keyed by the interned type, but only those with no cycle cut beneath
  them. A node containing a variable currently being expanded resolves differently at top level.
  The steps are counted, not read off the size of the type the call started from, because a
  variable has size one whatever its binding holds: a chain of variables that each bind a tuple
  holding the next one twice is a walk of 2^depth steps without the table
  (`infer::tests::a_subtree_shared_through_variables_resolves_once`). Most calls end within a few
  steps, where a table costs more than the walk.
- **Nothing outlives the call.** Bindings change between calls, so a table kept across calls
  needs an invalidation rule. A type whose id says it holds no variable and applies no alias
  returns without a walk, which is what most repeated calls ask.

Correctness: silent depth truncation is replaced by the established cycle semantics, so an infinite
type resolves to `Unknown` at the point of self-reference instead of expanding arbitrarily deep.
The fixture and fuzz suites confirm this is observation-equivalent everywhere covered. Performance:
corpus inner resolve steps went from 397 million to 4.2 million, which is linear in corpus size.
Resolve wall time went from 30.8 s to 0.3 s, and a whole corpus pass from 53.2 s to 12.7 s.
`RESOLVE_CALLS` stays as a standing instrument, because a near-linear step count is now an
invariant the performance harness can watch.

## The interface fixpoint is canonical per group, so a cyclic scheme does not depend on forcing order

**Status:** built, except `interface_sccs` and the per-item `interface_group` walk: groups come from one walk per file ("Every host resolves names through one path, and trees live in the host's store"), and an item reads its edges from `item_links`.

A corpus-scale finding from the multi-core instrument drove this. Pre-forcing the per-file phases
gave 64852 findings, and forcing file diagnostics directly gave 64835. Both counts were stable
across runs and thread counts, and one worker equalled four exactly, so the difference was never a
parallelism race. It was query-order semantics.

A cyclic package-interface group used to resolve through dynamic cycle recovery alone, in
`item_check_recover` and `global_scheme_recover`. Whichever member was queried first became the
cycle head, the fixpoint iterated from that head, and a group still changing at the round cap
pinned from that head's perspective. Which items lost their types to `Unknown` therefore depended
on which query happened to arrive first. Every individual forcing order was deterministic, but
hover-then-check, check-then-hover and differently ordered cold passes could disagree with each
other. The previous stack's whole-package rounds were entry-order-independent. Per-item cycle heads
were not.

The shape now lives in `crates/semantics/src/semantics.rs` and has three parts.

- (Superseded in part by "a file's check names only what its interface reaches": the graph is
  now walked per item by `interface_group`, and groups are keyed by their first member.)
  `interface_sccs(files)` built the static interface-reference graph over every item of every
  file, statements included. Each read is resolved by `item_interface_reads` through the same binder
  the check uses (the nearest earlier writer in the reading file, or a script's last writer, then the
  test layer, the package-layer winner, and the conditional writers up to the join cap), so the
  graph has exactly the edges a check follows. One iterative Tarjan pass condenses it in canonical
  order (project file order, then item order). Only a cyclic group is recorded, meaning one with
  more than one member or with a self-edge.
- `scc_exports(files, group)` is the canonical fixpoint of one group. Every member starts at the
  tolerant `Unknown` export (its scheme and the schemes of the top-level names it binds). Each round
  re-checks every member against the previous round's table, which is Jacobi iteration, so one
  propagation hop happens per round and within-round order cannot matter. Convergence is table
  equality. A group still changing at the round cap, which is 16 and shared with the backstop, pins
  all members to `Unknown`. A member check runs through `SalsaGlobals` with the group table, which
  looks up by resolved item, so the fixpoint and the ordinary check share one code path and no
  salsa cycle forms.
- `item_check` adopts the canonical exports as a member's exported scheme, which keeps a single
  source of truth. `global_scheme` reads `item_check` only. Salsa's cycle recovery stays as a
  backstop for references the static graph cannot see, and such a reference is a bug: salsa never
  backdates a cycle member, so an edit that breaks a recovered cycle and restores it panics in its
  backdating check. That is how a winner-only graph that missed statement items, earlier writers,
  script and test files and conditional writers surfaced (`test_incremental.rs`,
  `removing_a_cross_file_cycle_and_restoring_it_revalidates` and its siblings).

Correctness: forward, reverse and phase-pre-forced forcing now render identical diagnostics. The
regression test is `cyclic_group_answers_are_forcing_order_independent` in
`crates/semantics/tests/test_parallel.rs`, and the corpus instruments agree at 64835 findings for
both forcing shapes. The growing-self-reference pin stays `Unknown`. Simplicity: the fixpoint is an
ordinary tracked query over an explicit graph, instead of emergent cycle-head dynamics.
Performance: the sequential corpus pass improved from 12.7 s to 10.1 s, because canonical rounds
replace per-head cycle re-iteration. A keystroke in a 53k-line package costs about 3 ms more, which
is about 8%. That is the once-per-revision validation walk of `interface_sccs`, whose dependency
surface is every item's naming. Narrowing that surface to a per-item read-name projection is the
known lever if it ever matters. Incremental analysis: the graph derives from naming only, so an
edit that leaves every member's read set unchanged backdates `interface_sccs`, and the group
fixpoint re-runs only when a member's check output changes.

## There is no third constraint kind, so two flexible comparison operands stay unconstrained

**Status:** built, except the same-family error on two concrete operands, which "Comparisons coerce across atomic types" supersedes, and the two-kind limit: `Constraint` has `Key`, `Number`, `Eq` and other kinds.

The question was whether comparing two flexible operands in `function(a, b) a < b` should constrain
them, either to each other or to a new comparable constraint kind covering numeric, `character` and
`logical`.

It should not. Two flexible comparison operands stay fully unconstrained. The function infers as
`<T, U> fn(a: T, b: U) -> logical`, and a cross-family call is accepted.

Superseded in part: the same-family rule and the numeric constraint from a numeric partner are
both gone (see "comparisons coerce across atomic types" below). The reasoning here against a
comparable constraint still holds.

Three reasons.

- R's runtime comparison coerces across atomic families. `1 < "2"` is legal and compares
  `"1" < "2"` as characters. Any constraint tying flexible operands to a family, or to each other,
  therefore rejects a legal program the checker cannot prove wrong.
- A comparable constraint would be the third independent constraint kind, which is the recorded
  traits tripwire. Comparisons alone do not justify designing traits. The constraint would be
  nearly vacuous, because every atomic family is comparable, so it would buy almost no precision
  for real machinery cost.
- The same-family error on two concrete operands stays. That case is decidable and catches a real
  bug, such as `x < "10"`.

Correctness: this ratifies existing behavior, pinned by the fixture
`two_flexible_comparison_stays_unconstrained`. Simplicity: no new machinery, and the traits
tripwire stays armed. The typing reference states the flexible-operand comparison rules explicitly.

## Union compatibility commits a flexible argument at first use, in program order

**Status:** built.

A flexible argument checked against a union-typed parameter binds to the whole union. With
`f : fn(x: integer | character)`, the call `f(v)` pins `v := integer | character`. A later use of
`v` against a different union, such as `g : fn(x: logical | character)`, then errors, even though
the intersection `character` would satisfy both. The question was whether commits should be made
order-free through constraint collection and intersection solving.

First-use commitment is the specification. A flexible argument checked against an expected union
binds to the whole union at that use, exactly as unification would. Uses commit in program order,
and a later conflicting use reports at its own site against the committed type. The fix for a
genuine intersection case is an explicit annotation naming the intended member type.

Three reasons.

- Program-order commitment is how the checker already treats every other type. First use binds is
  standard Hindley-Milner. Making unions special would demand intersection constraints, which is a
  new constraint former squarely on the traits frontier and deliberately out of scope.
- The order dependence is bounded and predictable. It never changes whether an inconsistent pair of
  contracts errors, because some site always reports. It only decides which site is blamed, and
  that is the later use, which is where a reader's attention should go.
- Program order is the order R evaluates in, so the blamed site matches the first call that would
  misbehave at run time under the committed reading.

Correctness: this ratifies existing behavior. The fixtures
`flexible_argument_commits_to_the_union_at_first_use` and
`union_commit_blames_the_later_conflicting_use` pin both orders. Simplicity: no constraint-solving
machinery. The typing reference's union-compatibility section states the commitment rule and its
annotation escape hatch.

## Strict mode attributes a recursive binding the fixpoint cannot fully type

**Status:** built.

The canonical per-group interface fixpoint types converging recursion precisely. A top-level `fact`
exports `fn(n: integer) -> integer`, and mutual `is_even` and `is_odd` export
`<T: numeric> fn(n: T) -> logical`. Fixtures pin this, and it supersedes the older contract under
which self-recursion deliberately stayed a tolerant `Unknown`. The typing reference is updated.

Two shapes still resolve to `Unknown`.

- **A growing self-reference pinned at the round cap.** These already surface under strict mode
  through the undetermined-reference origin at the recursive read, because the read sees a literal
  `Unknown`.
- **A cycle that converges with `Unknown` embedded.** `f <- function() f()` settles at
  `fn() -> Unknown`. The read sees a function type, so no origin fires anywhere, and the export
  silently carries `Unknown`. This was the attribution hole.

After `item_check` adopts the canonical group scheme, a `StrictOriginKind::RecursiveUnknown` origin
is recorded on the whole binding when two conditions hold: the member's body produced no errors and
no other strict origins, and the adopted scheme still contains `Unknown` by `types::contains_unknown`.
It renders as "strict mode: could not determine the full type of `f`; it is defined recursively;
add a type annotation". The clean-body gate keeps the propagation doctrine, so a binding is not
re-reported when something inside the body already attributes the `Unknown`.

One over-report is accepted. A clean-bodied cycle member whose `Unknown` propagates from a
sibling's origin is still attributed. Detecting that would need group-wide origin bookkeeping
inside the fixpoint, and the advice to annotate this binding genuinely closes the member's export
regardless of the sibling.

Correctness: every `Unknown`-carrying export now has at least one strict attribution, and fixtures
cover the self, mutual, annotated, growing and pure-self-call shapes. Simplicity: one new origin
kind and a type walk, with no fixpoint machinery. Incremental analysis: the check runs inside
`item_check`, so there are no new queries.

## Script frame semantics

**Status:** built, except a conditional top-level slot typing `Unknown`, which "Conditional slots type, export edges generalize, and `missing()` flows through the environment" supersedes.

A script's top level is one frame executed top-down. The question was what a cross-item read
resolves to, for naming, for the unused check and for typing, when the frame holds several bindings
of the name, when the read precedes every binding, and when the read sits inside a closure. The
fuzz arm drove this, by exposing that script unresolved checking was entirely missing and that
cross-item resolution had no defined contract.

There is one rule per kind of read.

- **An immediate read**, executed at its position in the top-down run, resolves sequentially. The
  nearest earlier top-level binding wins, before package globals and stubs. A use before every
  definition is an unresolved name, because it errors at run time. That includes a use inside the
  very statement that first binds the name, such as `x <- x + 1L` with no earlier `x`.
- **A deferred read**, from inside a nested function, resolves against the whole document, because
  the closure runs after the frame settled. The last top-level binding wins, including the
  enclosing statement's own binding, so self-recursion resolves and types through the cycle
  fixpoint. `a <- function() a()` exports `fn() -> Unknown` through the round cap, and a later
  rebinding is what the recursive call actually sees at run time.
- **A conditional top-level write**, inside a top-level `if`, `for`, `while` or `repeat`, creates
  the document's variable slot exactly as the package specification already said. A later read
  resolves to it. The slot exports no scheme yet, so such a read types `Unknown`. Lifting that is
  in the backlog.
- **A quiet read**, from data masking or an opaque operator, is never reported unresolved. It still
  counts as a use for the unused check and gets full navigation, because at run time it falls back
  to the enclosing binding.
- **The unused check follows the same model.** A deferred read keeps every binding of the name
  alive. An immediate read marks definers backward through conditional ones, because a conditional
  rebinding does not end an earlier binding's liveness. A loop that reads its carried variable
  keeps both its own write and the earlier binding alive, because the first iteration reads the
  outer one.

Correctness: scripts get the unresolved class for the first time, which the specification mandated
and which was silently absent. A duplicate `@type` or `@alias` name now errors at every site, and
six fuzz-found gaps are fixed with a fixture pinning each. Simplicity: one `deferred` bit threaded
through `GlobalEnv`, instead of a second resolver. Incremental analysis: resolution facts stay
per-item queries, and `frame_slot_positions` is one small per-file map.

## An undeclared type name errors once at the reference and compares like `Unknown`

**Status:** built.

A misspelled nominal inside a `@type` body, and in every other annotation position, was silently
lowered to an opaque nominal. The shape now has three pieces, each with one source of truth.

- **Recording.** Annotation lowering, in `annotations::lower_annotation`, records every
  `TyKind::Named` mint with the referencing token's range, in `Annotation::nominal_references`. A
  primitive or an in-scope binder never reaches the record, because lowering resolves it first.
  Binder scoping therefore stays single-sourced instead of being re-derived by a diagnostic walk.
- **Reporting.** `unknown_type_diagnostics` checks the recorded references against the project's
  `@type` and `@alias` declarations, plus the file's own for a script, and against the stub
  corpus's nominal vocabulary. It errors at the precise token with a nearest-name hint, so
  `Instument` asks whether you meant `Instrument`. A forward reference stays legal, because the
  vocabulary is position-independent.
- **Tolerance.** An undeclared nominal compares like `Unknown` at the relation level. `unify`,
  `compatible` and the operator checks' `structural()` projection all consult one
  `undeclared_nominal` predicate. The typo is therefore reported exactly once and never cascades
  into a value-level mismatch, a call-site error in another item, or operator noise.

The declared-annotation check was found along the way to silently skip `Named`, `Record` and
`Tuple` declarations. A positive-list gate meant for tolerance had become a hole, so `#: Point` on
a structural value minted the nominal without `@new`, which contradicts the nominal-introduction
contract. The gate is now a negative list holding `Unknown` and `Any` only, which enforces the
`@new` discipline at a declared site and checks a record or tuple declaration for the first time.
The typing reference states both contracts.

Correctness: the bug class is closed, with fixtures and fuzz templates guarding it. Simplicity: one
predicate instead of per-site suppression guards. Incremental analysis: the diagnostic is a
per-file pass over already-lowered annotations.

## An annotation shape violation refuses the whole block

**Status:** built.

The question was where annotation-shape validations live, and what happens to a violating block's
typing payload. The validations cover directive ordering, duplicate and unknown type parameters,
applied binders, the `@new` payload shape, nesting caps, vector-element atomicity, and attachment
rules.

- **One refusal semantics.** A block with any shape violation keeps only its errors. The whole
  typing payload is dropped, which covers the declared type, definitions, `@new`, `@strict` and
  nominal references. One mistake therefore yields one error and no follow-on findings.
  `Annotation::errors` and `typing_errors` hold the errors, and `lower_annotation` strips the rest.
  A consumer observes the payload's absence rather than a validity flag. An inlay hint gates on
  surviving payload, meaning a declared type, `@new` or trust, rather than on the annotation's
  presence, so a refused binding hints its inferred type again.
- **Attachment is single-sourced.** `statement_annotations` computes each top-level block's target
  as attached, blank-line-separated or dangling. Both annotation application, in
  `item_annotation_syntax`, and the dangling-annotation diagnostics read it. A blank line or an
  interposed comment genuinely detaches the annotation. The checker previously applied silently
  across a blank line, which contradicted the reference.
- **Two classes gate differently, on purpose.** Past 160 levels of nesting the annotation shape is
  refused and always reported. Past 128 the type is refused for checking, which is a typing-class
  finding that disappears under `# typing: off`. The vector-element rule works the same way.
  Lowering records every `[]` element with its range in `Annotation::vector_elements`, and a
  diagnostics pass with the project vocabulary judges it: an alias expands, a nominal refuses, and
  an undeclared name stays silent because the unknown-type error owns it. It reports in the typing
  class at the use site. The vector finding does not strip the payload, because the judgment needs
  vocabulary that lowering lacks, so the declared shape still serves hover and navigation. That is
  an accepted and documented difference, visible only in exported schemes.
- **A definition is top-level only.** A nested `@type` or `@alias` block errors and does not enter
  the vocabulary, because `file_type_definitions` reads top-level children only.

Correctness: the whole validation family closes, with fixtures pinning each shape and message.
Simplicity: one errors vector and one attachment walk, instead of per-consumer validity checks.
Incremental analysis: everything stays in per-file parse-pure passes except the vocabulary
judgment, which joins the existing per-file semantic families.

## A statement-level annotation attaches at any depth and applies where the expression infers

**Status:** built.

An annotation below the item root used to be invisible to the checker, so the constructor idiom did
nothing. Writing `#: @new Person` on a local assignment, or on a block-final expression inside a
function body, was silently ignored.

The shape keeps one source of truth per fact.

- **Association.** `statement_annotations(parent)`, which is the existing adjacency walk, runs over
  any statement sequence, whether the file root or a braced block. Top-level attachment,
  expression-level attachment and the dangling-annotation diagnostics, which now cover a nested
  block, therefore share one rule. `expression_annotations(db, item)` maps each attached block
  inside an item to the annotated expression's HIR id by exact range. It is a plain function rather
  than a tracked query, because `Annotation` carries text ranges with no memo plumbing, and its
  callers are tracked queries whose dependencies already flow through `item_syntax` and `item_hir`.
- **Application.** The checker owns one `apply_expression_annotation` seam. An assignment applies
  it before the slot write, so the binding takes the annotated type. Every other expression applies
  it where it infers. A non-assignment item root routes its own annotation through the same seam,
  which closes the bare-expression checked-annotation gap. `@new` reuses `check_new_nominal`, which
  checks the representation and mints the nominal. A checked declared type enforces the same
  directional `compatible` contract as at the root, and `@trust` overrides unchecked. The loop-body
  re-walk discards errors, which covers the new errors for free.
- **IDE consequences.** Goto-type-definition and hover pick the nominal up from the recorded
  expression types with no work on the feature side. An inlay hint skips an annotated nested
  binding, because the annotation already names the type, which is symmetric with the root gate on
  surviving payload.

Correctness: fixtures pin the constructor idiom in both forms, the mismatch error at the value,
trust, bare-expression checks and nested blank-line detachment. Simplicity: one association walk
and one application seam, instead of per-position special cases. Incremental analysis: everything
stays inside existing per-item queries.

## Capture liveness is frame-scoped

**Status:** built.

This settles which writes a closure's captured read keeps alive for the unused check. The checker
marked by name across all frames, so a shadowed outer binding never warned.

A read from inside a nested function keeps every write of the name alive in the frame the read
resolves to, and in no other frame. Sequential rebindings of one name in one frame are a single
run-time variable, and the closure runs after the frame settled. A same-named binding in an
enclosing frame that the resolved binding shadows is not what the closure reads, so it stays
reportably dead. This is exactly R's environment semantics.

The mechanics are these. A frame carries a stable identity in `Scope::id`, minted per defining
expression like a binding id, so a loop re-walk reuses it. Each assignment write records its slot's
owning frame, and the capture sweep filters on frame and name. A write recorded after the read
stays covered by the existing per-slot `captured_slots` marking at the write site. The typing
reference documents the rule with an example in each direction.

Correctness: a false-negative class is gone, which is a dead shadowed binding in closure-heavy
code. Simplicity: one id per scope, instead of a second liveness structure. Incremental analysis:
naming stays a per-item pure function.

- `environment()` and the other functions in `EXPLICIT_NAME_ACCESSORS` read every name of the frame they are called in, so each binding of that frame counts as read.
- The `duplicate` rule drops a pair of definitions when the earlier value is read eagerly between them or when a `setGeneric` call sits between them (`earlier_value_read`). A deferred read does not count, because a closure sees the later definition.
- Implicit reads that exist only for the unused check (`tail_assignments`, `escaped_frames`, `mark_formals_read`) stay in naming's slot reads. They never enter `captured_slots`, which the checker uses for types.

## Conditional slots type, export edges generalize, and `missing()` flows through the environment

**Status:** built.

These three lifts closed the last gaps in cross-item typing.

**A conditional top-level slot types.** A statement item's conditional write, such as
`for (i in 1:3) total <- i`, already created the document's variable slot for naming. It now types.
`ItemCheck::top_level_bindings` carries the settled, export-closed scheme of every name the item's
top-level frame binds. `statement_binding_scheme(item, name)` projects it per binding, as a
value-equality firewall with `global_scheme`-style cycle recovery, because a statement item reading
its own conditionally written name routes back into its own check. Readers consult it after
`package_definitions`, joined across multiple writers, and inside the script sequential search. The
winner order is unchanged, so an unconditional definition still shadows the slot.

**The export edge generalizes a constrained residual.** `erase_residual_vars` erased every unbound
variable to `Unknown`, which destroyed real information. `mixed_apply <- invoke(mirror)` lost its
numeric bound, and cross-item calls stopped checking. `close_scheme` now generalizes an unbound
variable that carries a constraint into a fresh scheme binder, and erases only an unconstrained
one. The synthetic names never display, because the renderer canonicalizes rigids. Instantiation
stays per reader, which matches R's call-by-call semantics for the immutable closures this shape
produces.

**`missing()` supplied state flows through the environment.** A third entry kind,
`EnvEntry::MissingFormal`, rides the existing branch mark, rollback and join discipline instead of
a parallel liveness structure. The true edge of a `missing(name)` guard marks the slot of a formal
that has no default. A read of a marked slot errors, because it would fail at run time. Any write
supplies it back to an ordinary entry. The marker is branch-local at a join, so a rejoined state
means only possibly missing, which reads as the supplied type. Only a definite run-time failure
reports, as the reference specifies.

Correctness: every new behavior is pinned by fixtures in both directions. Simplicity: each lift
reuses an existing mechanism, which is the per-item check, the erase walk and the environment
discipline, instead of adding a parallel structure. Incremental analysis: two new tracked
projections with value-equality firewalls, and no new interface surfaces beyond them.

## NAMESPACE and DESCRIPTION metadata feed resolution

**Status:** built.

The user asked for `importFrom` to work. The NAMESPACE file was parsed only at the CLI and server
layer, for import-site problems such as an unknown import or an unused import. Resolution ignored
what the package imports, and DESCRIPTION was never read. Real packages therefore saw two
false-positive classes. A bare read of a name imported from a namespace the stub corpus does not
describe warned that it could not resolve, which hits code using
`importFrom(data.table, ':=')`. A `pkg::` call into any undescribed namespace warned about an
unknown package namespace, even for a declared dependency.

One new singleton input, `metadata::PackageMetadata`, carries normalized import pairs of
`(namespace, Option<name>)`, sorted and deduplicated, plus the DESCRIPTION dependency name set. A
host installs it next to `StubSources`. The CLI installs it per target. The server installs it at
startup and refreshes it on a NAMESPACE buffer sync and on NAMESPACE or DESCRIPTION watcher events,
diffing the parsed facts so a formatting edit does not invalidate anything. The NAMESPACE parser
moved from the host crate into `semantics::metadata`, which makes it the single source of truth,
and the host keeps problem rendering.

Consumption is a few predicates at the diagnostic edges. `imported_by_name` and
`imports_every_name` join the unresolved-check skip set, and `declared_dependency` quiets the
unknown-namespace warning. Typing is untouched, so
an imported but undescribed read stays `Unknown` with the usual strict origin.

One tolerance call matters. An `import(pkg)` of a namespace without stubs makes every otherwise
unresolved bare read in the package quiet. The export set is unknowable, and guessing would trade
the zero-false-positive mandate for typo detection. Typo detection resumes when stubs describe
`pkg`, because the export set then gates exactly, and an `importFrom` name is always exact. Bare
resolution of a stub name stays ungated, as the earlier record says. Metadata only ever widens the
resolved universe, and never narrows it.

Correctness: both false-positive classes die on real packages, and the `typing-imports` fixture
suite pins both directions, meaning an imported name is quiet and an unimported one still warns.
Simplicity: two predicates over one input, with no naming or inference changes. Performance and
incremental analysis: the predicates run only after every cheaper skip fails, which means on a
genuinely unresolved name, and input diffing confines invalidation to a real metadata change.

## Data.table awareness

**Status:** built, except "column knowledge is deliberately not modelled" for a `data.table` whose columns are known: `data.table{...}` tracks them.

The masked-bracket recognition was purely syntactic and its result was always `Unknown`. A chain
lost its class after one bracket. `DT[speed > 20]`, which carries no marker, warned that it could
not resolve `speed`, on the most idiomatic data.table line there is. No shipped stub could give a
value the `data.table` class in the first place.

The shape has three pieces, each with one gate.

- **The stub namespace is conditional.** `types/data.table.Rtypes` ships, carrying the
  `@type data.table` nominal and about 45 high-traffic declarations. It joins `stub_library`'s fold
  only when `metadata::namespace_active` says the project uses the package. That means a
  DESCRIPTION dependency, a NAMESPACE import source, or a `library()`, `require()`,
  `requireNamespace()` or `loadNamespace()` call with a literal package argument in any project
  file. `metadata::file_attached_namespaces` is a per-file syntax scan, and hosts union it into the
  `PackageMetadata.attached` field. The CLI does it once after load, and the server does it
  incrementally per synced file plus the idle prime, so there is never a per-keystroke sweep.
  Gating at assembly means every consumer inherits the same universe with no per-site check, which
  covers bare resolution, `pkg::` validation, the nominal vocabulary, completion, shadow lints,
  typo suggestions and masked verbs. While inactive, data.table behaves exactly like any
  undescribed package, so its names cannot steal a typo warning. The assembly cycle risk, which
  runs naming to stubs to activation to naming, is broken by keying activation only on inputs. It
  never keys on a naming or item query.
- **A result-shape classifier runs in `infer_index`.** A single bracket whose subject resolves to
  the `data.table` nominal, from the shipped stub or from any project `@type`, classifies
  `[.data.table` by the bracket's own syntax. A missing or empty `j`, which covers filters and
  joins, a `:=` call, a `.()` or `list()` call, and any grouped `j` with `by =` or `keyby =` all
  keep the subject's class. Another `j` shape stays a sound refusal, so `Unknown` with a strict
  origin. The class is a real type, so it survives chains, checks against annotations and
  constrains calls. Column knowledge is deliberately not modelled, which covers element types,
  membership and `:=` evolution. The typing reference documents the result-class table as the
  contract.
- **Masking uses the typed subject.** The same classification records every read under the
  bracket's index arguments in `ItemCheck::masked_reads`, nested closures included, because they
  are created in the data's frame. The unresolved-warning renderer skips them. This is
  checker-derived masking on top of naming's syntactic recognition, so `DT[speed > 20]` and
  `DT[, x]` go quiet exactly when the subject's class is known. The syntactic path stays untouched
  for an unknown subject.

Correctness: this kills the dominant data.table false-positive class, which is a bare column read
in an unmarked bracket, and gives a chain or an annotation a real class to check. Sound-by-refusal
is preserved everywhere column knowledge would be needed. Simplicity: one assembly gate, one
classifier function and one diagnostics skip. Incremental analysis: activation reads only inputs. A
flip rebuilds the stub library, which is rare and worth the full refresh, and the per-keystroke
cost is one memoized single-file scan on the edited document.

## The native pipe desugars at lowering

**Status:** built.

`x |> f(y)` is not an operator in R at all. R's own parser rewrites it to `f(x, y)` before
evaluation. Modelling it as an opaque binary operator, with quiet reads and a silent `Unknown`,
threw away exact static knowledge on one of the most common constructs in modern R. Desugaring is
not an approximation. It is R's definition.

`hir::lower_pipe` intercepts `PIPE_GREATER` before binary lowering. A call on the right-hand side
lowers as that call with the piped value inserted as the first positional argument. When a `_`
placeholder sits as the whole value of exactly one named argument of that call, which `pipe_shape`
determines by a syntax-level scan, the piped value is substituted as that argument's value instead.
The `_` token never lowers, so nothing dangles in the arena. Everything R rejects keeps the old
opaque-operator lowering, which is sound silence and never a guess. That covers a non-call
right-hand side, a positional, repeated or nested `_`, and `_` as a tag.

Naming, typing, overloads, arity checks, strict mode and every IDE feature inherit the real call
with no changes. Error blame on a bad piped value lands on the left-hand expression's own range.

magrittr's `%>%` stays opaque. It is a real function with dot-substitution semantics rather than
parse-time sugar, so modelling it would be a separate decision.

Correctness: a pipeline types end to end, so `x |> length() |> sqrt()` is `double`. An argument
error inside a pipeline surfaces with precise blame, and R's placeholder pitfall of a missing first
argument is caught statically. Simplicity: one lowering seam, with no checker or naming changes.
Incremental analysis: the change is lowering-local, so per-item firewalls are unaffected.

## `@masked` is formal-aware, and dplyr is a conditional namespace

**Status:** built.

The contract was ahead of the implementation. The typing reference always said that a `@masked`
argument matching a declared formal resolves normally, but naming hardcoded the first positional
argument as the data. That breaks a zero-formal mask, where every argument is a column reference as
in `join_by(x == y)`, and it breaks a named data argument.

The stub loader now records each masked verb's formals declared before `...`, as
`StubLibrary::masked` mapping a name to its leading formal names, extracted from the lowered
`FunctionType`. The naming walk resolves an argument normally when it matches a leading formal by
position or by name, and masks everything the `...` absorbs. An empty formal list masks every
argument. The base family of `with`, `within`, `subset` and `transform` keeps its one data argument
through its real formal names, which are `data` and `x`.

dplyr rides the existing rails. `dplyr.Rtypes` joins `CONDITIONAL_NAMESPACES`, so the data.table
record's activation semantics apply unchanged. The verb set is `@masked` and class-preserving, as
`<T> fn(.data: T, ...) -> T`, so `mutate` on a data.frame is a data.frame and on the data.table
nominal is a data.table. A join preserves the left class. `join_by` is a zero-formal mask. The
tidy-select helpers and the verb vocabulary, such as `n()`, `row_number()` and `if_else`, are
declared so they resolve inside a mask. Composed with the native-pipe desugar, a masked verb call
in a pipeline is just a call, so `df |> filter(cyl > 4) |> mutate(r = mpg / wt)` types
class-preservingly with no unresolved-column warnings. Where a dplyr name collides with an attached
stub name, such as `filter` or `lag` in stats, source order makes the dplyr declaration win exactly
when dplyr is active, which matches R's own attach shadowing.

Correctness: the documented masking contract is now the implemented one, and the dominant dplyr
false-positive class disappears for a project that declares or attaches dplyr. That class is a
column read in a verb, in a project without hand-written project stubs. Simplicity: no new
mechanism, just one map where a set was and one namespace entry. Incremental analysis: unchanged,
because the masked map lives in the same set-once library.

## A vendored export manifest carries the name-level truth beside the typed stubs

**Status:** built.

The typed stub corpus, at about 530 declarations, was also the resolution universe. Any real
standard-library export outside it warned that it could not resolve. `recover` and `traceback` were
user-reported instances of a false-positive class of about 2,500 names, because base alone exports
about 1,400. Chasing completeness with hand-written typed declarations does not scale, and it was
never the corpus's job.

Every namespace R ships now pairs with a generated `types/<ns>.exports` manifest holding its
complete export list from a live R session. `scripts/export-manifests.R` generates it, and the
header records the R version. `datasets` uses the search-path listing, because its objects are lazy
data rather than namespace exports.

The `StubSources` input carries sources and manifests. The loader unions manifest names into
`exports_by_namespace`, so `pkg::name` validation and shadow lints see them, and into a flat
`known_exports` set that `package_scheme_exists_in` consults after schemes and nominals. A manifest
name resolves everywhere a typed name does, which covers bare and qualified reads, completion and
the typo-suggestion corpus, but it types `Unknown`. Precision stays the typed corpus's job, and the
manifest's job is silence about a real name.

Three tiers mirror R. A default-attached namespace is bare-visible unconditionally, which now
includes `datasets`, whose famous frames are typed `data.frame` in `datasets.Rtypes`. A namespace R
ships but does not attach always validates a `::` read but gates bare visibility on an attach or a
declaration. Those are listed in `QUALIFIED_ONLY_NAMESPACES`: tools, parallel, compiler, grid,
splines, stats4 and tcltk. A conditional CRAN namespace gates both, along with its stubs.

A unit test gives the corpus teeth. It asserts that every `.Rtypes` value declaration is a real
export of its own namespace. A `@type` nominal is exempt, because it names a class rather than a
binding, and a conditional namespace may override a base name, as data.table's class-preserving
`merge` does. Writing the test immediately caught two misfiled declarations, because `traceback`
and `standardGeneric` are base exports rather than utils and methods exports. Both moved.

One thing was discovered on the way. An agent container can have real R. Installing it through
`apt` and the CRAN repository takes minutes, and data.table and dplyr compile from source. R-dependent
tooling therefore runs in-container after all, which covers manifest regeneration and the REPL
end-to-end suite. The long-standing assumption that no agent container has R is dead.

Correctness: the could-not-resolve false-positive class dies for the whole shipped standard
library, at no check-time cost for an unused name. Completion and suggestions widen to the full
export lists. A non-syntactic name is excluded from bare completion and is backtick-quoted after
`pkg::`, because inserting it raw would change the syntax. The not-exported warning for `pkg::name`
becomes accurate instead of based on a curated subset.

## One source of truth for where an item spells its name

**Status:** built.

**Problem.** Three consumers located a top-level item's declared name independently. The IDE asked
naming for the top-level binding and otherwise took the first `NAME` node, which for an S4 generic is
`setGeneric` itself. The duplicate-definition sites took the first `NAME` node whose text equals the
name, which has no answer for a string target. The script unused check asked naming and otherwise
fell back to the whole statement. Where naming mints no top-level binding (`x <<- 1` at top level,
`setGeneric("name", …)`), the unused warning therefore highlighted the entire statement instead of
the name. The classification also read `:=` as an assignment spelling, publishing a definition that
neither R nor the HIR (a call to `:=`) makes.

**Shape.** `item_spans` classifies each top-level node through `top_level_definition`, which
returns the item's kind, its name, and the range of the syntax that spells the name: the assignment target, or the
`setGeneric` name inside its quotes. `:=` is no longer a binding spelling. In the IDE, a
project-defined generic is one global symbol, so its `setGeneric`, `setMethod`, and `standardGeneric`
strings and its calls navigate, reference, and rename together. `semantics::item_name_range(db, item)`
re-reads the range off `item_node` at the rendering edge, so item identity stays free of positions.
All three consumers read it, and the heuristics are deleted.

**Impact.** Item identity and the name site cannot disagree about which name a statement binds.
Unused warnings, goto, hover, and the outline land on the name for every definition shape. Nothing
new is stored and no query is added: the lookup is a hash probe plus a descent into one statement.

## A check reads per-name firewalls, never project-wide maps

**Status:** built, except `interface_group` and the per-name lookups for names an item spells, which "Every host resolves names through one path, and trees live in the host's store" replaces with `file_interface_groups` and `item_links`. `package_name`, `test_name` and `frame_binder` serve the names a check constructs.

`item_check` used to resolve cross-item reads against three whole maps that it read eagerly:
`package_definitions`, which holds every winner in the project, the membership of
`interface_sccs`, and its own file's item positions. A new or renamed definition anywhere changed
those maps, so every check in the project re-ran on its next demand, even a check that reads no
name at all. The previous engine had per-symbol firewalls and the salsa stack had lost them. Nothing
measured the loss, because the keystroke probe edits one file and demands only that file.

Each of those reads now goes through a small tracked projection. After a structural edit, each
projection re-runs once per read and is backdated when its answer is unchanged, so a check that
depends on it does not re-run.

- `package_name(files, name)` answers with the name's winner and its conditional writers.
- `interface_group(files, item)` answers with the item's cyclic group, if it has one.
- `frame_binder(item, name, deferred)` answers with the file-local binder that an immediate or
  script read sees. A package function body, which is the common item, never consults it.

`SalsaGlobals` holds no map any more. `item_check` also grows its stack on demand through
`stacker`, because demand recursion goes as deep as the longest chain of definitions that each
read the next one.

Correctness is unchanged, and every fixture suite is byte-identical. A new definition now re-checks
only its readers, and an inserted statement re-checks only the reads it intercepts, which
`crates/semantics/tests/test_incremental.rs` pins. Resident memory grew 3.5% at 794K lines, from
1,270 to 1,314 bytes per line, and cold time is unchanged. One gap remains: a salsa-level cycle
through statement items, which `interface_sccs` does not model, re-iterates on every structural
edit. `backlog.md` tracks it.

## Cold-pass memory and time: interned names, compact HIR, parallel setup

**Status:** built, except the per-thread rowan token cache and the boxed-slice HIR lists, which "The syntax tree is ry's own flat preorder arena" and "HIR as struct-of-arrays, lowered from the green tree" supersede.

- **Previous shape.** Every HIR expression, naming binding and resolution owned its own `String`
  for each identifier, `Vec`s with spare capacity held block and argument lists, the HIR arena kept
  its growth slack, per-item memos used hash maps keyed by `ExprId`, and each parse allocated a
  private rowan node cache. `ry check` then re-parsed every file serially twice (attached
  namespaces, used tokens) before analysis, and dropped the whole database at exit.
- **Weakness.** The same spelling was stored once per mention (a name like `x` appears hundreds of
  thousands of times), the setup phase was single-threaded, and the drop walked every
  allocation only to exit. Peak memory was about 1 GiB and the cold pass 6 s.
- **Target shape.**
  - `hir::Symbol` is a process-wide interned name: a `Copy` 4-byte index into a sharded table. HIR,
    naming and the check output carry `Symbol`. Project-level maps stay keyed by `String` so their
    lookups keep borrowing `str`, converting at the boundary.
  - HIR lists are boxed slices and `lower_item` shrinks the arena, so a memoized item holds no
    slack. `ItemCheck.expression_types` is a dense table indexed by `ExprId` (ids are dense within
    an item).
  - Parsing shares one token cache per thread, replaced every 256 parses so a long-running
    server does not pin the garbage of replaced trees.
  - `check` parses each file once on all cores while collecting attached namespaces and used
    tokens, forgets the database instead of dropping it, and the binary uses mimalloc (a default
    feature; massif only sees the system allocator, so profile with `--no-default-features`).
- **Impact.** Cold check 6.1, 6.7, 6.2 s to 3.4, 3.6, 2.8 s and peak 1 GiB to about 770 MiB on the
  three benchmark code bases, with byte-identical diagnostics. Item-level firewalls are unchanged
  (the new types compare structurally, so backdating still applies), and edit latency fell from
  3 to 5 ms / 95 to 120 ms to 2 ms / 45 to 55 ms (measured, cause not isolated: the allocator and smaller
  memos both touch the incremental path).

## Syntax trees are evicted, and only small syntax facts are memoized

**Status:** built, except tree eviction and `RootDatabase::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes. Kept for the rule that tree readers are memoized queries with small results.

Tree eviction itself is superseded: see "Every host keeps every tree" below.
The small memoized syntax facts remain.

- **Previous shape.** Every file's rowan tree stayed memoized for the whole process, beside the
  splice cache's own copy of the previous tree, and `AnalyzedFile` kept a second copy of each
  file's source. Diagnostics and lints walked the tree directly (error ranges, annotation
  references, inline-versus-comment agreement, the syntax lints), so dropping a tree meant
  re-parsing it.
- **Weakness.** The trees were the largest holder after compaction, retained only for queries that
  reduce them to a few small facts, and the same text lived twice.
- **Target shape.**
  - `parse` (LRU 128), `item_syntax` and `item_annotation_syntax` (LRU 4096) are evictable. Every
    tree reader is a memoized query with a small result: `file_error_ranges`,
    `file_annotation_references`, `agreement_candidates`, `lints::file_syntax_lints`, and
    `item_annotations` (the root and expression annotations of an item, boxed).
  - A fact that also needs naming or project declarations is split: the memoized syntax half has
    no such dependency, and a cheap join adds it (`SyntaxLints.unless_redefined`, `SlotPair`'s
    `same_type` step). Keeping the syntax half naming-free is what lets the cold pass compute it
    before naming, while the tree is resident.
  - `ry check` uses `RootDatabase::one_shot` (no splice cache) and runs three passes. Only the
    syntax pass works in batches of 16 files per core with `trigger_lru_eviction` between them
    (which needs no new revision). The naming and diagnostics passes read no tree. Naming runs
    as one batch; diagnostics evicts between batches of 64 files per core, because one batch
    raised peak memory from about 750 to 870 MB on a 3000-file corpus with many findings.
    `diagnostics::derive_from_syntax` lists every tree-derived fact a later pass reads; a new tree
    reader must be added there, or the cold check re-parses (the one-parse test in
    `test_incremental.rs` catches it).
  - Source text lives once, in `SourceFile`; the CLI reads it back with `ProjectAnalysis::source`.
  - The capacities are set per host, through `limit_resident_trees` (declared `lru = 0`, which is
    unbounded with no per-fetch bookkeeping). Only `RootDatabase::one_shot` sets them. The server
    keeps every tree, because eviction measured about 10 ms slower per settled re-check.
  - The server enters workspace files at `Durability::MEDIUM` and lowers a file to `LOW` on its
    first edit (restored on close). Settled-wave time was two thirds salsa memo verification;
    with durability, memos over untouched files verify without walking their dependencies.
- **Impact.** Peak 795, 789, 813 MiB to 566, 568, 577 MiB on the benchmark code bases, cold check
  unchanged to slightly faster, JSON findings byte-identical. An evicted memo keeps its dependency
  edges, so an untouched file revalidates without re-parsing, and the LSP keeps its incremental
  behavior; only the edited file parses again. Settled LSP re-check went from 48, 63, 47 ms to
  39, 52, 40 ms (finance, data science, biostatistics), mostly from durability.

## A call's result drops unconstrained type-parameter members of a union

**Status:** built.

- `Result.and_then : <A, B, E, F> ... -> Result<B, E | F>`. Without a rule, a step that never fails
  left `F` an open variable, so the result rendered `:Err(:InvalidNumStr | ?)` and later
  unification could bind it to anything.
- `Checker::callback_outputs` picks the scheme's instantiated variables that occur in parameter
  types only as a callback's return; `Checker::drop_unconstrained_members` removes those that are
  still unbound after the call and that no variable older than the call reaches. A broader first
  version dropped any still-open instantiated variable and wrongly typed `Find` in an unannotated
  function and every return-only generic (`<T> fn() -> T | NULL`) as `NULL`; the fixtures
  `standard-library.R.test` "an_open_parameter_filled_by_an_argument_is_kept" and
  "a_return_only_parameter_is_kept" pin both. A union of only such members stays. Contract:
  `type-system.mdx`, "Union normalization".

## The cold check searches green trees and caches globals per item check

**Status:** built.

- **Before.** Every whole-file tree reader (parse-stage diagnostics, annotation sites, attached namespaces, used tokens) walked the red tree with `descendants()`, building a red node per element, about 8 ns each. One item check read `frame_definition` and `package_name` through salsa for every reference, and each read interned a key tuple and verified a memo. `map_child_types` re-interned every type it visited, even when nothing changed.
- **Shape.** `syntax::matching_nodes` filters with borrowed `NodeRef` cursors and makes owned `SyntaxNode`s only for matches; `NodeRef::tokens` reads token text with no owned cursors. `testing::check_parse_invariants` pins both to the owned-cursor walks. (With the flat tree an owned cursor is only a refcount bump, so the gap is now small.) `SalsaGlobals` holds per-check `RefCell` maps for the two lookups, and `map_child_types` returns the input type when no child changed.
- **Why it is safe for salsa.** The first read inside a query records the dependency edge, and repeats only dedupe, so caching a read within one query execution changes no edge set. The cache lives as long as one `item_check` execution and is never shared.
- **Allocator.** mimalloc's eager arena commit made the kernel zero transparent huge pages the process never used. `crates/ry/src/main.rs` turns it off from an `.init_array` constructor, before the first allocation. It cut peak RSS on the benchmark code bases from about 790 MiB to about 520 MiB.
- **Counters.** `infer::RESOLVE_CALLS` is thread-local: a shared atomic bumped on every resolve step was a contended cache line across the worker threads.

## Call checking follows R's lazy argument and matching rules

**Status:** built, except the rule that a branch's write counts only when every arm makes it, which "A conditional write of a formal covers the paths that run it" supersedes.

- **Required formals.** Previous source of truth: a defaultless formal was required unless the body called `missing()` on it, which rejected `f(1)` for `function(x, unused) x`, valid R. Target shape: a formal is optional unless the body may force it. `check::FormalUses` walks the body in evaluation order (a read that no write dominates forces: a branch's writes count only when every non-diverging arm makes them, a loop body's or a `&&` operand's never, and `<<-` never assigns the local formal; `missing()`, `substitute()` and `quote()` arguments do not; reads in nested functions count, since the closure may run; a forced default forces what it reads, to a fixpoint). Inferred and declared types share the walk, so a declared `[name]` is legal over an unforced formal. Laziness only relaxes inferred signatures: a declared signature is honored as written, so `fn(a: integer, b: character)` over `function(a, b) a` keeps `b` required, because the annotation is the contract callers read. The rendered message names the missing formals instead of an arity count.
- **Partial matching.** `argument_targets` runs R's middle pass: an unclaimed name that is a prefix of exactly one unclaimed formal before `...` claims it. Matching stays case-sensitive, so `x =` goes to `lapply`'s `...`, not `X`.
- **`match.fun` strings.** A character argument at one of `check::MATCH_FUN_CALLERS` (base callees only, never a project function of the same name) checks as the named function when it is a literal that names a known scheme, else as `Unknown`. The resolved type rides on `CallArgument::named_function` and is used only where the expected type is a function, so the same argument still reads as `character` elsewhere.
- **Variadic actuals.** `infer::pair_parameters` lets a variadic function serve a fixed interface (its rest requires nothing). Base generics such as `mean` and `as.numeric` declare `...` so method arguments (`as.numeric(d, units = "secs")`) pass, and without this the apply family rejected them as callbacks.
- **Forwarded `...` to a callback parameter** adds no parameter to the demanded function type, because it may be empty.
- **`trailing-comma` is a warning.** R fails on the empty argument only when the callee forces it, so it is not a certain run-time error.

## Called names are looked up as functions

**Status:** built.

- **Previous shape.** A call head resolved like any read, to the nearest binding, so `names <- toupper(names(x))` made `names(x)` a call of a character vector, and cli's `.Call <- NULL` made every `.Call` an error.
- **Target shape.** R's `findFun`: `Checker::function_lookup` skips a binding whose type is definitely no function and continues through the enclosing slots, the project namespace (or, past a test file's or a script's top level, `GlobalEnv::enclosing_scheme`) and the library. An untyped binding that the body only ever calls is a callback and stays the function, so `function(format) format(1L)` types its parameter as a function as before. An untyped binding that is also read as a value somewhere (`dir <- path; dir(dir)`, `x[names(x) %in% names]`), next to an outer function of its name, types the call as `Unknown`, so neither is constrained. A unification probe was considered instead of the value-read test and rejected: at the call head the binding is still an open variable that any function type unifies with, and the conflict only appears at a later value use. The reached binding is stored once, in `ItemCheck.callee_targets`, and the IDE reads it there rather than recomputing the lookup.
- **Qualified names.** `pkg::name` never takes this lookup: a foreign package reads only its stubs, and the project's own package only its namespace (`GlobalEnv::namespace_scheme`), never the reading file's bindings.
- The lookup skips a binding whose type is known not to be a function. A binding typed by a type parameter counts as untyped and is not skipped, because skipping it produces schemes the annotation grammar cannot write, which `test_roundtrip.rs` reports.

## testthat files mirror testthat's environments

**Status:** built.

- **Previous shape.** Every file under `tests/testthat/` was a package file, so test bindings leaked into the namespace and showed up as duplicates or shadowed package functions; a first fix pooled every test file into one shared test namespace, which let one test file resolve another's names.
- **Target shape.** `DocumentKind::{Test, TestHelper}`. Helper and setup files fill the shared test namespace (`test_definitions`), each test file keeps its own bindings, and a closure in a test file prefers the binder its own top-level statement writes, because each `test_that()` block is its own environment. The kind is derived from the path in one place per surface (`cli::document_kind`, `semantics::test_document_kind`).
- **Impact.** One more per-name layer (`test_name`) in the global lookup, read through the same tracked projection as `package_name`, so an edit to a test file does not invalidate package items. The unresolved-name diagnostics read `test_name` too, and their typo suggestions come from the per-name `project_typo_suggestion` firewall, so a new helper definition re-diagnoses no test file whose answer it does not change (`test_incremental.rs` pins it).

## useDynLib native symbols resolve only as a native routine's name

**Status:** built.

- `metadata::NativeSymbols` parses `useDynLib` (explicit names, `.registration = TRUE` with its `.fixes` prefix). Explicitly listed names and names carrying the `.fixes` prefix resolve anywhere, because `useDynLib` binds them in the namespace. A registering `useDynLib` binds the routines its compiled code registers, which ry cannot see, so any other bare name resolves only where naming recorded it as the `.NAME` (or first positional) argument of `.Call`, `.External`, `.External2`, `.C` or `.Fortran` (`naming.native_routine_reads`), and a typo elsewhere still reports.

## Comparisons coerce across atomic types

**Status:** built.

- **Previous shape.** `<`, `==` and the rest required both operands in one family (numeric or `character`), and an open operand compared against a number was constrained to `numeric`.
- **Weakness.** Both rules rejected ordinary R. `Sys.getenv("RSTUDIO") == 1`, `type == "a"` after `if (is.logical(type))`, and `x > 0` on a flag all failed, and the numeric constraint leaked through exported schemes into every caller (zoo's `na.fill`, cli, lubridate).
- **Target shape.** A comparison accepts any mix of `logical`, `integer`, `double` and `character`, because R coerces both sides to a common type. `complex` and `raw` stay unsupported. An open operand is never constrained. `comparison_operand_shape` in `check.rs` owns this.
- **Cost.** `while (n && out)` now pins a counter `n` to `logical`, because the comparison that used to say "numeric" no longer does (rlang `stack.R`).

## A test-guarded write to a formal normalizes it; inferred types admit the default

**Status:** built.

- **Previous shape.** An `if` without `else` joined a branch write with the pre-if type by unification, so `if (is.logical(type)) type <- "q"` pinned `type: character`, and every caller passing the logical the code tested for was reported. A union join here instead produced `T | logical` everywhere, which cascaded through zoo, digest and xts.
- **Target shape.** In `infer_if`, a non-`NULL` write to a slot whose pre-if type is an open formal (`is_open_parameter`), under a condition that reads the same slot (`reads_slot`), *normalizes*: the slot takes the written type after the `if`, and the formal's variable is not unified. Other writes still go through `join_writes`.
- **Default widening.** After inference, a defaulted formal whose settled type does not admit its default's concrete type is widened to `settled | default`, probed with `compatible` under a snapshot. A call passing the default's own value is always fine.
- **Defaulted flags.** A condition or `!` on an unannotated defaulted formal does not constrain it to `logical` (`reads_open_defaulted_formal`), because option flags are passed `1`/`0` as often as `TRUE`.
- **Approximation.** After a normalizing write, the body sees only the written type, not a union with the input. This is unsound when the test does not cover every non-normalized input, and is accepted because the code after such a test assumes the normalized type.

## An open `T | NULL` parameter binds `T` to the argument's non-`NULL` part

**Status:** built.

- **Previous shape.** `is.null(x)` on an open formal shapes it to `T | NULL`, and a caller's open argument then unified with the whole union, so the caller's variable became `U | NULL` and every later use of it was reported (httr2's `check_request` pattern).
- **Target shape.** `compatible_inner` in `infer.rs`: when the expected type is exactly a two-member union of an open variable and `NULL`, and the argument is a variable or a union with no other tag, the variable member unifies with the argument minus its `NULL` members, under a snapshot. The rule is deliberately this narrow; a broader one (any union with a variable member) produced `logical[][]` cascades.

## `switch(typeof(x), ...)` narrows `x` per arm

**Status:** built.

- An arm runs only for the types its labels (plus fall-through labels) name, so `infer_switch` narrows the subject slot before each named arm (`narrow_to_typeof_labels`, `typeof_label_membership`): a union keeps the matching members, an open variable gets a fresh one so that `logical = if (x) ...` does not pin the parameter for the other arms. The narrowing is a refinement, not a write, so it does not join back.
- `NULL = ...` is a legal label in R's grammar and names the string `"NULL"` (`hir.rs`).

## Nested replacement rewrites the spine

**Status:** built.

- `x[[1]]$a <- v` used to leave `x` as it was. `replacement_written_type` now walks the accessor spine: each intermediate container is projected (`project_replacement`), the innermost takes `v`, and each enclosing container takes the rewritten child through the same `write_replacement_step` that single-level writes use. An undescribable intermediate projects as `Unknown`; a `[` or `@` step leaves its container unchanged.

## A strict library file's top level holds only definitions, and purity is declared

**Status:** built.

- **Previous shape.** Strict mode checked types only, so a strict package file whose top level called `cat()` or `options()` checked clean and ran the call on every load.
- **Target shape.** `semantics::top_level::item_top_level` classifies each top-level item of a library file as a definition, a constant, or an effect with its range, and `strict` reports the first effect of each statement (`reference/type-system.mdx`, "A package file's top level"). A library file is a `DocumentKind::Package` file (`R/`); scripts, tests, helpers and the console are entries and unrestricted. Class definition forms are read from `object_system_function`, so the lint, the runtime refusal and this rule share one table.
- **Purity is declared, never inferred** (Roc-style effect inference was rejected: R's higher-order functions take pure and effectful callbacks under one name, and an inferred effect in a type would let one added `cat()` change every transitive caller's interface). `item_declared_pure` is a per-item `bool` read from `#: @pure` or the inline `@pure` marker (`PURE_MARKER` in the syntax tree), so a body edit backdates it and callers never re-check; stubs mark names with `@pure`. The marker is trusted and means "no effect whatever the arguments", which is why `lapply` is not pure. A project definition shadows the stub, and a name with a conditional writer is never pure (`CalleeOrigin::Conditional`), because which definition runs depends on the condition.
- **Lazy constants.** In a strict file, a single-writer constant is bound lazily by the loader (`load_index::package_index`) only when every package name its initializer mentions has one writer earlier in collation order; otherwise a later or second write would change what a late evaluation reads. `loader::diff` rebinds a lazy constant when anything its initializer reaches changes, and because laziness now depends on other files, it checks every name, not only the dirty items' names, for a flip. The loader installs the stubs and sets `ProjectFiles` only for a strict package, so non-strict loads never parse the stubs. The loader cannot read `ry.toml`, so each host passes strictness in with `Loader::set_strict`: the GNU R hosts read the planned root's config, and ry's runtime gets the project's through the `RUN_FLAG_STRICT` bit of `ry_run`, because libR links its own copy of `loader` and no process-global registration reaches it. Classification resolves callees through the project-wide definition maps. Their conditional-writers half skips `name <- function(...)` items without naming them, since a function body cannot write a top-level name, so strict planning names only the few non-function items and plans faster than a non-strict load (on finbench, about 0.8 s against 0.9 s, and a full load about 150 ms faster). A `<<-` anywhere in a package function body (`top_level::item_super_assigned_names`, a scan of the item's flat expression arena) counts as a writer of its target placed after every top-level statement: the namespace locks once loading finishes, so such a write can only land during `.onLoad` or something it calls, and counting every function over-approximates that call graph without building it. A bare stub call takes its purity from the namespace of the name's winning declaration (`StubLibrary::declarations`), the same one its type comes from, instead of a flat name set. A replacement inside `local()` of an outer name is accepted when the outer binding is a literal or `c()`/`list()` of literals (`item_binds_plain_value`), because R replaces in a local copy; reading the outer constant's inferred type would put type inference on the load-planning path.
- **Lazy R6 generators.** `name <- R6Class(...)` (bare or `R6::`) is bound lazily in any file, not only strict ones, when its arguments are literals, names, functions, and `list()` or `c()` of them (`top_level::TopLevel::Generator`, `generator_part`). The rule is syntactic on purpose: non-strict loads install neither the stubs nor `ProjectFiles`, so a purity-based rule would classify differently under `ry load` and `ry check`. Unlike a strict constant, whose laziness counts every name its value mentions because a pure function it calls may read them, a generator counts only its immediate reads (`IndexedItem::reads`): its methods read the namespace when they run, lazy or not, and R6 already evaluates `inherit` late. A general purity rule for non-strict constants was rejected, because a pure function such as `toupper` still reads the locale, which an earlier eager statement in a non-strict file may have changed. On the finance code base all 199 generators stay promises after load, and the load on an index-cache hit drops from about 0.4 s to about 0.29 s.
- **Impact.** Loading a strict package binds names and registers classes only, so evaluation order between constants no longer matters and a reload reruns less. Incrementality holds: `test_incremental.rs` pins that adding `cat()` to a pure function re-derives only that function, and removing a marker re-classifies only its callers.

## Cold-check constant costs: one green walk per file, immortal interning, high durability

**Status:** built, except the salsa-interned `Name` and `Ty`, which "Types and names are hash-consed outside salsa" supersedes, and `item_frame_binders`, which `item_links` replaces ("Every host resolves names through one path, and trees live in the host's store").

- **Previous shape.** Each per-file syntax collector and the syntax lints walked the red tree
  separately. `annotation_rule_diagnostics` rebuilt the project's duplicate-type map for every
  annotation. An item check looked up each frame binder through a tracked query keyed on
  `(item, name, deferred)`, and `package_name`/`test_name` took the project file list as an
  argument. `Name` and `Ty` were interned with salsa's default revision tracking, and the CLI
  created inputs at low durability.
- **Weakness.** All of this was per-file or per-name constant work: red nodes built only to be
  discarded, an interned multi-argument key and a memo verification per name read, last-use
  atomics shared across cores on every type intern, and a deep verification of every syntax-pass
  memo once the attached namespaces were set.
- **Target shape.**
  - `file_syntax_sites` collects every per-file site in one pass over the nodes, and
    `lints::syntax_lints` scans the elements in preorder with borrowed cursors, making an owned node only where
    `may_lint` says a lint can fire.
  - The duplicate-type map is read once per file. `item_frame_binders` resolves an item's frame
    binders in one query, cached in `SalsaGlobals`.
  - `Name` and `Ty` are never garbage-collected. That costs nothing in a one-shot check. In the
    server the tables grow with the distinct types and names ever seen, which repeat heavily
    because types are structural. They later left salsa entirely ("Types and names are
    hash-consed outside salsa").
  - The CLI creates every input at `Durability::HIGH` except `PackageMetadata.attached`, the one
    input it sets after analysis starts.
- **Impact.** About 26% less CPU, 30% less wall time and 70 MiB less peak memory on the
  benchmark code bases, with byte-identical findings. Invalidation is unchanged, because every new
  query is per file or per item; the one server-visible difference is that the `Name` and `Ty`
  tables never shrink.

## Types and names are hash-consed outside salsa

**Status:** built, with the storage superseded: a type is an id into columns, not a pointer to an
entry in a sharded table ("Types and symbols are four-byte ids into append-only
columns"). Hash-consing outside salsa and the reasoning for never sweeping stand.

- **Previous shape.** `Ty` and `Name` were `#[salsa::interned]` structs. Every type construction went
  through salsa's interner (a shard lock plus an id-to-slot lookup), every `ty.kind(db)` resolved an
  id back to its slot, and `type_size` was a tracked query keyed by the type.
- **Weakness.** Types are built and read per expression, so this salsa bookkeeping was the largest
  constant cost in item checking (`Ty::intern_id` alone was 4% of samples). It bought nothing: types
  carry no revision-dependent state, and the immortal-interning decision above already gave up
  collecting them. The `SalsaGlobals` name cache existed only to dodge the interner.
- **Target shape.** `Ty` is a `&'static` pointer to a leaked `TypeEntry { kind, size }` in a global,
  64-way sharded `hashbrown` table, with pointer equality and hashing. `Name` wraps a `Symbol`.
  Both keep the `Ty::new(db, kind)` and `ty.kind(db)` API, so no inference rule changed. The size is
  computed once at construction. A tracked function keyed by one name takes `QueryName`, a
  salsa-interned wrapper, because salsa requires a salsa struct as a single key. The name cache is
  gone.
- **Impact.** On the benchmark code bases, about 12% less CPU and 15-18% less wall time on the cold
  check (with the parallel `NAMESPACE` parse and borrowed token scan of the same change), and
  17-21 MiB less peak memory, with byte-identical findings. Invalidation is unchanged: a type has no
  inputs, so memos that hold one compare it by pointer exactly as they compared ids. The table is
  process-global, so two databases in one process share it, which is harmless because entries are
  immutable and structural.
- **Why the table is never swept.** The type universe saturates under real editing. `TyKind` has
  no literal variant, so changing a literal's value builds no new type. In 5,000 `ry server` body
  edits on the finance benchmark, RSS levels off at about 373 MB whether the edits change
  literals or add one new record field name each time. Only edits that keep inventing names grow
  memory, by about 240 B per new name (main's salsa-interned types took about 380 B), which is
  about 25 MB over 100,000 such edits. Add revision-scoped sweeping only if a workload shows growth
  past that.

## The checker presumes R code is correct where R's semantics leave the question open

**Status:** built.

- **Previous shape.** Inference treated every use as a demand. A formal read on any path was required at every call, a field a list literal lacks read as `NULL` (so `opts$force` on `list(spec = ...)` was a finding), `list()` was a closed empty tuple whose reads were errors, a vector partner in arithmetic pinned its operand to a scalar, a callback called only with forwarded `...` got the arity of its first use, and a `pkg::name` read of a shipped package was "unknown" until something attached it. Run over the GNU R corpus with typing on, this gave 3,112 errors on code that CRAN checks, essentially all false.
- **Target shape.** Each rule now asks what R does at run time and refuses only what R itself would fail on, or what is a near-certain typo:
  - A formal is required only when every path forces it (`collect_branch_uses` returns the forced set, `join_forced` intersects branches), and a same-name argument to a project function (`inner(finally = finally)`) leaves the decision to the callee.
  - A name a record-like list lacks reads `Unknown` with a strict origin, because R lists are open and code adds fields later; a near miss of a field is still an error. An empty `list()` reads `Unknown`, or contributes `NULL` inside a union, which is what the field-by-field accumulator idiom needs.
  - Logical widens under the numeric constraint (`widen_numeric_binding`), and a vector partner leaves the flexible operand open with a `double[]` result.
  - A callback called with nothing but forwarded `...` is `fn(...: Any) -> T`.
  - `pkg::name` on a shipped namespace is accepted without attaching it, since R's `::` loads without attaching.
  - A call to an inferred function narrows a union parameter that its return echoes to the argument's own member (`narrow_echoed_parameter_unions`), so `f(x)` with `x: list` does not return `list | vector`. A remaining member that is a variable of the callee alone, such as a getter-or-setter closure's setter value, makes the return `Unknown` rather than leak.
  - A union of lists with different elements, passed where a generic callback consumer expects one list, joins element-wise (`joined_list_union`), skipping `list()` members, because each path's list holds only its own elements.
  - Arithmetic on an operand whose type is still open fits any class with arithmetic methods (`Arith.matrix`, `Ops.Date`), except a method that takes `Any`, like `Arith.data.frame`, and except inside an overload probe, where it would pick `sum`'s `difftime` candidate.
  - A `<<-` write to a top-level variable is read back in the rest of the writing body, never in other items, so one function's cache write does not change what another item reads. The path that skips the write holds the top-level binding (`materialize_pre_state`), except a top-level `NULL`, which is a lazily filled cache's placeholder.
  - Untyped arithmetic does not fit an operator method's parameter (`operator_method_depth`), or `day - 2 * x` would take `-.Date`'s `difftime` candidate.
- **Why `Unknown` rather than `NULL` for an absent field.** `NULL` is precise only when the literal is the whole value; in real packages the literal is a default that callers and later code extend, and a `NULL` type then spreads into `if` and arithmetic as false errors. `Unknown` keeps the read quiet and makes strict mode name it.
- **Impact.** The corpus count fell to 624 (`backlog.md`, "what `ry check` still reports on the GNU R corpus"). Correctness: fewer true positives on literal-only lists and qualified typos of inactive namespaces, both recorded as gaps. Incremental analysis is unchanged: every rule reads the item's own body or per-name projections.

## A computed function that may be `NULL` is called as the function

**Status:** built.

- **Previous shape.** Calling a value typed `fn | NULL` was a `type-mismatch` finding wherever the `NULL` came from, including a local set by a `switch` without a default (testthat's `load <- switch(style, ...)` after `arg_match(style)`) or by an `if` whose other branch is correlated with the call's guard (nloptr's `eval_g_ineq_wrapper`). A union of functions and plain values (shiny's `ns <- NS(namespace)`) was "not a function".
- **Target shape.** `infer_ordinary_call` drops `NULL` beside function members unless the callee names a parameter, and `dispatch_call` checks a union of functions and values against its function members only, with an `Unknown` result. A parameter's `NULL` comes from its default or its declared type, so the caller supplies it and the call is still reported.
- **Why.** The rule in "The checker presumes R code is correct": the `NULL` path of a computed function is one the code presumably rules out, and R fails only on that path.
- **Impact.** Loses the true positive of a `switch` on an unvalidated value. Incremental analysis is unchanged: the rule reads the item's own naming.

## A file's check names only what its interface reaches

**Status:** built, except the per-item `interface_group` walk and `item_interface_reads`: one walk per file finds the groups and `item_links` holds the edges ("Every host resolves names through one path, and trees live in the host's store").

- **Previous source of truth.** Cyclic interface groups came from one project-wide query,
  `interface_sccs`, which built the reference graph over every item of every file and condensed it
  with Tarjan. The conditional-writer map (`conditional_slot_items`) came from
  `item_top_level_names` of every package item, and `globalVariables()` declarations from lowering
  every statement. So the first check of any one file named all 56,546 items of a 300,000-line
  package, and the server's warm-up had to name everything across cores before a file opened at
  startup could get its settled diagnostics (about 0.5 to 0.6 s).
- **What was structurally weak.** Group membership is a property of the items an item reaches, but
  it was stored in a whole-project map, so one file's check paid for the whole project's naming.
  The top-level-writes projection paid a full naming pass to learn, for nearly every item, that it
  binds only its own name, which its syntax already says.
- **Target shape.** `item_interface_edges` is one item's edge list, and `interface_group` runs an
  iterative Tarjan walk from the item over only what it reaches. That is exact, because every
  member of the item's strongly connected component lies in its reachable set. Members come back
  in canonical order, and `scc_exports` is keyed by the first. `file_conditional_writers` screens
  each item on the file's tree (`binds_only_own_name`): only an assignment operator, `for`, `%<-%`
  or `%->%`, or a `setnames`/`setcolorder` call outside a nested function can write a top-level
  name, so an item with at most its own write is skipped without any per-item query. The screen
  may overcount, never undercount, and `testing::check_write_screen` asserts it against naming in
  the fuzz battery. `file_global_variable_declarations` checks the file text for
  `globalVariables` before walking anything. The server's warm-up now derives only the per-file
  folds (`warm_project_folds`) and names nothing.
- **Correctness.** Same groups and same findings: `ry check` reports 2,337 problems on dsbench
  before and after, and the typing, incremental, parallel and fuzz suites pass.
- **Simplicity.** One whole-project query and `file_interface_reads` are gone; the group walk is a
  plain per-item query.
- **Performance.** A cold check of one dsbench file executes `item_naming` 318 times instead of
  56,546, and takes about 0.25 s on one thread after parsing instead of 2.3 s. A file opened at
  server startup settles in 170 to 240 ms instead of 490 to 620 ms on all three benchmarks (4
  cores). A keystroke after a cold prime (`examples/keystroke_probe.rs` on dsbench) takes a median
  of 60 to 74 ms instead of 240 to 290 ms, because no project-wide walk re-runs per edit. `ry
  check` is unchanged within noise, since it names everything anyway.
- **Incremental analysis.** `file_conditional_writers` now reads the file's tree, so it re-runs on
  any edit to that file, which costs one walk of one file and backdates when the writers are
  unchanged. A group's walk re-runs only when an edge list it visited changes.

## Every host keeps every tree

**Status:** built for the server. A command keeps a bounded number of trees in its `TreeStore` ("Every host resolves names through one path, and trees live in the host's store"). Kept for the measurement that salsa LRU capacities serialize parallel reads.

The server still keeps every tree; `ry check` now releases them once (see "`ry
check` releases its trees once, and memoized records are compact" below).

- **Previous source.** `limit_resident_trees` gave `parse`, `file_item_annotations`,
  `item_syntax` and `item_annotation_syntax` salsa LRU capacities in `RootDatabase::one_shot`,
  and `ry check` ran its passes in batches with `trigger_lru_eviction` between them. The server
  already kept every tree.
- **Weakness.** Salsa records each read of an LRU query under one mutex per query. On four cores
  the cold check's reads of `item_syntax` and `parse` queued on those mutexes; a gdb sample of
  the worker threads found them the largest single wait. Removing the item-level capacities
  alone removed the cost and the memory saving together, because item green subtrees are most of
  a file's tree.
- **Target shape.** No query has an LRU capacity and no host evicts. `ry check` keeps its three
  passes (derive from syntax, name, report), each one plain parallel loop (`in_parallel` in
  `crates/ry/src/cli.rs`). Tree readers stay memoized queries with small results, so a check
  never walks another file's tree. The type interner (`crates/semantics/src/types.rs`) gained a
  direct-mapped per-thread cache in front of its 64 shard mutexes, which sampling showed
  contended on hot types. A lookup that takes no lock replaced both ("Types and
  symbols are four-byte ids into append-only columns").
- **Correctness.** Findings are identical on the three benchmark code bases (2337, 1853, 784).
- **Simplicity.** The batching driver, `limit_resident_trees` and the eviction-only comments are
  gone.
- **Performance.** Cold `ry check` went from about 2.2 to 2.5 s to about 1.6 to 1.8 s on the
  benchmark code bases; peak RSS rose from about 485 to 565 MB. mimalloc's remaining time is
  fresh-page setup, and its purge and huge-page options measured no difference.
- **Incremental.** Unchanged: the server never evicted. The remaining waits in the cold check are
  salsa claims on project-wide queries (`duplicate_binding_map`, `project_arithmetic_classes`,
  `project_type_definitions`) that one thread computes while others need them.

## `ry check` releases its trees once, and memoized records are compact

**Status:** built, except the tree release (`release_trees`) and `item_interface_reads`, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- **Previous source.** `ry check` kept every tree to the end, because salsa's LRU records each
  read under one mutex per query. The checker recorded strict-mode origins for every item, and
  the host decided strictness on its own (`file_is_strict` in `crates/ry/src/diagnostics.rs`).
  Memoized records held `BTreeMap`s, `Vec`s and boxed slices, one `Ty` per expression, and
  `item_top_level_names` was a `BTreeSet`.
- **Weakness.** Peak RSS was 536-544 MiB on the benchmark code bases. The trees were dead after
  the syntax pass. Strict origins (about 14 MiB) were built for projects that never read them,
  and the strict setting had two deciders. Most per-item lists are empty, yet an empty `Vec` is
  24 bytes and a boxed slice 16, so `ItemNaming` was 320 bytes and `ItemCheck` 248, times 56,000
  items, and the HIR module's twelve side arrays were 24 bytes each.
- **Target shape.**
  - The tree queries declare `lru = 0`, which records nothing per read. `semantics::release_trees`
    raises the capacity to one, touches every tree on one thread, calls
    `trigger_lru_eviction`, and lowers it again. The CLI calls it after the syntax pass and hands
    each file tree to the naming worker that names that file, which frees it.
    Superseded: a cold check now holds one tree per worker and memoizes none ("A cold check
    holds one parse tree per worker").
  - `AnalysisSettings` carries `[check] strict`, and `semantics::file_is_strict` decides a
    file's mode for the check (the loader's `load_index::file_is_strict` reads the package
    default). The checker records strict origins only when it is true.
  - Frozen per-item tables are `SortedMap`/`SortedSet` (`crates/semantics/src/sorted.rs`) over a
    `ThinVec`, which is one pointer wide and allocates nothing when empty. The same goes for the
    check and naming lists and the HIR module's side arrays.
  - `ExpressionTypes` is a palette of the item's distinct types plus one `u16` slot per expression
    (`u32` past 65,535 distinct types).
  - Interned types carry content flags (`HAS_VAR`, `HAS_RIGID`, `HAS_UNKNOWN`, `HAS_NAMED`),
    computed once at intern time, so a walk skips a type with nothing to rewrite.
  - `file_binders` is keyed by `Symbol`, and `item_interface_reads` looks names up by symbol
    instead of interning a query name for each read.
- **Correctness.** JSON findings are byte-identical on the three benchmark code bases and on a
  full-size typed (strict) finance bench, and both `[check] strict` and a `# typing:` directive
  give the same findings as before on a small project.
- **Performance.** Interleaved runs against main (4 cores): peak RSS went from 536-544 MiB to
  417-419 MiB, the median wall time from 1.41-1.52 s to 1.39-1.44 s, and the median CPU from
  4.7-5.0 s to 4.6-4.8 s. mimalloc's v2 build (489 MiB), its purge, reclaim, retention and
  huge-page options, and a forced `mi_collect` at worker exit measured no saving, or one inside
  the noise.
- **Incremental.** The server never releases trees. `item_interface_reads` reads `file_binders`
  directly, like `item_frame_binders` already did, and still backdates when a file's binders are
  unchanged.

## HIR as struct-of-arrays, lowered from the green tree

**Status:** built, except `Module` owning one vector per array: it holds every array in a single word buffer.

- **Previous shape.** `Module` held `expressions: Vec<Expression { kind, range }>` and
  `patterns: Vec<Pattern>`, and each composite kind owned its lists (`Box<[Argument]>`,
  `Box<[Parameter]>`, `Box<[ExprId]>`, `Box<[MatchArm]>` and the pattern lists), one allocation
  per list. Literal text was a `String`. Lowering walked rowan's red tree, which allocates and
  frees a cursor node for every node it visits, and grew fresh vectors per item, then shrank them.
- **What was weak.** About 60% of `item_hir`'s samples were red-cursor work, allocation was about
  12% of cold-check CPU, and the grow-then-shrink pattern touched about twice the memory the memo
  kept. Owned lists also made every kind non-`Copy`, so readers cloned lists to release a borrow.
- **Target shape (`crates/semantics/src/hir.rs`).** An `ExprId` indexes separate `kinds` and
  `ranges` arrays, because inference reads kinds and mostly only diagnostics read ranges. Each
  list type has one side array in the module and a kind holds a `Span<T>` (`u32` start and
  length) into it; `module[span]` is the slice. Literal text lives in a per-module text buffer that a
  `Text` span addresses (`module[text]` is the `&str`), so every HIR kind is `Copy`. It is not a
  `Symbol`, because the interner leaks by design: interning literals would keep every
  intermediate spelling of a string literal typed in the editor for the life of the server. Children are visited with `Module::for_each_child` and `Module::any_child` instead of a
  per-kind child vector. Lowering reads the tree through borrowed `syntax::NodeRef` cursors, collects each list on a scratch stack and seals it into its side array
  once the list is complete. It runs in a per-thread scratch module that keeps its capacity, and
  the memoized module gets exact-size copies, which is the "scratch arena, owned compact result"
  rule of the data-oriented record above.
- **Names at their own site are one side array, and ids carry a niche.** An argument name, a
  field after `$` or `@`, a loop variable and a tag are a `NameId` into `Module::name_sites`, which
  holds the `Symbol` with its range, instead of a `Symbol` and an `Option<TextRange>` inline in
  every kind. `ExprId`, `PatId` and `NameId` are `NonZeroU32`, so `Option<ExprId>` is four bytes.
  An expression kind went from 28 to 16 bytes and an argument from 24 to 8; together with hashing
  `Symbol` by its index (`Ord` stays by spelling, so sorted output does not move) the cold check
  took 3 to 8% less wall time and about 30 MiB less peak RSS. Read names through
  `Module::name`, `Module::name_range` and `Module::argument_name`.
- **Kept deliberately.** `ExprId` allocation order is the old one, so ids, fixtures and every
  downstream map are unchanged. `Module` still owns plain vectors and compares by value, so salsa
  backdating works as before.
- **Impact.** Findings are byte-identical on the three benchmark code bases. In interleaved runs
  the cold check used 7 to 12% less CPU, 6 to 10% less wall time and 23 to 35 MiB less peak RSS
  than the list-owning HIR. The checker also records expression types in a vector indexed by
  `ExprId` instead of a hash map (`RecordedTypes` in `check.rs`), since the memoized
  `ExpressionTypes` was already dense. IDE scans that need both kind and range go through
  `Module::expressions`, which zips the two arrays.

## A function literal in a call argument is typed in context

**Status:** built.

- **Before.** Every argument inferred before the callee was consulted, so a literal's formals were
  open variables while its body was inferred. `c(x, x)` over an open `x` types as `Unknown`, so
  `List.map(xs, \(x) c(x, x))` passed the `item` constraint and failed only at run time.
- **Now.** `infer_call_arguments_against` (`check.rs`) infers the other arguments first, probes each
  candidate signature with them in a rolled-back snapshot, and hands the literal the expected
  function type's parameter types through `parameter_hints`. The literal is still inferred exactly
  once, because a probe snapshot does not reverse environment or recorded-type writes.
- **All or nothing.** A literal takes hints only when every formal without a default gets one. A
  partial hint broke `Reduce(function(a, b) a + b, xs)`: with `b` fixed and `a` open, `a + b` no
  longer ties the two together and the fold's result stayed generic.
- **Overloads.** A hint survives only where every candidate that accepts the other arguments agrees,
  so the list and Array overloads of `List.map` each hint their own item type.
- **Findings.** An operand error on a hinted formal's use becomes `ContextualParameter`, which names
  the formal and says the call supplied its type, and is reported on the use rather than the whole
  literal.
- **Accumulators get no hint.** A formal whose expected type shares a type variable with the expected result (`S` in `fn(S, T) -> S`) is a fold accumulator, but only when another argument is that bare variable (the initial state); otherwise `List.update`'s `fn(T) -> T` lost its item hint and a field typo in the lambda went unreported. Hinting it with the initial state pinned `List.walk(xs, list(), \(seen, x) List.append(seen, x))` to `list{}`, which the widened result no longer fit. `parameter_hints_for` reads the variables before the probe binds them.
- The spec is "Function literals in call arguments" in `reference/type-system.mdx`.

## Shipped stubs are whatever `types/` holds

**Status:** built.

- **Before.** `stubs.rs` listed every shipped `.Rtypes` and `.exports` file by hand, and a second
  hand list named the conditional namespaces. A stub added to `types/` without editing both lists
  either never shipped or was always active, which made a package's names resolve in projects that
  never use it.
- **Now.** `crates/semantics/build.rs` scans `types/`, embeds every file, orders the stubs (R's own
  packages in search-path order, CRAN namespaces alphabetically, `ry` last, so later sources
  override earlier ones the way R's search path does) and derives `CONDITIONAL_NAMESPACES` as every
  namespace except R's own packages and `ry`. One source of truth: the directory.
- `QUALIFIED_ONLY_NAMESPACES` stubs are gated by activation like conditional ones, so an inactive
  package's `pkg::name` stays a manifest lookup rather than a typed call.

## Override-stub problems are computed after `PackageMetadata` exists

**Status:** built.

- `stub_library` is a HIGH-durability memo. Computed before the `PackageMetadata` singleton input
  existed, it saw no active namespaces and was never re-verified, so a project with any
  `stubs/*.Rtypes` lost every shipped conditional namespace. `cli.rs` creates the metadata input
  first; `check_keeps_declared_packages_active_beside_override_stubs` in `test_cli.rs` pins it.

## Strict mode implies type errors

**Status:** built.

- `[check] strict = true` without `typing` used to report the strict findings but no type errors,
  which made a strict project look clean while its calls did not type. Strict now turns typing on
  (`diagnostics.rs`); `typing = false` beside it no longer turns type errors off. Strict code is
  fully typed by definition, so there is no useful strict-but-untyped mode.

## An all-`Any` overload does not veto agreement among unchecked fits

**Status:** built.

- With an unchecked (`Unknown` or `Any`) argument every candidate fits, and the call is `Unknown`
  unless the fits agree. A catch-all fallback such as `rep`'s `fn(...) -> Any` always disagreed, so
  every overloaded call with one unknown argument was `Unknown`, the largest strict friction in the
  port. `overloaded_call` (`check.rs`) drops all-`Any` fits when a more specific fit exists and
  every specific fit still fits with each unchecked argument replaced by a witness type that only
  `Any` or an unconstrained type parameter accepts (`unchecked_arguments_decide_nothing`), then
  applies the agreement rule. Without the witness test, `levels(unknown)` became `character[]`
  and `rep(unknown, 2)` a vector: the unknown argument was the one choosing the candidate. Rejected: taking the union of the fits' results. It is sound, but it
  turns silence into false operand errors downstream (operators do not dispatch per union member),
  against "the checker presumes R code is correct".
- When the fits still disagree and no unchecked argument is an inferred `Unknown` (each is `Any`,
  or a forwarded `...`, whose element type is `Any`), the call is `Any`, not `Unknown`, as in mypy:
  the `Any` is an opt-out, and opting out of a call's argument opts out of the call. Strict mode
  reports only `Unknown`, so `#: fn(x: Any) -> double` over `sum(x)`, and `utils::head(...)`, are
  clean, while an `Unknown` argument still makes the call a strict finding. A stub's `Any` result
  (`nrow`) was already silent when the stub has one signature, so this makes overloaded and plain
  calls agree. An unannotated parameter is a type parameter, not an inferred `Unknown`, so
  `t(m)` on an untyped `m` is `Any` too; strict mode already reports the parameter itself.
- The cost is that an `Any` result hides a mismatch every candidate would share:
  `#: fn(x: Any) -> character` over `sum(x)` is clean, though each `sum` candidate returns a number.
  Rejected: joining the disagreeing results (`integer | double`), because the union then fails an
  honest `-> double` annotation and operators do not dispatch per union member. Picking a common
  family without a union would be the refinement, if the hidden mismatches turn out to matter.

## A formula is the nominal `formula`, and its operands are not judged

**Status:** built.

- `~` (unary and binary) types as `formula` (`infer_formula` in `check.rs`). Its operands are
  inferred for hover and goto-definition, but their errors and strict origins are dropped, because
  they name model variables (data-frame columns, `.`, `I(x^2)`) that are not bindings in scope.
  Previously `~` was `Unknown`, so every model call and every `dcast(dt, id ~ n)` was a strict
  finding that could not even be annotated inline. Rejected: typing operands as ordinary
  expressions, which would report every column name as unresolved.

## `$`, `[` and `[[` dispatch to a class's subset method

**Status:** built.

- A stub or project method named `$.class`, `[.class` or `[[.class` types the access on a value of
  nominal type `class` (`subset_method_result` in `check.rs`), as R's S3 dispatch would run it. It
  is consulted before the structural read, so an opaque type gets typed field reads without
  exposing a representation (`nominals.R.test`, `a_dollar_method_types_field_reads_on_its_class`).

## A new top-level name costs one project fold

**Status:** built, except that `item_interface_edges` is part of `item_links`, which reads the project name maps once per item ("Every host resolves names through one path, and trees live in the host's store").

- **Before.** A name being typed is a new top-level definition on every keystroke. That re-ran
  `package_definitions` and `duplicate_binding_map`, two folds over every package file's
  definitions keyed by `String`; `item_interface_edges` read the project-wide maps directly, so
  every item whose check was demanded afterwards re-derived its edges; and the unresolved-name
  typo suggestion collected the characters of every project name before checking its length.
  `examples/keystroke_probe.rs` measured 23 to 27 ms per new-name edit on dsbench and finbench.
- **What was structurally weak.** Two folds of the same data, and a per-item query reading
  project-wide maps despite the per-name firewall (`package_name`) built for exactly that.
- **Shape.** `package_definition_index` folds once into the winners and the duplicated names;
  `package_definitions` is a plain borrow of its winners, and `duplicate_binding_map` a tracked copy
  of its duplicates, empty in a healthy project, which keeps the per-file duplicate check firewalled.
  `item_interface_edges` reads `package_name` and `test_name`. Map keys are `Symbol`, whose hash
  is the spelling's FxHash computed once at interning (not the index, so iteration order does not
  depend on which thread interned a spelling first, which the robustness contract forbids from
  reaching output; this replaces an earlier index hash, which saved only the read of the stored
  hash from the symbol table); `Symbol` therefore no longer implements
  `Borrow<str>`, and a lookup by text interns first (`Symbol::new`). The symbol table also stores
  each spelling's character count, which the typo scan filters on before reading any spelling.
- **Impact.** New-name edits take about 4 to 6 ms on dsbench and finbench, body edits about 2 ms
  (unchanged). Pinned by `a_new_definition_rebuilds_no_other_files_interface_edges` in
  `crates/semantics/tests/test_incremental.rs`. The probe's earlier 60 to 74 ms figure was an
  artifact: it edited a digit inside an identifier, which renamed an item, and it entered the
  stubs at `LOW` durability, which made every keystroke deep-verify about 51,000 memos.

## Typed checking costs per name, not per file

**Status:** built.

- **Before.** Each file built `visible_type_definitions`, a merged map of every stub and project type
  definition, so typed analysis paid files times types in time and memory: on typed finbench the
  diagnostics stage took 2.5 s against 0.2 s untyped. Every `#:` block was also lowered again by each
  diagnostic pass that read it (parse stage, dangling annotations, argument lists, agreement,
  declaration sites), and the non-local read classification ran twice, once for the ordinary check
  and once for strict mode.
- **Now.** `TypeDefinitions` (`semantics.rs`) is a `Copy` view over one file that looks a name up in
  that file (scripts, or no `ProjectFiles`), then in the tracked per-name projection
  `project_type_definition(files, name)`, then in the stubs; nothing merges the sets. The unknown-type
  check tests each reference by name and builds the full known set only to rank suggestions after a
  miss. `file_comment_annotations` lowers each `#:` block once per file, keyed by start offset, and
  `comment_annotation` finds one by binary search. `file_unresolved_non_locals` (`diagnostics.rs`)
  memoizes the classification per item, keeping only reads that do not resolve.
- **Impact.** Typed `ry check` on finbench went from 2.43 s / 831 MiB to about 1.9 s / 644 MiB
  (untyped 1.65 s / 533 MiB). The per-name projection also means an edit to one type definition
  re-verifies only the readers of that name.
- **`ry load` reuses `ry check`'s record for its strict verdict**: see "The strict load check shares the check record".

## A conditional write of a formal covers the paths that run it

**Status:** built.

- **Previous shape.** `FormalUses::assigned` held the names written on every path, so a read after
  `if (flag) x <- 1` forced `x`. That contradicted the rule the spec opens with (a formal is required
  only when every path forces it) and reported ks `kdde.binned(..., bin.par = bin.par)`, whose `w`
  is rebound from `bin.par` on the branch the call takes.
- **Target shape.** `assigned` holds the names some path wrote before forcing them, joined by
  union, and a write of a name its path already forced is not recorded. A later read forces the
  formal only when no path wrote it first. Loops and lazy operands keep the writes of the paths that
  run them, and `local()` still discards its writes.
- **Impact.** Reports only omissions that fail on every path, matching the `missing()` narrowing.
  A call that omits the formal and takes the non-writing path is no longer reported; that is the
  accepted cost of a path-insensitive analysis that must not report correct calls.

## `<<-` writes flow forward, and a NULL-initialized cache reads its own first write

**Status:** built.

- **Previous shape.** A `<<-` write joined the enclosing slot's type in place, so the rest of the
  writing body read the join (`cache <<- f(); cache` read `NULL | T`), and a closure write beside
  a reset pinned the callback's return type (memoise's `fun` lost its generic return).
- **Target shape.** The writing body reads the value it just wrote (`write_target`, Super branch).
  The enclosing scope still joins every write through `pending_enclosing_writes`, and an
  undetermined write joins as a union member (`join_closure_write`). On entry to a body that
  superassigns a slot whose enclosing type has a `NULL` member, the slot reads
  `existing | seed`, where `seed` is a fresh variable that the body's first write binds
  (`superassign_seeds`). That models a closure that runs many times: the lazily filled cache
  `if (is.null(cached)) cached <<- dir("css"); cached` reads `character[]`. Only NULL-initialized
  slots are seeded, so a counter's `count <<- count + 1L` still reads the concrete count. A seed
  the body never binds is unified with `Unknown` so it cannot generalize.
- **Impact.** Clears the bslib cache and memoise reports with no new fixture regressions. The
  seed is per body and lives in the inference table, so no cross-item state is added.

## The overload probe's unchecked-argument witness is a nominal no candidate declares

**Status:** built.

- **Previous shape.** An overload probe stands an unchecked argument in as a `Named` type. Its
  name counted as an undeclared nominal, and undeclared nominals are compatible with everything,
  so `months(unknown)` picked the `Date | POSIXct` candidate and returned `character`.
- **Target shape.** The witness has one reserved name, `UNCHECKED_WITNESS` in `infer.rs`, which no
  source annotation can spell, and `undeclared_nominal` excludes it. Only `Any` and an
  unconstrained variable accept it, so a call with an unchecked argument picks no class candidate
  and stays `Unknown`.

## A call on a parameter whose arity the body tests is unchecked, and a later sibling does not force

**Status:** built.

- A parameter's function type inferred from a call (`handler(src)`) is no declared arity. When
  the body reads `formals()` of that parameter, a later call with another argument count is
  unchecked (`arity_tested_by_formals`), because such code picks the call by the callback's arity
  (evaluate `new_source`). Without the `formals()` read an extra argument stays an error.
- `collect_formal_uses` runs before the body's locals are typed. A bare formal passed to a local
  whose first write comes after the call (a sibling defined further down a `local()` block) is not
  forced (`optional_argument_targets`), because that callee decides what it reads (callr and
  processx `format_advice(x)`). An earlier-defined local still forces, as before.
- An inferred `integer` slot accepts a `double` (`fits_inferred_scalar`), since R's
  integer-taking functions truncate one and a first call's `length(x)` pins the shape only
  incidentally. A string still fails. Only an unresolved inferred scalar slot is covered: ks
  `col.fun(n = length(abs.cont) + 1)` still errors, because `col.fun` has a default lambda that the
  join binds, so `n` is a variable already bound to `integer`.
- A `NULL` read is a placeholder only when no ordinary write to the slot so far gave a type other
  than `NULL` (`valued_slots`), so a local narrowed or overwritten to `NULL` is checked. A `<<-`
  write does not count, because sibling closures read the `NULL` until the defining frame joins
  the write (pkgbuild's `now`), and a top-level variable read from a function has no local slot,
  so it stays a placeholder (brew's `.extendedError`).
- A read before a body's first `<<-` can bind the superassignment seed. A first write that then
  fails to unify is reported, except for an occurs failure, which is the accumulator
  `acc <<- c(acc, list(item))` reading itself inside the write.

## Corpus-driven stub and overload fixes

**Status:** built.

- A name the item writes with `<<-` reads as `Unknown` unless the project defines it (`read_non_local` in `check.rs`, via `top_level::super_assigned_names` over the item's own HIR, so no project-wide map is read). Before, openxlsx's Reference Class field `t <<- t[-inds]` resolved to base `t` and indexing it was a false report.
- `grepl`, `grep`, `regexpr` and `gregexpr` take `pattern: character[]` as a second overload, not a union: a union parameter makes an untyped caller parameter infer the union, which then fails at `paste0(collapse = p)` (gt). `sub` and `gsub` keep `character` because no corpus code needed more.
- Overload resolution settles one narrowing disagreement: a scalar narrowing wins over the same atom's vector. A general "most specific by `compatible`" rule was tried and rejected, because it picked list shapes (`list[named: ...]`) that later reads rejected, adding ten corpus false reports.
- `menu`'s `choices` is one union including `list[Any]`, because a list and a character overload disagree and would make an untyped caller `Unknown`.
- The cost of the second `grepl` candidate: `grepl(1L, x)` now reports "no overload matches" over the whole call instead of the argument.

## Strict-friction rules for matrices, unions, `...: Any` and `$` members

**Status:** built.

- `m[i, j]` on a `matrix` types in `matrix_index` (`check.rs`): elements are `Any` because a matrix carries no element type, two vector or empty slots give `matrix`, a scalar slot gives `Any` (R drops dimensions), and `drop = FALSE` keeps `matrix`. Any other named argument falls back to the general rules rather than guessing.
- An operator on a union beside a class dispatches per member, and a member with no class takes the ordinary operator rules (`ordinary_operator_result`). The per-member path applies only when some member is a class and commits only if it adds no error, so plain unions keep their old messages.
- An expected `fn(...: Any)` pads a lambda's parameter hints with `Any` past the declared formals: R would forward any arguments through `...`, so `Any` is the honest type, and `Unknown` would make every callback in plumber or shiny a strict report.
- An annotation covers strict origins anywhere inside its value except inside a nested function literal or braced block (`annotation_covers`), because those have their own bindings to annotate. Before, only an `Unknown` right-hand side as a whole was covered.
- A stub declares an environment member as `Class$member : T` (`is_stub_name`, `infer_field`). This is how polars types `frame$select(...)`; it is looked up before `$.class` dispatch and keyed by the class name, so no new table exists beside the global scheme map.

## Inferred annotations follow the project's annotation style

**Status:** built.

- `ide::inferred_annotations` is the one source of the edits behind the per-binding code action, the whole-file action and `ry check --annotate`, so the editor and the CLI cannot write different annotations.
- With inline annotations on (the default) the type goes where inline syntax puts it: binders after `function` or `\\`, each formal's type after its name, ` -> R` after the parameter list, and `name: T` for any other binding. A right assignment, a quoted name, or a generic type on a value that is not a function literal has no inline place, so it falls back to a `#:` line.
- The whole-file action and `--annotate` write top-level bindings only, because locals carry no interface and annotating each one would bury the signatures; the per-binding action still offers a local.
- `--annotate` is its own flag rather than part of `--fix`: a fix repairs a finding, while an annotation records what inference found and rewrites every signature, which `--fix` users have not asked for.
- A formal that is optional only because the body may never read it (`[value]: T`) also falls back to `#:`: inline syntax spells optionality only as a default, so the inline form would make a caller that leaves the argument out an error.

## The cold check's per-call work: a bump arena, borrowed schemes, a token-only green builder

**Status:** built, except the token-sharing green builder, which "The syntax tree is ry's own flat preorder arena" supersedes.

- **Previous shape.** Argument matching allocated its target and match vectors on the heap per
  call, each item check cloned the stub library's schemes out of a salsa fetch per lookup, the
  `GlobalEnv` lookups took `&str` and re-interned the name at every layer, parsing went through
  rowan's `GreenNodeBuilder` with a per-thread `NodeCache`, and `ry check` read, rendered and
  printed findings on one thread after analysis.
- **Weakness.** Every one of those is per-call or per-token work that repeats millions of times on
  a 300k-line project: allocator churn, scheme clones, repeated interning, and the node cache's
  hashing of every small node to deduplicate it.
- **Target shape.**
  - Argument matching allocates in a `bumpalo::Bump` that lives for one item check
    (`check_item_with_annotation`), and the checker borrows `ItemCheck`s and stub schemes instead of
    cloning them. The stub library is fetched once per check.
  - `GlobalEnv` methods take `hir::Symbol`; `types::Name::new` accepts anything `IntoSymbol`, so a
    caller holding a symbol never round-trips through a string. `hir::Symbol` orders by a cached
    8-byte big-endian prefix before comparing text, so sorted sets of symbols rarely touch memory.
  - `syntax::parser::TreeBuilder` builds green nodes directly from a child stack and shares only
    tokens, through a per-thread hash table plus a per-kind "last token" slot. Small nodes are
    shared again on a cheaper key (see "One-shot checks record coarse dependencies").
  - The lexer has ASCII fast paths for identifiers, numbers and comment bodies, and keyword
    lookup rejects by length and first byte.
  - `ry check` reads sources and renders findings in parallel and prints them in path order, and it
    hashes sources for the cache key only when the cache is read or written.
  - `TypeDefinitions::of`, `read_context` and `SalsaGlobals::new` fetch the stub library, the
    project files and the file kind up front instead of on first use. That records a few
    dependencies a lookup answered file-locally used to skip, so an edit to those inputs
    re-runs slightly more; all three change rarely (a stub or project-layout edit), and one
    fetch per check is cheaper than one per lookup.
- **Impact.** Callgrind on the finance bench (`ry check --no-cache`) went from 24.60 G to
  22.6-22.8 G instructions across runs, with byte-identical diagnostics. In interleaved runs the
  one-core median fell from 4.7 to 4.3 s and the 4-core median from about 1.6 to 1.48 s. The 1 s
  cold-check target is not met, and the rest of the profile is flat across mimalloc, salsa's
  dependency edges, hashing, parse, HIR lowering and naming (`backlog.md`, "Check cache
  follow-ups").

## The item check's working maps live in a pooled bump arena

**Status:** built, except the reads of a green tree, which "The syntax tree is ry's own flat preorder arena" supersedes.

- **Previous shape.** Every `Checker` built a dozen `FxHashMap`s and `FxHashSet`s on the global
  heap (expression annotations, binding types, superassign seeds, read slots and the rest), so
  each item check paid their growth and their frees. Call arguments carried names as `String`,
  S3 method names were built with `format!`, `substitute_rigid` took a hash map and memoized every
  type it visited, and several hot syntax queries (top-level definitions, annotation token scans,
  the duplicate-argument lint) built red nodes and owned strings to answer a yes-or-no question.
- **Weakness.** All of it is per-item or per-call allocation whose lifetime is one item check or
  one query, which is exactly what the data-oriented rule says should come from an arena.
- **Target shape.**
  - The checker's maps are `hashbrown` maps allocated in a `bumpalo::Bump` (`ArenaMap`,
    `ArenaSet` in `crates/semantics/src/check.rs`), through an `Arena` adapter that implements
    std's `Allocator`. Bumpalo's `allocator-api2` impl does not work here, because the JIT turns
    on hashbrown's `nightly` feature and that switches every hashbrown map in a workspace build
    to std's trait; bumpalo's own `allocator_api` feature is no way out either, since it breaks
    Cranelift's register allocator. The arenas live in a per-thread pool; it is a
    stack, not one arena, because an item check can start a nested check (a callee's interface)
    while its own arena is live. An arena is reset, not freed, when its check returns, so a
    thread reuses the same chunks for every item.
  - Call argument names are `Symbol`, overload and namespace lookups take `Symbol`, and S3
    method names are spelled in a per-thread buffer before interning (`method_symbol`).
  - `substitute_rigid` takes a slice of bindings and memoizes only types larger than 64 nodes,
    where sharing actually repeats work.
  - `top_level_definition`, the annotation token scans and the duplicate-argument prescan read
    the green tree directly; the full argument list is built only when a name repeats.
- **Impact.** On the whole finance bench (`ry check --no-cache`), callgrind went from 23.59 G to
  22.68 G instructions and mimalloc calls from 19.78 M to 15.82 M, with byte-identical findings.
  Interleaved wall time on 4 cores moved within noise (median 1.35 s against 1.34 s). Committed
  memory rose about 24 MiB from the pooled arenas and pre-sized resolution caches. The remaining
  allocations are rowan's red and green nodes, salsa's memo bookkeeping and scattered `Vec`
  growth (`backlog.md`, "Check cache follow-ups").

## `$` fields resolve through the values that built the receiver

**Status:** built.

- Before, a `$` field was no navigation target at all: goto, references and rename found nothing.
- Now `ide::fields` follows the receiver back to its constructions: a constructor call (`list`,
  `data.frame`, `DataFrame`, `env`...), a record, an R6 class seen from outside or through
  `self$`/`private$` (following `inherit`), `Class$new(...)`, pass-through calls, the returns of a
  project function, `if` branches, and `receiver$name <- value` writes. The named element is the
  declaration. Depth is capped (`MAX_DEPTH`) to bound a request.
- A receiver that cannot be followed (a parameter) falls back to every same-named declaration in
  the project. That target is imprecise: references list every same-named access, and rename is
  refused, because a guess must never edit code.
- A precise target's references are a closed set: a read that reaches one of its declarations
  brings in every other declaration it reaches (both `if` branches). Rename is refused while any
  same-named read in the project is untraced, because that read might be this field; the same
  holds for a method parameter, whose calls are the field's reads.
- Named arguments that are a constructor's own parameters (`stringsAsFactors`, `key`,
  `.name_repair`) are no fields (`CONSTRUCTOR_PARAMETERS`).
- `Resolver::sources` is memoized per resolution with a cycle placeholder, so a variable
  reassigned from itself costs one pass per assignment, not a power of them.
- A receiver a library function returns has its fields declared by that function, so goto goes
  to the function (stub or installed package) and the hover carries its help page.
- A tag's payload record is not a field declaration, because patterns, not `$`, read it.
- A named argument passed to a project function that forwards its `...` into a constructor
  (`new_rate <- function(...) list(...)`, then `new_rate(pause = 1)`) declares that field for the
  by-name fallback, unless it names one of the function's parameters.
- `object$method(arg = 1)` navigates `arg` to the parameter of the function the precise field
  holds, and references of that parameter include such calls, so rename edits them too.
- Field spellings come from `field_index`, a tracked map built from per-file `file_fields`
  memos, so a references request does not scan every file and an edit re-derives one file's list.
  References on a precise field resolve every same-named access through one `Resolver`, which
  caches each item's field writes, so a common name costs one scan per item, not per access.
  `goto_definition` returns several locations because an `if` can build the value two ways.

## Packages without stubs are read from the R library by the host, lazily

**Status:** built.

- Before, a name that neither the project nor a stub declared had no definition and hovered as
  `Unknown`.
- Now the analysis returns `DefinitionTarget::Package` with the candidate packages, in R's lookup
  order (`importFrom`, `import`, attached packages, then the default search path). The analysis
  never touches the R library, so it stays pure, deterministic and independent of the machine.
- The host (`crates/ry/src/installed.rs`) resolves the target through the `rdb` crate, which
  reads a package's lazy-load database (`R/<pkg>.rdb`/`.rdx`) and Rd help without running R. A
  package is read once, on first use, and its namespace is written as R source under
  `<cache>/ry/packages/<pkg>/<version>/<pkg>.R` so the editor can open it; the server ignores
  that file's open and change notifications so it never becomes project code.
- Base primitives have no R code. Their argument lists come from the closures in `.ArgsEnv` and
  `.GenericArgsEnv`, which the database stores as separate environment records (the index's
  `references` table), and the generated source shows them as `name <- function(args)
  .Primitive("name")`.
- The LSP simulator runs with `installed_packages: false`, because the machine's library would
  make it nondeterministic. No stub is generated from what is read: this is navigation only.
- The host answers through the `ide::Installed` trait (formals, exports), which signature help,
  completion and the import quick fix take; `()` answers nothing, and the fixture harness answers
  for one fake package. An installed package's exports are its `NAMESPACE` `export` names plus
  the `.rdx` index names an `exportPattern` matches. Completion offers them after `pkg::` for a
  package the stubs do not declare, and bare for the packages the project imports whole or
  attaches. The import quick fix looks only in the packages the project already depends on,
  attaches or reads with `::`, so it never scans the whole library.
- Completion ranks a name local to the enclosing function ahead of library names at the same
  match quality, unless a `(` follows the cursor: then it ranks with the globals by use, because
  most locals hold data. Measured with `ry debug analysis-stats`, this raised httr2's
  two-character variable completion from 91% to 95% with no role falling on R6, purrr, httr2 or
  finbench; ranking locals first unconditionally cost call heads (`req |> req_url(...)` lost to
  the local `req`).

## One-shot checks record coarse dependencies

**Status:** superseded by "Every host resolves names through one path, and trees live in the host's store", which removes `Db::one_shot`. Kept for two measurements that still shape the code.

- A walk that records one salsa edge per project item leaves its handle's reused query-stack
  frame sized for the project, and every later query at the same depth then clears a table of
  that size (about 0.75 G instructions on the finance bench). `ry check` therefore builds each
  project-wide name map on a throwaway database handle.
- The global allocator enters mimalloc through `mi_malloc` and friends when the alignment is at
  most 16 and no larger than the size; the `mimalloc` crate always took the aligned path.

## Inferred signatures carry body facts that a call reads

**Status:** built.

- **Previous shape.** An inferred `FunctionType` was its parameters, its return and the `inferred`
  bit. A call could only see the joined return and each formal's optionality, so three things R
  decides from the call were lost: which branch a flag's default selects, which arguments the callee
  captures with `substitute()` instead of evaluating, and which formals another formal's default
  forces. The checker reported `pDist(...) - p` (DistributionUtils), typed withr's `wrap(f, pre)`
  template in the caller's frame, and accepted `g()` for `function(x, y = x) y`.
- **Shape.** `types::FunctionType` gains inferred-only fields, empty on every declared or stub
  signature: `flag_return` (a formal with a literal logical default, never reassigned, that the
  tail `if` tests, plus both branch types), `quoted` (formals read only through `substitute()`, from
  `check::FormalUses`; a bare forward to another call counts as an evaluation), `default_reads` (an
  optional formal that a forced formal's default forces), `missing_tested` (formals tested with
  `missing()`) and `condition_only` (formals read only as a whole `if`/`while` condition, the only
  ones that take logical text, since `!` and `&&` reject strings). `map_child_types` and `for_each_child` walk the branch types, so they
  instantiate and generalize with the signature. The call site reads them: `flag_selected_return`
  returns one branch when the flag is omitted or a literal (a forwarded `...` is assumed not to
  carry the flag, since the opposite reports DistributionUtils' ordinary `pDist(densFn, q, ...)`), `quoted_arguments` types a captured
  argument as `Unknown` without inferring it, and `match_arguments` reports a formal as missing when
  both it and the formal whose default reads it are left out.
- **Why on the type, not a side query.** The facts belong to the function value, which flows through
  bindings, lists and callbacks; a per-item side table would only serve direct calls to a named
  definition and would need its own invalidation. They are derived once from the body that inference
  already walks, so there is no second source of truth.
- **Limits.** An annotation has no syntax for these facts, so a written-back annotation is less
  precise (`test_roundtrip.rs` lists the flag case). Branch selection needs the flag's default to be
  a literal and the tail to be the only return.
- **Related leniencies, no model change.** An apply-family element gets the inferred-scalar leniency
  a direct call gives (`lapply(0:(n - 1L), Eulerian, n = n)`); a string that reads as a logical fits
  an inferred `logical` parameter listed in `condition_only`; and at a `match.fun` caller, a bare
  formal the body also reads as a value is accepted unpinned (`check::reads_as_value`), because
  `match.fun` takes a function or its name and looks a bare name up as a function. A formal used only
  as a function still infers as one, so `function(xs, f) lapply(xs, f)` keeps `f: fn(T) -> U`.

## `Reduce` folds a named function instead of matching `fn(U, T) -> U`

**Status:** built.

- Previous shape: `Reduce`'s stub `fn(f: fn(U, T) -> U, ...)` pinned the accumulator to the start type, so a function whose return is wider (`T | NULL`, a union of list shapes) was reported as "must return `T`", although R simply carries the wider value forward.
- Target shape: `infer_reduce_fold` in `check.rs` calls a directly named function (or an `if` between two names) on `(accumulator, element)` repeatedly, joining each return into the accumulator until it stops growing (bounded at four rounds). Only the last round's findings are kept, unless a round fails, which is reported as is.
- A function that mentions type variables created before the call (a closure over the enclosing function's formals, like purrr's `reduce` wrapper) keeps the stub path, because repeated calls would pin those shared variables to every step. Lambda literals also keep the stub path, which already infers their formals from the body.
- `FunctionType.missing_tested` (inferred only) lists the formals a body tests with `missing()`. `optional_argument_targets` treats such a parameter as one the callee may leave unread even when it has a default, so a missing formal forwarded there (knitr's `kable_format(format)`) stays optional in the caller.
- **Previous shape (facts).** A refinement only narrowed a slot's type, so nothing outside a type could be remembered on an edge: a call like `is_loaded(package)` proved nothing, and two `is.null()` tests on different slots stayed independent. pkgload's `parent.env(ns_env(package))` after `if (!is_loaded(package)) stop()` and pracma's `if (!is.null(A)) b <- -b` after an "A and b both or neither" check were reported.
- **Shape (facts).** `check::Environment` carries `facts: Vec<Fact>` beside its slot entries, with `Fact::Holds(slot, predicate)` (a project predicate tested true) and `Fact::NullTogether(a, b)`. They live in the same undo log as entries (`Undo::Established`, `Undo::Dropped`), so forks and joins treat them like any refinement, and a write to a slot drops every fact that mentions it. `join_writes` drops them for every slot a branch or loop body wrote even when the joined entry is unchanged (a rollback restores the fact, and a same-typed write is still a different value), and a loop counts that as a change so its body is rechecked. An open, `Any` or `Unknown` member of another return counts as possibly `NULL` for `null_unless`. A `NULL` guard on one slot also refines its null partners. `FunctionType.null_unless` (inferred only) records a callee whose only `NULL` return is an opening `if (!pred(formal)) return(NULL)` on a formal it never writes; `null_excluded_return` strips `NULL` at a call whose argument holds `pred`. Facts are not on the type because they are about variables, not values.
- **`FunctionType.reduces`** (inferred only) records a body ending in `Reduce(f, x, init =)` over its formals, possibly through a local `function(a, b) formal(a, b, ...)`, with no early return. `forwarded_reduce` folds the caller's callback through `fold_reduce`, the loop `infer_reduce_fold` uses, so purrr-style wrappers get the fold that a direct `Reduce` gets without the shared-variable hazard above: the callback is the caller's, not a closure over the wrapper's variables.
- **`FunctionType.from_calls`** marks a callback type synthesized from the calls in a body. Its later calls may omit an argument an earlier call passed (fewer arguments, a missing one), because the real callee supplies a default. Making the synthesized named parameters optional was rejected: the type-system contract says an optional parameter in an expected callback type requires the actual callback to have a default, which would report the callers.
- **Bare `NA` returns** (`Checker::bare_na_returns`) take the type of the other returns when all of them, the trailing value included, share one atomic kind; otherwise `NA` joins as `logical`. Only the inferred return is lenient: a declared `-> character` still reports `return(NA)`, so the round-trip test lists the case.
- **Branch joins keep a bounded variable open** when the closed side is not a plain scalar or vector (mgcv's `rho + rho0` against `L %*% rho`), extending the rule that an unconstrained variable is never pinned by the other branch. A bound such as `numeric` does not make a matrix the parameter's type.
- **An `exists("name")` probe** for a function an attached package ships and the project does not define makes the `else` diverge (codetools' `if (exists("emptyenv")) emptyenv() else NULL`). The fallback serves older R versions that ry never presents.

## References are checked against an independent resolver, and a replacement copy is its source

**Status:** built.

- **The oracle shares no code with the analysis.** `crates/ry/src/scope_oracle.rs` resolves names from the syntax tree with R's lexical rules (parameters, locals visible from the end of their assignment, enclosing functions seeing all their locals, `<<-` falling through to package level). A check against the analysis's own resolver would agree with every bug. Where scoping does not decide (formula, quoted code, `:=` target, an argument of a call with no visible function literal, a `<<-` target defined by nothing lexical such as R6's generator environment) the site is undecided and left out.
- **Each `for` loop variable is its own binding.** ry links a loop variable to its loop, an assignment after a loop joins that loop's binding, and a read links to the nearest preceding definition, because each loop overwrites the value. The oracle models the same split; reassignments with no loop between them stay one binding.
- **A replacement assignment's local copy refers to its source.** Previous shape: `the$x <- v` inside a function minted a fresh local slot (R semantics: `the <- \`$<-\`(the, ...)`), and the editor showed it as an unrelated variable, so renaming the global `the` left the function's later uses pointing at nothing. Naming now records `ItemNaming::replacement_copies` (slot to `CopySource`: an enclosing slot or a non-local name), and `ide::target_at`, hover's definition and `occurrences` follow `copy_origin`. The checker still types the copy as its own variable, so typing and narrowing are unchanged.
- **Refused renames are not misses to fix.** A parameter of a function stored in a field whose reads ry cannot all trace (`generator_funs$set`, an R6 `private$` method, `list(set = function(token))`) is refused, because a named argument at an untraced call would break. These are the rename samples left below 100% on the corpora.
- Measured with `ry debug analysis-stats <path>` ("editor coverage" section) on R6, purrr, httr2 and finbench: hover and definition are at or above 90% for every role, references are 100% on all four, and rename is 95.5% (httr2) to 100%, where the rest are refusals of untraceable method parameters. Call hierarchy and workspace symbols, checked against the same resolver, are 99% or more on all four. The misses left are receivers or callees that nothing in the project declares: a field of a parameter no constructor names, `.Call` symbols, and `<<-` targets defined in an enclosing generator.
- A references p99 regression on finbench came from `declaring_item` scanning every file on a miss. `project_declarations` indexes that lookup, and the probe's timing table catches such a regression.

## `if` in NAMESPACE is evaluated by the host, then resolved in text

**Status:** built.

- **Previous shape.** The loader's `NamespaceSpec` put every `if` directive in `unsupported`, and the load warned and ignored it; the checker's `namespace_directives` skipped it too.
- **Shape.** R evaluates conditions, Rust parses directives. `.ry_namespace_conditions` (runtime.R) walks each `if` at top level or inside a top-level `{` as `parseNamespaceFile` does (global environment, only the taken branch, depth first) and encodes the values; every transport carries them as one string (`loader::parse_namespace_conditions`), because the embedded `.Call` routines take only strings. `Loader::read` replaces each `if` with the directives on its taken branches (`metadata::resolve_namespace_conditionals`) before anything parses `NAMESPACE`, so the spec, the diff, the index cache key and the analysis all see the NAMESPACE R would build, with directive order kept.
- **Unknown directives.** Values are keyed by the directive's source text. A directive without values fails the read with `LoadError::Conditionals` before any stamp is taken (a stamp taken for a discarded read would hide the edit from the next one), the host returns `list(unresolved = TRUE, namespace_conditionals =)`, and R evaluates and plans again, repeating while the plan names new directives. The first request of every load sends no values, because evaluating the conditions the last load had would run conditions the file may no longer hold, which GNU R never does; a package with an `if` in NAMESPACE therefore plans twice per load.
- **Checker.** Analysis cannot evaluate R, so it reads both branches, which avoids false unresolved reports for platform-conditional imports.

## Reference Class and R6 method bodies resolve object members in naming

**Status:** built.

- **Before.** A file that called `R6Class` accepted `self`, `private` and `super` anywhere in it
  (`diagnostics::file_defines_r6_class`), and a Reference Class method's fields, `.self` and sibling
  methods were unresolved reads. A sibling method sharing a top-level function's name typed as that
  function, so openxlsx's `getBaseFont()` method call was reported for leaving out `wb`.
- **Shape.** `naming::ref_class_definition` recognizes `setRefClass(...)`, `Gen$methods(...)`,
  `Gen$fields(...)`, `R6Class(...)` and `Gen$set("public", ...)` syntactically, and
  `naming::resolve_item` takes an `is_member(RefClassKey, Symbol)` oracle. A read inside a method
  body (or a closure in one) that has no lexical slot and that the oracle accepts records nothing:
  it is neither a non-local nor a resolution, so diagnostics report nothing and the checker reads
  `Unknown`. R6 methods answer only `self`, `private`, `super`.
- **Membership is a per-name projection.** `semantics::file_ref_classes` indexes one file from HIR
  alone, `project_ref_classes` unions them, and `item_naming` asks
  `project_ref_class_binds(files, class, member)`, so a class edit re-runs only the methods whose
  answers change (`test_incremental`,
  `an_edit_rechecks_the_edited_items_and_the_readers_whose_answers_change`). The index reads
  `item_hir` only, never naming, so there is no cycle.
- **Unknown classes bind everything.** A superclass the project does not define, or a generator the
  project does not bind to `setRefClass`, hides the members, so every name binds rather than
  guessing. The file-level R6 guard stays for helper functions placed in an R6 list by name.

## A literal `source()` path is followed for resolution

**Status:** built.

- **Before.** A script saw only its own definitions and the package's, so every call to a helper it
  `source()`d was unresolved and every helper was unused.
- **Shape.** `semantics::file_source_calls` collects the literal paths of `source`, `sys.source` and
  `base::source` calls anywhere in a non-package file. A `SourcePaths` input maps project-relative
  paths to files; `sourced_files` resolves each literal against the sourcing file's directory, then
  the project root (R's usual working directory), transitively. `sourced_definition(file, name)` is
  a per-name projection layered after the file's own scope and before the test and package layers,
  so the last sourced binding wins and an edit to a helper rechecks only the reads whose answer
  changes (`test_incremental`, `a_sourced_helper_edit_rechecks_only_the_script_reads_it_changes`).
  A sourced file reports no unused assignments (`file_is_sourced`).
- **Position is ignored.** A sourced name is visible anywhere in the script, not only after the
  `source()` call, because a call inside a function or a branch runs at an unknown time.
- **The server loads sourced scripts from disk.** It otherwise holds only `R/` and open documents,
  so `Worker::sync_sourced_files` keeps the loaded scripts exactly the open documents plus what they
  and the package transitively source, dropping a helper nobody sources any more, which keeps a
  session equal to a fresh start. Its read of `file_source_calls` retries on cancellation, because
  the set must be complete before the project file list is rebuilt.
- **Package files' `source()` calls are ignored**, since a package's namespace is its `R/` files.

## A masked argument is its own scope, and the base masking family is typed

**Status:** built.

- **Before.** `subset`, `transform`, `with` and `within` were bare `Any` values in `base.Rtypes`,
  so every call gave `Unknown` and a strict finding. A masked argument resolved in the caller's
  scope, so `within(d, c <- a * 2)` bound `c` at the top level of a package (a conditional
  definition that then shadowed `base::c`) and reported it unused.
- **Shape.** naming pushes a `ScopeKind::Mask` scope around each non-data argument of a masking
  call (the base family and `@masked` verbs) and pre-mints its assignments there; the enclosing
  frame's pre-mint skips those arguments (`syntactic_masking_family`). A write in a mask scope is
  never reportable as unused, because `within` copies the data environment back. `top_level`
  walks a base-family masked argument with fresh locals, as for `local()`. The stubs type the
  results and are `@pure`, because the arguments are walked for effects on their own.
- **Ambiguous overloads propagate `Unknown` silently.** An overloaded call that only an
  already-`Unknown` argument leaves ambiguous returns `Unknown` without a new strict origin. The
  argument's own origin is the report (or it was silenced on purpose, as a masked read is), so the
  old extra origin on the call only duplicated it (`unknown-origins.R.test`).

## `pkg::name` on an inactive shipped namespace checks the shipped export list

**Status:** built.

- An inactive conditional namespace stays out of the stub library, so its names never resolve
  bare reads. `stubs::dormant_namespace_exports` reads the shipped manifest plus that package's
  stub declarations directly from `StubSources`, without activating anything, and
  `diagnostics::namespace_read_message` reports a missing name as "not exported".
- A package with stubs but no shipped manifest is not checked, since partial stubs would report
  real exports. `:::` reads stay unchecked as everywhere.

## A name inside a string's `{...}` is a read for liveness

**Status:** built.

- **Before.** A local used only in a glue or cli template (`n <- length(x); cli_abort("{n} files")`)
  was reported unused: about 280 of the corpus's 3,180 unused warnings.
- **Shape.** naming scans every string literal for `{...}` segments, lexes each with the R lexer,
  and marks the binding each identifier would resolve to as read (`interpolated_names`). It records
  no resolution and no unresolved read, since the string may be plain text, so rename and hover
  ignore template names. It applies to every string, not a list of interpolating functions,
  because wrappers that forward `parent.frame()` (devtools' `ui_*`, waldo's `should_be`) are as
  common as direct glue and cli calls; the cost of a false read is only a missed unused warning.
- `{{` is glue's escape, a `.`-led segment is cli markup whose first name is a style, a `?`-led one
  is pluralization text, and a name after `$`, `@` or `::` is a member.

## An `if` with an `Any` branch is `Any`; a two-path write still unifies

**Status:** built.

- **Before.** `join_types` unified the branch values, and `Any` unifies with anything, so
  `if (is.null(f)) order else f` with `f: Any` typed as `order`'s signature and the `Any` path was
  then checked as a function (sets `as.list.cset`).
- **Shape.** `join_branch_values` returns `Any` when either side is `Any`, matching the union rule.
  `join_written_values` keeps unifying, so a variable written `Any` on one path keeps the typed
  path's type and a later misuse is still reported; applying the rule there too dropped NlcOptim's
  two true positives (a matrix called as a function).

## tidyr and tibble ship masked-verb stubs beside their manifests

**Status:** built.

- `types/tidyr.Rtypes` and `types/tibble.Rtypes` declare only the `@masked` verbs; every other
  export still comes from the `.exports` manifest. Masking is keyed by stub declarations, so a
  manifest-only package could not mask, and `pivot_longer(d, cols = c(a, b))` reported its columns.
  Reshaping verbs return `Any` because their columns depend on the data; row-preserving ones
  (`drop_na`, `fill`, `complete`) keep `<T>`. tidyr re-exports `tibble`, so it declares it too.

## `length(x)` guards drop `NULL`

**Status:** built.

- `recognize_guard` treats `length(x)` as a condition and `length(x) <op> literal` (either side) as a
  test whose edge a zero length cannot reach is non-`NULL` (`non_null_edge`). It only drops `NULL`
  from a union, never shapes an open variable, because a length test says nothing else about the
  value. usethis `if (length(r) != 2) return(...)` then `r[1]:r[2]`.

## A numeric binding yields to a matrix or a generic argument

**Status:** built.

- **Before.** Arithmetic ties its operands to one variable (`a * H` gives `fn(a: T, H: T)`), and the
  first argument bound it, so `weigh(2, m)` reported `expected double, found matrix`, and
  `outer <- function(H) weigh(2, H)` pinned `H` to `double` (ks `mise.mixt`, the shape pins
  through call chains).
- **Shape.** `widen_numeric_binding` extends the existing `f(1L, 0.5)` rebind: a `matrix` absorbs a
  numeric binding (and is absorbed by one already bound to a matrix; the element type is not
  tracked, so a character matrix passes). An open variable or numeric rigid argument meeting a
  variable bound to `double` is only constrained numeric and accepted, leaving the binding at
  `double`, so the caller's parameter stays `<T: numeric>` and the result keeps R's promotion.
- **Rejected:** redirecting the bound variable to the open argument. It dropped the literal's
  `double` (`outer(1L)` typed `integer`) and moved a variable between levels without adjusting
  them, so an outer function's variable was generalized by an inner one.

## A splice reparse refuses a prefix with an unclosed delimiter

**Status:** built.

- **Shape.** `reparse::try_splice` (`crates/syntax/src/reparse.rs`) falls back to a full parse when the old tree reports an unclosed delimiter or unterminated string before the edit, or a missing closer (an error whose message says "to close", as for a `[[` closed by a single `]`, which never says "unclosed"). The closer phrase also matches other recoveries, such as a type argument list's `>`; that only costs a full parse.
- **Why.** Recovery ends such a group early, so a closer the edit adds anywhere after it closes the group in a full parse and reshapes the tree up to it. Files with such an error are already broken, so the fallback costs little.

## Alias cycles are reported at the declaration, and short names get narrow suggestions

**Status:** built.

- **Previous shape.** An alias cycle was found only when an annotation used the alias, and reported on that use with a whole-statement range; an unused cyclic alias was never reported. `nearest_name` returned nothing for a name under four characters, so `person$nam` got no suggestion.
- **Shape.** `alias_cycle_diagnostics` (`crates/semantics/src/diagnostics.rs`) walks each alias the file declares with `check::alias_cycle_walk`, seeded with the alias itself, and reports every alias on a cycle at its declaration. A use of a cyclic alias drops the annotation silently, so one mistake is reported once, where the fix goes. A name of two or three characters is suggested a candidate only when exactly one candidate is that name with one character inserted (`nam` and `nme` find `name`; `df` never finds `dt`), because a substitution budget on a short name matches unrelated names.
- **Impact.** The fix site is the declaration, and the check runs per file over that file's declarations, so it adds no project-wide read.

## The syntax tree is ry's own flat preorder arena

**Status:** built.

- **Previous shape.** rowan's green tree: every node a refcounted heap allocation (`GreenNode::new`
  about 660 instructions per node, plus a per-thread node cache), and every consumer that wanted
  parent links or offsets built red nodes, one more allocation per visited node. Metadata's
  namespace and call scans, `item_declared_pure`, `lower_type`, `statement_annotations` and the
  inline-annotation sites all built red nodes, and a second borrowed walker (`GreenRef`,
  `walk_green`) existed beside the red API only to avoid that cost. Two traversal APIs over one tree
  was the structural weakness.
- **Target shape.** `syntax::Tree` stores one file as struct-of-arrays columns in preorder: `kinds`,
  `starts` (absolute offsets), `ends` (the index one past the subtree) and `parents`, plus the text.
  Indices are dense `u32`s. A node's children are the run between its index and `ends`; the next
  sibling is `ends[i]`. The parser's builder records postorder events and `finish` scatters them
  into preorder in one pass, so building a file is a handful of `Vec` pushes, not one allocation
  per node.
- **Cursors.** `SyntaxNode` and `SyntaxToken` are an `Arc<Tree>` plus an index, so making one is a
  refcount bump; their API mirrors rowan's so the typed AST, the formatter and the IDE did not
  change shape. `NodeRef` and `TokenRef` are borrowed `Copy` cursors for hot walks, and
  `NodeRef::descendants_with_tokens` is a flat index scan. `TokenRef` is a tree and an index that
  reads kind, range and text lazily: eager reads cost about 1 G instructions per cold check. The one cursor model replaced `GreenRef` and `walk_green`.
- **Items.** A `Tree` is a window over shared columns: an `Arc` of the file's columns plus the
  window's first index, its length and its base offset. An item's syntax is `Tree::subtree`, which
  bumps the refcount and records the window, so every accessor reports indices and offsets
  relative to the item and nothing is copied. Equality compares kinds, subtree ends and
  item-relative offsets, which keeps per-item cutoffs position independent without rowan's
  widths-only encoding. A first version copied each item's index range into fresh columns; that
  copied the whole file's columns once more (about 114 MiB on the finance bench, 14 bytes per
  element) and raised peak memory by about 80 MiB, which is why the window shape won.
- **Splice.** `tree::splice` concatenates index runs from the old tree and the reparsed middle,
  rebasing offsets and indices; `enable_invariants()` cross-checks it against a full parse, and
  `syntax::testing` cross-checks every navigation primitive (`token_at_offset`, sibling steps,
  `next_token`, `child_or_token_at_range`) against a naive reference on every fuzzed tree, on the
  root and on nested windows. The splice invariant also compares parents, which equality omits.
- **What was given up.** Token sharing across files and the node cache. An old tree held by a
  window keeps its whole file's columns alive, which is fine because an edit replaces the item
  trees of that file too.
- **Impact.** One source of truth per file instead of green plus transient red copies, no
  per-node allocation, and contiguous columns that a walk reads sequentially. Together with the
  name-lookup, frame-binder and overload-shape changes (the records "One-shot checks record coarse
  dependencies" and "Overload probes reject by call shape first"), a whole-process callgrind run
  of the finance bench went from 23.6 G to 22.5 G instructions and from 19.8 M to 11.1 M `mi_malloc`
  calls; in two sets of seven interleaved runs the wall median fell from 1.35-1.45 s to 1.11-1.15 s,
  CPU from 4.4-4.7 s to 3.6-3.7 s and peak memory from 431 MiB to 371 MiB, with byte-identical
  findings. The windows cost about 0.85 G instructions of offset arithmetic over copied item
  trees but measured faster, because the copies' memory traffic cost more. The 1 s cold-check target is still not met.

## Overload probes reject by call shape first

**Status:** built.

- **Previous shape.** Each overload candidate was instantiated and its arguments matched with full
  type inference before arity was known to fit, and the finance bench ran about 155 k probes for
  17 k overloaded calls.
- **Target shape.** `shape_verdict` in `crates/semantics/src/check.rs` decides arity, overflow and
  missing required arguments from the argument targets alone. `match_arguments` uses it for its
  own arity findings, so the two never disagree, and `probe_overload_candidate` runs it before
  instantiating a candidate's scheme and rejects a candidate whose shape cannot fit. A call that
  forwards `...` gets no shape verdict, and neither does one that names an unmatched argument
  unless an argument also overflows the parameters; those fall through to full matching.
- **Impact.** Rejected candidates no longer pay for instantiation or inference, and the findings
  stay byte-identical because the verdict is the same one full matching reached.

## `[[` by name on a named atomic vector is the element, not `T | NULL`

**Status:** built.

- Previous rule: `c(foo = 1L)[["foo"]]` was `integer | NULL`, treating a named atomic vector like a
  list. R stops with "subscript out of bounds" on an absent name in an atomic vector, so a read that
  returns never yields `NULL`, and the `NULL` member only produced false reports downstream (diffobj
  `symb[["add"]]` into `gsub`). Lists keep `T | NULL`, which R does return.
- Target shape: `extract_result` gives `T` for `T[named]`. The spec (`type-system.mdx`, "`[[` on
  vectors") and the indexing fixtures say so.

## Correlated-path allowances added for the fresh sample

**Status:** built.

Each one trades a narrow class of missed errors for a class of false reports seen on real packages.

- A flag-selected return also accepts branches written with `return()` and an earlier
  `if (flag) return(value)`, when those are the body's only early returns (Matrix and VGAM `iam`).
- `isTRUE()` and `isFALSE()` narrow like `is.logical()`, deliberately loose in the false branch
  (car's `if (isFALSE(fit)) ... else fit$m`).
- When the project defines any `$` method, `$` on an atomic vector is `Unknown`: the project-wide
  fact is a tracked per-file projection (`project_defines_dollar_method`), so an edit that does not
  add or remove a `$.` definition does not invalidate checks.
- In arithmetic, a list member beside a numeric one is skipped, and a list or `NULL` operand beside
  an `Unknown` one is not reported (mgcv offsets, ggplot2 `+`).
- A range endpoint read through a list field drops its `NULL` member (mgcv `b$off[i]:...`); a local
  or a directly indexed value keeps it, as `control__a_length_test_that_admits_zero_keeps_null` pins.
- A guard on `is.x(name <- value)` narrows `name`.
- A bare `NA` local that a closure fills with `<<-` is a placeholder like `NULL` (knitr).
- `seq`'s integer and double overloads come before the Date and POSIXct ones, so a `from` of an
  unresolved numeric type does not pick a date.

## A cold check holds one parse tree per worker

**Status:** built, except `with_held_tree`, `parse_memo`, the per-item syntax memos and `Db::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes: trees live in the host's `TreeStore`, and `RootDatabase::for_command(editor_facts)` replaces both one-shot constructors.

- **Previous source.** `ry check` memoized every file's tree in the syntax pass and then released
  them all at once (`release_trees`, a serial LRU eviction of about 56 ms on finance). Per-item
  `item_syntax` memos and the `file_item_annotations` memo pinned windows of those trees, the item
  check stored editor-only per-expression facts nobody reads in a CLI run, and `sourced_definition`
  interned a query key for every name read even in files that source nothing.
- **Weakness.** Every tree was live at the end of the syntax pass (128 MiB of the heap), so the
  peak was 383-407 MiB of RSS on the benchmark code bases. The release was serial work, and the
  per-item memos and edges were bookkeeping a run that never edits cannot use.
- **Target shape.**
  - `semantics::with_held_tree` parses a file and parks the tree in a thread-local slot that
    `parse` serves while the file's syntax-pass work runs, and the tree is freed when the worker
    moves on. Any other read goes through the tracked `parse_memo`, which keeps that tree for the
    run; a full cold check makes none, while partial cached runs, the cache planner and
    `--annotate` do. `a_one_shot_cold_check_memoizes_no_tree` in `test_incremental.rs` pins it.
  - On a one-shot host `item_syntax` and `item_annotation_syntax` compute their value without a
    memo; on other hosts they go through `item_syntax_memo` and `item_annotation_syntax_memo`, the
    firewalls `test_incremental.rs` pins.
  - `file_item_annotations` stores detached subtrees (`syntax::Tree::detach`), so an annotation
    memo never pins its file's tree on any host.
  - `Db::editor_facts` (false on `RootDatabase::one_shot`) makes an item check leave the
    editor-only facts (expression and pattern types, literal expectations, loop variables,
    selected overloads, callee targets) empty. It is separate from `Db::one_shot` because
    `ry migrate` is one-shot but its rewrites read expression types
    (`RootDatabase::one_shot_with_editor_facts`, chosen by `cli::Purpose`). `match_findings`
    reads types straight from the inference table, so exhaustiveness does not need them.
  - `sourced_definition` returns early for a file that sources nothing; the tracked part is
    `sourced_name_definition`.
  - `TypeScheme::binders` is a `ThinVec`, because one lives per item for the whole run. A one-shot
    check builds an item's frame binders inside its `item_check` instead of memoizing
    `item_frame_binders`.
  - mimalloc's purge delay is 10 ms instead of a second (`crates/ry/src/main.rs`), so the trees
    freed during the run return to the OS before the peak, which is late in the check.
- **Correctness.** JSON findings are byte-identical against main on all three benchmark code bases.
- **Performance.** Interleaved runs against main (4 cores, `--no-cache --output json`): peak RSS
  383-407 MiB to 304-306 MiB on finance, 374-390 to 305-306 on data science, 384-398 to 298-299 on
  biostatistics; wall time about 1.35-1.6 s to 1.18-1.36 s on a busy machine. The heap peak under
  the system allocator went from 305 to 261 MiB before the frame-binder and scheme cuts; what remains is real data (HIR kinds and
  ranges about 54 MiB, salsa edges about 29 MiB, source text 14 MiB, the naming and check memos).
  Instruction counts barely moved (23.4 G to 23.0 G), so the wall gain is less contention and less
  serial work.
- **Incremental.** The server path keeps every memo and firewall; only the tracked names changed
  (`parse_memo`, `item_syntax_memo`, `item_annotation_syntax_memo`), and `test_incremental.rs`
  pins the first two under those names.

## The one-shot check scans kind columns and overlaps its serial phases

**Status:** built, except the one-shot frame-binder path, `project_interface_groups` and the `SalsaGlobals::layer` memo, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- **Previous source.** Whole-file readers (lints, write-token counts, used namespace tokens, the
  syntax sites) visited every element in preorder and tested each one. Item HIR, binder memos and
  per-item syntax flags were separate allocations and separate queries. The lexer scanned comments
  and strings byte by byte, `TreeBuilder::finish` made two passes, the namespace verdict, the
  project name maps and the cyclic group walk ran serially between parallel phases, and
  `classify_non_local_read` fetched the package name maps through salsa for every read.
- **Weakness.** The cold check was about 18.7 G instructions on finance and peaked at 297-306 MiB,
  1-3 MiB under the old 300 MiB target, so one feature would have broken it.
- **Target shape.**
  - `NodeRef::descendants_of_kind`, `NodeRef::elements_of_kind` and `NodeRef::tokens_outside`
    scan the kind column and touch only wanted elements; `tokens_outside` jumps over a pruned
    node's subtree. Lints, `write_tokens`, `collect_used_tokens` (non-trivia only) and
    `file_syntax_sites` use them.
  - Item HIR is one exactly sized allocation, and one `item_syntax_flags` query answers the
    per-item syntax questions; items with no annotation skip the annotation walks.
  - A one-shot check skips the per-item frame binder memo (binders are built inside the item
    check) and keeps rarely set `ItemNaming` and `ItemCheck` fields behind one optional box.
  - The lexer finds comment ends and string delimiters with `memchr2`, and `TreeBuilder::finish`
    computes preorder positions and parents in one backward loop.
  - The namespace verdict runs beside checking, the project name maps are built on throwaway
    database handles between naming and edges, and `project_interface_groups` runs on a cloned
    handle beside the parallel edge pass.
  - `ReadContext` holds the file's package definition and conditional-slot maps, fetched once,
    and `package_scheme_exists_in` takes a probe closure so both paths share one rule.
  - Measured and kept: the per-item `SalsaGlobals::layer` memo. Skipping it in one-shot mode cost
    about 64 M instructions, because `name.symbol()` is dearer than the memo hit.
- **Correctness.** JSON findings are byte-identical against main on all three benchmark code bases
  after every step.
- **Performance.** Finance went from 18.7 G to 17.6 G instructions under whole-process callgrind
  (the typed strict 60K-line package from 3.90 G to 3.73 G). Peak RSS is 222-228 MiB in release
  under the memory-budget test (`cold_check_memory_budget` in `crates/ry/tests/test_perf.rs`, which
  fails above 256 MiB). Wall time is about 1.0-1.05 s on a quiet 4-core machine and 1.05-1.23 s on
  a loaded one, against 1.08-1.36 s before, so the 1 s target is borderline. The profile is flat:
  the parser and tree builder (about 1.4 G), the lexer (about 0.65 G) and spread salsa fetch costs
  are what remain, besides `infer_ordinary_call`.
- **Incremental.** The server path keeps every memo and firewall. The new scans are pure reads of
  one tree, and the throwaway handles share the one-shot database's memos.

## The checker's per-item facts are columns and bitsets indexed by dense id

**Status:** built.

- **Previous source of truth.** `Checker` held 22 hash containers keyed by `ExprId`, `PatId` or
  `BindingId`, nine of them sets, and call matching allocated a `vec![false; n]` per call.
- **What was structurally weak.** An item numbers its expressions, patterns and bindings from
  zero, so every probe hashed a key that was already an array index, and the hottest table
  (`Environment.entries`) was probed on every variable read.
- **Target shape.** `crates/semantics/src/dense.rs` has `Column<K, V>` (one optional value per
  id) and `Bitset<K>` (one bit per id), both in the check arena and both unallocated until the
  first write, because most columns stay empty for most items. The first write sizes a column
  for every id the item minted. The checker's tables and its per-call scratch sets use them.
- **Impact.** Same findings on every fixture suite. A fact is an array slot, iteration is in id
  order (so no result depends on hash order), and the arena frees every column in one step.

## Every host resolves names through one path, and trees live in the host's store

**Status:** built.

- **Previous source of truth.** `Db::one_shot()` chose between two implementations of three
  lookups. The server interned a name and read `package_name` or `test_name`, resolved frame
  binders through `item_frame_binders` and a `frame_binder` memo per unlisted name, and found an
  item's cyclic group with a per-item `interface_group` walk. `ry check` read the project-wide
  maps through `NameLookup`, built the frame binders inside the check, and read one
  `project_interface_groups` walk. Trees had three homes: the tracked `parse_memo` and per-item
  `item_syntax_memo` on the server, a thread-local slot (`with_held_tree`) in `ry check`, and the
  splice cache's copy of the previous tree beside both.
- **What was structurally weak.** One fact had two code paths, so the command and the server
  could disagree, and only an invariants cross-check compared them. `SalsaGlobals` kept four
  hash-map caches in front of the per-name queries, which existed because a salsa read per name
  cost more than the fact. A tree's lifetime was decided in three places.
- **Target shape.**
  - `TreeStore` (`Db::trees`) is the one home of syntax trees, and `parse` is a plain function
    that reads it. It splices on a changed text, and its capacity is the host's only choice:
    every tree for the server, four per core for a command (`RootDatabase::for_command`). A
    command's worker holds the file whose first pass it runs (`TreeStore::hold`), and a held
    tree is never dropped, so the pass parses once whatever the other workers read. The rest of
    the capacity keeps the most recently read trees. Holds alone were rejected: a read of a
    file nobody holds (an item of another file, a rewrite after the check) would parse the
    whole file on every read.
  - `item_syntax` is a plain window of the stored tree. "An item's syntax has a
    stamp, so an edit lowers only the items it changed" says which items an edit lowers.
  - `item_links(item)` resolves every name the item's naming lists against the project name
    maps, in parallel arrays (one row per name, one per binding), and holds the item's interface
    edges and whether its file belongs to the project. It is the item's projection of the
    project-wide maps and backdates unless one of the item's names moved.
  - A name the check constructs (mostly S3 method names such as `+.Date`) goes through
    `package_name`, `test_name` and `frame_binder`. An item of a package file in the project
    skips `frame_binder` for a name the package layer does not bind. Every named item is a
    definition, and every other top-level write is a conditional write, so the package layer
    lists each top-level name a package file binds, and a name it does not list has no binder
    in the file. The verdict costs no query beyond `package_name`. The helper files' layer
    (`test_name`) holds a helper file's names the same way, so an item of a helper file skips
    `frame_binder` on the same test. A test file or a script has no layer over its own names
    and asks `frame_binder`, which reads the binders of its own file.
  - `file_interface_groups(file)` is one Tarjan walk from the file's items, and an item finds its
    group by its position in the item tree.
  - `SalsaGlobals` keeps one column of resolved schemes indexed by link position, because
    resolving a scheme copies it. The other three caches are gone.
- **Rejected.** A project-wide set of bound names, folded from the binders of every file and
  read by `package_name`, answered "bound anywhere?" for every kind of file, and it was built
  first. A file's binders need the top-level names of every item that writes a name besides
  its own, so the fold lowered and named those items in all 1,006 files of the finance bench
  before the first file's check could finish: one package file's cold diagnostics ran
  `file_binders` 1,006 times and `item_hir` 9,905 times, most of them for test and script files
  a package file never sees. It also broke the rule that a check reads a per-name or per-item
  projection, never a fold over the project. A per-file name set read by the check was coarser
  (any new name in the file re-checks its items). A `frame_binder` memo per constructed name
  costs about 100,000 memos (18 MB) on the finance bench, nearly all for names nothing binds.
  Keeping the edges in their own query cost a second memo per
  item for a value derived from the same inputs. One per-file query for all items' links would
  remove the per-item firewall that keeps a body edit to one item.
- **Correctness.** Under `enable_invariants()` every linked lookup is compared with the per-name
  answer, every unbound verdict with `frame_binder`, and every item's group with a walk from the
  item alone. `test_incremental.rs` runs that comparison, for a package file and for a helper
  file, over each way a file binds a name its own check constructs: a definition, a
  conditional write, a chained write, a write inside a call's argument and a write in a loop.
  Each of those cases fails when the verdict ignores the winner or the conditional writers.
  The test also pins the query counts, that a package file's check
  reads the binders of its own file alone and lowers no item of another file, and that a
  diagnostics-only host reports what an editor host reports.
- **Simplicity.** `Db::one_shot`, `NameLookup`, `projected_name_layer`, `project_interface_groups`,
  `interface_group`, `item_interface_reads`, `item_frame_binders`, `parse_memo`,
  `item_syntax_memo`, `with_held_tree` and the `SpliceCache` type are gone.
- **Performance.** On the finance bench (4 cores, `ry check --no-cache`, seven interleaved
  runs of each build), the median wall time is 0.71 s (0.68 to 0.74) against 0.70 s (0.66 to
  0.72) with the two paths, CPU time is 2.61 s against 2.57 s, and peak RSS is 248 MB against
  237 MB. The extra memory is the per-item `item_links` memos. In the server
  (`tools/bench/tooling/lsp_latency.py`, six runs of each build), a body edit and a signature
  edit settle in 3 ms and a new top-level definition in 5 to 6 ms at the median on both builds.
  A file opened on an idle server settles its diagnostics in 13 ms at the median on both
  builds. The file open at start-up settles in 44 ms at the median (38 to 46) against 38 ms
  (35 to 52). "Types and symbols are four-byte ids into append-only columns"
  has the measurements after the memory and the start-up cost were taken back.
- **Incremental.** A new top-level name re-runs `item_links` for every item whose check is
  verified afterwards and re-walks the groups of files that reach a changed edge. Both backdate,
  so the re-check stays with the readers of the name. A file's groups list items, not positions
  in the file, so the value stays equal when a statement is added beside a recursive function.
  An item's links read the project's file list for "is my file in the project" and no fold
  over the project's names, so a name a test or script adds re-runs no package item's links.

## Types and symbols are four-byte ids into append-only columns

**Status:** built, except the per-item store for types that contain inference variables, which
is rejected below.

- **Previous source of truth.** A `Ty` was a `&'static` pointer to a leaked
  `TypeEntry { kind, size }`, found through a 64-way sharded `hashbrown` table with a
  direct-mapped per-thread cache of 1,024 recent entries in front. A kind owned its children in
  `Vec`s. A `Symbol` was an index into a `boxcar::Vec`, found through a sharded map with a
  thread-local map in front. `InferenceTable` kept `resolve_cache`, a table of resolved types
  cleared by a counter that every binding change bumped, and `definition_cache`, a hash map from
  a type name to its definition.
- **What was structurally weak.** Four caches stood in front of two interners because a lookup
  took a shard mutex that the checking threads contended for. A type reference was eight bytes.
  "Does this type contain a variable" was a walk or a cache probe, and it is asked on every
  resolve.
- **Target shape.**
  - `crates/semantics/src/intern.rs` holds the two halves of an interner. `Column<T, CHUNKS>` is
    an append-only column in fixed chunks of 4,096 that never move, so a read is a shift, a load
    and an offset. `IdTable<W>` is an open-addressing table from a hash to an id whose slots
    change once, from empty to filled, and that grows by replacement, so a lookup takes no lock
    and writes no shared memory. Storing a new value takes the table's one lock.
  - A `Ty` is a `NonZeroU32`. The low 27 bits index two parallel columns, the kind and the
    facts derived at construction (size, and the type names it applies). The upper bits say
    whether the type is a variable and whether a variable, a named type, a rigid parameter or
    `Unknown` occurs in it. `TyKind` is `Copy`, with child slices in a bump arena behind the
    writer lock. The 15 leaf kinds and the first 4,081 inference variables fill the first chunk
    as constants, so building one is arithmetic.
  - A `Symbol` is an id into a column of `{ text, hash, first eight bytes, length }`. It hashes
    as its stored spelling hash and orders by spelling. `Symbol::lookup` finds a spelling
    without interning it, which is what a probe with text nobody may have spelled needs.
  - `InferenceTable::resolve` and `substitute_rigid` return on the id's bits when nothing in the
    type can change. Otherwise each keeps a table of finished subtrees for one call, and only
    when the work is large: `resolve` once its walk has taken 64 steps, `substitute_rigid` for a
    type of more than 64 nodes.
  - `InferenceTable::named_types` holds the definitions of the type names an item has asked
    about as two parallel inline lists, searched front to back.
- **Rejected.**
  - A per-item store for types that contain inference variables. Such types leave the item
    that made them: `EditorFacts` carries them, 59 fields of `TypeErrorKind` carry them, and a
    diagnostic renders a variable by name. Moving them out of a per-item store means a copy
    into the shared store at every one of those exits. On the finance bench 696 of the 2,589
    distinct types a check builds contain a variable, so the shared store pays little for
    holding them.
  - Dropping the definition lookup's memo outright. 91% of 1.08 million lookups on the finance
    bench repeat a name the item already asked about, and an uncached answer is two salsa
    fetches and a scan. The memo is sized to what an item asks (a handful of names), not a hash
    map.
  - Freeing entries. The reasoning in "Types and names are hash-consed outside salsa" holds.
    The store holds at most 2^24 types and the symbol store 2^28 spellings, and a full store
    panics.
- **Correctness.** `ry check` prints byte-identical output on the finance bench, and every
  fixture suite passes unchanged. An id is assigned in interning order, which differs between
  runs of a parallel check, so nothing that reaches output may walk types or symbols in id
  order: `Symbol` orders by spelling for that reason.
- **Simplicity.** The per-thread type cache, the thread-local symbol map, `resolve_cache` with
  its epoch, the `boxcar` dependency and the two sharded tables are gone. One table type and
  one column type serve both interners.
- **Performance.** Together with the two records that follow, on the finance bench (4 cores,
  `ry check --no-cache`, three interleaved rounds of seven runs, medians per round):

  | Build | Wall | CPU | Peak RSS |
  |---|---|---|---|
  | Two lookup paths (before "every host resolves names through one path") | 0.81 to 0.91 s | 2.97 to 3.17 s | 239 MB |
  | One lookup path, pointer types | 0.85 to 0.92 s | 3.09 to 3.28 s | 248 MB |
  | Ids, stamps and the tables of the next two records | 0.77 to 0.80 s | 2.81 to 2.91 s | 230 MB |

  The machine ran about a fifth slower than for the numbers in the record above, so compare
  within this table only. Under callgrind a check executes 18.06 billion instructions, against
  19.07 billion with one lookup path and pointer types.
  In the editor on the same project (`tools/bench/tooling/lsp_latency.py`, time until
  diagnostics settle; the start-up open is the median of six interleaved server starts, the
  others are medians within one run of six rounds):

  | Build | Start-up open | Open at idle | Edit in a body | Edit to a signature | Edit to a definition |
  |---|---|---|---|---|---|
  | Two lookup paths | 46.5 ms | 15.9 ms | 5.4 ms | 5.2 ms | 10.7 ms |
  | One lookup path, pointer types | 47.4 ms | 17.4 ms | 3.4 ms | 3.4 ms | 6.7 ms |
  | Ids, stamps and the tables of the next two records | 36.4 ms | 15.9 ms | 3.0 ms | 2.7 ms | 5.8 ms |
- **Incremental.** A type or symbol has no inputs, and its id is stable for the life of the
  process, so a memo that holds one compares four bytes where it compared a pointer.

## An item's syntax has a stamp, so an edit lowers only the items it changed

**Status:** built.

- **Previous source of truth.** `item_syntax` read the item's range from `item_spans` and took
  that window of the file's tree. Every reader of an item's syntax therefore depended on the
  file's spans, which change with every edit to the file.
- **What was structurally weak.** After an edit, every item of the file lowered again, and
  `item_hir` backdated for the untouched ones. The cut-off was an equality test on the lowered
  item, paid per item per keystroke. `TreeStore` was one mutex for the process, and two workers
  that read one file at the same moment both parsed it.
- **Target shape.**
  - The store keeps one `SyntaxStamp` (a `u64`) per top-level node of each file. On a reparse,
    the nodes at the start and at the end of the file whose subtrees equal their counterparts
    in the previous tree keep their stamps, and every other node takes a fresh one from a
    store-wide counter. Equal stamps imply equal subtrees, by an exact comparison.
  - `item_stamp(item)` is a tracked query that returns the stamp. It runs for every item of an
    edited file and backdates for every item outside the edited region. `item_syntax` records
    a read of the stamp alone and asks the store for the node that carries it, through a
    per-file index sorted by stamp. When a bounded store has dropped that tree, it falls back
    to the item's range in a fresh parse.
  - `item_annotation_syntax` and `item_name_range` are plain reads of per-file memos
    (`file_item_annotations`, `item_spans`). `file_item_annotations` holds detached subtrees,
    which compare by content, so it backdates while no `#:` block changes.
  - `TreeStore` has 16 shards by file with a lock each. A parse runs outside every lock, and a
    second reader of a file that is being parsed waits for that tree.
- **Rejected.**
  - The stamp as a field of `item_syntax_flags`. Other items read an item's flags (whether a
    callee is declared pure, whether an item binds only its own name), so they would re-run
    when that item's body changed.
  - A content hash as the stamp. Two different subtrees could share one, and a wrong match
    would keep a stale lowering with no way to notice.
  - Matching moved nodes anywhere in the file. One edit changes one region, so the prefix and
    suffix comparison covers it, and it is linear with no table.
- **Correctness.** Under `enable_invariants()` every stamp read compares the subtree the stamp
  names with the item's node in the file as it stands. `test_incremental.rs` pins the query
  counts for an edit inside an item, a new item above the others, a removed item, the second of
  two equal statements removed, and edits at both ends.
- **Simplicity.** One more tracked query per item. Its memo boxes and dependency edges take
  3.3 MB at the heap peak of a check of the finance bench (50,815 items).
- **Performance.** A cold check does the same work. After an edit to one item of a file,
  lowering, annotations and naming run for that item alone.
- **Incremental.** Edits at both ends of a file between two reads change one region that spans
  the file, so the items between them lower again and `item_hir` backdates. An edit to any `#:`
  block re-runs `item_annotations` for every item of that file, and each comes out equal and
  backdates unless its own block changed.

## The analysis core's per-item and per-file tables after the type store

**Status:** built.

- **Previous source of truth.** `ItemLinks` was an 80-byte value of five allocations: four
  boxed slices (the names, where each name's bindings end, the kinds, the items) and the
  interface edges. Three definitions of "is this file strict" disagreed on the default. `item_links` sorted its
  edges by `Symbol::index()`. `StubLibrary` was nine maps keyed by `String`. The server's worker
  kept `attached_by_file`, a hand-written copy of what `file_attached_namespaces` answers. Tag
  completion and two reference searches walked every file's tree on each request. The checker
  sized two per-body facts to the whole item.
- **What was structurally weak.** `item_links` keeps one memo per item for the whole run
  (50,815 on the finance bench), so every byte of the value's inline size and every separate
  allocation is paid that many times. Interning order differs between runs of a parallel check, so an order by symbol
  index is not reproducible. A copy of a query's answer can drift from the query.
- **Target shape.**
  - `ItemLinks` is two thin vectors: `words` (a header, the listed names, then one word per
    binding holding its name's position above its kind) and `items` (the item of each binding,
    then the interface edges). The memo is two pointers wide and an item without links
    allocates nothing. The names are searched and never walked for a result. The edges are
    sorted by file, name spelling, kind and disambiguator, which is the same on every
    run.
  - `file_follows_strict` is the one answer to "is this file strict".
  - `Item.name` is a `Symbol`. `item_tree_positions` is the one map from an item to its
    position in its file.
  - Only an item that binds more than its own name keeps a memo of the names it binds
    (`item_bound_names`). For every other item `item_top_level_names` yields the item's name.
  - The stub library's collections are `SymbolMap` and `SymbolSet`. A caller that holds a
    symbol probes without hashing text, and a caller that holds text uses `Symbol::lookup`.
  - The server derives the attached set from the per-file scans. Before an edit it reads the
    edited file's scan, as a cancellable read, and recomputes the union only when the scan
    moved.
  - `ide::spelled_names` lists the tags a file spells beside its names, so tag completion and
    the tag and type-name searches open only the files that can match.
  - A function body keeps its superassignment seeds and its callees as short inline lists.
- **Correctness.** The scan of the edited file's `library()` calls ran outside the worker's
  cancellation catch at first. A notification queued behind the edit then cancelled it, and
  the uncaught cancellation stopped the worker (about one run in ten of the server suite under
  load, in `burst_of_edits_settles_on_the_final_text`). Every tracked read on the worker
  outside `Worker::cancellable` has that failure, so the worker's exit message names an
  escaped cancellation.
- **Performance.** Part of the measurements in "Types and symbols are four-byte
  ids into append-only columns". At the heap peak of a check of the finance bench the
  `item_links` memos hold 10.5 MB, where 11 MB was measured before: 2.4 MB of memo boxes (48
  bytes each, down from 112), 4.2 MB in the two vectors, and 3.9 MB of the dependency edges
  salsa records per memo. `item_links` with what it calls is 4.6% of the check's instructions
  (824 million), down from 4.8% (922 million). Both are mostly the cost of resolving every
  listed name in one memo per item, which this layout does not change; the backlog has it.
- **Incremental.** `item_links` backdates as before: its value lists the item's own names and
  bindings and nothing that moves with another item.

## A function has one signature, resolved in `syntax::annotation`

**Status:** built.

- **Decision (orchestrator, on the user's mandate that the two annotation forms are equal).** A function whose signature is written in a `#:` comment, or on the name it is assigned to, runs exactly like its inline-annotated twin: the same boundary checks and the same native compilation.
- **Previous source of truth.** There were two. The checker resolved a function's signature from all three forms (`semantics::annotations::merge_annotations`, `comment_slots`, the definition check's pairing of declared parameters with formals). The runtime (`interp::annotations::register_definition`) read only the `TYPE_ASCRIPTION` and `RETURN_TYPE` nodes on the literal. So `#: fn(x: I64) -> I64` above `function(x) x` was typed for the checker and untyped at run time, and the checker carried a warning (`Uncompiled::DetachedSignature`, `Annotation::inline_signature`) whose only job was to report that the two disagreed.
- **What was structurally weak.** The attachment rule (which `#:` block belongs to which statement), the pairing rule (a formal takes the declared parameter of its name, else the next unnamed one, and `...` the rest type) and the precedence between forms lived in `semantics` only, next to code that needs the type database. The runtime could not call them, because it lowers code the checker never sees (`eval(parse())`, an installed package, a lazy definition) and `interp` does not depend on `semantics`.
- **Target shape.** `syntax::annotation::signature(function, inline)` resolves, from syntax alone, the type node of each formal and of the result. Per slot, a type written on the literal wins; otherwise the function type on the binding (`name: fn(...) <- function(...)`), which replaces a `#:` block as a whole; otherwise the `#:` block attached to the statement, compact or `@param`/`@return`. A refused block, a block whose directives are out of order (`misplaced_directives`, which the checker reports from the same function) and a type that is not a written function type declare nothing. A coercion (`@trust`, `@if-unknown`, `@new`) in the block or on the binding governs the value wherever it is written, as the typing reference says, so a coerced function has no signature at all: the runtime checks nothing on it, and the checker does not judge it native. With `inline = false` (`[check] inline-annotations = false`) only the `#:` block counts. `register_definition` calls it, and the checker's merge, its definition check and its blocker judgement use the same `attached_annotation`, `block_declaration` and `pair_formals`. The checker lowers the types itself; what is shared is which syntax declares which slot.
- **Why syntax and not the checker's resolved signature.** Handing the runtime the checker's scheme would need a serialized signature per function and a checker run before every lowering, and it would leave code the checker never saw without one. Two readers of one syntax-level rule cannot drift, and the one case where syntax is not enough is a signature that is only the name of a type alias (`#: Pick`, `f: Pick <- function(n) n`): the checker resolves it (`check::declared_function` peels transparent aliases), the runtime cannot, and `native::Uncompiled::AliasedSignature` reports it as an `interpreted` warning that asks for the function type in place of the alias. The warning is raised only when an alias was peeled and the written types are incomplete, so a signature that is incomplete for another reason (a parameter name the function does not have, which is a type error) is not blamed on an alias.
- **Where the two readers differ.** The checker drops a block's whole payload on any lowering error (`Lowering::finish`), including errors only it can see, such as an unknown type name. The runtime reads no diagnostics, so it checks the types it recognizes in such a block. Every such block carries an error diagnostic, and strict mode refuses to run the file.
- **Consequence for plain R code.** A `#:` comment is enforced on ry's runtime: a value whose storage type contradicts the comment is an error at the function's boundary, where GNU R (and `ry R`) reads a comment. This is the mandate's equality taken literally, and it applies to `ry check --annotate` output too. The check is first-order and looser than the checker, so it fails only on a value that really does not have the declared type.
- **Where lowering needed the statement.** A lazy package definition lowers the binding's value alone, and the cached-index and kept-source paths lower it from file text. `loader::Code` carries `statement_start`, and `loader_backend::lower_code` parses from the statement's line, extended over the `#:` lines directly above it, with the prefix masked by spaces so byte offsets and line numbers are the file's. A parse error inside a `#:` line does not fail the lowering, because such a line is a comment to R. The console is the other place: `syntax::input_is_unfinished` holds a command that ends in a `#:` block on its own lines until the next line arrives, and a blank line ends the wait, so a block typed before its function attaches as it does in a file.
- **Lazy definitions resolve at compile time.** `typed::lower::resolve_function` forces a loader lazy-definition promise (`ryLoaderForce`), so a native function that calls a not yet forced function of the same package compiles on its first call. The loader binds only function definitions and pure constants of strict files that way, so forcing one early is not observable.
- **Impact.** Correctness: the checker and the runtime agree about what a function declares for every block the checker accepts, and the mandate's two forms count the same in scripts, the console, `ry load` and `ry install`. Simplicity: the warning, its flag and the inline-off special case are gone, and three private copies of the pairing rule became one. Performance: one walk back over trivia per function literal lowered, and comment-annotated functions gain the per-call check their inline twins already pay. Incremental analysis: unchanged, the checker reads the same nodes as before.
- **Pinned by** `typing-strict/native.R.test` (`signature_written_in_a_comment`, `signature_from_a_type_alias`, `signature_alias_is_blamed_only_when_an_alias_is_the_cause`, `signature_in_a_comment_reports_what_blocks_it`), `crates/ry/tests/native/blocked-aliased-signature.R`, `crates/syntax/tests/test_annotation.rs`, `a_console_reads_on_only_when_the_input_ended_too_early`, and the scenario `typed-comment-signatures`.
