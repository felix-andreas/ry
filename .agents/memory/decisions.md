# Decision record: the analysis core is a memoized query framework

The analysis core uses a memoized query framework with automatic,
dependency-tracked invalidation. The substrate is salsa, and `crates/semantics`
is the database.

The alternative was to hand-roll incrementality per structure, with a
reverse-dependency index, an incremental package naming pass, an incremental
type index, and debug-only drift assertions that compared each incremental
result against a full rebuild. That model shipped first and was then replaced.

Four reasons decided it, and they still apply to anything proposed in this area.

- Hand-rolling the invalidation core is empirically error-prone, independent of
  how simple R's dependency graph is. The hand-rolled model produced several
  silent-stale bugs, where analysis served an answer computed from text the user
  had already changed.
- The safety net was structurally incomplete. The drift assertions ran under
  `cfg(debug_assertions)` only, so release-path correctness was unverified. They
  were also blind to a shared-rule bug, because the incremental path and the
  oracle shared the membership predicate, so a wrong rule was invisible to both.
- The real dividing line is automatic dependency-tracked invalidation against
  hand-maintained mirrored invalidation. In a framework the tracked path is the
  default, and an untracked read is the exception. Hand-rolling inverts that, so
  every new read is a chance to reintroduce staleness.
- A framework delivers cancellation, parallelism, and eviction structurally. The
  live-as-you-type model below needs all three, and hand-rolling provides none
  of them without large additional machinery.

Only salsa's core is wanted, which is memoized queries with automatic
invalidation. Its heavier machinery, for macro expansion, a trait and coherence
solver, and a multi-crate graph, is aimed at a deep, dynamic dependency graph.
R's graph is shallow and static, so that machinery stays unused.

`docs/src/content/docs/contributing/architecture.md` is the contract for what
the core does today. Read it before touching the analysis core or the server's
scheduling.

# Decision record: diagnostics are live as you type, not on save

Diagnostics publish while the user types rather than only on save.

rust-analyzer runs two diagnostic streams. It publishes its own type and name
diagnostics live as you type, debounced and cancellable, and it runs
`cargo check` on save for the authoritative full error set. ry has no second
stream, because it is the only static type checker for R. Publishing on save
alone would therefore give a user zero type feedback while typing, which is
strictly worse than what rust-analyzer does.

The interactive queries, which are hover, completion, goto, and signature help,
answer on demand rather than on a schedule.

The architecture page records how the server implements this today, through a
fast first wave and a settled wave at idle time.


# Decision record: the standard library ships as declaration-only stub files

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

`docs/src/content/docs/contributing/authoring-stubs.md` is the contract for the
format, the overload rules, and the export manifests.

# Decision record: the beta semantics

This record holds the core type-system decisions. `backlog.md` holds the prioritized work that
follows from them. The semantics contract is `reference/type-system.md`, and it is updated before
the behavior it describes ships.

## The R variable model is mutable slots with union joins

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

## A multi-member union is for joins and annotations, never for a unification variable

A general union `A | B | C` comes from a join or from an annotation. It normalizes flat,
deduplicated and order-insensitive, and `T | NULL` is a special case of it.

A union imposes no union constraint on an inference variable. That is what keeps inference fast.
Binding a variable to a union value by plain substitution is allowed, like any other type.
Unification stays syntactic, so a union unifies only with a structurally equal union. All
member-wise directional logic lives in `check_compatibility`. Inference therefore stays decidable
and fast, and the split follows the existing rule that unification is the invariant floor.

## A type error inside a test assertion stays reported

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

## The type system is Hindley-Milner, and stays fast to check

This is a user directive. Admit only a feature that is fast to check, which means Hindley-Milner.
Ask one question of any proposed addition. Does it keep inference to unification over the existing
constraint mechanism, decidable and close to linear, with no search and no global solving? If it
does not, it does not go in, however useful it looks. Expressiveness never buys soundness or speed.

Three things are ruled out.

- Type classes and traits are declined rather than deferred.
  `contributing/design/open-questions.md` records the reasoning.
- General subtyping is out, because subtype inference is a different and slower algorithm than
  unification. A declared coercion at a named boundary is Hindley-Milner, and that is the shape any
  variance work must take.
- Any construct that requires backtracking search over a program-wide constraint set is out.

A declaration file is the one sanctioned exception. A `.Rtypes` stub may do something a user's own
annotated code may not, and today that is ad-hoc overloading. The exception is bounded on purpose.
The cost of a non-principal feature is proportional to how much code it applies to, and the stub
surface is a fixed corpus the project maintains. The need there is real. R's base library was never
designed with types, so no principal scheme describes `min` or `abs`, and a gradual checker that
cannot describe the standard library is not usable. A user's `#:` annotation stays pure
Hindley-Milner, which is what keeps the user-facing promise honest.

## An overload set is a bounded, ordered probe

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

## The boundary for R's object systems is dispatch, not Hindley-Milner

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

## `T[]` carries an atomic-element constraint, not a trait

The core vector generalizes to carry an element type, so it can hold a variable. That needs a new
atomic-element constraint kind on an inference variable. It is the same mechanism as the existing
numeric constraint `<T: numeric>`, and it renders as `<T: atomic>`. A trait system is not justified
by this need alone, because the constraint mechanism is already built, proven and fast.

A valid vector element is a `Scalar(_)`, a constrained `Variable(_)`, or `Any` or `Unknown` for an
element the checker does not track. `Constraint` is a lattice merged by `join` rather than by
`Ord`. Numeric meets atomic-element as `ScalarNumeric`, which is a scalar `integer` or `double`,
renders as `<T: scalar numeric>`, and defaults to `double` at a binding boundary like plain
numeric. Annotation lowering records the atomic-element bound directly on the element variable's
entry instead of going through `constrain_type`, because the element may be a rigid `<T>` binder.
The annotation itself makes the promise there, while a function body must not add a bound the
annotation never declared.

## Coercion policy at parameter positions

Three verified false-positive factories are fixed at the compatibility level.

- A whole-number `double` literal is accepted at an `integer` parameter. This generalizes the rule
  `:` already had.
- `integer` widens to `double` at a parameter position.
- A vectorized stdlib stub declares `T[]` parameters, so a scalar coerces up. A scalar parameter
  would otherwise reject a vector.

The old policy punished precision. The more precise a stub was, the more false positives it
produced. `seq_len(10)`, `toupper(c("a","b"))` and `round(x, 2)` all errored, which pressured the
corpus toward `Any`.

## Signature matching is name-aware

An R call site matches arguments by name. Matching an annotation against a definition by flat
position therefore routes a value to a wrongly-typed formal, which accepts code that crashes at run
time. Both the contract and the implementation match by name where names exist.

## Strict mode

Under strict mode an `Unknown` origin is an error, and an unresolved-name reference is an error
rather than a plain naming warning. A recursion-induced `Unknown` return is an origin. Explicit
`Any` remains the sanctioned escape hatch, so an allowed unknown is expressed by annotating it and
never silently.

A `#: @strict` or `#: @strict off` directive at the top of a file overrides the configured default
in both pipelines. The gates are already per file, and the directive is derived from the parse so
incrementality holds.

Refusing an unsupported construct loudly is acceptable policy. Mistyping one silently is not.

## A reported column counts characters, and caret art counts terminal cells

The CLI reported byte columns in the rendered header and in `--output json` alike, and padded the
caret by the same byte count. R source carries non-ASCII text as soon as a string holds a name or a
unit. On such a line the number disagreed with every editor, and the caret sat to the right of the
code it accused, sometimes past the end of the line. A byte column served nobody, because a
consumer would have to re-read the file as bytes to use one, and no editor counts that way.

The two jobs need two units.

