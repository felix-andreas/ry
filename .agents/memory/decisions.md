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
follows from them. The semantics contract is `reference/type-system.mdx`, and it is updated before
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
surface is a fixed corpus the project maintains. The need there is real, and it comes from R's coercion
table rather than from a gap in the declaration language. `abs(TRUE)` is an `integer`, so a
type-preserving `<T: numeric> fn(x: T) -> T` would be wrong for a `logical` argument, and `min`
accepts `character`, which no numeric binder covers. Never say these functions have no principal
scheme. A gradual checker that cannot describe the standard library is not usable. A user's `#:` annotation stays pure
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

Under strict mode an `Unknown` origin is an error, an unannotated parameter nothing constrains is an
error (see the inline-annotations record), and an unresolved-name reference is an error rather than
a plain naming warning. A recursion-induced `Unknown` return is an origin. Explicit
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
`test_malformed_lower` controls and the `error_tolerant_lowering` project fixtures pin it.
Simplicity: one policy sentence governs every case. Performance: the malformed-flip engine witness
went from two index refolds plus a referrer recheck to no refolds and a recheck of the edited file
only, because exports stay byte-equal across the flip and early cutoff does the rest. Incremental
analysis: typing inside a construct is the highest-churn edit state, and it now has the smallest
blast radius.

# Decision record: `is.null` shapes an unconstrained inference variable

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

Annotations are one internal concept, and the surface spelling is pluggable: both spellings lower
through `annotations.rs` into one `Annotation`. `#:` is structured trivia, and a `#:` file must
always remain valid ordinary R. The inline spelling is the superset dialect this seam kept open,
chosen by the mandate in `AGENTS.md` (the next two records).

## The inline annotation syntax is a superset of R

The previous position was that a superset dialect "stays a product decision" (the entry above),
with `declare()` as the valid-R route. The mandate in `AGENTS.md` makes that decision: the inline
syntax is on by default and ry warns when it is turned off, which only makes sense for a syntax that
GNU R cannot parse. So the inline form is the superset that `contributing/design/inline-type-syntax.md`
costed (generic binder, parameter, return and binding positions), typed files keep the `.R`
extension because ry executes them directly, and `declare(type(...))` stays a recognized spelling for
code that must remain valid R. The cost is that other R tools fail on a typed file, so code that must
run on GNU R goes through a strip step that moves inline types into `#:` comments. The reasoning is in
`contributing/design/r-runtime.md`, section 8.

## How inline annotations are built

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
side winning, except that a `#:` coercion form (`@trust`, `@if-unknown`, `@new`) governs the value
(`merge_annotations`). Disagreement is judged separately in `annotation_agreement_diagnostics`, on
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
   invariants. It ran against every parser increment from the first one, and a bounded pass runs in
   the `syntax` crate's own test suite.
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

`FUZZ_ITERS` scales the budgets. A bounded pass runs in each crate's own test suite, and the
`fuzz_deep` variants carry the long runs.

**CI does not fuzz today, and it is worth stating plainly rather than assuming otherwise.** The root
`Cargo.toml` sets `default-members = ["crates/ry"]` and the workflow omits `--workspace`, so
`cargo test --all-targets --all-features` lists 171 tests where the workspace has 712. Every fuzz
arm lives in another crate. The second job runs `-- --ignored`, which lists no tests at all for the
same reason, so it is a release build followed by an empty test run. `backlog.md` holds the blockers
that a workspace-wide CI run has to clear first.

The first semantics fuzz runs found two real crashes within seconds. One was a non-converging
cycle, where a growing self-referential type rode the iteration cap into a panic. The other was
inference variables leaking through exported schemes into foreign tables. Both are of the class
that only surfaces in the large, which is exactly what per-stage fuzzing exists to catch early.

# Decision record: deep resolve is memoized per binding epoch and cuts a cycle to `Unknown`

`InferenceTable::resolve`, the deep resolver in `crates/semantics/src/infer.rs`, walked the interned
type structure recursively with only a depth-64 cap as protection. Interned types form a directed
acyclic graph, so a shared subtree appears once in memory but was re-resolved once per occurrence.
A self-referential binding, meaning a variable whose binding transitively contains itself or a
self-referential alias, expanded as a tree up to the cap. On the real-file corpus of about 507k
lines this measured 397 million inner resolve steps, and resolve alone cost more wall time than the
previous stack's entire pipeline. The depth cap also truncated meaning. Past depth 64 a type silently
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
other. The previous stack's whole-package rounds were entry-order-independent. Per-item cycle heads
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

# Decision record: one source of truth for where an item spells its name

**Status:** decided and implemented.

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

# Decision record: a check reads per-name firewalls, never project-wide maps

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

# Decision record: the Linux release binary links glibc 2.17 through zig

**Status:** decided and implemented. The user directed one release build per platform, plus a
native package for Nix users.

**Problem.** The Linux release package was crane's native build against nixpkgs' glibc, so the
shipped binary's ELF interpreter and RUNPATH were `/nix/store/...` paths. It started only on a
machine whose store held that exact glibc.

**Shape.** Two Linux derivations, one job each:

- `ry-linux-x86_64`, symmetric with `ry-macos-aarch64` and `ry-windows-x86_64`, is cross-linked by
  zig through cargo-zigbuild for `x86_64-unknown-linux-gnu.2.17`. It uses the standard loader
  `/lib64/ld-linux-x86-64.so.2`, needs only `libc`, `libm`, `libpthread`, and `libdl`, and uses no
  symbol newer than `GLIBC_2.17`, so it runs on every glibc distribution since 2.17 and can `dlopen`
  R for the console.
- `default` is the native crane build against nixpkgs, for NixOS and flake users. Its console loads
  nixpkgs' R.

Every release derivation sets `allowedReferences = [ ]`, so an interpreter, a RUNPATH, or any other
store path in a release binary fails the build. The allocator is glibc's own, and the release
binary measured within noise of the native build (0.95 to 1.08 times its time on `ry check` over
rlang, ggplot2, dplyr, and testthat, and on `fmt --check`).

**Trap: release derivations cannot run their tests.** A release binary uses the FHS loader, which
the Nix build sandbox lacks, so a test binary linked the same way cannot start there. The native
package carries the test suite, and the release configuration is exercised outside Nix with
`cargo-zigbuild test --target x86_64-unknown-linux-gnu.2.17`.

**Rejected: a static musl binary.** It runs everywhere, NixOS and Alpine included, but a static
binary cannot `dlopen`, so it would lose `ry repl` and `ry run`; keeping the console in the release
binary decided it. Measurements for whoever revisits this: musl's own `malloc` made `ry check` 2 to
5 times slower; zig's libc, whose `malloc` is zig's `SmpAllocator`, restored glibc's speed with 13 to
23% more peak memory; jemalloc was 10 to 32% faster than glibc but compiles C through `configure` and
`make`; and mimalloc was 24 to 25% slower than glibc on the largest packages.

**Also rejected.** Static glibc, because its `dlopen` runs libR against a second, uninitialized libc
("Error parsing /proc/self/maps", "R home directory is not defined"). And a glibc download beside a
musl download, the shape uv and ruff use, because the directive is one release build per platform.

**Accepted cost.** The release binary does not run on musl distributions such as Alpine, or on
NixOS without nix-ld. NixOS users install the flake's `default`, and Alpine users build from source.

**Field practice.** rust-analyzer's main Linux binary targets glibc 2.28. uv and ruff default to
glibc builds (2.17 and 2.31) and fall back to static musl only where glibc is missing or too old.
rustc's Linux toolchain targets glibc 2.17.

# Decision record: runtime licensing

ry is UPL-1.0, so no GPL code from GNU R is copied into the repository. The runtime's headers and
implementation are written independently.