- **A column counts characters.** `LineIndex::line_column_chars` produces the JSON `column` and
  `endColumn` fields and the server's `file:line:column` strings. The CLI's snippet source
  re-counts miette's byte column the same way for the report header. This is the number a person
  can act on. The JSON field documentation changed with it, and that is a stated contract, so the
  change is recorded here.
- **Underline art counts terminal cells.** A CJK character or an emoji occupies two cells, so a
  character count would under-pad. The graphical reporter measures cells, so the two units never
  have to be reconciled by hand.

The LSP path is untouched. It converts to the negotiated encoding, UTF-16 by default, at the
protocol edge. That is what LSP specifies, and it is not what a byte column was ever meant to
serve.

## The CLI reports through miette

Every user-facing message the CLI printed used to be assembled by hand. `render_human_diagnostic`
computed a gutter width, sliced the finding's first line out of a `LineIndex`, padded a caret row
by terminal cells, truncated a multi-line range with a dim note of its own wording, and printed a
related location as a one-line `= note: ... --> path:line:col` trailer. A second path styled the
`error: ` and `warning: ` prefixes with `console` and printed an underlying I/O failure with a bare
`eprintln!` on the next line. `ConfigParseError` built its own sentence of the form
`in <path> for <key> at line L, column C`. That is three renderers and three notions of where a
message points. Every improvement meant more hand-rolled layout.

The shape now is one report type in the CLI, drawn by miette's `GraphicalReportHandler`. A report
holds a severity, an optional diagnostic code, a message, an optional cause, an optional snippet
made of a named source and a labelled range, and nested related reports. That is enough for a
finding, a companion note and a bare failure alike, so one reporter draws all of them. An error
that knows its own source implements `miette::Diagnostic` itself. `ConfigError` carries the config
text and the toml span, and the CLI hands it to the same entry point. The look follows the
destination: colour and unicode on an attended terminal, monochrome unicode when colour is refused,
and plain ASCII into a pipe or a file.

The effect on each axis:

- Correctness. The underline is the library's, so a multi-byte or wide glyph is handled in one
  audited place. A related location is drawn from its own file instead of being reduced to a
  coordinate. A configuration failure is shown rather than described.
- Simplicity. The caret, gutter and width arithmetic is gone. `RelatedNote` resolution moved out of
  the parallel worker into the sequential render step, so a worker returns a plain diagnostic.
- Performance. The reporter is built once in a `LazyLock` instead of being re-derived per finding.
  The snippet source is borrowed through a custom `SourceCode` implementation rather than a
  `NamedSource`, which owns its text and would copy the whole file into every finding reported
  against it.
- Incremental analysis. Untouched. This is presentation, and the JSON Lines contract is unchanged.

Two things miette does not do here. It counts the header column in bytes, so the borrowed source's
`read_span` re-counts it in characters. Otherwise the human header and the JSON record would
disagree about the same finding. It also draws every line a range covers, so a range spanning more
than three lines is clamped to its first line with the reach stated in the label. A finding on a
long item must not print the item.

The theme is the project's own, not the stock one. A snippet is a window on the source, not a box.
The gutter runs unbroken down every row, with `vbar_break = vbar` in place of the stock dotted row.
The header opens with a plain rule rather than a corner, with `ltop = hbar`. The range is
underlined with carets, with `underline = '^'`. The closing rule under each snippet is dropped,
which saves one line per finding, and a run of findings prints dozens. The closer is the one part
that is not themeable. miette builds it from `lbot` and `hbar`, and the cause-chain arrows and the
multi-line span brackets draw from those too, so it is filtered out of the rendered string instead.
The filter matches only a line that is nothing but the rule, because a nested report indents its
own in the parent's colour.

The README's hero image is generated, and its renderer had to learn the palette.
`scripts/render-diagnostic-svg.rs` runs `ry check` under a pty and turns the ANSI it emits into an
SVG, because GitHub strips escapes from a code block but honours a `<style>` block inside an
embedded SVG. Its SGR parser matched whole escape bodies. That handled the previous renderer's
single-parameter codes and silently dropped every compound one the graphical reporter emits, such
as `36;1;4` for the underlined locus and `35;1` for the caret. The failure mode is a monochrome
image that still looks plausible, so the render is worth an eye after any change to the palette.
Parameters are applied one at a time now, and cyan, magenta, green, yellow and underline were
added. The snippet-closing-rule width is read off miette 7.6, which hardcodes it.
`check_draws_the_snippet_as_a_window` fails if an upgrade moves it, which was verified by changing
the constant.

## The annotation formatter parses, then pretty-prints

The formatter used to re-indent a `#:` block line by line, with no bracket matching, and it never
split a line. It now joins the block, parses it with the real annotation type parser, and
pretty-prints the result with per-bracket hug bits, honoring `indent_width`. An opener followed by
content on its own line stays hugged, and its closer mirrors it. On a parse failure the block is
left verbatim, which also ends prose corruption and removed the drifted duplicate tokenizer. Both
the fully expanded style and the hugged style are stable fixed points, and a mixed shape
normalizes.

## Config subsystem

The workspace root comes from `InitializeParams` and never from the process working directory.
Discovery finds the nearest configuration file in an ancestor directory, identically in the LSP and
in the CLI. An unknown key is an error with a toml span, surfaced as a diagnostic on the
configuration file plus a window message, and never as a startup panic. A config reload triggers a
diagnostics refresh. One config struct chain runs end to end, replacing four parallel
representations.

## Guard narrowing

Flow-sensitive narrowing is branch-edge entry refinement on the slot model, not a separate flow
analysis. A recognized guard condition computes refined types for the guarded slot's two edges. A
recognized condition is `is.null(x)`, a member of the `is.*` family, or a negation of one. Each
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

## The expanded parameter directive is `@param name {TYPE}`

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

# Decision record: a syntax error does not erase a file

A single error node anywhere in a tree used to short-circuit `lower_with_diagnostics` to an empty
module. One half-typed keystroke therefore dropped the file's whole export set. The package symbol
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
`test_malformed_lower` controls and the `error_tolerant_lowering` project fixtures pin it.
Simplicity: one policy sentence governs every case. Performance: the malformed-flip engine witness
went from two index refolds plus a referrer recheck to no refolds and a recheck of the edited file
only, because exports stay byte-equal across the flip and early cutoff does the rest. Incremental
analysis: typing inside a construct is the highest-churn edit state, and it now has the smallest
blast radius.

# Decision record: durability tiers and a memoized IDE read path

The red-green engine used to validate every memo by deep-walking its recorded dependencies once per
revision. The first read after any keystroke therefore re-walked every unopened file's parse,
lower and naming chain. That is work proportional to the number of files times the chain depth, and
it cost about 108 ms at 281k lines of code across 30k files before any real work started.

Two more costs sat on top. Every hover, definition and completion request rebuilt a package-sized
`NamesGlobal` map from the symbol index. Completion additionally primed every exporting file's
`Lower` and `LocalNaming` to compute item kinds, which is a package-sized prime per
keystroke-completion and cost 95 ms at rest. Separately, the idle-preemption token pairing could
lose a preemption and stall a read behind a full idle unit, because the frontend stored the token
and then sent, while the worker reset it inside the idle unit.

The shape now has four parts.

- **Durability lives in the core**, in `engine.rs`. An input declares `LOW` for an open document
  and `HIGH` for everything else. A memo records the minimum durability its last recompute read,
  and validation greens in constant time when `last_change[durability] <= verified_at`. One
  subtlety is load-bearing. A durability transition must stay truthful even on a memo whose value
  never changes, so the deep-validation walk re-records each visited memo's durability minimum at
  the early-cutoff bump. Without that, opening a file is an equal-value downgrade from HIGH to LOW
  that re-records only the recomputing bottom of the chain. Every value-stable ancestor would keep
  its stale HIGH record, the next keystroke would be invisible to its fast path, and a stale read
  would be served as green. A live probe caught this during implementation, and
  `durability_downgrade_flushes_through_cutoff_nodes` pins it.
- **The winner index is the IDE shape.** The value of `PackageSymbolIndex` is a `NamesGlobal`,
  borrowed per request through a `Shared` clone and never rebuilt. `DefiningItem` projects through
  it.
- **Completion reads one fold.** A per-file `CompletionExports` holds label, kind and callability
  entries that stay value-equal across body edits, and they fold into `PackageCompletionIndex`.
  `generic::completion` takes the entries as an explicit parameter, so the engine passes the memo
  and the from-scratch path builds them per request.
- **Preemption pairing is lossless.** The worker resets `idle_interrupt` before each empty poll and
  the frontend flags after it sends. Every interleaving either delivers the job to the poll or
  leaves the flag set for the unit's next cancellation check.

Correctness: behavior is unchanged, the IDE differential is green, and the downgrade staleness
class is structurally closed and unit-pinned. Simplicity: one new concept in the core, two new
queries, and one deleted per-request synthesis path. Performance at 281k lines of code across 30k
files, given at rest and then after a keystroke: hover went from 5.3 ms to 0.010 ms and from 142 ms
to 44 ms, completion from 95 ms to 26.5 ms and from 228 ms to 90 ms, definition from 5.4 ms to
0.032 ms and from 108 ms to 20 ms. Per-edit diagnostics went from 121 ms to 91 ms and a cold run
from 13.5 s to 9.0 s, which also removed the per-inference clone of the type definition
environment. Incremental analysis: the per-keystroke walk no longer touches an unopened file's
chain at all. The committed witnesses bound an at-rest read to 32 memos and a post-keystroke walk
to 12 per file. `backlog.md` records the deferred lever, which is a durable sub-fold plus an
open-file overlay to drop the fold-edge walk to the number of open files.

# Decision record: `is.null` shapes an unconstrained inference variable

At an `if (is.null(x)) fallback else x` join where `x` and `fallback` are both unbound inference
variables, the join used to unify the two variables. The type model deliberately never invents a
union for a variable, which is a long-standing decision that keeps inference fast. The call
`or_else(NULL, "text")` then bound the single variable to `NULL` from the first argument and
rejected the second.
That is a false positive on the coalesce idiom, and it was recorded as a structural design tension
whose workaround was to annotate.

The guard itself carries the missing information, so the fix is to consume it. In
`condition_refinement`, when the recognized predicate is `is.null` and the guarded local slot
resolves to a completely unconstrained inference variable `A`, meaning entry `Unbound` and
constraint `Unconstrained`, bind `A := T | NULL` for a fresh `T`. The model already supports that
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

# Decision record: data-masked resolution for data.table and the with-family

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
- A read that fails lexical resolution inside a mask is recorded in `NamesLocal.masked_reads` in
  addition to `non_locals`. A masked name still resolves stubs and package globals normally, so a
  `sum` in `j` keeps its scheme. The first design kept masked reads out of `non_locals` and lost
  stub typing, so it was revised. Both could-not-resolve emitters skip masked ids, which are the
  analysis package pass and the engine's per-file query. The typecheck fallthrough for an
  unresolved non-local was already a silent `Unknown` with no strict origin.
- The recognized bracket itself lands in `NamesLocal.masked_subsets` and types as a silent
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

# Decision record: recursion in a function-valued assignment is monomorphic

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

# Decision record: bare-name resolution is not gated on NAMESPACE imports

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

# Decision record: top-level mutual recursion forms whole-file letrec groups

Any top-level recursion, whether self or mutual, used to resolve through the package interface
fixed point with members starting at `Unknown`. Arithmetic and joins over `Unknown` cannot sharpen,
so every recursive function exported an `Unknown`-flavored scheme. An annotated consumer then false
errored, so `#: logical` on `is_even(4L)` reported `expected logical, found Unknown`.

The shape has two deliberate halves.

**A mutual group of two or more members is a letrec group.** A module pre-pass detects candidates,
which are top-level function-valued assignments taking the last writer per symbol. It
overapproximates reference edges by source-range containment and pre-binds every member on a mutual
cycle to a fresh variable one level below module scope. That level is the load-bearing subtlety.
Unification adjusts the group's shared variables up to the placeholders' level, so placeholders at
module level would make finalization's generalize quantify nothing, and the export would carry a
free variable. `import_scheme` on a consuming document erases a free variable to `Unknown`. This
was found empirically, where the producer's table was perfect while consumers saw `Unknown`.
Members stay monomorphic through the module walk, so siblings constrain each other. One
finalization pass then exits the group level, defaults escaping numerics, generalizes each member,
and rebinds both of its environment keys. `is_even` and `is_odd` now export
`<T: numeric> fn(n: T) -> logical`, and a consumer checks against it.

**Pure self-recursion at top level keeps the tolerant fixed point.** A first implementation applied
letrec to a self-loop too, and the real-world corpus immediately caught the cost. The idiomatic
tree fold `if (is.list(x)) sum(sapply(x, sum_leaves)) else x` needs the recursive type
`T = double | list[T]`, which Hindley-Milner cannot express. Monomorphic recursion pinned the
parameter to `double`, and the nested-list call site false errored. The old `Unknown` is
load-bearing gradual tolerance for exactly this shape, so a self-loop is excluded on purpose. An
earlier whole-file variant also made every top-level function monomorphic within the file, which
broke polymorphic reuse such as `mirror(1L); mirror("x")`. That is why the scope is cycles rather
than all definitions.

Fixtures pin all three behaviors and carry the rationale in the file: the typed mutual pair in both
the project and the context-less form, the tolerant self-recursive package function, and the tree
fold staying clean. Local recursion inside a function is typed by the per-assignment letrec. Both
pipelines share `check_module_with_naming`, so no pipeline-specific code was needed.

Correctness: mutual recursion was always `Unknown` and caused consumer false positives. It now
types precisely, with no regressions across any suite or the corpus. Simplicity: one pre-pass, one
finalization and a shared member map. Performance: a per-module candidate scan bounded by arena
size, which is negligible. The backlog records the open remainder, which is strict attribution for
the deliberately `Unknown` self-recursive schemes.

# Decision record: a parse tree is not a memo value

The `SourceText(f)` engine input used to be the parsed document, holding a rope and a tree-sitter
tree, for every workspace file. A `Parse(f)` query projected it. Measured at 302k lines of code
with `ry debug analysis-stats`, which reports per-phase resident-set growth for exactly this kind
of diagnosis, about 398 MiB of the roughly 1 GiB peak was trees retained for files nobody edits.
That is about sixty times the source bytes. The server also parsed the whole workspace
synchronously at load.

A tree is a pure function of the text, so it never lives in an engine value. `SourceText` carries
the rope plus an optional tree. An open document carries the host's incrementally maintained tree,
so a keystroke still never re-parses, and the corpus is rope-only. `Key::Parse` is gone. A
tree-consuming body, such as lowering or lint, and a host alike call `RoughlyQueries::document_for`,
which uses the input's tree or parses on demand into a small LRU of sixteen entries. The LRU serves
an entry only when the cached rope equals the input's rope, so staleness is unrepresentable. A
text-only consumer reads the rope and never materializes a tree, which covers LSP range encoding,
annotation re-lexing and suppression scanning.

A whole-project IDE scan stays fast without resident trees through two `IdeDatabase` seams.
`document_rope` lets an annotation scan re-lex text only. `candidate_document_ids` is a
conservative rope-substring prefilter, because an identifier or S4 string spelled `name` implies
`name` appears in the text. References, rename and S4 navigation therefore parse only documents
that textually mention the target. The engine IDE view primes ropes and candidate trees per
feature, and a point query keeps its constant at-rest prime scope, which a benchmark witnesses.

Two memory fixes landed with it. `LoweringResult` holds its module behind an `Rc` and the engine's
`Lower` projects that pointer through `Stored::from_shared`, so each HIR is retained once instead
of twice. `Expression` boxes its rare attached annotation, which took an arena node from 256 bytes
to 136.