Base R's R-level sources (the open question in `contributing/design/r-runtime.md`, section 11) are
GPL-2-or-later. The default is to load them at runtime from the user's own GNU R installation
rather than vendor them, the way the `repl` crate already loads R at runtime. Relicensing the
project is the owner's call and is not assumed.

# Decision record: the runtime's object model, collector and native boundary

This settles step 2 of `contributing/design/r-runtime.md`. The code is `crates/heap` (object
model and collector) and `crates/libr` (the C API, built as `libR.so`, with its C half in
`src/native.c` and its headers in `include/`).

**Two crates, split by who may see the layout.** `heap` owns the object layout, allocation, the
collector, interning and environment frames, and knows nothing about C. `libr` owns the runtime
state, the native boundary and every exported name. The interpreter and the JIT will depend on
`heap` and link into `libr`, because a package binary built by GNU R records `libR.so` as a needed
library: whatever answers that name must contain the whole runtime. `libr`'s cdylib has the soname
`libR.so`, so the dynamic linker satisfies a GNU-R-built package from ry's already loaded library
instead of searching for GNU R's.

**One header, three payload shapes, one handle type.** Every object starts with a 16-byte header:
type code, collector bits, object and ALTREP flags, GNU R's 16 general-purpose bits at GNU R's
positions (packages test `LEVELS(x) & 8` for UTF-8), a 16-bit saturating reference count, and the
attribute pairlist. Vectors store length, capacity and then their elements, so `INTEGER()` is an
interior pointer at offset 32 and every element type is aligned. Pairlists, calls, closures,
promises, symbols, binding cells and ALTREP instances are triples. Atomic vectors start zeroed (GNU
R leaves them uninitialized), because Rust code reading uninitialized memory is undefined behavior
and calloc'd pages cost nothing.

**Binding cells are heap objects.** A frame maps symbols to cells (`SexpType::Binding`, internal
code 26, which GNU R leaves unused and native code never receives). A cell's address is stable for
as long as anything references it, so compiled code can watch one and still hold it safely after
`rm()`. The base environment and base namespace keep their cells on the symbols, as GNU R keeps
base bindings in `SYMVALUE`, so a base lookup is one load. Small frames scan; above eight bindings a
frame keeps a hash index.

**The collector: non-moving mark-sweep with sticky mark bits.** A surviving object keeps its mark,
so outside a collection "marked" means "old". A minor collection marks only from the roots, the
remembered set and the escaped set, and sweeps only the young list (every object allocated since
the last collection), so its cost is proportional to young data, not the heap. A major collection
clears the marks first and sweeps every page. Small objects live in 64 KiB pages of 17 size
classes, the 40-byte class being a triple; larger objects are allocated one by one. Freed slots are
poisoned, with the kind byte set to `FREESXP` and the collector byte cleared, so a stale handle
reads as freed and unmarked; any set of handles kept across a sweep is filtered before it. The write barrier records an old parent
that receives a young child. A vector whose writable element pointer escaped (`STRING_PTR`,
`DATAPTR` on a list) bypasses the barrier from then on, so it joins the escaped set and every minor
collection rescans it. Weak references follow ephemeron rules, and the `CHARSXP` intern table is
weak, with minor collections checking only the young strings. GNU R ages objects through several
generations; one promotion after one survival is simpler and costs some premature tenuring, which
profiling can revisit without changing the API.

**Roots.** The `PROTECT` stack, the precious multiset, the handle stack, registered globals, every
interned symbol, the weak-reference list, and any `RootSource` an interpreter or JIT registers. A
function that allocates while it holds a handle that nothing else roots must root it first, and collection stress is what
exposes a miss.

**The handle scope roots what the API hands out, not every allocation.** While a native call is on
the stack, every handle an API function returns to native code is pushed to the handle stack, and
the native call's return truncates it. Rooting results rather than allocations matters for
`Rf_eval` called from native code: the evaluator's own temporaries stay collectable, and only the
value native code receives is kept. The cost is that a native loop that allocates and drops many
temporaries keeps them until it returns, which GNU R would have collected.

**Each native call owns a `PROTECT` region.** `UNPROTECT` below the region's base is an error, in GNU
R's words, rather than silently unprotecting the caller's objects, which GNU R allows. An imbalance
on return is a warning in GNU R's words, and the region is cut back, so a leak never accumulates.

**The boundary rule.** Inside Rust every non-local exit is an `Err(Unwind)`. Native code ry calls
runs in a C trampoline with a jump target (`ry_call_native` and the per-signature ALTREP, finalizer
and init trampolines). An API function runs its body under `enter`, which catches the `Err` and any
panic, drops everything, and only then calls `ry_jump`. The frames the jump skips are `enter`'s and
the exported function's, and they hold nothing with a destructor at that point, which is the
condition (plain old frames) under which skipping a Rust frame with `longjmp` is defined. Callbacks
that native code passes to the API (`R_UnwindProtect`, `R_ToplevelExec`, `R_ExecWithCleanup`,
`R_tryCatch`, `R_mapAttrib`) run from C, so a jump out of them never skips a Rust frame, and a C++
exception thrown by an `R_UnwindProtect` cleanup unwinds only C frames compiled with
`-fexceptions`. A jump restores the `PROTECT` depth and the `R_alloc` watermark of its context, as
GNU R does.

**Exporting C symbols from a Rust cdylib.** rustc's version script exports only Rust-defined
symbols, so `build.rs` writes a second one from the `RY_EXPORT` markers in `native.c`, which lld
merges. The variadic functions (`Rf_error`, `Rprintf` and their family) must be C, because stable
Rust cannot define C variadic functions. GNU ld rejects a second version script, which matters once
a platform without lld is targeted.

**Finalizers run at safe points, not inside a collection.** A collection happens deep inside an
allocation, where native code must not run, so ready finalizers queue and run when a native call
returns, on `R_gc`, and on `R_RunPendingFinalizers`, each as if at top level.

**Evaluation is a deliberately small subset until the interpreter exists.** `Rf_eval` evaluates
constants, symbols, promises, and calls to a handful of primitives (`.Call`, `.External`, `stop`,
`warning`, `dyn.load`, `list`, `identity`, `invisible`, `quote`), which is what native code needs to
drive native code. Calling a closure is an error that says so.

**ry presents itself as R 4.6.2 with revision 89194**, the lowest revision that keeps data.table on
its modern branches, and its native encoding is UTF-8.

**Considered and rejected.** Source compatibility alone saves nothing, because the opaque `SEXP`
already forces every accessor to be a real function. A moving nursery cannot coexist with raw
pointers held by native code. Rooting every allocation made inside a native call (instead of every
handed-out result) would make `Rf_eval` loops from native code retain all interpreter garbage.
Defining the C wrappers for every API function in C (instead of `enter`) would duplicate every
signature in two languages for no safety gain.

# Decision record: packages and R versions in the runtime

`contributing/design/packaging.md` holds the design and the measurements. Nothing existed before, so
there was no previous source of truth; the choices that settle architecture are these.

- **One installed package directory serves GNU R 4.6 and ry's runtime,** because ry's `libR` is
  ABI-compatible and presents as R 4.6. ry treats the `Built:` field as GNU R does (refusing only
  pre-3.0.0 builds), so existing libraries load unchanged. A package built under ry carries an extra
  `RyBuilt:` field until the oracle shows ry-built binaries load under GNU R, and the corpus is built
  only by GNU R.