Correctness: unchanged, and all differential suites stayed byte-exact. Simplicity: one query fewer,
and one honest ownership story for trees. Performance: the peak resident set went from 1014 MiB to
294 MiB at 302k lines of code, workspace load parses nothing because parsing spreads into the
background prime, and per-keystroke behavior is unchanged. Incremental analysis: dependencies are
unchanged, because bodies record the same `SourceText` read. The known cost is that the first
workspace-symbols query, and a cold `references` on an extremely common name, re-parse candidates
on demand. The LRU bounds that, and per-file symbol items stay cached across requests.

# Decision record: an all-files fold splits into a durable sub-fold and an open-file overlay

Every all-files fold recorded one dependency edge per package file. That covers the symbol index,
the completion index, declared globals, type definitions, the type index and both candidate orders.
Each keystroke's validation therefore deep-walked every fold, which measured about 11,200 memo
visits per keystroke at 1248 files and grew linearly. The analysis-stats typing probe measured it,
and it now prints per-keystroke recompute counts and walk attribution.

A new `OpenFiles` input holds sorted file ids. It is a defaulted input, so an unset input executes
to the empty set and a headless host or a test needs no change. Each fold splits in two.

- A durable sub-fold runs over non-open files, keyed by `ProjectFiles` position. It reads no LOW
  input, so it greens in constant time per keystroke through the durability fast path.
- The public fold merges the durable half with the open files' per-file values by position. It
  replays path-order last-writer-wins byte-identically.

The split is value-identical for any contents of `OpenFiles`, because a misclassified file merely
lands in the other half. It is therefore a pure performance seam. The host invariant is that
`OpenFiles` lists exactly the LOW-durability `SourceText` files, and the server derives both from
`open_documents` in one funnel. The engine memo table and cycle set hash with FxHash, because query
keys are small integers and the walk hashes twice per visited slot.

Correctness: unchanged. The differential suites cover it, and fold values are byte-identical by
construction. Performance: the per-keystroke validation walk went from 11,244 slots to 278 and is
now size-independent. It is equal at 1248 and 2496 files, and the benchmark witness pins equality
at 100 against 300 files. The keystroke median went from 6.7 ms to 4.9 ms at 302k lines of code
with recompute counts unchanged. Simplicity: seven mechanical durable and public pairs sharing one
pattern. Incremental analysis: an open or close transition is a rare HIGH change that revalidates
once, exactly like the existing durability downgrade.

# Decision record: a same-file backward reference never routes through the interface

A real 697k-line workspace report through `analysis-stats` found this. Typecheck was 95% of a
300-second cold pass, one 170-line file took 4.9 s, and a keystroke in an 18.5k-line class file
took 16 s.

`infer_file` imported an interface scheme for every referenced package global, including a symbol
defined in the very file being inferred, and `InterfaceDeps` edges were file-granular. But the
typecheck walk's winner path rebinds a symbol's global environment entry with the freshly inferred
scheme at its defining assignment. For any reference that first reads after that assignment ends,
the imported scheme was provably shadowed dead weight. Its only observable effect was the
dependency edge, and that edge made every same-file reference chain a fake mutual strongly
connected component. A chain where `h1` calls `h0` and `h2` calls `h1` is the dominant shape of a
real R package. The cost was per-symbol Tarjan over a clique, plus a fixed point that re-inferred
the whole file per round. A 150-line chain file cost 387 ms, and a large hub file scaled worse than
quadratically.

Each referenced same-file-winner symbol is now classified by byte order.

- **Walk-shadowed.** Its last top-level assignment, which is the export and winner site, ends
  before its earliest non-local read starts.
- **Forward.** Everything else: self-recursion inside the defining range, a forward reference from
  an earlier body, and a read between repeated writes.

A walk-shadowed symbol is not imported and contributes no `InterfaceDeps` edge.
`walk_shadowed_definitions` in the engine's query layer decides it, and `infer_file` and
`interface_deps` use it identically. The edge set must mirror the fetch set exactly, or a genuine
cycle would slip past the SCC routing into the accidental-cycle guard. A forward symbol keeps
today's exact interface path, including the deliberate self-recursion tolerance and the
mutual-group letrec semantics. Semantics are unchanged by construction, because a walk-shadowed
import could never be observed: every read happens after the rebinding. Production therefore needs
no change and the differential stays byte-exact. The full suite verified it, and the counter
witness `backward_reference_chain_stays_off_the_interface_scc` pins it.

One related fix landed with it. A script's type-definition environment now overlays its own
declarations on the memoized `PackageTypeDefinitions` clone, through
`TypeDefinitionEnvironment::extend_from_module`, instead of rebuilding from every package module's
view. The rebuild recorded one edge per package file per script, which is a cold cost and a
revalidation walk proportional to scripts times package files. The reporting workspace had 2057
scripts and 503 files.

Correctness: no change, by the shadowed-import argument, and all 36 suites are green. Performance:
the chain-file pathology drops about a hundredfold, from 387 ms to 3.9 ms at 150 lines. A
hub-shaped 59k-line workspace, meaning a 1500-function hub with 300 chained files and 1500 scripts,
cold-passes in 3.8 s. A keystroke in a hub file costs about one authoritative re-inference of that
file. Simplicity: one classification helper and two consumers. Incremental analysis: the
`InterfaceDeps` and `SymbolScc` graphs shrink to genuine cycles, which collapses the per-keystroke
deep validation of interface memos on a hub file.

# Decision record: one inference per file per revision, and a memoized typo hint

Re-profiling the cold pass with `analysis-stats` after the interface-routing fix found this. The
staged diagnostics phase was 66% of a 10-second cold pass at 302k lines of code, and stack sampling
attributed nearly all of it to the could-not-resolve typo hint.

Four pieces of work were duplicated.

1. `Typecheck(f)` and `ExportedSchemes(f)` each ran `infer_file`. That is the same whole-file
   inference, once with expression-type recording for diagnostics and once without for the exports.
   Every file whose exports were demanded paid two full inferences per revision, and a keystroke in
   an open file paid both on its demand path.
2. `unresolved_reference_diagnostic` recomputed the did-you-mean hint per reference occurrence.
   That is a full stub-corpus scan over about 530 candidates, with four fresh `Vec` allocations per
   candidate inside the edit-distance dynamic program. A workspace that references many unmodelled
   library names is the normal case for any codebase using packages without stubs, and it paid a
   multi-second cold cost and re-paid it per keystroke per edited file.
3. `bind_module_letrec_placeholders` computed candidate reference edges by scanning the whole arena
   once per candidate. That is quadratic in file size, on every inference of every file.
4. `Diagnostics(f)` fetched `Typecheck` before the file-local tree readers. Inference's cross-file
   scheme chains then evicted the file's tree from the bounded parse cache and `Lint` re-parsed the
   file, which is a hidden second parse per file on the server's cold prime.

The shape now is this. `Key::Typecheck` stores a
`FileInference { check: ModuleCheck, exports: Shared<Vec<ExportedValue>> }` from a single recording
inference. Recording is collection-only and branches no inference outcome, so folding the exports
run into the recording run is safe by construction. `Key::ExportedSchemes` becomes a shared-pointer
projection through `Stored::from_shared` and keeps its role as the value-equality firewall that
referrers cut off on. When the whole `FileInference` compares equal, which happens on a same-shape
body edit, propagation now stops one level earlier at `Typecheck` itself.

The typo hint splits in two. `unresolved_suggestion` is memoized per unresolved symbol in the query
group, because its value depends only on the name and the set-once stub corpus.
`unresolved_reference_with_suggestion` renders, and both pipelines share it verbatim.
`nearest_name` reuses its dynamic-programming rows and its candidate buffer across the whole
candidate scan. The letrec edge scan is one arena pass with a binary search over the disjoint
candidate value ranges. `Diagnostics` fetches the tree-reading file-local queries adjacently,
before `Typecheck`.

Correctness: unchanged, and all differential and fixture suites stayed byte-exact. Performance: the
cold pass at 302k lines of code went from 10.1 s to 3.7 s, with the package-naming stage from
3.75 s to 0.06 s, one parse per file instead of two, and one inference per file instead of up to
two. The keystroke median went from 5.4 ms to 1.7 ms. Memory: 4 MiB more at 302k lines of code,
because each file's exports are now retained once behind the shared pointer, where the same data
previously lived in the `ExportedSchemes` memo. Simplicity: one inference body instead of two call
modes on the demand path. Incremental analysis: the `ExportedSchemes` seam's dependencies collapse
to one edge, which is `Typecheck`. `analysis-stats` now stages lint adjacently and splits the old
diagnostics phase into lint, package naming with folds, and diagnostics rendering, so the next
regression of this kind is visible at a glance.

# Decision record: the inference-state data model

Callgrind over a 7.5k-line hub file, run after the demand-path fixes, drove this. Inference itself
cost about 25 microseconds per line, and nearly all of it was allocator and tree churn rather than
typing work.

Five weaknesses were structural.

1. `free_type_variables` materialized the resolved form of a type, calling `resolve` on a clone at
   every recursion level, and allocated a fresh set per node. The per-check constraint sweep also
   cloned every recorded expression type into a `Vec` first.
2. The union-find `entries` table was a `BTreeMap` keyed by a densely allocated id, so it paid a
   tree search per resolve step.
3. `bind_module_letrec_placeholders` answered whether a candidate sits on a mutual cycle with a
   transitive walk per candidate, which is quadratic over reference chains.
4. The winner test ran two linear scans per top-level assignment, which are
   `top_level_expression_ids.contains` and `find_exported_binding`, and `exported_value_schemes`
   re-scanned the module per exported symbol.
5. The environment, recorded-type and overload-selection maps were ordered maps whose order nothing
   reads.

The shape now is this. Free variables are collected by a read-only walker,
`visit_unbound_variables`, which follows redirect chains without materializing the resolved form
and mirrors union normalization, so a member resolving to `Any` or `Unknown` absorbs the union.
`entries` is a plain vector indexed by id, as an `EntryTable`. Probe rollback truncates the tail,
and the id counter is the length, so a dangling id is unrepresentable. Letrec mutual-cycle
membership is one iterative Tarjan pass over components of size two or more, and a self-edge alone
never qualifies, which leaves semantics unchanged. `ResolutionContext` carries the precomputed
top-level id set and exported-binding map, and `exported_value_schemes` batch-collects bindings in
one walk. `environment`, `recorded_expression_types` and `selected_overloads` are FxHash maps,
because nothing iterates them in state. The public `ModuleCheck` fields stay ordered maps and are
converted once at assembly.

Correctness: nothing observable changed. All fixture, differential and witness suites stayed
byte-exact, and free-variable ordering is preserved by sort-and-dedup where quantifier order
matters. Performance: whole-file inference on the hub file went from 190 ms to about 50 ms, and a
hub keystroke from 308 ms to 168 ms, measured alongside the demand-path work. Simplicity: one
free-variable implementation instead of two, and one dense table instead of a map plus a counter.
Incremental analysis: unchanged, because all of this is inside one query body. The backlog holds
the remaining follow-up, which is that whole-file inference and re-lowering are still the keystroke
floor for a huge file. The per-definition granularity design addresses it.

# Decision record: interface SCC rounds run per definition, and skip on unchanged reads

A 700k-line user workspace still spent 95% of a 179-second cold pass in typecheck after the
demand-path and constant-factor rounds, with a 10-second keystroke in an 18.5k-line file. A
synthetic reproduction of ten mutually referencing files with sixty chained functions each, which
is 3k lines, cost 3.6 s of typecheck.

Interface edges are file-granular. They must mirror `infer_file`'s import set exactly, or a genuine
cycle slips past the SCC routing into the accidental-cycle guard. Real packages reference each
other's files both ways, so whole file clusters collapse into a single interface SCC.
`resolve_interface_scc` re-inferred every member file per Jacobi round, and rounds grow with the
in-SCC scheme-chain depth. The cost per fixed point is chain depth times cluster size, and it was
re-paid on every keystroke into a member file. Two pieces of bookkeeping were quadratic in cluster
size as well: the per-round full-table clone plus a `render_type_scheme` of every member every round
for the oscillation guard, and a per-symbol `SymbolScc` Tarjan that cloned every visited node's edge
list.

The shape now has three parts.

- **A file that provably decomposes is re-inferred per member definition.** `scc_definition_plan`
  admits a file when every top-level binding is a single-assignment function or a scalar literal,
  so schemes are fixed at their defining site, and when the file has no letrec members, no
  captured-write re-pass, and no other statement writing the top-level frame. Each member
  definition is checked by `check_definition_scheme` against three sources: the round's table for
  in-SCC reads, memoized `GlobalScheme`s for out-of-SCC reads, and locally resolved same-file
  helper definitions. The out-of-SCC reads are provably acyclic, because a cross-file symbol whose
  file reaches back into the cycle is itself a member. The same-file helpers must resolve locally,
  because fetching their `GlobalScheme` would re-enter the fixed point through the file's own
  `Typecheck`. Under the plan's conditions this environment equals the whole-file walk's at every
  read, so results match up to inference-variable identity. An ineligible file keeps whole-file
  rounds, which covers S4 blocks, monotype accumulation and letrec groups.
- **Both granularities skip on unchanged reads.** A unit re-infers in round k only if one of its
  in-SCC reads changed in round k-1, counting reads transitively through local helpers. The round
  function is pure in those reads, so the previous output is reused verbatim and the trajectory is
  identical. Contributions are per file and are merged in ascending file order each round, so a
  symbol exported by several member files keeps the exact last-writer-wins value. A single
  symbol-keyed map let a stale exporter overwrite the winner. The differential caught that, and
  `test_interface_scc.rs` pins it.
- **One inference state serves the whole fixed point.** A definition check snapshots and rolls back
  completely, which keeps per-definition variable ids deterministic. This replaces a stub-seeded
  template clone per definition. The oscillation-guard history records value changes only, because
  a consecutive duplicate render affected nothing. A pure-oscillation pin can therefore land one
  round later, which the round-cap slack covers and which converges to the same table. `SymbolScc`'s
  Tarjan uses dense indices and borrows fetched edge lists.

Correctness: the differential suites stayed byte-exact, including the multi-exporter regression the
first cut introduced. Performance: the cluster reproduction's typecheck went from 3.6 s to 0.2 s
and its member-file keystroke from 359 ms to 34 ms. The hub workspace keystroke went from 168 ms to
136 ms, and a big flat workspace was unchanged. Simplicity: the fixed point gains a planning phase,
and the convergence and pinning contract is unchanged. Incremental analysis: a keystroke into a
cluster file re-runs the fixed point at frontier cost instead of cluster cost.

Three follow-ups are in the backlog. The per-symbol `SymbolScc` is still quadratic for a very large
cluster, and a file-level quotient-graph SCC would fix it. The `InterfaceScc` key carries the member
list, which is heavy for a huge component. The authoritative whole-file `Typecheck` remains the
keystroke floor, which the per-definition incremental inference design addresses.

# Decision record: `ry check` runs on the query engine

The CLI used to run production's from-scratch `Analysis` and `run_full`. That path's whole-file
package-interface loop has the same file-cluster blowup the engine's fixed point fixed, so
`ry check` on a real mutually referencing package stayed slow after the server got fast. Every
future engine performance win would also have needed a production twin.