- **ry's per-package data (object index, bytecode) lives in ry's own cache, never inside the
  package,** so the differential oracle loads byte-identical packages under both runtimes and shared
  libraries (site libraries, renv's cache) are never written to. It is filled on first load, keyed by
  a hash of the installed `DESCRIPTION` and `.rdx` plus the runtime version, because hashing the whole
  directory on every load would cost what the cache saves.
- **Package management runs as R code under the runtime** (`install.packages`, renv, pak). A package
  manager of ry's own (`ry add`, `ry sync`, a lockfile) is deliberately left open, because the runtime
  does not need it; if it comes, it must read and write `renv.lock`.
- **The runtime is chosen by configuration, never by failure** (`[r] runtime`, `--runtime`,
  `RY_RUNTIME`), because a switch on failure hides what the transition must surface. GNU R is the
  default until build step 4's exit criterion, then ry. An explicit version other than 4.6 selects GNU
  R, and combined with `runtime = "ry"` it is an error. The `renv.lock` R version and `Depends: R` are
  advisory only, or most renv projects would stay on GNU R after the flip. ry's own `R_HOME` has
  `bin/` scripts that start ry (so `parallel` and `callr` workers inherit the runtime), ry's `etc/`
  and `include/`, and `library/` and `share/` linked to a GNU R 4.6 installation.
- **The analysis reads the installed library, not a running R,** found by a fixed rule (`[r]
  libraries`, else renv's project library, else `R_LIBS`/`R_LIBS_USER` and the selected R's
  libraries; `.Rprofile` is never evaluated). Per package name it reads the installed `DESCRIPTION`,
  `NAMESPACE` and `.rdx` object names through a tracked by-name lookup, with stubs chosen by version
  range. Generated export manifests remain for base R and as the no-library fallback.
- **The corpus is pinned with renv** on a dated P3M snapshot in build step 1, because renv is in the
  benchmark set and R's own installer cannot backtrack to older compatible versions.

# Decision record: the interpreter and base R

This settles step 3 of `contributing/design/r-runtime.md`. The code is `crates/interp`; before it
there was no evaluator, so there was no previous source of truth.

- **`interp` sits between `heap` and `libr`.** `libr` depends on `interp` and installs it on the
  runtime's heap (`interp::install`), passing host hooks (console output, ALTREP materialization,
  finalizers) and replacing the foreign-function builtins (`.Call`, `dyn.load`, ...) with
  `interp::register_builtins`. `interp` never sees C, so it builds and tests without libR, and the C
  API's `Rf_eval`, `R_tryEval` and `R_ToplevelExec` are thin calls into it.
- **Every non-local exit is a Rust `Err(Unwind)`.** `return`, `break`, `next`, exiting handlers and
  restarts are `Jump`s to a frame's environment; an error raised by a builtin stays unsignalled
  (`Unwind::Error`) until the nearest `eval` or builtin boundary signals it, so calling handlers run
  before any frame unwinds. Only the libR boundary converts to and from `longjmp`. This keeps Rust
  destructors sound and needs no context stack beyond `Interp::frames`.
- **Frames mirror GNU R's function contexts exactly** (closure calls, `eval()` and internal
  dispatch push one), because `sys.call`, `sys.function`, `parent.frame` and the oracle driver's own
  `sys.nframe()` arithmetic observe them.
- **Base R's R code loads from the user's GNU R installation** (the base lazy-load database,
  `R_HOME/library`), per the licensing record; GNU R's byte code reads back as its source
  expression, which ry compiles itself.
- **The default packages' native code is reimplemented in Rust** (`builtin_libraries.rs`). GNU R
  builds the shared libraries of methods, utils, stats, grDevices, graphics and tools against its
  internal headers, so they inline its object layout (`LENGTH`, `PROTECT` through `R_PPStack`) and
  cannot run on ry's heap. `dyn.load` of one returns a library record backed by a table with GNU R's
  routine names and arities, so `useDynLib` binds every `C_` symbol and an unimplemented routine fails
  naming itself. CRAN packages are unaffected: since R 4.2 they see an opaque `SEXP`.
- **Tier-0 code is a register machine stored in the closure** as a `BCODESXP` (instructions,
  constants with the source first, inline caches), compiled on a body's second call. Keeping it in
  the closure means it is collected with it and needs no side table; `closure_body` is the one
  reader that must know. Inlined constructs run behind a cached guard that the symbol still names
  the base primitive, falling back to the AST evaluator for the whole expression, so redefining
  `if` or `+` keeps GNU R's semantics instead of GNU R's compiler's assumption that base is not
  shadowed.
- **Inline caches are validated by one heap counter, the binding epoch,** which moves when a
  binding is created or removed outside a function frame, or an enclosure changes. Function frames
  (flagged `FUNCTION_FRAME`) are excluded because bindings appear in them on every call, so a cached
  lookup never resolves through one and the frame itself is probed directly. A function lookup that
  skipped a non-function binding is not cached, because that binding could later hold a function
  without moving the epoch.
- **A complex assignment's `*tmp*` does not count as a reference** (`Heap::untrack_references`,
  GNU R's `DISABLE_REFCNT`): counting it made every `x[i] <- v` copy `x`, which made fill loops
  quadratic.

# Decision record: the full C API surface and package installation

This settles the part of step 4 of `contributing/design/r-runtime.md` that does not need the
interpreter. Before it, `libr` exported the top 150 measured names, `Rf_eval` evaluated nine
primitives, and nothing built or installed packages.

- **Evaluation is a hook the interpreter installs, not a dependency.** `libr::eval::Evaluator`
  (eval, apply, parse, force_and_call, print_value, current_env, current_srcref, dot_last) is set
  once with `install_evaluator`; every evaluation entry point of the C API (`Rf_eval`,
  `R_tryEval`, `R_forceAndCall`, `R_ParseVector`, `Rf_PrintValue`, R-level finalizers,
  `R_GetCurrentEnv`, the S4 class functions through `methods`, namespace loading in
  `R_GetCCallable` and the unserializer) goes through it. The interpreter calls libR back as
  ordinary Rust (`native_call`, `connections::*`, `serialize::*`, `altrep::*`). Without an
  evaluator the built-in evaluator of native calls runs, so libR stays testable alone.
- **BLAS and LAPACK are header-only.** `R_ext/BLAS.h` and `Lapack.h` declare the system
  libraries, which packages link themselves (`R CMD config BLAS_LIBS` is `-lblas`), as Debian's
  GNU R and every corpus binary do (they record `libblas.so.3`/`liblapack.so.3`). A Rust LAPACK
  does not exist at reference quality, and re-exporting the routines from libR would duplicate
  the system's. Where R 4.6's declarations contradict the reference routine (missing or stale
  arguments, pointer-to-pointer typos), ry's headers follow the routine; hidden string-length
  arguments always follow R.
- **libR depends on zlib, bzip2 and xz,** as GNU R's does, because binaries (gert, httpuv) call
  zlib without linking it and find it through libR.
- **Names R 4.6 dropped stay exported** (`legacy.rs`), because binaries built for older R import
  them and a missing symbol stops a library from loading at all. `TRUELENGTH` is the capacity of a
  shrunk vector; any other value lives in `Runtime::truelengths`, and its object is preserved
  while it has one, so no address is reused under a stale value. `SETLENGTH` makes the vector
  resizable. `FRAME` returns a snapshot pairlist (frames are hashed). `R_PPStack` is a real array
  that a root source scans up to `R_PPStackTop`, for code compiled with R's internal macros.
- **The graphics engine is exported and errors when used.** Device packages (ragg, systemfonts)
  must load and register; the colour and text helpers work. Implementing devices stays the open
  question it was.
- **`.C` and `.Fortran` copy their arguments** and pass data pointers through the same
  dispatcher as `.Call` (all arguments are pointers), returning the copies as a named list.
  `.Call` accepts `NativeSymbolInfo` objects, which packages that call `R_forceSymbols` require.
- **`R_withCallingErrorHandler` runs its handler on the unwinding error,** after the native frames
  between the error and the handler are gone. GNU R runs it before unwinding; the difference shows
  only to a handler that inspects those frames.