The CLI now builds the same query graph the server uses. It creates one `Engine` per check target
and feeds inputs in the server's `ProjectFiles` order, which is package files first and then
ascending root-relative path, so last-writer-wins winners are identical. It honors the config as
is. It renders each file through `assemble_engine_file_diagnostics` in `crates/ry/src/diagnostics.rs`,
which holds the class assembly, config gating and type-error rendering extracted from the server so
the two surfaces cannot drift. A suppression applies against the source the CLI read. `run_full`
remains purely the differential oracle, which is the one consumer that must stay engine-independent.

Correctness: the differential already asserts that the engine and `run_full` agree byte-exactly on
rendered diagnostics, so the CLI's output set is covered by construction. All 146 CLI crate tests
pass unchanged, including the full CLI contract suite, and a diagnostic-heavy workspace produces
the exact count the engine stats report. Performance: the CLI inherits every engine property, which
covers per-symbol firewalls, per-definition SCC rounds, memoized typo hints and one parse per file.
The cluster reproduction went from 1.34 s to 0.28 s, and a hub-shaped workspace from 2.95 s to
1.90 s, which is now parse-bound. The gap grows with workspace size. Simplicity: one fast path
instead of two, and one shared diagnostics assembly. Incremental analysis: unaffected, because the
CLI engine is one-shot.

# Decision record: the shipping stack is a hand-written parser, rowan trees, and salsa

`docs/src/content/docs/contributing/architecture.md` describes the architecture as it stands. This
record holds the reasoning behind it and the risks that come with it, so that no session
re-derives either.

## Why the previous stack was replaced rather than tuned

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

## Why rowan rather than a hand-written tree

The parser is hand-written either way. rowan is not a parser. It is the tree data structure the
hand-written parser emits, and nothing about parsing is delegated to it. The green and red design
beats a classic typed AST of structs with spans for four reasons.

- **Losslessness.** Every byte lives in the tree, trivia included, and reprints exactly. A `#:`
  comment is type syntax here, so the formatter, the byte-exact round trip and the annotation
  tooling all come from the representation instead of from side tables.
- **Error resilience.** Every parse yields a tree whose error nodes are local to the break, and the
  typed AST layer returns `Option`, so a consumer never carries a parallel data model for broken
  code.
- **Position independence.** A green node carries a width and no absolute offset, so an untouched
  item's subtree stays structurally equal after an edit elsewhere in the file. That is the property
  per-item cutoffs are built on. An AST that carries spans shifts every span after any edit, which
  kills sub-file incrementality at the root.
- **Structural sharing.** Immutable refcounted subtrees with builder-level dedup of identical small
  nodes keep the resident tree near twice the source bytes.

Use the crate, not an in-house copy of the design. The value is subtle machinery already hardened
over years in rust-analyzer: the thin-DST layout, red cursors with lazy offsets, node caching and
splicing. Hand-rolling reproduces that code without the hardening and gains no design freedom.
rowan is small and dependency-free enough to vendor or fork if divergence is ever needed, and Biome
forked it, which is precedent for both its maturity and the exit hatch.

The acknowledged cost is that rowan traversal is dynamically kinded and slower than direct structs.
That is why inference never walks it. The checker runs on per-item HIR, and rowan serves the
fidelity layers, which are the IDE, the formatter and refactorings.

Value equality is the mechanism, not pointer identity. rowan's green `Eq` is structural with a
pointer fast path, and its node cache dedups only nodes with three children or fewer, so a
from-scratch reparse shares no large subtree. rust-analyzer's barriers are likewise value equality
on its `ItemTree` and `AstIdMap`. A statement-splice reparse of an open document can make those
compares pointer-fast, but it is an optimization only. Parse stays a pure per-file query and
correctness never depends on splicing.

## Why salsa rather than an in-house engine

Two things would otherwise have to be hand-rolled: parallel snapshot reads with write
cancellation, and first-class fixpoint cycles. Both are proven in rust-analyzer and in Astral's
`ty`, whose type inference uses salsa fixpoints. A concurrent red-green memo core is the one
component not worth building in-house. The query decomposition from the in-house engine is the
asset that transferred: per-symbol firewalls, names-only cutoffs, and the durable and open fold
split.

Four salsa risks are recorded so they stay managed.

- **Pin the version and upgrade deliberately.** The public API churns hard and often, with several
  breaking releases a year. Vendoring or forking is the exit hatch, exactly as for rowan.
- **Fixpoint non-convergence is a hard panic at 200 iterations.** The pin-to-`Unknown` rule lives
  inside the cycle function. Reaching salsa's cap is a bug, never a fallback.
- **Interned-value garbage collection is young.** rust-analyzer used about four times the memory on
  its salsa migration until it tuned per-query `lru`, and `ty` hit multi-gigabyte blowups. Per-query
  `lru` and interned GC are the first levers when memory regresses.
- **Parallel iteration over a fixpoint had real hang bugs**, fixed upstream. A parallel-cycle
  stress test covers it.

## Item granularity and item identity

The analysis unit is the item, not the top-level statement. A nested definition is an item too.
That covers a field or method inside a class-constructor call such as `R6Class`, `setRefClass` or
an S4 block, and a function defined inside a function body. Without that, R's common giant
single-statement object-oriented files degenerate straight back to whole-file granularity.

An item's identity hashes its kind and name, plus its parent with an index disambiguator. It never
hashes a bare position or index, so inserting an item does not shift an unrelated item's identity.
This follows rust-analyzer's current `AstIdMap` design. Its earlier index-based design had exactly
the shifting problem.

## An annotation is one concept with pluggable spellings

Annotations are one internal concept, and the surface spelling is pluggable. The seam sits at
annotation recognition during lowering, not in the lexer. `#:` is structured trivia today. R 4.4
ships `declare()` as an experimental base primitive, which is a runtime no-op, and Posit's quickr
already annotates with `declare(type(...))`. A valid-R inline form is therefore ordinary call
syntax recognized at lowering. A true superset dialect, which is the TypeScript road of inline
syntax plus a strip step, stays a product decision that the pluggable design keeps open. A `#:`
file must always remain valid ordinary R.

## Testing doctrine for `syntax`

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
   invariants. It ran against every parser increment from the first one, and CI runs a bounded
   pass.
6. Statement-reparse equivalence, so an incremental result tree equals a from-scratch tree for a
   randomized edit.
7. Acceptance cross-check against R's own parser where an R installation exists. This is
   local-only, like every test that requires R.

Redundancy across these layers is the point. The same construct covered five ways is deliberate.

## Cases where R parsers get subtle

Cover all of these exhaustively: raw strings in the `r"(...)"` and `R"[...]"` forms, where the
formatter has a byte-for-byte rule for a reason; escapes; `%op%` operators; backtick names;
multi-line `#:` blocks, where consecutive `#:` lines stitch into one annotation region;
statement-boundary and newline sensitivity, which is R's newline-versus-operator continuation rule;
`]]` against `] ]` in nested indexing such as `x[[y[1]]]`; the top-level `else` after a newline,
which is legal inside braces and a parse error at top level; `->` and `->>` assignment; `=` as
assignment against `=` as a named argument, which is context-dependent; unary-minus precedence,
where `-2^2` is `-(2^2)`; the hex, `L` integer and `i` complex literal forms; and `\(x)` lambdas
from R 4.1.

## Corpus mechanics

tree-sitter-r's parser corpus lives in its GitHub repository under `test/corpus/`, MIT-licensed.
Fetch it from the repository rather than from the crates.io package, which may omit tests. The
real-world corpus is R's base library sources plus roughly the top 100 CRAN packages. It lives in a
gitignored corpus directory, with a committed manifest and fetch script in `scripts/`. The fetch
needs outbound network, so run it where that exists. The acceptance cross-check against R's
`parse()` needs a local R installation. CI has no R, so the acceptance-divergence allowlist is
adjudicated against R locally once and then committed.

## Better syntax errors are a goal, not a side effect

Dramatically better error messages are an explicit goal of the parser, for R syntax generally and
for `#:` type annotations specifically. Recursive descent knows what it was parsing at every point,
so the bar is: expected-token sets such as "expected `)` or `,`"; paired-delimiter pointers such as
"unclosed `(` opened here" carrying both spans; statement-anchored recovery, so one broken
construct never poisons the file; and, because annotations are first-class grammar, real type-syntax
errors with exact token spans inside a `#:` comment, such as "expected a type after `|`". The
golden error-message suite pins the wording, and the diagnostics goal in `AGENTS.md` sets the bar.

## The legacy stack shares no code

The legacy crates under `legacy/` are frozen. They take bug fixes only. No code is ever shared or
abstracted between the two stacks, which is a user directive. The duplication is deliberate, and
introducing an abstraction to share code with legacy is a mistake even where the duplication is
verbatim.

# Decision record: every pipeline stage is fuzzed from its first commit

This is a user directive. The testing doctrine made fuzzing mandatory for the `syntax` crate from
day one. It applies to every other pipeline stage too. Fuzzing is never bolted on later, for any
layer.

Every stage gets fuzz and property coverage the day it exists, alongside its fixtures. That covers
lowering, naming, inference, diagnostics, the incremental layer, the formatter and the IDE
features. The formatter is fuzzed for idempotence and losslessness. An IDE feature is fuzzed for
never panicking at any cursor position.

The semantics harness at `crates/semantics/tests/test_fuzz.rs` is the template. It asserts four
things over a generator biased toward semantically live shapes, plus a token-soup robustness arm.

- Nothing panics across the full pipeline, which includes fixpoints converging.
- Results are deterministic across fresh databases.
- Diagnostic ranges have valid geometry.
- Incremental equivalence holds, so editing through the setter gives the same result as a fresh
  build. That is the red-green invariant.

`FUZZ_ITERS` scales the budgets. A bounded pass runs in the default test suite, so CI fuzzes on
every change, and the `fuzz_deep` variants carry the long runs.

The first semantics fuzz runs found two real crashes within seconds. One was a non-converging
cycle, where a growing self-referential type rode the iteration cap into a panic. The other was
inference variables leaking through exported schemes into foreign tables. Both are of the class
that only surfaces in the large, which is exactly what per-stage fuzzing exists to catch early.

# Decision record: diagnostic wording follows the project's own bar

This is a user directive. A message does not have to copy any other implementation, and improving
one is welcome.

Wording follows the diagnostics bar in `AGENTS.md`, which is the Rust and Elm standard. The golden
fixture suites are the wording contract, and they render the stack's own messages.

# Decision record: deep resolve is memoized per binding epoch and cuts a cycle to `Unknown`

`InferenceTable::resolve`, the deep resolver in `crates/semantics/src/infer.rs`, walked the interned
type structure recursively with only a depth-64 cap as protection. Interned types form a directed
acyclic graph, so a shared subtree appears once in memory but was re-resolved once per occurrence.
A self-referential binding, meaning a variable whose binding transitively contains itself or a
self-referential alias, expanded as a tree up to the cap. On the real-file corpus of about 507k
lines this measured 397 million inner resolve steps, and resolve alone cost more wall time than the
legacy stack's entire pipeline. The depth cap also truncated meaning. Past depth 64 a type silently
stayed unexpanded, which is a position-dependent semantics that no cache can be layered onto.

Deep resolve is now a memoized walk over the interned graph with explicit cycle detection.

- **A cycle cuts to `Unknown`.** The walk carries a `visiting` stack of the variables under
  expansion. Re-encountering one means an infinite type, and it resolves to `Unknown`. That matches
  the pin-to-`Unknown` doctrine used everywhere self-reference grows, including the fixpoint cap and
  loop widening. Alias expansion keeps a depth guard as a pure resource backstop, not as a
  semantics.
- **Only a clean subtree is memoized.** A result caches in `resolve_cache` keyed by the interned
  type, but only when no cycle was cut beneath it. A node containing a variable currently being
  expanded resolves differently at top level.
- **An epoch invalidates the cache.** Every binding mutation and rollback bumps an epoch counter,
  and the cache self-clears on an epoch mismatch. No entry can serve a stale binding. The common
  case, which is many resolves between mutations such as rendering a whole item's diagnostics, hits
  warm.

Correctness: silent depth truncation is replaced by the established cycle semantics, so an infinite
type resolves to `Unknown` at the point of self-reference instead of expanding arbitrarily deep.
The fixture and fuzz suites confirm this is observation-equivalent everywhere covered. Performance:
corpus inner resolve steps went from 397 million to 4.2 million, which is linear in corpus size.
Resolve wall time went from 30.8 s to 0.3 s, and a whole corpus pass from 53.2 s to 12.7 s.
`RESOLVE_CALLS` stays as a standing instrument, because a near-linear step count is now an
invariant the performance harness can watch.

# Decision record: the server threads one worker, and publishes diagnostics in two waves

The server runs one async-lsp frontend thread and one worker thread that owns the database.

**Cancellation rides the database's own token.** `notify_edit` cancels before it enqueues. A flip
is consumed by whichever in-flight query it kills, and every subsequent job starts on a fresh
storage-handle clone, so the latest edit wins. This was chosen over a hand-rolled cooperative flag
because the database checks its token at every operation, which means no query body needs
instrumenting.

**The publish waves gate on a real query split.** `parse_stage_diagnostics` covers the syntax and
annotation classes and exists as its own query precisely so the first wave never computes naming or
type checking. Filtering the full set afterwards would pay the whole cost and only hide it.
`file_diagnostics` builds on the same query, which keeps the first wave a faithful subset by
construction.

**The host assembly is shared**, in `crates/ry/src/diagnostics.rs`. Config gating, per-file typing
modes, strict escalation, lints and suppression comments run identically for the server's publish
path and for `ry check`, so the two surfaces cannot drift.

Two smaller decisions sit alongside. The `missing-comma` lint is retired, because the hand parser
rejects `f(1 2)` as R does and the lint only compensated for tree-sitter over-accepting it. Its
config key stays accepted and inert. Per-diagnostic related locations, such as the note on a
duplicate top-level binding, are not implemented, because `Diagnostic` has no related-location
model yet. That is recorded as open work rather than silently dropped.

The CLI contract suite and the LSP behavioral suite pin the surface. The LSP suite drives the real
binary over stdio. `stats_witness` asserts the performance and memory budgets as CI-checkable
thresholds.

# Decision record: the interface fixpoint is canonical per group, so a cyclic scheme does not depend on forcing order

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
other. The legacy stack's whole-package rounds were entry-order-independent. Per-item cycle heads
were not.

The shape now lives in `crates/semantics/src/semantics.rs` and has three parts.

- `interface_sccs(files)` builds the static interface-reference graph. It draws an edge from each
  named package definition item to the winner of every global name its body reads, which is
  `non_locals` plus validated `namespace_reads`. One iterative Tarjan pass condenses it in
  canonical order, which is project file order and then item order within a file. Only a cyclic
  group is recorded, meaning one with more than one member or with a self-edge.
- `scc_schemes(files, group)` is the canonical fixpoint of one group. Every member starts at the
  tolerant `Unknown` scheme. Each round re-checks every member against the previous round's table,
  which is Jacobi iteration, so one propagation hop happens per round and within-round order cannot
  matter either. Convergence is scheme-table equality. A group still changing at the round cap,
  which is 16 and shared with the backstop, pins all members to `Unknown`. That is the only
  entry-order-free pin. A member check runs `check_item_with_annotation` directly against an
  overlay environment called `SccGlobals`, which reads the round table first and falls back to
  ordinary global resolution, and which suppresses stub overloads for member names. It never runs
  through `item_check`, so no cycle forms.