- **Hash tables (`R_mkhashtab`) live entirely in heap objects** (an external pointer whose tag
  is `c(type, count)` and whose protected value is the bucket list), so the collector traces
  them and nothing is freed by hand.
- **ALTREP methods have three states:** default, native (through a C trampoline) and built in
  (Rust returning `ApiResult`, no `setjmp`), so ry's own compact sequences cost nothing at the
  boundary. A class handle has GNU R's layout (a raw vector whose attribute is the class,
  package and type), registered by (class, package) symbols. `Rf_coerceVector` of a plain
  integer or double vector to character returns a deferred string, as GNU R does.
- **Serialization writes ALTREP objects as `ALTREP_SXP` in version 3** and reads them through
  the class registry (loading the package's namespace for an unknown class when an interpreter is
  installed), so compact sequences round-trip byte for byte with GNU R. GNU R bytecode reads as
  the expression in its constant pool, because ry does not run GNU R bytecode. Environments are
  written unhashed. Namespace and package references go through a resolver the interpreter
  replaces; the default creates registered placeholders.
- **A connection's `conn_id` external pointer holds its `Rconn` address,** validated on use, so
  a stale connection object errors with "invalid connection" instead of reaching a reused slot.
  Buffering lives in each connection kind, not in `Rconn`'s `buff` fields, so
  `R_ReadConnection` after `readLines` sees exactly the unread bytes.
- **Header export coverage is checked by the compiler** (`-aux-info` and `-E` in
  `tests/test_headers.rs`), not by scanning header text, because rename macros and struct fields
  defeat a text scan.
- **Package installation is the `rcmd` crate.** ry's R home has ry's headers and `libR.so`, an
  `etc/Makeconf` written for ry, `share/make/shlib.mk`, and a `bin/R` that runs `ry-rcmd`, so
  `configure` scripts asking `R CMD config` get ry's toolchain. A source install compiles `src/`
  as `R CMD INSTALL` does and records `RyBuilt:`; the R-level half (lazy-load database, help,
  `Meta/`) waits for the interpreter, so `R/` is kept as sources. `bin/Rscript`, and `bin/R`
  for anything but `R CMD` and `R RHOME`, run GNU R for the R code configure scripts and
  `Makevars` evaluate (`R -f tools/examples.R`): an explicit build-time delegation that the
  interpreter replaces, not a fallback on failure.
- **Rmath, Applic, LINPACK and the Fortran helpers are Rust written from the published
  algorithms** (`rmath.rs`, `applic.rs`, `linpack.rs`, `utils.rs`), never translated from GNU R,
  which is GPL. The oracle is GNU R itself: `tests/data/rmath_cases.R` records answers and random
  streams through the `rmathtest` package, and `test_rmath` replays them in ry's libR with a
  per-function tolerance that names its reason. The random variates draw uniforms in GNU R's
  order, so seeded streams match. `R_binom_kind` reports `BUGGY_BTPE` and `R_sample_kind`
  `REJECTION`, the defaults of the R version the corpus was recorded with.
- **An error raised while `dlopen` runs a library's static initializers surfaces as a
  `dyn.load` error,** as in GNU R: `ry_call_dlopen` sets a jump target around `dlopen`
  (Rcpp's `Function c("c")` globals call `Rf_findFun` there). Longjmping out of an initializer
  leaves glibc's recursive loader lock with that thread, as it does in GNU R, so a process that
  exits from another thread deadlocks in `_dl_fini`; R runs on the main thread, and the tests
  that provoke it (`test_load_errors`) do too.
- **`getNamespace` is a built-in of the native-call evaluator,** answered by the serializer's
  namespace resolver, because Rcpp's initializers look it up before any interpreter exists.
- **The headers keep declaring the names R 4.6 removed** (`SETLENGTH`, `SET_TRUELENGTH`,
  `FRAME`, ...) and ship an empty `R_ext/PrtUtil.h`, because pinned package sources (readr,
  isoband, yaml) still use them and `legacy.rs` exports them anyway.
- **The numerical routines were audited and rewritten clean room.** A review flagged routines
  whose structure tracked GNU R's C closely (the LINPACK QR family `dqrdc2`/`dqrls` and friends,
  the optimizers `vmmin`, `nmmin`, `cgmin`, `samin`, plus `R_max_col`, `R_pretty`, and parts of
  Rmath). Their bodies were deleted and rewritten by agents that never read GNU R's sources, from
  the published algorithms (LINPACK's users' guide, Nash's *Compact Numerical Methods*, Nelder
  and Mead, the Wilcoxon and signed-rank recurrences) and R's documentation, with GNU R used only
  as a black-box oracle through recorded cases. Results match GNU R exactly except where the
  system BLAS (OpenBLAS) reorders sums; those cases carry a named tolerance. The rewritten incomplete
  gamma, noncentral chi-squared and beta, and discrete quantiles are more accurate than GNU R in
  far tails (checked with mpmath), so `test_rmath` accepts a quantile when ry's own distribution
  function maps GNU R's answer back to p within GNU R's error, and a noncentral function whose
  Poisson index exceeds i32 returns NaN with a precision warning. SANN does not accept a
  custom candidate generator and always takes the Gaussian step.
- **`R CMD SHLIB` passes `-l`, `-L` and `.a` arguments through to the link,** because configure
  scripts probe for system libraries with `R CMD SHLIB conftest.c -lfoo` (Rglpk). Other options
  are ignored, as before.
- **`Rf_rnbeta` is exported** although GNU R's library does not define it, because R's `Rmath.h`
  declares `rnbeta` and Rcpp's sugar headers refer to `::Rf_rnbeta`, so a declaration with no
  symbol fails the header coverage test and a missing declaration breaks Rcpp builds.

# Decision record: the package-development loader

Nothing loaded packages before; `contributing/design/r-runtime.md` section 6 and the user page
`guides/loading-packages.md` describe the result. These choices settle architecture.

- **The index lives in `semantics`, the diff in `crates/loader`, and only a backend knows the
  runtime.** `semantics::load_index` derives per-item facts (`item_load`, `item_immediate_reads`),
  a per-file layer (`file_items`) and the package join (`package_index`) from the analysis, with
  its own input `PackageSources` so a loader never perturbs a project's `ProjectFiles`. The diff
  produces a backend-neutral `loader::Plan`; the GNU R backend (`gnur.rs` plus `runtime.R`) applies
  it, and ry's runtime will apply the same plan, so the runtime swap touches neither the index nor
  the reload logic.
- **A plan travels to R as an uncompressed RDS file** written in Rust (`gnur::rds`), because
  `readRDS` builds the vectors in C without parsing R code, and one format serves both transports:
  the `.Call` routines registered on the embedding `DllInfo` inside `ry repl`/`ry run`/`ry load`, and
  the `ry load --emit-plan` process a plain R session runs. Out of process, the loaded state is a JSON
  `LoadState` in the session's `tempdir()`, committed by renaming `<state>.pending` once R applied the
  plan, so a failed apply never records a load that did not happen; in process, `Loader::commit`
  does the same.
- **Laziness is decided per name: lazy only when the one writer defines a function.** Two writers
  make every writer eager in order, which keeps `f <- memoise(f)` correct without special cases.
- **Reload rules (each one found by the reload property test):** statements match by content in
  order (longest common subsequence) within a file; a statement reruns when new, when it read a
  changed binding, or when it writes a changed name; a rerun statement reading a name rewritten at
  or after it reruns that name's earlier writers first; the rerun set is a fixpoint that repeats
  while the changed-name set grows; a `Collate` reorder falls back to a full load.
- **Durability, not a host-side cache, keeps a reload proportional to the edit.** Files enter at
  `Durability::HIGH` and drop to `LOW` on their first edit, so after the first edit of a file
  salsa verifies untouched files in O(1). The alternative, the host reusing per-file results it
  knows are unchanged, would duplicate salsa's own bookkeeping.