- `item_check` adopts the canonical scheme as a member's exported scheme, which keeps a single
  source of truth. Export, hover and every downstream reader see the fixpoint value rather than the
  one-hop-ahead re-derivation the item's own check just computed. `global_scheme` reads `item_check`
  only. The dynamic cycle recovery stays as a backstop for reference edges the static graph cannot
  see.

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

# Decision record: there is no third constraint kind, so two flexible comparison operands stay unconstrained

The question was whether comparing two flexible operands in `function(a, b) a < b` should constrain
them, either to each other or to a new comparable constraint kind covering numeric, `character` and
`logical`.

It should not. Two flexible comparison operands stay fully unconstrained. The function infers as
`<T, U> fn(a: T, b: U) -> logical`, and a cross-family call is accepted. A flexible operand is
still constrained to numeric when its partner is concretely numeric, which is the existing rule,
and two concretely known families must still match.

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

# Decision record: union compatibility commits a flexible argument at first use, in program order

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

# Decision record: strict mode attributes a recursive binding the fixpoint cannot fully type

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

# Decision record: script frame semantics

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

# Decision record: an undeclared type name errors once at the reference and compares like `Unknown`

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

# Decision record: an annotation shape violation refuses the whole block

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
- **Attachment is single-sourced.** `top_level_annotations` computes each top-level block's target
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

# Decision record: a statement-level annotation attaches at any depth and applies where the expression infers

An annotation below the item root used to be invisible to the checker, so the constructor idiom did
nothing. Writing `#: @new Person` on a local assignment, or on a block-final expression inside a
function body, was silently ignored.

The shape keeps one source of truth per fact.

- **Association.** `statement_annotations(parent)`, which is the existing adjacency walk, runs over
  any statement sequence, whether the file root or a braced block. Top-level attachment,
  expression-level attachment and the dangling-annotation diagnostics, which now cover a nested
  block, therefore share one rule. `item_expression_annotations(db, item)` maps each attached block
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

# Decision record: capture liveness is frame-scoped

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

# Decision record: conditional slots type, export edges generalize, and `missing()` flows through the environment

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

# Decision record: NAMESPACE and DESCRIPTION metadata feed resolution

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

Consumption is two predicates at the diagnostic edges. `imported_bare` joins the unresolved-check
skip set, and `declared_dependency` quiets the unknown-namespace warning. Typing is untouched, so
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

# Decision record: data.table awareness

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

# Decision record: the native pipe desugars at lowering

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

# Decision record: `@masked` is formal-aware, and dplyr is a conditional namespace

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

# Decision record: the shipping binary links no Apple frameworks

The user set the criteria: fewer dependencies, and no licensing exposure.

A release macOS binary is cross-linked on Linux by zig, through cargo-zigbuild inside the nix
build. Zig ships stubs for libSystem, libc and libm only. The binary linked nothing else before the
REPL existed, so the SDK-less link worked by accident. The REPL added the first Apple-framework
edge. reedline depends on chrono with the clock feature, which depends on iana-time-zone, which
depends on core-foundation-sys, which emits `-framework CoreFoundation`. Zig cannot resolve that
without a macOS SDK.

`[patch.crates-io]` replaces iana-time-zone with `patches/iana-time-zone`, a version-matched stub
whose `get_timezone()` always errors. That is safe because chrono consults it only as a fallback,
after its primary timezone sources of `TZ` and `/etc/localtime` and before its final UTC default.
The only local-time user in reedline is its default prompt's clock display, and the REPL does not
use it, because it renders R's own prompt.

The `release` recipe in the justfile preflights the aarch64-apple-darwin graph for known
framework-linking crates, so a regression fails in seconds with a named culprit instead of deep
inside the nix zig link. The stub must stay version-compatible and feature-compatible with what
chrono requests, or cargo silently prefers the real crate, and the preflight catches exactly that.

One nix-specific trap applies. crane's dependency-only builds compile dependencies against a
dummified workspace copy, where local `.rs` files are emptied so the dependency cache survives a
source edit. That would empty the patch crate too and break chrono's compile, so `flake.nix`
restores `patches/` verbatim into the dummy source through crane's `extraDummyScript`. It
interpolates only that directory, so the cache stays source-independent.

One alternative was implemented first and then reverted. It fetched a macOS SDK from the widely
used third-party mirror of Apple's SDKs and exported `SDKROOT` for the darwin cross-build. It was
mechanically verified, because with the SDK the failing build links a valid arm64 Mach-O, and it is
the most general option with no behavior change. The user directed reverting it. It adds a large
third-party artifact to the release closure, and Apple's license on a redistributed SDK is gray.
Vendoring the tarball is legally worse, because you become the redistributor.

Framework-free is sustainable for this product. libR is dlopen'd at run time, so it has no
link-time dependency, and a terminal REPL's plotting story is file output, a terminal image
protocol, or a browser. Nothing on the roadmap needs `-framework` at link time.

Two things would trigger a revisit: a dependency that genuinely needs an Apple framework, such as
native windows or clipboard integration, or pressure to sign and notarize. At that point build the
mac artifact on a real macOS runner, which is the only fully license-clean way to use Apple's SDK.
That is also the natural home for running the REPL end-to-end suite against a real R in CI.

# Decision record: the identity-parity program is retired

The user decided this.

The differential suites that proved the rewrite equivalent to the frozen oracle are deleted. That
covers the typing, scripts and strict arms, the seeded fuzz differential, the legacy-corpus sweep,
the real-file corpus arm, and the per-position IDE comparison, together with their adjudicated
divergence ledgers. The rewrite's own fixture suites are the semantics contract, and an improvement
lands on its own terms with no oracle renegotiation. The legacy IDE fixture inputs worth keeping
were ported first, which is 81 cases, and the port surfaced two real defects that the backlog
records.

`legacy/differential` is benchmark-only now. `test_stats.rs` times and memory-measures the same
corpus through both stacks. It, and the legacy crates it depends on, stay until the deletion sweep
the user will call for. The performance witnesses migrate to a home in the new stack as part of
that sweep.

Every future semantic improvement now costs one fixture bless, instead of a fixture bless plus
per-arm adjudication entries. The battery loses its slowest suites. The deletion sweep's only
remaining prerequisite is the witness migration.

# Decision record: a vendored export manifest carries the name-level truth beside the typed stubs

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
`known_exports` set that `package_scheme_exists` consults after schemes and nominals. A manifest
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

# Decision record: the Zed extension versions on its own line

Three shipped artifacts carry a version, and only two of them derive from the workspace
`Cargo.toml`. The CLI is the source of truth. The VS Code extension bundles that binary, so its
manifest carries the same number with the prerelease suffix stripped, because the marketplace
rejects `X.Y.Z-alpha`. That is a mechanical derivation.

The Zed extension bundles nothing. It locates a binary at run time, through the LSP settings path,
then `PATH`, then the latest GitHub release. Its version therefore describes the extension's own
code and says nothing about the CLI. That was settled once and still failed to hold. The release
recipe stopped bumping it, the number was hand-realigned to the CLI's twice anyway, and the second
time a test was added to mandate the alignment. A version that must be manually resynchronized to a
number it has no relationship with is the duplication, not the cure.

`editors/zed/extension.toml` is now a plain semver line of its own, at `0.1.0`. It restarted
because the extension has never been published to Zed's registry and nothing constrains its
history. The wasm crate's `Cargo.toml` version tracks that manifest rather than the workspace, and
neither inherits `version.workspace`. It is bumped by hand when the extension changes. The
release-metadata test asserts the VS Code derivation only, and its module documentation states why
the Zed manifest is absent, because that test is what would otherwise re-couple them. The
prerelease suffix is dropped for good, because `-alpha` and `-beta` name the CLI's release channel,
and an extension that only locates a binary cannot be in one.

Each artifact now has one number with one owner. A Zed release no longer implies a CLI release, or
the reverse. The recurring commit that realigned the stale Zed extension version has no reason to
exist.