- **The R runtime is byte-compiled once per runtime hash and R version and cached** under
  `dirs::cache_dir()/ry`, because R's JIT otherwise compiles each runtime function on first call and
  that alone cost more than a reload (about 200 ms per session). The cache stores the whole
  environment, because `environment<-` strips bytecode from a closure.
- **A statement's dependencies include calls.** Function items record the identifiers their body
  mentions (a syntactic superset, cheap without name resolution), and the diff closes the changed
  set over that call graph inside the rerun fixpoint, and applies the earlier-writer rule to
  everything a statement's calls reach.
- **The REPL reloads by feeding `.ry_autoreload()` as its own top-level command** before the user's
  command, when a stat fingerprint of the loaded packages changed and R is at its top-level context
  (`R_GlobalContext` has no parent; comparing prompt text broke on `options(prompt =)` and fed
  `readline()`). A failed reload is retried only after the next change on disk. This is the one place the console feeds input after startup; it is a
  user-visible command like typing `load_all()`, and it keeps `.Last.value`.
- **Deferred:** updating other development packages' import copies of a changed binding, loading a
  rebuilt shared library under a new name, a `system.file()` shim for `inst/`, testthat helpers, and
  evaluating `if` in `NAMESPACE` (ignored with a warning). `backlog.md` lists them.

# Decision record: the loader on ry's runtime

`ry load` applies the loader's plan on ry's interpreter by default; `--runtime gnu-r` (or
`RY_RUNTIME=gnu-r`) keeps the GNU R path. `ry repl` and plain-R `load_all()` stay on GNU R.

- **Previous shape:** the plan crossed to R as an RDS file whose code entries carried source text,
  which GNU R parsed again on force, so every forced function was parsed twice (once by the
  analysis, once by R).
- **Target shape:** one R runtime (`runtime.R`) with backend hooks (`.ry_request`,
  `.ry_sources`, `.ry_bind_lazily`, `.ry_run`). GNU R's hooks (`gnur.R`) keep the RDS plan and
  `makeLazy`; ry's hooks (`ry.R`) call `.Internal(ryLoaderPlan/Commit/Bind/Run/Force)` in
  `loader::ry_runtime`, which keeps the plan in process. A lazy binding is a promise over a call to
  `ryLoaderForce(tree, start, end)`, where `tree` is an external pointer to the file's `Parse`
  from the analysis, and forcing lowers the node spanning `start..end` with
  `interp::lower::lower_node`. The plan carries byte offsets (`SourceSpan::source_start/end`) so the
  node is found without text.
- **Why a tree, not text:** the analysis already parsed every file, the rowan green tree is shared
  and cheap to clone, and lowering a node skips the lexer and parser entirely. Memory is the trees
  the analysis already holds. The finalizer drops a tree after no promise of its file remains, at the
  collector's next finalizer point (a native call's return, `gc()`, or exit), not immediately.
- **Why `.Internal`s and not `.Call`:** the interpreter registers Rust builtins directly, so the
  backend needs no `DllInfo`, no RDS round trip and no source strings in R memory.
- **Planning runs on its own thread** with `ANALYSIS_STACK_SIZE`, as every analysis entry point
  does, because the analysis recurses as deeply as the code nests.
- **Runtime fixes the load path needed, each pinned by an oracle case:** `readDCF`, `load()`/`save()`
  of XDR workspaces (`R/sysdata.rda`, gzip, bzip2 or xz through `gzfile()`), `readChar` on a connection, comparisons and `&`/`|` keeping
  only names, dim and dimnames, and two hot paths (`match()` on interned ASCII strings by pointer,
  `names(env)` and `ls()` from print names without a string round trip).
- **Impact:** on the 326,000-line bench package a cold load is faster than the GNU R backend on the
  same machine and a one-function reload slightly slower (932 ms and 58 ms against 1,036 ms and
  53 ms); on the 14,000-line package the cold load is slower (104 ms against 82 ms), because the
  interpreter evaluates the eager statements more slowly than GNU R. Correctness
  and the reload logic are shared, because both backends apply the same plan; `test_load.rs` runs its
  namespace, S3/S4, collation and reload tests on both runtimes.

# Decision record: base R's linear algebra

- **Previous shape.** `%*%`, `crossprod` and `tcrossprod` were a plain Rust loop in `arith.rs` with no complex support, and base's `.Fortran` routines (`dqrdc2` and friends) were listed by `getRegisteredRoutines` with null addresses, so `qr()` failed. The routine list was a hard-coded copy of names and arities.
- **Target shape.** `crates/interp/src/linalg.rs` calls the system's BLAS and LAPACK, opened at runtime with `dlopen` (the workspace builds without them; an operation that needs a missing library is an R error), with GNU R's arguments, workspace queries and gfortran's hidden character lengths. On the same libraries the results are GNU R's bit for bit, which `crates/interp/tests/test_linalg.rs` pins.
- **`dqrdc2` is Rust, written from its description** (R's LINPACK variant, which no system library ships), as are the `dqrsl` family and `dtrco`. They still call BLAS level 1 (`dnrm2`, `ddot`, `daxpy`, `dscal`, `dasum`) because those decide the bits: reference `dnrm2` is a scaled algorithm, not a plain square root of a sum.
- **One table of base's native routines**, `native::BASE_ROUTINES`. `getRegisteredRoutines` derives from it, and every `.Call`, `.C` and `.Fortran` (the interpreter's and libr's) asks `native::call_base_routine` first, so `qr()` works with or without libR. A routine is base's when its `NativeSymbolInfo`'s `dll` is named `base`, or when it is named by a string with no `PACKAGE` or `PACKAGE = "base"`.
- **GNU R's quirks are kept on purpose**: `%*%` uses BLAS unless an operand may hold a NaN or an infinity, then a plain loop (which also stands in when BLAS is missing, since reference BLAS sums in the same order); `La_solve`'s condition check norms the already factored copy when `a` was not double; `La_dgecon` estimates with `n` as the leading dimension, which ry refuses for `m < n` instead of reading past the matrix.

# Decision record: the default packages' utils, tools, grDevices and graphics routines

The routines live in `crates/interp/src/{utils,tools,graphics}.rs`, wired by package in
`builtin_libraries::implementation`. Each behavior was derived from GNU R's documentation and its
observed output (the oracle cases `io__*`, `colours__*` and `utils__sizes_hashtabs_and_tools`),
never from its C source.

- **The session has only the null device.** `dev.cur()` is device 1, `.Device` and `.Devices` are
  bound at startup, and every grDevices or graphics routine without an entry in the tables fails
  with "graphics devices are not supported in ry yet" (`graphics::no_devices` is the fallback, so a
  new routine needs no stub). `cairoProps` reports no cairo, so the default device is `pdf` and
  `bitmapType` is `"Xlib"`, as in a headless build. A real device is a later step.
- **The palette is thread-local state in `graphics.rs`**, packed as GNU R packs colours (red in the
  low byte), because only the colour routines read it and the interpreter is single-threaded.
- **`scan` is implemented in `utils.rs`, not `connections.rs`**, because `scan`, `readtablehead`
  and `countfields` share one tokenizer and only `read.table` drives all three. Connections expose
  their unread input through `connections::with_input`, and `pushBack` splices into it.
- **A hash table keeps its entries in R vectors behind the external pointer** (type, keys, values,
  hashes, count), and a lookup compares a hash that agrees with `identical()` before calling it. The
  collector then sees every key and value without a Rust-side table it would have to trace.
- **`contourLines` reproduces GNU R's line order and direction** by rules found from observation,
  written on `graphics::contour_level`: segment edge order, the saddle rule, the triangle for one
  missing corner, the nudge of values equal to the level, and closing a loop. A saddle whose
  bilinear saddle value is within rounding of the level can still pick the other pairing.

## Cold-pass memory and time: interned names, compact HIR, parallel setup

- **Previous shape.** Every HIR expression, naming binding and resolution owned its own `String`
  for each identifier, `Vec`s with spare capacity held block and argument lists, the HIR arena kept
  its growth slack, per-item memos used hash maps keyed by `ExprId`, and each parse allocated a
  private rowan node cache. `ry check` then re-parsed every file serially twice (attached
  namespaces, used tokens) before analysis, and dropped the whole database at exit.
- **Weakness.** The same spelling was stored once per mention (a name like `x` appears hundreds of
  thousands of times), the setup phase was single-threaded, and the drop walked every
  allocation only to exit. Peak memory was about 1 GiB and the cold pass 6 s.
- **Target shape.**
  - `hir::Symbol` is a process-wide interned name: `Copy`, two words, leaked `&'static str`. HIR,
    naming and the check output carry `Symbol`. Project-level maps stay keyed by `String` so their
    lookups keep borrowing `str`, converting at the boundary.
  - HIR lists are boxed slices and `lower_item` shrinks the arena, so a memoized item holds no
    slack. `ItemCheck.expression_types` is a dense table indexed by `ExprId` (ids are dense within
    an item).
  - Parsing shares one rowan `NodeCache` per thread, replaced every 256 parses so a long-running
    server does not pin the garbage of replaced trees.
  - `check` parses each file once on all cores while collecting attached namespaces and used
    tokens, forgets the database instead of dropping it, and the binary uses mimalloc (a default
    feature; massif only sees the system allocator, so profile with `--no-default-features`).
- **Impact.** Cold check 6.1, 6.7, 6.2 s to 3.4, 3.6, 2.8 s and peak 1 GiB to about 770 MiB on the
  three benchmark code bases, with byte-identical diagnostics. Item-level firewalls are unchanged
  (the new types compare structurally, so backdating still applies), and edit latency fell from
  3 to 5 ms / 95 to 120 ms to 2 ms / 45 to 55 ms (measured, cause not isolated: the allocator and smaller
  memos both touch the incremental path).

# Decision record: the built-in frame

ry's typed table type is spelled `frame`. It is a Polars `DataFrame` held by the `table` crate,
which links Polars as a Rust crate, and the checker types it as `frame{col: type, ...}`.

- **The name is `frame`, not `table`.** Base R's `table()` and its `"table"` class must keep
  working, so the type, the constructor `frame(...)` and the runtime class
  `c("frame", "data.frame")` use a name base R does not give a meaning to in a type position.
  `graphics::frame()` exists, so the checker treats `frame()` as the built-in only with at least one
  argument and when the project defines no `frame` of its own.
- **At run time `frame` is one closure that delegates a call without arguments to
  `graphics::frame`**, and it is bound in an environment attached ahead of `package:graphics` on
  the search path. `graphics::frame` is `plot.new`, which takes no arguments, so every call that
  worked before still runs graphics' code, and every call with arguments, which was an error, now
  builds a frame. R's own lookup then gives the checker's rule for free: a `frame` defined in
  `.GlobalEnv` or a package namespace is found first, and a package that imports `graphics::frame`
  keeps it. The rejected options: binding it in base (top-level code finds `package:graphics` before
  `package:base`, so `frame(a = 1)` would hit `plot.new`), and renaming the type (the name was
  chosen to stay clear of base's `table`). The cost is that `identical(frame, graphics::frame)` is
  `FALSE`, which no known code tests. The interpreter wiring that binds it is still open.
- **`:=` has value semantics in the checker.** `f[, y := e]` rebinds `f` to the extended type, and
  aliases keep the old type. Column sets are exact. At runtime the update is copy-on-write: the
  interpreter passes an unshared `&mut Table` (reference count one) and the update happens in place,
  or it calls `Table::fork` first, which copies only the column list, because Arrow buffers are
  immutable and shared. data.table's own `:=` on a `data.table` keeps its by-reference semantics.
- **`:=` writes only a slot the current scope already owns** (`naming.rs`, the `Index` arm).
  Naming runs before types, so it cannot tell a frame from a `data.table`. Minting a local binding
  for every `NameRef[, y := v]` made a global `data.table` updated inside a function resolve to the
  `:=` statement, which broke goto-definition, references and rename. So a top-level name, a local
  or a parameter is rewritten in place, and any other subject binds nothing. For a global frame the
  checker still records the update as a refinement of the non-local name (`non_local_slots`, the
  mechanism guard narrowing uses), so later reads in that function see the new columns.
- **`:=` into an existing column keeps the column's type unless the value replaces it whole**,
  because data.table coerces the value to the column. The engine (`assigned_type` in `query.rs`)
  keeps the type under an `i`, under a `by`, and for a value that collapses to one row; a full-length
  value with neither replaces the type. A string written into a factor keeps the factor and appends
  the new labels as levels in order of appearance, as data.table does. The checker cannot see that
  `sum(x)` is one value, so it treats only a literal as one (`is_literal` in `check.rs`), because a
  scalar-like type such as `as.character(x)`'s is not a length guarantee. A column the statement
  creates takes the value's type.
- **Queries go through a small IR** (`crates/table/src/query.rs`), not straight from R syntax to
  Polars, so the interpreter, the checker's result types (`result_type`) and the tests share one
  description of `tbl[i, j, by]`. Lowering gives operations R semantics on top of Polars: integer
  `+ - *` overflow to `NA`, a top-level integer `sum` that does not fit becomes a double (as in
  R 4.3 and data.table's gsum), a NaN comparison is `NA`, `max`/`min` return NaN when one is
  present, `!`/`&`/`|` treat a nonzero number as `TRUE`, aggregates propagate `NA` unless
  `na.rm` (a `null_count` guard that costs nothing when the input has no nulls), `%/%` floors, `%%`
  takes the divisor's sign, and `cor` is `NA` where Polars gives `NaN`.
- **data.table's layout is the contract for joins**: rows follow `i`, `x`'s columns come first with
  the join columns carrying `i`'s values, then `i`'s other columns, a clash prefixed `i.`. Join
  columns of different types meet through a common type (two factors as strings), and the join
  column is cast back to `i`'s type.
- **`by` keeps first-appearance order and `keyby` sorts and keys**, matching data.table, through
  `group_by_stable` and a sort.
- **ALTREP lives in the `table` crate behind the `runtime` feature**, which links `libr` and `heap`.
  The dependency points from the engine to the runtime so that `libr`, which every native package
  loads, never compiles Polars. An exposed column's `data1` owns the Arrow array and `data2` holds
  R's layout once something needs it; a column that R never materialized comes back from
  `import_column` as the same buffer.
- **NA is Arrow validity, NaN is a value.** A column without nulls has the same bytes in both
  layouts, which is what makes the zero-copy paths possible.

- **The ALTREP object is the only R-layout copy of a column.** `Table` once tracked its own
  per-column mirrors next to the ALTREP `data2`, two sources of truth for the same fact. `Table`
  now holds only the Polars frame and its key; residency is read from the ALTREP object.
- **Keyed joins without `on` match x's key positionally against i's key, or against i's leading
  columns when i has no key, and NA keys join each other** (`join_nulls`), all as in data.table.
  `Join.other` is a `Table` rather than a bare `DataFrame` so that i's key reaches the lowering.
  An equi join keeps i's order and then x's (`MaintainOrderJoin::LeftRight`), so several matches
  for one row of i come back in x's order. A rolling join marks x's rows with a
  hidden column so an inner rolling join keeps matches whose values are NA.

- **Native kernels run below Polars, with the Polars plans as the fallback and the reference.**
  `kernel.rs` (row-wise expressions and `:=`), `group.rs` (grouped aggregates) and `join.rs` (equi
  and rolling joins) cover the shapes the db-benchmark exercises and return `None` for anything
  else, and the query then runs as a plan. Polars' lazy engine pays for planning, for widening
  integers to 64 bits to detect overflow, for rebuilding a whole column when `:=` touches a subset,
  and for a per-column `take` that nests parallelism badly, which is where data.table won.
  `query::execute_with_plans_only` skips the kernels, and the differential fuzz in
  `tests/test_table.rs` checks every kernel against it (`FUZZ_ITERS` raises the iteration count).
- **Every `Table` column is one contiguous chunk** (`normalize`, `put_column`, `set_key`,
  `from_normalized`). Kernels read a column as one slice, and the CSV reader returns many chunks,
  so without the invariant every query copied its keys and inputs again (a third of an
  integer-keyed group-by's time).
- **Grouping maps keys to dense codes**: factors by their codes, integers by `value - min`, logicals
  by two values, combined as mixed-radix digits of one `u64`. A code range no wider than the rows
  (and at most 2^20) indexes states directly, and a wider one hash-partitions, because dense states
  cost memory per possible code; dense runs split across threads only when the rows outnumber
  every thread's states. The equi-join kernel bounds its buckets the same way, by the rows on
  both sides. Each aggregate is a `State` that is updated per row, merged across
  threads, appended across partitions and finished per group in output order.
- **Moments use Welford updates with a symmetric co-moment and merge with Chan's formula**, so
  `cor(a, b)` equals `cor(b, a)` bit for bit. They differ from Polars' two-pass result in the last
  few digits, which is why the fuzz compares doubles to 1e-6.
- **Median collects `(slot, value)` entries, counting-sorts them by slot in parallel, and selects
  each group's middle in place**; a valid `NaN` without `na.rm` falls back to the plan, whose
  ordering of NaN matches R's.
- **Join output gathers column by column through `gather_column`**, parallel over rows, with
  factors gathered by code and unmatched rows marked in validity. The rolling join searches
  integer and double times natively after a parallel sortedness check.
- **`mult = "first" | "last"`** keeps one match per row of `i`, as data.table does; `"all"` is the
  default.
- **R's integer `sum` is a double exactly when a total overflows**, as in R 4.3, and a group that
  holds an `NA` (without `na.rm`) is `NA`, not an overflow. A top-level sum keeps 64 bits until
  its totals are known (`narrow_sums`). A sum nested in a selected expression is staged: the
  aggregates first, then the expression (`staged`); after a join, the joined rows are built first
  and the query reruns over them. A new `:=` column nesting one is computed both
  as written and with each sum over doubles (keeping the expression's own type, so `sum(v) > 0L`
  stays logical), and hidden probe sums pick the version
  (`assign` in `query.rs`). Where data.table's grouped `:=` takes the column type from the first
  group, the engine makes the column a double if any group overflows.
- **Non-equi joins and `.EACHI` follow data.table, with one naming exception.** A join condition is
  an `On { x, op, i }`; an ordering compares numbers only (data.table refuses characters too), and
  a rolling join must be all `==`. `join_layout` is the single description of the output columns,
  read by the plan, the kernel and `join_schema`: `x`'s columns with a join column taking `i`'s
  values from its first condition, then one column per further condition on the same `x` column
  (`t.1`, `t.2`), then `i`'s unused columns. data.table names those extra columns `t, t`, a
  duplicate that ry does not reproduce. A name that still repeats (a real column `t.1`, or `i`
  holding both `v` and `i.v`) takes the next free numeric suffix, as `make.unique` would. NA and NaN match only the same missing value, and only
  under `<=`, `>=` or `==`, as data.table does. The kernel (`non_equi` in `join.rs`) groups the
  equality conditions, sorts each group's rows of `x` by the first ordering column and narrows
  every row of `i` by binary search, checking any other condition per candidate. `.EACHI` reruns the
  query as a join with hidden row numbers and groups by the row of `i`, so `j` sees an unmatched row
  as one NA row with `.N` zero, and a column of `i` outside an aggregate is that row's value.
- **Warnings are data in `Executed`, not side effects.** `execute` returns the output with a list
  of `Warning`s, which the interpreter raises as R warnings after the query succeeds, so a failed
  query raises none and the kernels and plans can be compared on them. Two exist, raised where
  data.table raises its own: an integer `sum` that a grouped query (`by` or `keyby`) selects plainly
  or assigns with `:=` and that overflows (one warning per column, none for an ungrouped, nested or
  `.EACHI` sum), and a `:=` into an existing column whose type loses part of the value (a double
  truncated or out of range in an integer column, a number other than 0 or 1 in a logical one).
  NaN becomes NA silently, as in R. In a grouped join, `j` is typed against the joined rows, whose
  join columns carry `i`'s types.
  The wording is ry's. A narrowing `:=` skips the kernel, because detecting the loss needs the
  value before the conversion; it is not a db-benchmark shape.

## Records, tuples and tags are ry values, not R lists

- **Previous shape (draft only):** a record or tuple was a plain list and a tag a classed list with
  an `ry_tag` attribute, so R and C code could read them unchanged. That made R's list semantics
  (attributes, partial `$` matching, `c()` coercion, `names<-`) the semantics of ry's own types,
  and gave the JIT no fixed layout to rely on.
- **Chosen shape:** one heap object kind of ry's own (a type code GNU R leaves unused) holding a
  32-bit interned layout id and eight-byte slots. The layout records the tag (an interned small
  integer), the field names sorted canonically, and per-slot storage: an unboxed length-one
  `double`, `integer` or `logical`, or a traced handle. Values are immutable with value semantics.
- **Types:** `record{...}` and `tuple{...}` are distinct from `list{...}`, since the values differ.
  Tags are structural; a tag union is an ordinary union, `..` makes it open, and there are no row
  variables, which keeps the checker Hindley-Milner.
- **R boundary:** explicit. `as.list()` converts out, `as.record()`/`as.tuple()`/`as.tag()` convert
  in. Each kind has an implicit class (`ry_record`, `ry_tuple`, `ry_tag`) for `class`, `inherits` and
  S3 dispatch. Printing, `identical`, `all.equal`, field access, the type predicates and builtins
  that only store a value (such as `list`) accept native values; every other builtin raises one
  error that names the conversion. C code sees an opaque object (`TYPEOF` 27): list element moves
  work and every other accessor raises the same error, because a deep scan at `.Call` would cost
  time in proportion to the data. `serialize` refuses them, so they never leave the session.
- **Layout:** keyed on tag and sorted field names only; which slots hold raw scalars is a per-object
  mask, so same-typed values share a layout id whatever their contents.
- **Impact:** plain R is untouched, because every new form is a syntax error in R. Field reads with
  a known type compile to a slot load after one layout-id comparison, and tag tests are integer
  comparisons. The design draft is `docs/src/content/docs/contributing/design/records-and-tags.md`.
- **Raw slots under NaN prefixes:** a raw integer or logical sits under its own quiet-NaN prefix, so
  the one-bit mask suffices and a slot read needs no layout lookup to know its storage. The object
  keeps its slot count in header padding, so the collector never reads the layout table.
- **The refusal is a builtin flag, not a scan:** `BuiltinDef::accepts_native` is set at startup from
  `syntax::native_values::ACCEPTS_NATIVE`, and `install_all` asserts every listed name exists, so a
  renamed builtin cannot silently drop off the list. The check tests only top-level arguments,
  because looking inside lists would cost time in proportion to the data.
- **The checker reads the same list.** It reports a record, tuple or tag passed to an R function
  that is not on it as a `type-mismatch`. The list lives in `syntax` because both `semantics` and
  `interp` already depend on that crate, so the static error and the runtime refusal cannot drift
  apart. The checker reports only when every member of the argument's type is the same native
  kind, and never for a function the project itself defines or shadows.
- **`$<-` on a record keeps its type.** Writing a field checks the value against the field's type,
  and writing a field the record lacks is an error rather than a widening, because the runtime's
  layout is fixed by the field names.
- **Pattern encoding reuses the value constructors** (`.ry_tag`, `.ry_tuple`, `.ry_record`), so a
  quoted pattern deparses with the same code as a quoted value, and `_`/`..` stay bare symbols.

# Decision record: inline annotations at run time

ry's runtime checks a function's inline parameter and return types at its boundary, and erases
binding annotations. GNU R, through `ry run` and `ry repl`, reads the erased program. The contract
is the typing reference, "Inline annotations at run time".

**Why check at all.** An annotation that the runtime ignores can be false forever without anyone
noticing, because the checker trusts it at every call site in other files. A boundary check turns
a wrong annotation into an error at the call that broke it. It also gives the JIT a type guarantee
inside the body: after entry, a `double` parameter is known to be stored as logical, integer or
double, so a guard on it is redundant.

**Why parameters are checked when forced.** Checking on entry would force every annotated
argument, which changes R semantics (laziness, `missing()`, `substitute()`, a never-used argument
that would fail). The promise carries `gp::CHECKED_PROMISE`, and `force_promise` runs the check
before storing the value: a failed check leaves the promise unforced and still checked, so code
that catches the error cannot read an unchecked value, and the JIT guarantee below holds. A promise
forwarded through `...` is pending in several typed frames, and forcing it checks every one; the
bit is cleared only once no frame still holds it. A value that is already computed (a constant, a forced promise) is
checked on entry, because there is no later moment to hook.

**Why storage masks.** A check is one bit test of the value's `SEXPTYPE` against a `u32` mask
(`crates/interp/src/annotations.rs`). Each mask is at least as loose as the static type (vector
shape ignored, the numeric promotion order admitted, an alias, class, type variable, `Any` or frame
unchecked), so a value the checker would accept never fails at run time. Element types and
attributes would need a walk of the value, which is not cheap enough to run on every call.

**Where the signature lives.** Lowering a `function` registers its masks in a map keyed by the
formals pairlist (or the body, for a function without parameters), which closures share by pointer,
so a closure copy (`environment<-`) keeps its checks and `formals()`, `body()` and `deparse()` see no
change. The signature also holds the definition's body (and the formal names, for messages), and a
closure matches only when its body is that same object: `function` builds closures from whatever
formals it is handed, so `rlang::new_function(formals(f), body)` would otherwise inherit `f`'s
checks. The key carries `gp::TYPED_DEFINITION`, so an untyped call pays one header bit test. The map
holds a weak reference per entry, whose key identity guards against a dead object's address being
reused; dead entries are swept when the map doubles. A function without parameters whose body is a
constant or a symbol has no unique key and is not checked.

**Why bindings are not checked.** `x: T <- v` would need a check on every assignment and a choice
about later reassignments, and the checker already reports a wrong value statically. Erasing it
keeps assignment as cheap as plain R.

**The JIT.** A compiled call site whose argument types the checker proved can skip the check, because
the mask test is pure and its failure path is an ordinary R error. The pending-check stack
(`annotations::State::pending`, popped by frame depth at closure exit) is the only state a compiled
frame has to honor.

**Opt-out and strict mode.** `[check] inline-annotations = false` erases annotations and registers no
checks (`RuntimeFlags::annotation_checks`), so the setting means the same thing in analysis and at
run time. Before running, `ry run` and `ry repl` check the code as a one-file project
(`crates/ry/src/preflight.rs`): they print the `inline-annotation` and `annotation-mismatch`
warnings, and in strict mode they refuse code with an error. Other findings stay `ry check`'s, so a
plain script runs exactly as before; a script with no inline annotation and no strict mode skips the
analysis.

**The console sees whole commands.** The editor hands the console filter one line at a time, so the
filter holds an unfinished command and releases it whole. "Unfinished" is
`syntax::input_is_unfinished`: every syntax error is marked `incomplete` (an opener never closed,
or raised with only trivia left), which the parser knows and a range check cannot, because an error
at a trailing newline is reported on the last code character. ry's own console uses the same rule. Otherwise strict mode could refuse the last line of a definition whose
first lines R had already read, leaving R mid-command.

# Decision record: the JIT tier

`crates/jit` compiles hot tier-0 bytecode with Cranelift. `contributing/design/r-runtime.md`
(section 5, "What tier 1 does today") is the contract for what it does; this records why.

- **The tier interface is a hook table, not a dependency.** `interp` exposes `bytecode::Tier`
  (`enter`, `back_edge`, the back-edge threshold) and `set_tier`, plus `Machine`, `TierExit`,
  `step_one` and `load_variable`. `jit` depends on `interp`, never the reverse, and `libr`
  installs it after the interpreter. Tier 0 changes from other work land without touching the JIT
  as long as these stay stable.
- **The typed plan is the IR.** One action plus one abstract state per bytecode instruction, with
  Cranelift `Variable`s building the SSA form. A PIR-style R-level IR was deferred because the
  first goal, unboxed loops, needs no rewriting across instructions; fusion, bounds-check
  elimination and inlining will need it.
- **Compiled code shares tier 0's register window.** Boxed values live in the window or in slot
  root cells after it, so the collector sees them without stack maps, and deoptimization at any
  instruction is a matter of boxing the unboxed live values in place and returning
  `TierExit::Resume` with the pc.
- **Deoptimization returns to tier 0's loop instead of calling into it.** An earlier shape ran
  the rest of the body in tier 0 from inside the compiled frame, so a loop that deoptimized and
  re-entered through on-stack replacement on every iteration nested native frames until the stack
  overflowed. Returning the resume pc and any pending unwind keeps the native stack flat.
- **Rare slow paths hand off to tier 0.** Integer overflow, an out-of-range element read and
  `sqrt` or `log` of a negative number deoptimize to the instruction (reason `HANDOFF`, which does
  not invalidate) instead of calling a helper, because they warn, and a calling handler may read
  or rebind any variable the loop keeps in a register. Tier 0 runs them with the frame written
  back, and the next backward jump re-enters compiled code.
- **Plain arithmetic and pure builtins check for calling handlers.** They can warn, so when a
  handler is established they write back and forget the frame like a generic call. Without a
  handler a warning is only deferred, so the common case pays one helper call.
- **Variables live in slots with a runtime status** (unloaded, local, enclosing, dirty). The
  environment stays the truth for everything outside the body, so it is written back before any
  operation that can observe it and forgotten after any that can change it. This keeps reflection
  through callees (`parent.frame()`, `sys.function`) correct without proving it absent.
- **The first read of a formal whose default is an expression writes back and forgets.** The
  default is evaluated in the function's own frame and can read or assign the body's variables
  (`[.data.frame`'s `drop` reads `cols`). Supplied arguments evaluate in the caller's frame and
  need no write-back.
- **A refused body is not retried, except for `COLD`.** A `for` loop over a value with no type
  feedback yet is a temporary refusal; every other refusal and a fifth compile mark the body as
  never to compile.
- **The default mode compiles only bodies with loops.** Measured on `micro/closures`, compiling
  `function(a, b) a + b` made each call slower, because entering compiled code and forcing two
  promises through helpers cost more than tier 0's dispatch. Straight-line bodies wait for direct
  calls; `RY_JIT=force` still compiles them so the tier tests cover the same code.
- **Interrupts deoptimize.** A compiled backward jump reads the interrupt flag and deoptimizes to
  tier 0's backward `Jump`, which raises the interrupt, so there is one place that handles it.
