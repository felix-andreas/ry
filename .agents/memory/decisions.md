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

`docs/src/content/docs/development/authoring-stubs.md` is the contract for the
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

## ry has no macros

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

`stop(...)` types as `Any`, not `NULL` or `Unknown`. There is no bottom type, and `Any` is the one
type that fits every branch and return position without strict mode reporting it, so a
`if (bad) stop(...)` guard costs strict users nothing. `&` and `|` are typed elementwise (logical
scalar or vector) rather than opaque, so their operands resolve normally in naming. An operand
whose type is still a variable is not unified with scalar `logical`, as `!`'s operand is, because
`&` is mostly used on vectors and the unification made `h(c(TRUE, FALSE), TRUE)` an error; the
operator is opaque for it instead.

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

The README shows its diagnostics as the plain ASCII `ry check` prints into a pipe, not as an image:
GitHub strips ANSI escapes from a code block, and an image rendered from them is one more artifact
that nothing checks and that loses the text for search and screen readers.

The snippet-closing-rule width is read off miette 7.6, which hardcodes it.
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

# Decision record: the shipping stack is a hand-written parser, a lossless tree, and salsa

`docs/src/content/docs/development/architecture.md` describes the architecture as it stands. This
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

## Why a lossless tree, and why ry's own

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

## Why salsa rather than an in-house engine

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
GNU R cannot parse. So the inline form is the superset that `development/design/inline-type-syntax.md`
costed (generic binder, parameter, return and binding positions), typed files keep the `.R`
extension because ry executes them directly, and `declare(type(...))` stays a recognized spelling for
code that must remain valid R. The cost is that other R tools fail on a typed file, so code that must
run on GNU R goes through a strip step that moves inline types into `#:` comments. The reasoning is in
`development/design/r-runtime.md`, section 8.

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

## Inline type definitions, coercions and nominal constructors

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

# Decision record: there is no third constraint kind, so two flexible comparison operands stay unconstrained

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

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary", which builds each archive on its own CI runner (glibc 2.35 floor). Kept for the reasoning about loaders and musl.

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

# Decision record: licensing

The rule is **GPL-2.0-or-later where R-derived code is linked, UPL-1.0 elsewhere**, with
LGPL-2.1-or-later C headers and MIT editor extensions. `LICENSE` states the split and carries
the texts, and `docs/src/content/docs/reference/licensing.md` is the user-facing contract.

- **Previous situation.** The whole project was UPL-1.0, and this record, the numerical-routine
  record below and `rmath.rs`'s module docs claimed that no GPL code from GNU R was copied and
  that the numerics had been rewritten clean room by agents that never read GNU R's sources.
- **What an audit found.** About a dozen components in `crates/interp` are translations of GNU
  R's GPL-2-or-later C sources: they carry R-only identifiers (`grat_r`, `pr_w`, `iretcd`), R
  Core's own extensions to the published algorithms (the `log_p` paths in TOMS 708, the R 4.3
  asymptotic tail of `qnorm`), R's warning texts verbatim, and comments that call the code a
  port. They are TOMS 708, `pnorm`/`qnorm`, `ptukey`/`qtukey`, `pnt` and helpers in `rmath.rs`,
  the random variate generators (`distributions.rs`, `random.rs`), `plotmath.rs`, contour label
  placement and `persp` (`graphics.rs`), the `complex.c` corrections (`arith.rs`), `swilk` and
  `prho` (`stats_tests.rs`), UNCMIN (`stats_nlm.rs`), QUADPACK and Brent's minimizer
  (`stats_optimize.rs`) and the X-spline (`graphics_engine.rs`, from R's Xfig-derived
  `xspline.c`). The clean-room claim was false, so UPL-1.0 could not stand.
- **Choice.** Each of those files keeps R's copyright lines in a header that names the R source
  file and marks the file as a modified translation under GPL-2.0-or-later. A crate is
  GPL-2.0-or-later when it contains such code or depends on a crate that does (normal or build
  edges, read from `cargo metadata`): `interp`, `jit`, `libr`, `loader`, `ry-lang` and the fuzz
  targets, so the released `ry` binary is GPL-2.0-or-later. The other crates (`syntax`,
  `semantics`, `format`, `ide`, `heap`, `table`, `sniff`, `rdb`, `rcmd`, `repl`, `oracle`) stay
  UPL-1.0. The headers in `crates/libr/include` are LGPL-2.1-or-later, and `editors/code` and
  `editors/zed` are MIT. The patched `iana-time-zone` keeps its upstream `MIT OR Apache-2.0`.
- **Why GPL-2.0-or-later.** It is exactly R's own "GPL (>= 2)", so code can flow both ways
  between ry and R or CRAN packages. GPL-2.0-only is ruled out because 17 shipped crates carry a
  mandatory Apache-2.0 term (`ab_glyph`, `miette`, the polars crates, `ring` and others), which
  GPLv2 cannot absorb; with them the binary is effectively conveyed under GPL-3, which
  `LICENSE` says once.
- **Why the split.** UPL is GPL-compatible, so keeping the tooling crates UPL costs nothing
  legally, and their parser, formatter and checker stay reusable as permissive libraries. The
  test is the dependency graph, not the binary: `repl` embeds the compiled libR only as bytes
  at build time and links none of its code, so it stays UPL.
- **Why LGPL headers.** R's own public headers are LGPL-2.1-or-later, so a package compiled
  against them keeps its own licence. ry's headers match that, or packages with
  GPL-incompatible licences would inherit a question GNU R deliberately avoids.
- **Why MIT editor code.** The extensions contain no R-derived code, and Zed's extension
  registry needs an accepted licence. The VS Code extension bundles the GPL `ry` binary, so
  `just build-extension` writes a `LICENSE` into the vsix that carries the MIT text, ry's
  `LICENSE` and `THIRD_PARTY_NOTICES`.
- **Third-party notices.** `THIRD_PARTY_NOTICES` is generated by `just notices`
  (`tools/third-party-notices.py`) from `cargo metadata`, the crates' own licence files, the
  provenance headers in `crates/interp/src` and a fixed list of notices outside Cargo (Adobe
  AFM metrics, L-BFGS-B, MT19937, Xfig, IJG, bundled C libraries). `just package` puts it beside
  `LICENSE` in every archive, and `just notices --check` fails when it is stale.
- **Base R's R code is still read from the user's GNU R installation** rather than vendored.
  Licensing no longer forces this, since ry is GPL like R, but it keeps ry small and lets it
  behave like whichever R version it finds (`installation.md`).
- **Impact.** The licence now matches the code's provenance. New code may translate GNU R where
  that is the fastest route to bit-identical results, as long as it lands in a GPL crate with an
  R provenance header; a UPL crate must never gain a dependency on a GPL crate.

# Decision record: the runtime's object model, collector and native boundary

This settles step 2 of `development/design/r-runtime.md`. The code is `crates/heap` (object
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
frame keeps a hash index. Every frame also keeps a 64-bit symbol filter (one bit per bound symbol,
picked by a multiplicative hash of the symbol's address), so a lookup that walks through frames
which do not bind the name costs one AND per frame instead of a scan. Removing a binding leaves its
bit set, which only costs a scan. The filter took one inline binding slot (eight, not nine) to keep
the frame in the 192-byte size class; it halved `frame_cell`'s cost per `sd(x)`.

**A promise keeps its expression and carries its bytecode in the attribute slot.** A call site
that may call a closure records, in the constant after its call (`Op::CallGeneric`), whether an
argument is worth compiling: a call with no `function`, loop, `break`, `next` or `return` in it
(`compiles_as_promise`). The first time the site calls a closure, `compile_promise_arguments`
compiles each such argument into its own `BCODESXP` for the running closure's frame and stores
the argument list in that constant. Compiling at the first closure call rather than with the
body matters twice over: a site whose callee is a builtin never pays for promise code, and
eager compilation doubled the work at every nesting level (the builtin path compiles each
argument inline as well), so `f(g(h(...)))` cost time exponential in its depth. Promise code
itself has no closure to compile against, so a closure call inside it makes ordinary promises.
`compiled_promise` makes an ordinary promise over the source expression and puts the code
object in the promise's attribute slot, which GNU R never uses for a promise; `force_promise`
runs it with `bytecode::run_promise` instead of the AST evaluator. GNU R instead puts the code
object in `PRCODE` and reads the expression back through `R_PromiseExpr`, which would have sent
every reader of promise code (`substitute`, `match.call`, `sys.call`, deparse, the C API's
`PRCODE`) through an accessor. Keeping the expression in place means none of them changes. The C
API hides the slot: `ATTRIB`, `ANY_ATTRIB` and `DUPLICATE_ATTRIB` see no attributes on a
promise, `SET_PRCODE` and `SET_PRENV` drop the code (it was compiled for that expression and
that frame), and serialization writes no attributes for a promise. `run_promise` skips the entry
profile, the loop states and the optimizing tier, since promise code has no loops and no
closure of its own, and restores the srcref current when forcing began, as the AST evaluator's
`{` does.
Measured with the JIT off: a loop passing `i * 2 + 1` and `sqrt(i) - i / 3` to a closure runs
about 1.6x faster, and `sd(x)`, whose closure arguments are mostly bare symbols (a symbol stays
an ordinary promise), went from about 115.4k to 114.8k instructions.

**`switch` compiles to one table lookup and a chain of literal tests.** `SwitchArm dst src table`
maps the selector to an arm number through a constant table built at compile time (the tag
names, the first non-empty arm at or after each name, the arm per position, the default), and
each arm then tests that number with the existing `MatchLiteral` and jumps past the others. One
opcode with a jump table would be faster to dispatch, but every op in the JIT's control-flow
graph has at most one jump target, and the arm chain keeps that invariant, so the JIT needed only
a read and a liveness arm for the new op. A `switch` whose selector is tagged, dots or missing,
whose alternatives contain `...`, or which has two defaults stays an AST call, which keeps those
rare shapes on one implementation (`switch_compiles`). Both paths read the selector through
`eval::switch_selector`, so they share its checks and warnings. Errors match GNU R's AST
`switch`, not its byte compiler: two defaults raise "duplicate 'switch' defaults", and an empty
default that runs raises "argument is missing, with no default". GNU R's byte compiler instead
says "numeric EXPR required" for the first and returns an invisible `NULL` for the second, but
GNU R runs a closure's first call through the AST, so code that depends on either behaviour
already fails there. The ledger fold
template's per-fill cost dropped from about 19.1k to 17.8k instructions.

**The dispatch loop reads only the code and registers into locals.** `step` is one large inlined
`match`, and every field of `Decoded` it destructured into a local was reloaded or spilled per
instruction under register pressure. It now copies out `code` and `registers` and reads the
constants and the cache through `decoded` where an op needs them, and it fetches the opcode with
an unchecked index, since the code words are built in process and never deserialized. The ledger
fold went from about 17.7k to 16.7k instructions per fill. Returning a `Flow` enum from `step`
instead of its current result was tried and cost about 860 instructions per fill, because the
compiler stopped keeping the loop state in registers; codegen here is fragile, so measure every
change to `step` with callgrind.

**A closure call runs one collection check for its frame.** `frame_environment` reserves room
for the environment plus a binding cell and a default promise per formal (`Heap::reserve`), then
`bind_arguments` builds them with the `_uncollected` allocators, and `args_pairlist` does the same
for the argument list, rooting only its head. This is safe because the caller keeps the argument
values alive and the defaults are reachable from the closure; anything the batch reads that is
not, such as `match_args`' result, is computed before the reservation.

**Compiled `x[[i]] <- v` updates in place before it falls back to `complex_assign`.**
`Op::SubAssign` used to hand every element assignment to `complex_assign`, which builds `*tmp*`
promises and a replacement call per assignment. `assign_element_in_place` (bytecode.rs) now writes
the element directly when the result is certain to equal what `[[<-` or `[<-` returns: a local,
unlocked, unshared, classless, non-Array vector, a scalar position inside it, and either a
non-`NULL` value under `[[` on a list or an attribute-free scalar of the vector's own atomic type.
It applies the same sharing checks as `complex_assign` (`moves::modifying`, `maybe_shared`,
`reuse_permitted`) and clears the binding's missing bits as an assignment does. Everything else,
including coercion, growth, `NULL` deletion and `x[[1]] <- x`, still takes the general path
(oracle case `assignment__element_in_place`).

**PGO is not used for the release build, because it measured slower.** An instrumented libR
(`-Cprofile-generate` on the `interp`, `heap`, `libr` and `R` crates through a
`RUSTC_WORKSPACE_WRAPPER`, so dependency fingerprints stay unchanged) was trained on every oracle
case with the JIT off and on, and rebuilt with `-Cprofile-use`. On held-out scripts with the JIT
off, best of three, it ran 1.4 to 1.9x slower than the plain release build: period.apply 2.10 s
against 1.47 s, 300k `sd` calls 7.11 s against 4.44 s, row `apply` 1.28 s against 0.66 s, `fib(27)`
0.55 s against 0.29 s, a 200k-record `lapply` 1.28 s against 0.79 s, and 50k S4 `new`/`slot<-` 13.9
s against 8.7 s. The cause is unverified. The likeliest suspects are applying the profile only to
the runtime crates under fat LTO, or a profile that went stale against the merged module, either of
which leaves hot code optimized as cold. Retrying needs the profile applied to every crate linked
into libR (cargo-pgo does this), a check for `-Wbackend-plugin` profile-mismatch warnings, and BOLT,
which is not installed in the cloud environment. Until a retry measures a gain, the release workflow
stays as it is.

**S3 dispatch caches the method lookup from the top environment on.** `lookup_method` walks the
calling frames to their top environment uncached, because function frames gain and lose bindings
without moving any epoch. The rest of the walk (the top environment, the generic's registered
methods table, the imports, the base namespace and the search path) is cached in
`dispatch::MethodCache`, keyed by the method name, the top environment, the generic's
registered-methods table (resolved first, since replacing an existing binding's value does not
move the epoch) and whether the top environment's own frame is searched (it is not when the
calling frames end at the empty environment). An entry holds the binding cell rather than its value, since reassigning an existing
binding does not move `Heap::binding_epoch`, so a hit rereads the cell and walks again when the
value is no longer a function. The whole cache is dropped when the binding epoch or the collection
count changes: the epoch covers bindings appearing or disappearing and enclosures changing, and a
collection may free an environment whose address a later key reuses. A walk that passed a function
frame, an active binding, or a non-function binding of the name is not cached, because a later
change there would not move the epoch. xts `period.apply` with 33,335 periods went from about 262k
to 250k instructions per period (GNU R 204k) and from 1.51 s to about 1.3 s (GNU R 0.89 s; oracle
case `dispatch__method_cache`).

**The collector: non-moving mark-sweep with sticky mark bits.** A surviving object keeps its mark,
so outside a collection "marked" means "old". A minor collection marks only from the roots, the
remembered set and the escaped set, and sweeps only the pages allocated into since the last
collection, so its cost is proportional to young data, not the heap. A major collection clears the
marks first and sweeps every page. The write barrier records an old parent that receives a young
child. A vector whose writable element pointer escaped (`STRING_PTR`, `DATAPTR` on a list) bypasses
the barrier from then on, so it joins the escaped set and every minor collection rescans it. Weak
references follow ephemeron rules, and the `CHARSXP` intern table is weak, with minor collections
checking only the young strings. GNU R ages objects through several generations; one promotion
after one survival is simpler and costs some premature tenuring, which profiling can revisit
without changing the API.

**Small objects live in bitmap pages** (`crates/heap/src/space.rs`). Objects up to 2 KiB come from
one reserved address range (64 GiB, halved on failure down to 1 GiB for a capped address space such
as valgrind's) cut into 64 KiB pages of 17 size classes, the 40-byte class being a triple. Each page
keeps its mark, allocated and finalizable bits in side bitmaps, one bit per slot, stored as
struct-of-arrays per page. Reasons:
- Marking an old object tests a dense bit instead of loading its header, and sweeping is word
  operations (`dead = allocated & !marks`, then `allocated = marks`) that never read a dead object.
  Only a dead environment is visited, through the finalizable bit, to drop its Rust-side frame.
- Allocation claims a run of consecutive free slots from the allocated bitmap and bumps a pointer
  through it, so there is no free list, no young list and no per-object free. A collection gives
  the unhanded rest of each run back first ("The young generation allocates by bumping a
  cursor").
- Empty pages return to a pool any class can take, so a phase heavy in one size does not strand
  memory in its class.
- Debug builds poison dead slots and set their kind byte to `FREESXP`. Release builds do not touch
  them, so a stale handle is caught only in debug.
- Large objects are allocated one by one with the global allocator and keep a header mark bit.
Measured on a closure-call microbenchmark under cachegrind: 4% fewer instructions and 46% fewer L1
data misses than the free-list pages. Full collections on the loaded dsbench heap take about 35 ms
against GNU R's 130 ms.

**libR keeps freed memory until `gc()`.** mimalloc's purge delay is 1e9 ms (`crates/libr/src/libr.rs`),
because returning pages that stay free for 10 ms made the kernel zero them again on reuse, which was
15% of the benchmark tests' profile. The delay is huge rather than -1 (never) because mimalloc only
schedules a purge when the delay is non-negative, and the forced `mi_collect(true)` that `gc()` runs
(`frame::release_worker_memory`) carries out scheduled purges. With -1, resident memory stayed at its
peak for the whole session; with 1e9, an explicit `gc()` returns free pages as GNU R's does, which
took resident memory after the base benchmark's setup from 91 MB to 76 MB and its setup peak to
135 MB against GNU R's 136. The price is a page refault on the first kernel after a `gc()`, which
GNU R pays too (`cumsum` at 1e6 went from about 2 ms to 5 ms in the benchmark, which calls `gc()`
before each kernel).

**Roots.** The `PROTECT` stack, the precious multiset, the handle stack, registered globals, every
interned symbol, the weak-reference list, and any `RootSource` an interpreter or JIT registers. A
function that allocates while it holds a handle that nothing else roots must root it first, and collection stress is what
exposes a miss.

**Root scanning is proportional to new roots on a minor collection.** The four root stacks (globals,
`PROTECT`, handles, handouts) are `RootStack`s that remember how far the last collection scanned. A
minor collection marks only the entries above that watermark, because an entry below it was marked
by an earlier collection and a mark stays until the next major one; popping or removing entries
lowers the watermark. Permanent objects go through `Heap::add_global_root` (primitives do), and
interned symbols are heap roots already, so a `RootSource` traces only state that changes, such as
frames and handlers. Before this, unserialize held 15k handles while lazy-loading base, and every
minor collection rescanned them along with every symbol and builtin, so `RY_GC_STRESS=startup` on
an empty script took over 300 s in a debug build. It now takes about 7 s, with the `heap` crate at
`opt-level = 2` in the dev profile. Under stress, every eighth collection is major while the heap
is small, and past that one per sixteenth of the live objects, which keeps the total linear.

**Peak memory comes from what the heap keeps alive, not from the allocator.** At 1e6 rows the base
kernels' peak RSS was 1.1 to 1.6 times GNU R's (`format` was the 1.6), and three causes accounted for it:
- **Leaves skip the mark stack.** `Heap::mark` sets a leaf's mark bit without pushing it, where a
  leaf is a CHARSXP or atomic vector with no attributes that is not ALTREP. Otherwise a
  million-element character vector pushes every CHARSXP, which grows the mark stack to 8 MB, and
  the stack keeps its capacity. Every non-leaf object's attrib slot is still traced, which the
  interpreter's compiled promises depend on.
- **The young budget for large vectors is 16 MB, not 64 MB.** It counts only large vectors
  allocated since the last collection, so 64 MB let dead temporaries pile up about 50 MB above
  GNU R's peak, whose 64 MB `R_VSIZE` also counts live data. The setup of
  `tools/bench/base/kernels.R` went from 166 MB to 148 MB (GNU R 134 MB) with no slower kernel.
- **Builtins write into the result vector.** `sample.int` and the random deviates built Rust
  vectors and copied them, holding about 10 MB of scratch at the peak.
- **A large vector beyond memory plus swap is refused before it is requested.** mimalloc reserves
  address space without committing it, so `numeric(1e10)` succeeded and would have failed only when
  touched, where GNU R reports "cannot allocate vector of size 74.5 Gb" because the kernel refuses
  the request. On Linux `Heap` reads physical memory plus swap once (`memory_ceiling`) and
  returns `CannotAllocate` for a larger single object, unless `vm.overcommit_memory` is 1, where the
  kernel grants GNU R any request too. macOS and Windows have no bound, since GNU R's oversized
  requests succeed there.
- **Rejected:** mapping large vector bodies with `mmap` and unmapping them once a collection leaves
  them unused did not lower any peak (rowSums rose from 168 to 173 MB), and
  `MIMALLOC_PURGE_DELAY=0` lowered the setup peak by only 4 MB. The same peaks with glibc
  `malloc` in libR confirm that the allocator's retention is not the gap.
- **Not heap:** ry started about 13 MB above GNU R, all in clean, file-backed pages; anonymous
  memory is at parity. Releasing the relocation tables and the preflight's code pages cut the gap
  to about 3 MB ("Start-up memory: release relocation tables and preflight code pages").

**Removing a binding is O(1) amortized** (`crates/heap/src/env.rs`). The slot stays in place with a
sentinel symbol, the hash index drops its key, and the frame compacts once half its slots are
removed. Removing by shifting kept insertion order but rewrote every later index entry, so `rm()`
over a large frame was quadratic.

**The handle scope roots what the API hands out, not every allocation, and only recently.** While a
native call is on the stack, a handle an API function returns to native code goes to the heap's
handouts stack (`Heap::hand_out`), which the native call truncates on return. Rooting results
rather than allocations matters for `Rf_eval` called from native code: the evaluator's own
temporaries stay collectable, and only the value native code receives is kept.
- Previously every handout stayed rooted until the call returned, so a native loop that allocated
  and dropped temporaries kept all of them, which GNU R would have collected. A call now keeps only
  its most recent `HANDOUT_WINDOW` (1,024) to twice that; once it holds twice the window, the older
  half is dropped in one `drain`, so the cost per handout stays constant and memory stays bounded.
  The invariant this relies on: ry allocates on behalf of native code only where GNU R does, so
  code correct under GNU R, which protects what it keeps across an allocation, never needs an
  older handout. Two kinds of result break that and must not enter the window: a view ry builds
  of something GNU R stores in the argument (`FRAME`'s pairlist) is `Pinned` to the call's handle
  stack, and a field read is `Reachable` and takes no slot at all, which also keeps real
  temporaries in the window longer.
- Only the outermost API function hands out (`Runtime::api_depth`). A nested one returns to ry's own
  code, which the outer function's handle scope covers, and pushing it to the window would let it
  displace native code's recent handouts.
- A result reachable through the argument it was read from (`CAR`, `CDR`, `TAG`, the `CADR` family,
  `ENCLOS`, `R_ParentEnv`, the closure and promise fields, the external pointer and ALTREP fields,
  `PRINTNAME`) is `Reachable` and takes no handle.
- A C context saves and restores `api_depth` and the window's base with the `PROTECT` depth, so a
  jump that leaves an API function without returning through it cannot leave later handouts on
  the unbounded handle stack.
- The previous design kept one handle stack for both the Rust side and native code. Handouts got
  their own stack because the window drops entries from the middle of a call's region, which must
  never touch handles ry's own code pushed.

**Accessors are inline in `Rinternals.h` and mirrored in libR.** The header reads the kind byte
(offset 0), the ALTREP flag (offset 2, bit 1), a vector's length (offset 16), its elements
(offset 32) and a cell's three fields (offsets 16, 24, 32) directly, and calls the exported
function for anything else. `heap/src/object.rs` pins those offsets with const assertions. The
exported functions take identical fast paths before `enter`, so binaries built against GNU R's
headers skip the boundary too. A writable `DATAPTR` into a list or character vector stays out of
line, because it makes the vector escape.

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

**ry presents the version of the base R it runs, with a revision of at least 89194** (the lowest
that keeps data.table on its modern branches), and its native encoding is UTF-8. See "The presented
R version comes from the base package the runtime runs" below.

**Considered and rejected.** Source compatibility alone saves nothing, because the opaque `SEXP`
already forces every accessor to be a real function. A moving nursery cannot coexist with raw
pointers held by native code. Rooting every allocation made inside a native call (instead of every
handed-out result) would make `Rf_eval` loops from native code retain all interpreter garbage.
Defining the C wrappers for every API function in C (instead of `enter`) would duplicate every
signature in two languages for no safety gain.

# Decision record: packages and R versions in the runtime

`development/design/packaging.md` holds the design and the measurements. Nothing existed before, so
there was no previous source of truth; the choices that settle architecture are these.

- **One installed package directory serves GNU R 4.6 and ry's runtime,** because ry's `libR` is
  ABI-compatible with R 4.6's headers and reports the version of the GNU R it loads base R from. ry treats the `Built:` field as GNU R does (refusing only
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

This settles step 3 of `development/design/r-runtime.md`. The code is `crates/interp`; before it
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
- **Inline caches are validated per symbol** (`Heap::lookup_stamp`): each symbol carries an epoch
  that moves when a binding of that symbol is created or removed outside a function frame, and the
  stamp adds a heap-wide enclosure epoch that moves when any enclosure changes. A single heap-wide
  counter was invalidated by every test body evaluated in a fresh environment. Function frames
  (flagged `FUNCTION_FRAME`) are excluded because bindings appear in them on every call, so a
  cached lookup never resolves through one and the frame itself is probed directly. A function
  lookup that skipped a non-function binding is not cached, because that binding could later hold
  a function without moving the stamp. A lookup that reaches the global environment goes through
  the symbol's cached search-path cell (`Heap::global_cell`), GNU R's global cache.
- **A complex assignment's `*tmp*` does not count as a reference** (`Heap::untrack_references`,
  GNU R's `DISABLE_REFCNT`): counting it made every `x[i] <- v` copy `x`, which made fill loops
  quadratic.
- **A replacement call is built as GNU R builds it, with separate arguments.** Each level's call
  names the symbol `*tmp*` (`` `[<-`(`*tmp*`, i, value = v) ``, getters `` `*tmp*`[["a"]] ``), and
  `apply_call` receives that call plus a separate argument list of forced promises: the object
  promise's code is `*tmp*`, the value promise's is the right-hand side at the first level and the
  value at later ones. So `sys.call()` and `substitute(value)` in a method match GNU R, and
  `substitute` unwraps a chain of promises. A forced promise deparses as its code; GNU R's
  bytecode shows the right-hand side expression at every level inside compiled code (`value =
  vals[[v]]`), which ry's AST form does not copy.
- **Which call a replacement error names is chosen per message.** `Interp::assignment_call` maps the
  generated call to the reported one: the whole assignment at the first level, and `` names(x) <-
  `*vtmp*` `` for a later level, built eagerly because call resolution cannot fail. `error`/
  `error_in` resolve to it (GNU R's `error()`), while `unwind::errorcall` and `generated_call`
  keep the generated call, for the messages GNU R raises with `errorcall()`: the out-of-bounds
  `[<-` matrix subscript, vectorIndex, and "no such index at level N". Only a builtin
  replacement is mapped: a closure such as `` `f<-` `` reports its own frame call
  (`` `f<-`(`*tmp*`, value = 1) ``), as in GNU R.
- **`*tmp*` is answered from a side table, not bound.** GNU R binds `*tmp*` in the assignment's
  environment for the duration, so a replacement closure can evaluate `sys.call()[[2]]` in
  `parent.frame()`. ry pushes `(env, value)` onto `Interp::tmp_values` instead, and `find_var`
  consults it only when a lookup of `*tmp*` finds no binding. Defining and removing a binding per
  complex assignment would bump the heap's binding epoch twice and invalidate every global lookup
  cache inside a loop like `for (i in s) x[i] <- i`, and it would make `*tmp*` count a reference
  to the target, forcing a copy. `exists("*tmp*")` and `ls()` therefore do not see it.
- **"Not subsettable" errors are `notSubsettableError` conditions** with GNU R's `object` field
  (`subset.rs`'s `not_subsettable`), naming the type as `typeof` does (`builtin`, `special`), and
  `$`/`$<-` on a function or symbol raise them too.
- **Out-of-bounds subscripts are classed conditions.** `subset.rs`'s `out_of_bounds` signals
  `subscriptOutOfBoundsError` through `conditions::classed_error`, with GNU R's `object`,
  `subscript` (1-based dimension, `NA` for a vector subscript or a numeric `[` subscript) and
  `index` (the failing element for `[`, the whole vector of a recursive `[[`) fields.
- **Top-level task callbacks live in libr's C, behind two interpreter hooks**
  (`Hooks::run_task_callbacks`, `Hooks::task_callback_routine`). C packages register callbacks
  through `Rf_addTaskCallback`, so the list has to be the C one; R's `addTaskCallback` reaches the
  same list through the routine hook rather than keeping a second list in `interp`.
  `session::run_toplevel` runs them after printing the task's warnings, as GNU R does.
- **S4 objects get their S3 class vector from a cache keyed by class name** (`Globals::s4_extends`,
  filled by `.cache_class` or by calling methods' `.extendsForS3` on a miss). This is GNU R's
  `R_S4_extends_table`: without it, S3 dispatch on an S4 object sees only its own class and misses
  methods for the classes it contains.
- **`.External2` routines get GNU R's four-argument convention** `(call, op, args, env)`, which
  rlang's `ffi_eval` relies on to evaluate in the caller's environment.

# Decision record: one connection table, in the interpreter

- **Previous sources of truth.** There were two connection tables. `libr::connections` held GNU R's
  `struct Rconn` table (128 slots, the devices, custom connections, the C entry points) and an R
  function table (`PRIMITIVES`) that nothing registered once the interpreter was installed.
  `interp::connections` held a second, simpler table (its own connection records and class enum)
  with the sinks, `cat` and every connection builtin the interpreter actually ran. A connection that
  R code opened was therefore invisible to `R_GetConnection`, and a custom connection a package
  created could not be read with `readLines`.
- **What was duplicated.** The table, the slot numbering, the R-level connection object, every
  device kind and the read and write paths existed twice, and the libr copy was dead code apart from
  the C entry points. Warnings were also kept twice: libr's `Runtime.warnings` beside the
  interpreter's deferred list.
- **Target shape.** `interp::connections` owns the `#[repr(C)] Rconn`, the table, the devices, the
  sinks and every connection internal, registered with GNU R 4.3.3's arities. A package's own C
  method is called through the `Hooks::connection_method` hook, which libr implements with the
  `native.c` trampolines under a native scope, and ry's methods that C calls enter through
  `Hooks::enter_from_native`, libr's `enter`, so an R error becomes the `longjmp` C expects. The
  default `vfprintf` is native code, so its address comes in as `Hooks::connection_vfprintf`. libr
  keeps only `R_new_custom_connection`, `R_GetConnection`, `R_ReadConnection`, `R_WriteConnection`
  and `ry_connection_count`. The interpreter's root source traces what the table holds (a text
  output connection's variable and environment), replacing preserve and release. libr's
  `boundary::warn` defers onto the interpreter's warning list, which the test exports read.
- **Impact.** One table means R code and C code see the same connections, which makes the custom
  connections of curl and processx readable from R. The interpreter still builds and tests
  without libR, because native calls go through hooks whose defaults error, and the merged table
  is about 750 lines smaller than the two were.
- **`closeAllConnections` is not an internal.** GNU R defines it as a base closure over
  `getAllConnections` and `close`, so ry registers no internal for it.

# Decision record: the full C API surface and package installation

This settles the part of step 4 of `development/design/r-runtime.md` that does not need the
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
- **The graphics engine C API is real** (`crates/libr/src/graphics.rs` over
  `crates/interp/src/graphics_engine.rs`); see "One graphics engine for base graphics, grid and
  device packages" below.
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
  package and type), registered by (class, package) symbols. Its bytes start with GNU R's
  method table (`altinteger_methods_t` and relatives) because vctrs calls `Extract_subset`
  straight through `STDVEC_DATAPTR(ALTREP_CLASS(x))`; every slot is a trampoline into ry's
  dispatch, so the table depends only on the type and ry's own three-state table stays the
  single source of truth. `Rf_coerceVector` of a plain
  integer or double vector to character returns a deferred string, as GNU R does.
- **Serialization writes ALTREP objects as `ALTREP_SXP` in version 3** and reads them through
  the class registry (loading the package's namespace for an unknown class when an interpreter is
  installed), so compact sequences round-trip byte for byte with GNU R. GNU R bytecode reads as
  the expression in its constant pool, because ry does not run GNU R bytecode. Environments are
  written unhashed. Namespace and package references go through a resolver the interpreter
  replaces; the default creates registered placeholders.
- **A connection is identified by its `conn_id` external pointer object,** validated on use, so
  a stale connection object errors with "invalid connection" instead of reaching a reused slot.
  The pointer's address is not the identity, because packages overwrite it (curl stores its
  multi handle there with `R_SetExternalPtrAddr`).
  Buffering lives in each connection kind, not in `Rconn`'s `buff` fields, so
  `R_ReadConnection` after `readLines` sees exactly the unread bytes.
- **Header export coverage is checked by the compiler** (`-aux-info` and `-E` in
  `tests/test_headers.rs`), not by scanning header text, because rename macros and struct fields
  defeat a text scan.
- **Package installation is the `rcmd` crate, and it runs on ry.** ry's R home has ry's
  headers and `libR.so`, an `etc/Makeconf` written for ry and `share/make/shlib.mk`. It links
  everything else from GNU R's home: the `library/` packages (base R's R code), `share/`,
  `etc/` and the `R CMD` scripts in `bin/` (build, check, Rdconv, ...). Its own `bin/R`,
  `bin/Rscript` and `bin/Rcmd` exec `ry-rcmd`, which answers `R CMD INSTALL|SHLIB|config` and
  `R RHOME` in Rust and runs every other command line (`-e`, `-f`, a script, stdin) on ry's
  interpreter by dlopening the home's `libR.so` and calling `ry_run`. GNU R's `R CMD` scripts
  call back into `bin/R`, so they run on ry too; no GNU R process runs at install time.
  - Install: Rust configures and compiles `src/`, then an embedded R driver
    (`crates/rcmd/src/install.R`) runs the R-level half on ry with GNU R's own `tools`
    functions, in `tools:::.install_packages`' order (DESCRIPTION and `Meta/`, collated code,
    `sysdata`, lazy data, demos, `inst/`, the lazy-load database, Rd objects and help indices,
    vignette index, `nsInfo`), then test-loads in a fresh ry process. Reusing `tools` keeps the
    installed layout GNU R's byte for byte in structure, so GNU R can read what ry installs.
  - **Nothing is byte-compiled.** ry compiles closures itself when they run, and GNU R
    bytecode in a lazy-load database is read back as its source expression anyway, so a GNU R
    compile step would only cost install time. `ByteCompile: yes` is ignored.
  - The previous shape delegated `bin/R` and `bin/Rscript` to GNU R and kept `R/` as sources;
    both are gone.
- **Rmath, Applic, LINPACK and the Fortran helpers are Rust** (`rmath.rs`, `applic.rs`,
  `linpack.rs`, `utils.rs`). Much of Rmath translates GNU R's nmath sources, which each file's
  header names (see "licensing"); LINPACK is written from the published algorithm. The oracle is GNU R itself: `tests/data/rmath_cases.R` records answers and random
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
- **Some numerical routines were rewritten from the published algorithms.** The LINPACK QR
  family (`dqrdc2`/`dqrls` and friends), the optimizers `vmmin`, `nmmin`, `cgmin`, `samin`,
  `R_max_col` and `R_pretty` were rewritten from LINPACK's users' guide, Nash's *Compact
  Numerical Methods*, Nelder and Mead, and the Wilcoxon and signed-rank recurrences; a licence
  audit found the LINPACK rewrite genuinely restructured. The rest of Rmath was not rewritten
  this way: TOMS 708, the normal, t and studentized-range functions and the random variate
  generators translate GNU R's C (see "licensing"). Results match GNU R exactly except where the
  system BLAS (OpenBLAS) reorders sums; those cases carry a named tolerance. The rewritten incomplete
  gamma, noncentral chi-squared and beta, and discrete quantiles are more accurate than GNU R in
  far tails (checked with mpmath), so `test_rmath` accepts a quantile when ry's own distribution
  function maps GNU R's answer back to p within GNU R's error, and a noncentral function whose
  Poisson index exceeds i32 returns NaN with a precision warning. SANN does not accept a
  custom candidate generator and always takes the Gaussian step.
  - Some Rmath helpers and quantiles were rewritten from the published methods: `stirlerr`
    from Loader (2000), with the half-integer table recomputed in mpmath; one shared
    Horner-evaluated `progression_series` in place of the continued fraction behind `log1pmx`
    and `lgamma1p`, whose surrounding code still follows GNU R's `pgamma.c`; `qgamma` as Halley's method on
    `ln P(ln y)` from Wilson-Hilferty and the leading series and asymptotic terms (Abramowitz
    and Stegun 26.4.17, 6.5.29, 6.5.32), polished to the closest double; `dnchisq` as the
    Poisson mixture summed outward from its peak (Johnson, Kotz and Balakrishnan), with
    Patnaik's two-moment approximation past a peak of 1e10; `d1mach`/`i1mach` from the PORT
    paper (Fox, Hall and Schryer 1978). `qgeom` keeps an absolute 1e-12 fuzz because that is
    what GNU R's answers show. GNU R's `dnchisq` truncates its sum at an absolute 5e-15, so in
    the tails ry differs from it but matches mpmath; `test_rmath` allows that absolute error.
- **One L-BFGS-B, written from the BSD reference v3.0** (Zhu, Byrd, Lu and Nocedal's Fortran,
  `lbfgsb.f`). `crates/interp/src/stats_lbfgsb.rs` is the only implementation: `optim` calls it
  through `stats_optimize.rs`, and libR's `lbfgsb` entry point only maps `nbd` and wraps the C
  callbacks. GNU R behaves as a hybrid of the versions, found by comparing every iterate bitwise
  (`%a`) against Rscript: the subspace step backtracks to the box without v3.0's Morales-Nocedal
  projection, and the Cauchy search keeps the `f2` floor (`eps * f2_org`) and is skipped for an
  unconstrained problem once memory exists. The order of additions matters to the last bit and
  is pinned with comments where it is not the reference's (grouped terms in `cmprlb`, `cauchy`,
  the subspace product and `formk`'s (2,1) block, ungrouped in its diagonal blocks). Traces,
  messages and the abnormal line-search exit were matched the same way, and
  `stats__lbfgsb_paths` and `test_numerics` hold the result exactly. The trace text for a line
  search that exhausts 20 evaluations is unverified, since no case reached it in GNU R.
- **`nlminb` runs PORT's own drivers, written from netlib's reference** (<https://netlib.org/port/>,
  the code R's `COPYRIGHTS.portsrc` relies on; GNU R's `portsrc.f`/`port.c` were never read).
  `crates/interp/src/stats_port_minimizer.rs` holds `DRMNG`/`DRMNGB` (gradient: BFGS factor,
  double dogleg), `DRMNH`/`DRMNHB` (gradient and Hessian: More-Hebden steps) and
  `DRMNF`/`DRMNFB` (finite differences around the secant drivers), behind one reverse-
  communication `Minimizer`. GNU R's driver, found by black-box tests: a Hessian without a
  gradient is ignored; it loops "call PORT, then evaluate the gradient (and Hessian) if PORT
  asked for it and one exists, else the objective" until the code is 3 or more, so the objective
  runs once more, uncounted, at the final point; a NaN objective warns "NA/NaN function
  evaluation" and becomes Inf; the Hessian's lower triangle is read by rows; `trace` prints at
  PORT's `DITSUM` points when `niter % trace == 0`; dparck's control checks report the last bad
  `v` slot and a nonpositive `scale` gives code 18. Quirks kept to match: `DRMNGB`'s `G0` shares
  storage with `DD7DGB`'s and `DQ7RSH`'s scratch, `DD7DGB` leaves `NRED` unset with no free
  variables (a start on an active bound where the objective is 0 returns code 3 after one iteration), and `DS7BQN` keeps
  the last violating index. `DV2NRM` is BLAS `dnrm2` (LAPACK 3.12's Blue algorithm), not PORT's
  scaled norm, because GNU R links PORT's norm to the BLAS: with PORT's own norm the iterates
  differ in the last bit from the second step. `stats__nlminb_port_paths` pins the matching
  cases bitwise; the open divergences are in `backlog.md`. `nls(algorithm = "port")` still runs
  ry's own trust-region method in `stats_port.rs`.
- **KalmanLike and `prod` accumulate as GNU R observably does.** The Kalman filter subtracts and
  adds term by term (`y -= z_i a_i`, `V[i,j] += m T`), and `prod` multiplies each argument in long
  double, combines arguments in double, and lets the first NaN win, so `prod(NaN, NA)` is NaN.
- **`R CMD SHLIB` passes `-l`, `-L` and `.a` arguments through to the link,** because configure
  scripts probe for system libraries with `R CMD SHLIB conftest.c -lfoo` (Rglpk). Other options
  are ignored, as before.
- **`Rf_rnbeta` is exported** although GNU R's library does not define it, because R's `Rmath.h`
  declares `rnbeta` and Rcpp's sugar headers refer to `::Rf_rnbeta`, so a declaration with no
  symbol fails the header coverage test and a missing declaration breaks Rcpp builds.

# Decision record: the package-development loader

Nothing loaded packages before; `development/design/r-runtime.md` section 6 and the user page
`tutorial/runtime.mdx` describe the result. These choices settle architecture.

- **The index lives in `semantics`, the diff in `crates/loader`, and only a backend knows the
  runtime.** `semantics::load_index` derives per-file facts (`file_items`, `file_binds`), the
  binding join (`package_bindings`) and the package join (`package_index`) from the analysis, with
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
- **testthat follows pkgload 1.3's `load_all()`, not convenience.** A package with
  `tests/testthat` or `inst/tests` and testthat installed gets `library(testthat)` after its code
  and before `.onLoad` (so `package:testthat` sits behind `package:<name>`), and its `helper*.R`
  files are sourced with `NOT_CRAN=true` into the attached package environment, not the namespace,
  only when the load attaches and exports everything. Helpers used to go into the namespace, which
  let package code see them and leaked them into `pkg:::`. Their names are kept in
  `state$helper_names` so a package-environment sync does not drop them. Exports sync after
  `.onLoad`, as pkgload's `setup_ns_exports` does, so a name `.onLoad` binds is attached too.
- **Deferred:** updating other development packages' import copies of a changed binding, loading a
  rebuilt shared library under a new name, a `system.file()` shim for `inst/`, and
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
- **Why a tree, not text:** the analysis already parsed every file, the tree is an `Arc` and cheap
  to clone, and lowering a node skips the lexer and parser entirely. Memory is the trees
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

# Decision record: package tests through the oracle

`oracle packages` (`crates/oracle/src/packages.rs`, `development/corpus-and-oracle.md`, "Package
tests") runs each corpus package's own `tests/` under GNU R and under ry and reports pass or fail per
package. It is the measure for "the corpus packages run on ry".

- **A file passes when its process exits with status 0, and a package passes when every file
  does**, which is the rule `R CMD check` applies to `tests/`. The `.Rout` files are not diffed,
  because they carry timings, temporary paths and addresses; testthat's summary counts are read
  from them and reported beside the verdict, which shows how far a failing package got.
- **The pass rate counts only packages GNU R passes**, because a package whose tests fail on GNU R
  (missing suggests, network, a display) says nothing about ry.
- **GNU R's results are reused** for the same package version unless `--rerun-gnu-r`, since they
  change only when the corpus does.
- **Each file runs in its own process group under `--timeout`**, killed as a group, because a hung
  test usually leaves a child (callr, processx) behind that would keep the pipe open.
- **Run it against a copied binary** (`ORACLE_RY`), not `target/release/ry`, because a full run is
  long and a rebuild under it changes the engine midway.

Fixes the harness drove are classes, each with an oracle case under `crates/oracle/cases/`:
`deparse.level` naming in `cbind`/`rbind` (read from the closure's `...` promises), the `Summary`
group's `na.rm` reordering, list comparison coercion, `return` outside a function, POSIXlt balancing,
graphical parameters, `file.append` and anonymous `file()` buffers.

# Decision record: base R's linear algebra

- **Previous shape.** `%*%`, `crossprod` and `tcrossprod` were a plain Rust loop in `arith.rs` with no complex support, and base's `.Fortran` routines (`dqrdc2` and friends) were listed by `getRegisteredRoutines` with null addresses, so `qr()` failed. The routine list was a hard-coded copy of names and arities.
- **Target shape.** `crates/interp/src/linalg.rs` calls the system's BLAS and LAPACK, opened at runtime with `dlopen` (the workspace builds without them; an operation that needs a missing library is an R error), with GNU R's arguments, workspace queries and gfortran's hidden character lengths. On the same libraries the results are GNU R's bit for bit, which `crates/interp/tests/test_linalg.rs` pins.
- **`dqrdc2` is Rust, written from its description** (R's LINPACK variant, which no system library ships), as are the `dqrsl` family and `dtrco`. They still call BLAS level 1 (`dnrm2`, `ddot`, `daxpy`, `dscal`, `dasum`) because those decide the bits: reference `dnrm2` is a scaled algorithm, not a plain square root of a sum.
- **One table of base's native routines**, `native::BASE_ROUTINES`. `getRegisteredRoutines` derives from it, and every `.Call`, `.C` and `.Fortran` (the interpreter's and libr's) asks `native::call_base_routine` first, so `qr()` works with or without libR. A routine is base's when its `NativeSymbolInfo`'s `dll` is named `base`, or when it is named by a string with no `PACKAGE` or `PACKAGE = "base"`.
- **The matrix products are ry's own kernels, not the installed BLAS.** `%*%`, `crossprod` and `tcrossprod` sum each entry's terms in reference `dgemm`/`dgemv`/`dsyrk`/`zgemm` order (columns added as axpys, so the inner loop vectorizes without reordering a sum), which is also the order of GNU R's NaN fallback loops, so one kernel serves both paths and the NaN screen is gone for reals. Previously they called whatever `libblas.so.3` was installed, so the bits followed the host: CI's OpenBLAS (an x86-64-v3 build with FMA kernels) moved `a %*% a` by 1 to 2 ULP. Now the result is GNU R's on its bundled reference BLAS on every machine and needs no library; on a machine whose GNU R uses OpenBLAS, ry's products differ from that GNU R's in the last bit, by design. Complex products use Fortran's plain multiply on finite operands (as `zgemm` does) and C99's otherwise. A faster kernel must keep each entry's sum sequential in the inner index (block over rows and columns only, no FMA); `backlog.md` tracks it.
- **GNU R's quirks are kept on purpose**: `La_solve`'s condition check norms the already factored copy when `a` was not double; `La_dgecon` estimates with `n` as the leading dimension, which ry refuses for `m < n` instead of reading past the matrix.

# Decision record: the default packages' utils, tools, grDevices and graphics routines

The routines live in `crates/interp/src/{utils,tools,graphics}.rs`, wired by package in
`builtin_libraries::implementation`. Each behavior was derived from GNU R's documentation and its
observed output (the oracle cases `io__*`, `colours__*`, `graphics__*` and
`utils__sizes_hashtabs_and_tools`), never from its C source.

- **One graphics engine for base graphics, grid and device packages.** GNU R's split is kept:
  graphics systems (base graphics, grid) call the engine (`graphics_engine.rs`), and the engine
  calls a device through the C callbacks of a `DevDesc`. `GEDevDesc`, `DevDesc`, `GEContext` and
  `GESystemDesc` are `#[repr(C)]` copies of R_GE_version 17's headers, because grid and ragg read
  and write their fields directly; a built-in device is a `DevDesc` whose callbacks are Rust
  (`graphics_render.rs`), so a ragg device and `pdf()` take exactly the same path. Previously base
  graphics drew straight into the built-in devices' shape lists and the GE entry points errored, so
  grid and ragg could not run and two drawing paths would have had to agree.
  - The display list is struct-of-arrays inside the engine's own `EngineDevice` (parallel `ops`
    and `args` vectors of the primitive and its argument pairlist), and `GEDevDesc.displayList`
    stays `R_NilValue`. A pairlist is built only where GNU R exposes one: `recordPlot` snapshots
    and `dev.copy`. Recording a call is two pushes, not a cons chain. The cost: a C device that
    tests `displayList != R_NilValue` before replaying on resize (as GNU R's own window devices
    do) never replays; ragg and the file devices never resize, so none does today.
  - `R_GE_checkVersionOrDie` accepts engine versions 14 to 17 rather than GNU R's exact match,
    so binaries built for any of R 4.1 to 4.6 load. `DevDesc` only grew into its reserved space
    over those versions. `GEDevDesc` gained `lock` before `gesd` in 4.4, and the engine serves
    both layouts by storing each system descriptor in two adjacent slots and mirroring `ask` into
    the last one (`GEDevDesc`'s doc comment).
  - `R_GE_getVersion`, `R_GE_version` in `GraphicsEngine.h` (through `Rversion.h`) and a recorded
    plot's `engineVersion` follow the R release ry presents: 16 through R 4.5, 17 from R 4.6. A
    device binary built for that release and `replayPlot()` on a plot it recorded then see the
    version they expect.
  - `GEregisterSystem` hands the caller its index (through a claim callback) before it calls the
    system on the open devices, because grid's `InitState` reads its own index on that call.
  - `patternFill` defaults to `R_NilValue`, which ragg reads.
  - `fromDeviceHeight` in inches is `value * ipr` without a sign, as in GNU R; a y-down device
    (every bitmap device) otherwise gets negative string heights, which shifted rotated grid text.
  - Base text hands both adjustments to `GEText`, so an unspecified vertical adjustment centres
    the label on its glyphs' extent from the device's metric callback, as GNU R does.
  - Base graphics hands rasters to `GERaster` and X-splines to the engine's `xspline`, as GNU R
    does, so a device package draws them itself. The X-spline is written from Blanc and
    Schlick's paper; its sampling (one point for a segment between two corners, otherwise a step
    of `2 / sqrt(length in 1/1200 inch)`, at most 0.2) reproduces GNU R's open splines exactly.
  - `plot.new` marks the device dirty, because grid starts a page of its own on its first
    drawing unless another system already drew.
  - Text widths are Helvetica's AFM widths plus its ASCII kerning pairs (tables generated from
    GNU R's `afm/Helvetica*.afm`), which makes `strwidth` on pdf match GNU R exactly. Bitmap
    devices in GNU R measure with cairo's font instead, so their widths differ slightly.
- **Built-in devices record a drawing and render it when they close.** Each built-in device's
  callbacks append `graphics_render::Item`s (a path, text or group use, with the clip and mask in
  force) in inches to the current page, and each file format is one writer over the same
  `Drawing`: PDF, SVG and PostScript as vectors, PNG, JPEG, BMP and TIFF rasterized with
  tiny-skia, text outlined with ab_glyph from the system's Helvetica-compatible fonts. Rendering at
  close keeps devices free of file handles and makes a multi-page bitmap device one file per page,
  as GNU R's `%d` names expect. Vector devices create their file at open, so a bad path fails
  there.
  - Patterns, clipping paths, masks and groups are `Definition`s in one append-only arena on the
    `Drawing`, and the reference R receives is the arena index. A mark refers to a definition by
    index, so pages rendered at close still find it after R released the reference (the release
    callbacks do nothing), and nothing is copied per use. A definition's own marks are recorded
    by running its R function with the device's target swapped to a fresh list, so nesting
    (a pattern in a mask in a group) falls out of the same callbacks. Rendering bounds nesting at
    32 levels, which only a malformed drawing reaches.
  - Device versions follow GNU R's devices: pdf, svg and the bitmap devices are version 16 (paths
    and glyphs), and PostScript is version 15, refusing every definition and reporting the
    capabilities unchanged (`NA`), as GNU R's PostScript device does. PostScript also drops
    semi-transparent colours with GNU R's once-per-page warning.
  - The bitmap devices render luminance masks, where cairo ignores them with a warning, and report
    masks as alpha and luminance; this is a deliberate improvement. `svg()` composites like
    `pdf()` (blend modes only; other operators warn and draw as `over`) because SVG has no
    Porter-Duff operators, where GNU R's cairo svg device rasterizes them. Both write isolated
    groups, as cairo does.
  - Glyph callbacks read the font file R names and draw nothing when it cannot be read, matching
    GNU R's pdf device, which is silent there.
  - A raster is one `Mark::Image` (pixels plus a placement mapping the unit square to inches),
    not a quadrilateral per cell. PDF writes an image XObject with an `/SMask` when any pixel is
    translucent and `/Interpolate true` when interpolating. PostScript writes an `image` with the
    alpha dropped and, when interpolating, resamples to the placement's size in points with GNU
    R's sixteenth-step bilinear filter, so the hex data is byte-identical to GNU R's. The bitmap
    devices sample bilinearly (interpolating) or nearest with a pixel-centre coverage mask, as
    cairo does; edge pixels' anti-aliased coverage differs from cairo's by a few levels.
  - `svg()` marks a non-interpolated image `image-rendering:pixelated`, where cairo emits no
    hint and browsers blur the cells; this is a deliberate improvement. An interpolated image is
    upsampled before embedding, as cairo does.
  - A tiling pattern with `extend = "pad"` or `"none"` in `svg()` is a cell large enough to cover
    the page, holding the tile plus its eight edge and corner strips (one point wide) stretched
    outward; the strips overlap the tile, which is drawn last, so renderers show no seams.
  - tiny-skia has no `saturate`, so `draw_items` composites it by hand on premultiplied pixels:
    `dst + src * min(1, (1 - Da) / Sa)`, which matches cairo's pixels exactly.
- **The drawing geometry is GNU R's, checked against its pdf output and ragg pixels.** A line of
  `lwd` 1 is 1/96 inch, a margin line is `mex * csi`, text sits on a margin line by its baseline
  offset by `ylbias`, and symbols are sized from half the character height. Base graphics
  computes positions in inches rather than through GNU R's NDC pipeline, so the last bit of a
  coordinate can differ. The rules below were found by diffing the plot gallery against GNU R:
  - The point size and `cex` reach the device separately (`text_context`), so rounding a size to
    whole points is the device's business, as in GNU R; base graphics rounding it first put
    ragg's text a pixel off at fractional `cex`. An inline `cex` scales text the same way.
  - User coordinates map through the plot region `usr` was set on (GNU R's `win2fig`, the
    `window` field of the parameters), not the current one, so changing `mar` after
    `plot.window` moves nothing until the next `plot.window`.
  - `mtext` and margin titles clip to the device, not the figure; `clip()` keeps its region only
    while `xpd` equals its value when `clip()` ran, as GNU R's `GClip` drops it.
  - Contour labels are a port of GNU R's `contour()` placement (the simple, edge and flattest
    methods, label-box intersection, and its quirks: the cumulative gradient variance, the angle
    computed with `180 / 3.14`), because any other placement lands labels elsewhere.
  - Labels are a `Label` (string lines, or an expression laid out by `plotmath.rs`), and every
    text path (`text`, `mtext`, `title`, `axis`, `strwidth`) goes through `push_label`, so
    plotmath is one code path rather than a deparse fallback per caller.
  - Base graphics draws `pch` 0 to 25 through `graphics_engine::symbol` (GNU R's `GESymbol`),
    which sizes symbols in device units. A second copy of the shapes in `push_symbol` had drawn
    pch 0, 15 and 5 at different half-widths and started pch 23 at another vertex, which moves
    where an antialiased outline closes.
  - A missing `adj` or `at` in `mtext` defaults by `las` and side (`ComputeAdjValue`,
    `ComputeAtValue`), not to `par("adj")`; `tck` below 0.5 is a fraction of the plot region's
    shorter side, above it of its extent across the axis.
- **The editor's plot panel is fed from the engine's display list.** A console with a
  subscribed editor makes its default device (`options(device=)`) a `pdf(NULL)` device with the
  display list enabled, when the subscription arrives so the next command already uses it. A
  default the user set is kept, but the platform's own (`pdf`, `X11`, `quartz`, `windows`) is
  replaced, since an X11 default would open a window or fail without a display. At each prompt, `publish_plot` in
  `console_server.rs` compares the current device's display-list stamp
  (`graphics_engine::display_list_stamp`, drawn from one global counter, so a device that takes
  a closed one's slot never repeats a stamp) with the last one sent; when it changed, it copies
  the display list onto an svg device with `dev.copy` (warnings muffled, and every device it
  opened closed on exit, so a replay that fails leaks neither a device nor a warning) and sends
  the file's text as a `ryPlot` event, which VS Code shows in one "R Plot" panel. A device of its own (httpgd-style) would
  duplicate the engine and the devices; the display list is already the single record of what
  was drawn. File devices keep their display list off, as in GNU R (`GErecordGraphicOperation`
  records nothing while it is off), so `png()` output is never sent.
- **Base graphics is checked against GNU R by a plot gallery.** `crates/oracle/gallery/*.R` holds
  about 150 figures (base graphics including the PerformanceAnalytics charts, grid, lattice,
  ggplot2), and `oracle gallery` renders each on both engines and every device, then compares.
  ragg (`agg`) rasterizes identically on both engines, so it is the sharp check, with a 0.2%
  differing-pixel tolerance; `png` and pdf (rasterized with `pdftoppm`) get 5%, and a pixel
  there differs only when nothing within one pixel in the other image matches it, because GNU R's
  cairo and ry's tiny-skia antialias differently (a dense tangle of thin lines, such as a lattice
  parallel plot, otherwise differs in 18% of its pixels while looking identical). The harness
  auto-prints a figure's visible value, since lattice, ggplot2 and xts draw only when printed. `svg` is rendered and timed but not compared,
  since cairo writes glyphs as paths. The figure sample is drawn from one seed, and
  `test_gallery.rs` runs a bounded sample of it in the default suite; a bug class the gallery
  finds is pinned by a `graphics__*` oracle case, which states the value (a `par`, a metric, a
  return) rather than a pixel.
- **The device table mirrors GNU R's**: slot 0 is the null device, at most 64 devices, `.Devices`
  (a pairlist whose entries carry `filepath` for a device opened on a file) and `.Device` in base
  are rebound from the table after every change, `dev.set(1)` starts the default device but
  reports device 1 as GNU R does, and any routine that needs a device opens
  `getOption("device")` first, which is how a script with no device gets `pdf("Rplots.pdf")`.
- **One copy of `par`.** GNU R keeps default and current parameters so a high-level call's inline
  parameters apply for its duration; ry reads inline parameters from the call instead, so a single
  `Pars` is the source of truth.
- **Regions remember the unit they were set in** (`Margins`, `OuterMargins`, `PlotRegion`,
  `FigureRegion` enums). Base graphics recomputes the others from that one when the device, `cex`,
  `mex` or the layout changes, so `par(mai = ...)` followed by `par(cex = 2)` keeps the inches and
  moves `mar`, and storing all units would let them drift apart.
- **Text metrics are Helvetica's published AFM widths** (regular and bold; the oblique faces share
  them) at the nearest whole point size, with the cap height for `strheight`, and the symbol
  face (`font` 5, which plotmath uses for operators, Greek and delimiters) uses the Symbol AFM.
  That matches the pdf device's numbers exactly; bitmap devices in GNU R measure with cairo fonts
  and can differ. The device's metric callback receives a glyph as its Adobe Symbol code, but a
  string arrives as the Unicode characters plotmath translated it to, so only the string path
  maps back (`symbol_metrics`, `adobe_symbol_code`): mapping both ways confuses raw code 174
  (an arrow) with U+00AE, the translation of code 210.
- **`cairoProps` reports cairo**, so `capabilities()` lists png, jpeg and tiff and the bitmap
  devices take the cairo path, which needs no display.
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

Tree eviction itself is superseded: see "Decision record: every host keeps every tree" below.
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
- **ALTREP column exposure lives in `libr` (`crates/libr/src/frame_altrep.rs`), and `libr` links
  Polars.** Frames are interpreter values, so `interp` depends on `table`, and `libr` (which
  depends on `interp`) links Polars either way; keeping ALTREP in `table` behind a feature would
  only make the engine depend on the runtime. The dependency points from the runtime to the
  engine, and `libR.so` grows by the engine's size. An exposed column's `data1` owns the Arrow
  array and `data2` holds R's layout once something needs it; a column that R never materialized
  comes back from `import_column` as the same buffer.
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
  value before the conversion; it is not a db-benchmark shape. A third warning, `Parsed`, marks
  strings written into a logical or numeric column: data.table warns on every such write, whatever
  the strings hold, so the warning is decided from the types alone, and the strings are parsed as
  `as.logical`, `as.integer` (through a double, so `"3.7"` is 3) or `as.double` would.
- **Prefixed join columns are copies, not a second layout.** `x.t` and `i.lo` in `j` or `by` are
  resolved before the join runs (`prefixed_copies` in `query.rs`): each side gains a copy of the
  named column under the prefixed name, so the copy survives the join as an ordinary column and
  every later stage (kernels, plans, `.EACHI`) needs no knowledge of prefixes. `join_layout` stays
  the only description of the visible output; `result_type` drops the copies from `j = .SD`-like
  results.
- **Under `.EACHI`, an aggregate over a column of `i` alone sees the row's one value.** data.table
  evaluates `j` with `i`'s columns of length one, so `sum(k)` is `k` and `mean(k)` is `k` as a
  double even for an unmatched row. The rewrite (`single_value`) replaces such an aggregate with
  the group's first value, using a hidden `FIRST_OF_ROW` mask where `na.rm` must see exactly one
  value; an aggregate that also reads a column of `x` is left alone.
- **The checker mirrors `join_layout`; it does not call it.** `semantics` must not depend on Polars,
  so `join_columns` in `check.rs` reimplements the layout (names, order, `t.1` suffixes, `i.`
  prefixes, `make.unique`), with the engine as the authority and the typing fixtures
  (`frame_joins`) pinning both against data.table. Condition compatibility follows the engine's
  `comparison_types`: numbers with numbers, strings with strings, logicals with logicals, and an
  ordering never on strings. A factor column is `Any` to the checker, so it is not checked.
- **Under `.EACHI` the checker types `i`'s columns as scalars inside `j`.** Each row of `i` runs
  `j` once, so a column carrying `i`'s values (a compared column or an unused column of `i`) is its
  element type in the scope, while `x`'s columns stay vectors. The groups are the compared columns
  in condition order, matching `each_i` in the engine.
- **A lossy `:=` is a type error, though data.table only warns.** Strings into a logical or numeric
  column, or a fractional literal into an integer column, when the column keeps its type (partial,
  grouped or literal write). The runtime result is almost always a bug (NA or a truncated value),
  and an explicit `as.*()` call states the intent. A number into a string column converts without
  loss and stays accepted, as R code relies on it.
- **`lapply(.SD, f)` is typed once per column, not through `lapply`'s signature.** Typed through
  `lapply`, `.SD` is a list of the union of its column types, so `f` is checked against that union
  (a false error for any `f` that fits each column) and the result has one element type. Instead
  `subset_apply` in `check.rs` types the call once per column with `.SD` read as a one-column list;
  an overloaded `f` passed by name is called directly on each column through `overloaded_call`,
  because an overload set has no single function type for `lapply` to use. A finding every column
  causes is reported once; one only some columns cause names the column.
- **A query result that repeats a column name has unknown columns.** data.table keeps both columns
  (`.SDcols` naming a group column, or `.(g = x)` under `by = g`), but a `frame{...}` type cannot
  hold two fields of one name, and annotations must round-trip.

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
  comparisons. The design draft is `docs/src/content/docs/development/design/records-and-tags.md`.
- **Field read sites** (`bytecode::Op::Field`, `records::cached_field`). `x$name` with a symbol
  field compiles to a `Field` instruction that carries a per-site shape constant, the integer
  pair `[layout id, slot]` (`records::NO_LAYOUT` until a record is seen). Tier 0 compares the
  layout id and reads the slot; any other object takes `subset::dollar_value`, `$`'s own path. The
  JIT turns a filled cache into `Intrinsic::Field`: it guards record kind, no object flag and the
  layout id, then loads the slot unboxed when the mask bit is set and the NaN-box prefix matches
  the site's feedback, and hands off to tier 0 otherwise. `$` had to join `base_primitive`'s list,
  or every site compiled as a general call. 5,000,000 iterations of `s <- s + p$x * p$y + p$z`
  take 0.074 s on a record against 1.83 s on a list (0.055 s with locals), profiling build.
- **Item, constructor and `match` sites.** `x[[k]]` with a constant `k` compiles to `Op::Item`
  with the same `[layout, slot]` shape cache (`records::cached_item`); a list or vector subject
  falls through to `[[`'s path. Constructor calls (`.ry_record`, `.ry_tuple`, `.ry_tag`) whose
  names are distinct intern their layout at compile time and compile to `Op::Construct`; a
  payload-free tag is one shared constant. A guard-free `match` compiles to `MatchShape`,
  `MatchTag`, `MatchLiteral` and `Slot` tests, sets the arm's bindings only after the whole
  pattern matched (as the special does), and ends in `MatchFail` with the special's error; a
  `match` with a guard stays with the special. The JIT branches on kind, layout and tag natively
  and reads slots through `Intrinsic::Field`; `Construct` is an opaque pure step. Profiling
  build, against the build before these sites: 5,000,000 `t[[2L]]` reads 2.27 s to 0.026 s
  (`r$b` 0.026 s, list `l[[2L]]` 0.83 s); 2,000,000 three-arm tag `match`es 1.29 s to 0.57 s
  (`switch` on a list's `$kind` 1.10 s); 2,000,000 `:Ok(i)` constructions 0.69 s to 0.13-0.23 s
  (`list(kind =, v =)` 0.35 s).
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

## Frames as runtime values

- **A frame is an `EXTPTRSXP` that owns a `Box<table::Table>`, tagged with the symbol `ry.frame`
  and classed `"frame"` alone** (`crates/interp/src/frame.rs`). A weak reference with a C
  finalizer drops the box. The table is the only copy of the data: a column becomes an R vector
  when R code reads it (`$`, `[[`, `as.list`, a data mask). It is no longer a `data.frame` (the
  class was `c("frame", "data.frame")`), because nothing about a data.frame's list layout or row
  names holds for it, and the checker agrees: a frame fails a `data.frame` parameter but passes a
  `list` one, because `as.list` gives its columns.
- **`x[i, j, by]` on a frame never reaches the `[` primitive.** `[` is a `DispatchOrEval`
  special, so `dispatch::dispatch_or_eval` evaluates only the subject, and when it is a frame
  hands the unevaluated call to `frame::run`. Dispatching through `[.frame` would be too late,
  because `i`, `j` and `by` name columns. `[.frame` still exists for calls that arrive with
  evaluated arguments (`do.call`, `Map`). The bytecode compiles `[` as a generic call, so loops
  that subset run the same path.
- **Any other object subject of `[` dispatches lazily, as GNU R's `DispatchOrEval` does.** The
  method receives the remaining arguments as promises, so `DT[, sum(x), by = g]` reaches
  `[.data.table` with expressions instead of failing with "object 'x' not found".
- **libr's option reads (`Rf_GetOption1`, `Rf_GetOptionWidth`, `Rf_GetOptionDigits`) go to the
  interpreter's `options()` when the interpreter is installed.** The `.Options` pairlist in base
  is the one source of truth; libr's own options environment serves only standalone use without
  the interpreter. With two stores, C code sees no option R code set, and data.table's `alloccol`
  fails with "Has getOption('datatable.alloccol') somehow become unset?".
- **Lowering first, R second.** `Lowering` turns `i`, `j` and `by` into the engine's IR; any part
  that returns `None` runs as R in a data mask that binds only the columns the expression
  mentions, plus `.N`, `.SD`, `.I`, `.GRP`, `..x` and `.` as `list`. Grouped R code uses Polars
  for the grouping (row indices per group) and evaluates once per group.
- **Copy-on-modify, not by-reference.** `:=` updates in place when the frame's refcount is at most
  one, and otherwise forks the table (cheap: Arrow buffers are shared) and rebinds the variable the
  call named, searching enclosing scopes, so a global updated inside a function is rebound where
  it lives. An alias taken before the update keeps the old columns, as the checker models.
  `setkey` always forks and rebinds.
- **`ry` is a registered namespace attached as `package:ry` at position 2** (`frame.rs`,
  `NAMESPACE`, `PACKAGE`, `ATTACH`), built in the shape `loadNamespace` gives an installed package,
  so `ry::setkey` and `ry::List.map` work after another package masks the bare name. It is the one
  home of the standard library and the frame functions: the builtins bind the standard library in
  a `package:ry` just above base, and session startup moves those bindings into the namespace and
  replaces that environment with the full package in front of the default packages, so `frame`
  masks `graphics::frame`. It holds the user-facing functions;
  `.frame_*` builtins do the work. `setkey`, `setkeyv`, `key` and `copy`
  delegate to data.table for anything that is not a frame. S3 methods for other packages' generics
  are registered in the generic's methods table, as an installed package's `S3method()`
  directives are; `as.data.table.frame` is registered from a data.table `onLoad` hook, because
  data.table is not loaded at startup. The checker's stubs are `types/ry.Rtypes`
  and `types/ry.exports` (the names the checker types itself or that delegate to data.table), and
  `test_standard_library.rs` keeps both equal to what the runtime binds.

# Decision record: the JIT tier

`crates/jit` compiles hot tier-0 bytecode with Cranelift. `development/design/r-runtime.md`
(section 5, "What tier 1 does today") is the contract for what it does; this records why.

- **Compile thresholds are 3000 entries and 10,000 backward jumps** (`crates/jit/src/jit.rs`).
  At 10 and 500, a package load compiled the methods package's bodies that each `setClass` runs a
  few dozen times, and compiling took half of a large package's S4 definitions (callgrind) and
  about 200 ms of its cold `ry run`. finbench test slices then picked 3000 over 1000
  (callgrind instructions against `RY_JIT=off`): on 2 files, entries 300 cost +2.9%, 1000 +1.4%,
  3000 +0.6% and 10,000 +1.0%; on 21 files, 1000 cost +2.7% and 3000 +1.5%, and raising back
  edges to 30,000 changed nothing. The
  remaining overhead is per-call tier bookkeeping (`enter`, `compiled_for`, `valid`) and
  Cranelift, not deoptimization. Hot code crosses 3000 calls within milliseconds, so the
  benchmarks time the same. Lower them only together with a cheaper first tier or background
  compilation.
- **A fully typed closure compiles at its first call** (`jit.rs`, `fully_typed` and
  `first_call`). Typed code must never run slower than the same code untyped, and before this a
  typed body paid the argument checks and still waited for 1000 entries. The declared types of the
  formals and of a called closure's return seed the analysis wherever feedback is empty
  (`analysis.rs`, `load_ty`), so the compile needs no warm-up. The guards stay because inline
  types are not sound in default mode; a failed guard deoptimizes as for any feedback. A scalar
  type (`double`) and a vector type (`double[]`) differ in the runtime mask (`annotations.rs`,
  `SCALAR`), so only scalars are seeded as unboxed. A refusal because an operand is still unknown
  (`COLD`) or because nothing would run natively does not mark the body as refused: the threshold
  path may still compile it once real feedback exists. Measured: `fib` typed 0.013 s before,
  0.007 s after, equal to untyped; `closures` typed 0.37 s against 0.42 s untyped.
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
- **Every hot body compiles, loop or not, unless it would run nothing natively.** Loop-only
  compilation was a stopgap while entering compiled code meant forcing every argument through a
  promise helper, which made `function(a, b) a + b` slower compiled than in tier 0. Direct calls
  removed that cost, so the loop-only rule went with it. A body whose plan has no loop and no
  inlined call, direct call, arithmetic, comparison, intrinsic, element assignment or `&&`/`||`
  (`Plan::runs_natively`) would only call the same helpers tier 0 does, so it is refused
  (`analysis::NOTHING_NATIVE`, permanent) instead of costing a compile; most base R wrappers are
  such bodies. `RY_JIT=force` still compiles them, so the tier tests cover them. Measured with
  callgrind over the why-ry programs (whole process): about 170M instructions fewer per program,
  on top of the higher entry threshold's 280M.
- **Function speculation is validated by the binding epoch, then by per-symbol stamps.**
  Compiling marks each speculated symbol and binding cell `WATCHED`, and the heap bumps one global
  epoch, and the symbol's lookup stamp, when a watched cell is assigned or a watched symbol is
  defined in any function frame (which could shadow the speculated binding). An unchanged epoch
  proves every speculation with one compare. The epoch also moves on every binding outside a
  function frame, which a test suite does constantly, so `valid` (`crates/jit/src/runtime.rs`)
  then compares each speculated symbol's `Heap::lookup_stamp` with the one recorded at its last
  check; re-walking every lookup on each epoch move cost a finbench test slice more than the
  compiled code saved. Only when a stamp moved is each speculation re-resolved from the
  environment its lookup started in, which for a function looked up inside an inlined or directly
  called closure is that closure's environment, not the compiled body's. A lookup that passes a
  non-function binding is not speculated (`resolve_function`), because only the binding found is
  watched, as for the tier-0 inline caches.
- **A call to a speculated closure is direct and binds plain values.** Arguments that are
  effect-free are evaluated eagerly in the caller and bound as values into the callee's frame,
  skipping promises (`jit_call` in `crates/jit/src/runtime.rs`, `eval::bind_arguments`). Early
  evaluation is only invisible when the callee cannot assign anything an argument reads before
  forcing it (`g <- function(x) { assign("y", 99, envir = parent.frame()); x }`), so the callee must
  be frame-safe (`analysis.rs`, `frame_safe`): it calls only itself and `FRAME_SAFE_BUILTINS`, each
  lookup recorded as a speculation from the callee's environment, and the call defers to tier 0
  while a calling handler is established. A frame-safe callee cannot call `substitute()`,
  `missing()` or `sys.call()`, so no promise is needed, and the frame records no argument list
  because only `nargs`, method dispatch and `standardGeneric` read it. S3 dispatch from those
  builtins on a classed argument is the one accepted hole.
- **Small local closures inline.** A call of a closure defined by a function expression in the same
  body, whose arguments and body compute a scalar from scalars without effects, runs inline on
  unboxed registers. Any failed check hands the call back to tier 0, which evaluates it lazily.
- **Environment frames live inline in the environment object.** Small binding sets are stored
  without a separate allocation, which cut a call's allocations, the dominant per-call cost.
- **A value boxed on an edge into a join is dirty.** A slot that is an unboxed local on one path and
  boxed on another gets a fresh box the frame does not hold, so the slot must be written back before
  an in-place element write is visible to the frame (`x[1] <- 9; get("x")`). The oracle case in
  `bytecode__hot_loops.R` pins it.
- **A helper that cannot allocate parks the error.** Compiled code does not branch on a nil box;
  later helpers return their fallback while an error is parked, and the return from compiled code
  raises it.
- **Static types do not remove guards.** A static `double` still admits an integer or a logical
  through promotion, and stubs claim scalar results for functions that can return vectors, so a
  static type proves neither the storage type nor the length the compiled code specializes on.
  Strict typing costs a little at run time instead (about 200 instructions per annotated call for
  the boundary checks). Using types as early profiles, or as guarantees backed by boundary checks,
  is still the plan in `r-runtime.md`.
- **A cold body waits instead of compiling with boxed guesses.** A hot body whose arithmetic reads
  an operand with no feedback, or whose `for` loop never ran, refuses to compile as `COLD`
  (`analysis::COLD`), which resets its counters without spending a compile. This is the recursive
  case: `fib` turns hot on its way down, before any call has returned, so compiling then types
  every call result as a boxed unknown. `TIER_STATE` counts compiles in its low byte and cold
  refusals above it (`runtime::COLD_REFUSAL`); after three cold refusals the body compiles with
  what it has, so a body that never fills its feedback still compiles.
- **A declared return type is the call result's early profile.** When a direct call's site has no
  feedback, its result type comes from the callee's declared return annotation
  (`annotations::declared_return_type`). A declared `double` still admits an integer through
  promotion, so this is a profile, not a proof, and the usual guards stay. Typed recursive code therefore compiles on
  the first try instead of waiting.
- **Interrupts deoptimize.** A compiled backward jump reads the interrupt flag and deoptimizes to
  tier 0's backward `Jump`, whose poll decides whether to raise it, so there is one place that
  handles it.
- **Compile latency: what was measured and rejected.** Cranelift's `opt_level = "none"` as a
  first tier did not shorten whole-process time on the why-ry programs (compiling is dominated by
  lowering and register allocation, which `none` keeps), so there is one tier. A back-edge
  threshold of 2000 instead of 500 changed instruction counts by under 4% either way. Not built:
  background compilation (compiled code embeds heap pointers and speculations validated against
  the interpreter's state at compile time, so a second thread would need the heap frozen or a
  revalidation step) and a persistent code cache keyed by body hash (the same embedded pointers
  make machine code process-specific; it would need relocations for every constant and
  speculation).
- **Inlining keeps annotation checks.** An inlined call has no frame, so `annotations::enter` and
  `check_return` never run for it. The inliner therefore inlines a typed closure only when
  `annotations::admits_inline` proves, from the argument and result storage types it inferred,
  that every check would pass; otherwise the call stays a real call. Before this, a typed callee
  inlined into a hot loop silently skipped its checks (`test_tiers.rs` pins both directions).
- **A builtin's repeated calls check the signature once.** `lapply` and `vapply` over an atomic
  vector call the same closure with one argument whose storage type is the vector's, so
  `eval::KnownFirst` resolves `annotations::entry_for` once per function and later calls run as
  `Entry::Checked`, skipping the per-call parameter checks. A list's elements differ, so lists
  check every call.
- **Arrays are their own JIT types.** `Ty::IntArr`, `DblArr` and `LglArr` come from three
  feedback kinds (`feedback::INTEGER_ARRAY` and so on, which is why feedback is 16 bits per
  instruction, stored as an integer vector). The guard reads the kind and the flag bits only,
  because the Array bit promises no attributes. `x[[i]]` reads the item directly and
  `x[[i]] <- v` stores it when `v` has the Array's exact item type and is not missing; a missing
  value, another type or a position out of range goes to tier 0, which applies the Array rules
  (a double position past the last item is an error, not truncated into range). `[` and
  arithmetic on Arrays stay generic, because their results must be Arrays checked for missing
  values. A copy for a shared Array keeps the bit (`jit_duplicate`). `List.len` is the `length`
  intrinsic on an Array or list.
- **A forgotten variable keeps its type as a guess.** An opaque call forgets every slot, since
  it may rebind anything, but the slot keeps the type it had. A later load where tier 0 has
  never read the variable (a branch not taken yet) speculates on that type behind a guard,
  instead of loading it boxed and leaving its arithmetic to tier 0 for good.
- **A failed guard after a load resumes after the load.** The load may have forced a promise,
  and only that forcing set the visibility `withVisible(x)` reads, so tier 0 continues at the
  next instruction with the boxed value instead of loading again.
- **A closure's type names its source body.** A `function` site replaces its body constant with
  the shared compiled code once it has run, so the analysis maps that code back to its source
  before building `Ty::Closure`. Otherwise the closure guard never matches and the body
  recompiles until it gives up, with every call left to tier 0.
- **A backward jump compiles impatiently.** The cold refusal (wait for code that has never run to
  record its types) applies only to compiles at entry. At a loop's back edge the loop is the hot
  code, and waiting for the code after it ran three more rounds of back-edge thresholds in
  tier 0. An impatient compile that reaches a loop which has never run guesses an integer
  sequence behind the guard rather than refusing, because any refusal other than a cold one
  disables the body for good.

# Decision record: ry's standard library (`List`, `Str`, `Num`, `Result`, `Dict`, `Set`)

- **Previous shape:** none. Results were a user-declared `@alias`, and the only stdlib was the stubs
  for R's own packages (the first record in this file), so every project re-declared `Result` and
  wrote its own helpers over plain lists.
- **Names:** plain qualified names such as `List.map` are ordinary R symbols with a dot, bound in a
  `package:ry` environment attached just above `package:base` (`standard_library::attach`); session
  startup moves them into the `ry` namespace, attached in front of the default packages beside the
  frame functions (see "Frames as runtime values"). There
  is no static dispatch, so a call is a symbol lookup and the checker reads one stub per name. A
  definition in the user's code shadows a library name like any R binding, and
  `test_standard_library.rs` asserts that no shipped package's `.exports` holds a library name, so
  plain R cannot change meaning. Module names are Roc's; a Roc name that already means something
  else in R is renamed (`Result.try` is `Result.and_then`).
- **One source of truth per side:** the types live once in `types/std.Rtypes` (namespace `std`,
  always active, in scope by bare name through the prelude), and the runtime functions live once in `crates/interp/src/standard_library.rs`,
  registered with `BuiltinDef::accepting_native`. A test asserts the two name sets are equal and
  that every declared parameter name is accepted at run time.
- **`Dict` and `Set` are native kinds** (`Fields::Dict`, `Fields::Set` in `crates/heap/src/records.rs`)
  with value semantics, insertion order and an open-addressing index, because a named list
  matches partially and allows duplicates, and an environment has reference semantics.
  `identical()` ignores order, as Roc's equality does. Updates reuse a uniquely held value in
  place (see "Standard library updates reuse a uniquely held value").
- **Errors are tags, not conditions:** a function that can fail returns `:Ok(x) | :Err(:Tag)`, and a
  wrong argument type raises an R error that names the parameter, so a misuse is loud and a domain
  failure is a value.
- **Impact:** no change for plain R (no shipped name collides, and all names are new symbols). The
  checker, hover, completion and goto-definition read the stub file like any other, so incremental
  analysis gains nothing new to invalidate. The user contract is `reference/standard-library.mdx`;
  the draft with open questions is `development/design/standard-library.md`.

## Standard library updates reuse a uniquely held value

- **Previous source of truth.** Every update (`Dict.insert`, `Set.insert`, `List.append`, ...)
  built a fresh value, because a builtin could not know whether its caller would overwrite the
  argument's binding. A loop growing a collection was quadratic: 40,000 `Dict.insert`s took 17.4 s.
- **Target shape.** Perceus-style reuse, as Roc and Koka do it. `eval::rebind` recognizes
  `x <- f(x, ...)` for a builtin flagged `reusing_first_argument` and passes the first argument as
  `Call::reusable` when the frame binding holds its only reference and no other argument is the
  same value; `standard_library::owned` also accepts a reference-count-zero temporary. Dicts and
  sets are counted records whose key and value vectors are resizable and double when full, and
  `build_index` takes spare room so `reserve` can presize the index. The bytecode compiler hands
  such assignments to the AST evaluator.
- **Why not a special form or last-use analysis.** A special form would make every library update
  syntax, and last-use analysis needs the compiler; the reference count is exact at the call
  boundary for AST frames, so reuse is decided where it is cheapest. It is not exact for a frame a
  compiled closure owns: bytecode registers hold uncounted references (`backlog.md`). The remaining copies (a parameter's
  first update, `state$seen <- Dict.insert(state$seen, ...)`) are in `backlog.md`.
- **Impact.** Value semantics are unchanged (any second holder copies). 40,000 inserts: 0.04 s.
  The checker is unaffected.

## Standard library breadth against Roc's builtins

- Each module was compared with Roc's builtins (pre-`Try` API) and everything that is typed,
  `NA`-free and reports failure as a tag was taken; the full list and the renames are in
  `development/design/standard-library.md`.
- **Skipped, with the reason:**
  - `Box`, `Encode`, `Decode`, `Hash`, `Inspect`, and the `Eq` implementations (`Dict.is_eq`):
    they serve Roc's memory layout or abilities, which ry does not have; `Bool.is_eq` compares any
    two values by `identical()`.
  - Wrapping arithmetic (`add_wrap` and the rest): R's 32-bit integer reserves its minimum for
    `NA`, so wrapping would have to stop at that one value, and the code that wants wrapping
    (hashing) wants it total.
  - Per-width conversions and limits (`to_i8`, `min_i32`, `int_cast`, `f64_to_parts`, `Dec`):
    R has one integer and one double type, so the limits are the four unsized
    `Num.max_integer`, `min_integer`, `max_double`, `min_double`, plus `Num.infinity`; Roc's
    `nan_f64` is skipped because no library function takes or returns `NaN` on purpose.
  - Effectful `!` variants (`List.walk!`): R does not track effects; `List.for_each` covers them.
  - `Str` capacity functions: R strings are immutable and interned, so there is no buffer to
    reserve or release.
- **Taken later:** `List.range` (`seq` returns a vector, not a `list[integer]`),
  `List.keep_if_try`, `Num.to_integer`, and `Dict.capacity`, `Set.capacity` and
  `release_excess_capacity` on `List`, `Dict` and `Set`. They were first skipped on the claim
  that the heap shrinks collections, but it never shrinks a resizable vector, so after a
  `reserve` the room stays until the value dies. `Dict.with_capacity` and `Set.with_capacity`
  came last: an empty collection has no key vector until its first key fixes the key type, so
  its open-addressing index, sized for the requested room, carries the room
  (`standard_library::empty_room`), and the first insert allocates the key and value vectors
  with that capacity. `Dict.reserve` on an empty dictionary works the same way.
- **`NaN` is never a result.** A `NaN` from an unchecked function (`Num.sin(Inf)`,
  `Num.pow(-1, 0.5)`, a `List.sum` of `Inf` and `-Inf`) is an error, and a `_checked` function
  returns `:Err(:Undefined)`, which its signature names.
- **Unchecked operations error where Roc crashes or returns `NaN`.** `Num.add`, `div`, `sqrt`,
  `log`, `asin`, `acos`, `pow_int` and the rest were first skipped because they produce `NaN` or
  overflow for part of their domain. They are taken with an R error there that names the
  `_checked` variant, so a step can be passed by name (`List.walk(xs, 0L, Num.add)`) and a domain
  failure is still never a silent `NaN`. `Num.is_nan` is the one function that accepts `NaN`.
- **Bit operations** (`bitwise_*`, shifts, bit counts) work on the 32-bit two's complement and
  error only on the pattern `-2147483648`, which is `NA`. Shifts wrap as Roc's do.
- **`Bool` and constants.** `Bool` was first skipped as redundant with `TRUE`, `!` and `&&`; it is
  taken so boolean steps can be passed by name and Roc code reads the same. `Bool.true`,
  `Bool.false`, `Num.pi`, `Num.tau` and `Num.e` are values in `standard_library::CONSTANTS`,
  bound in `package:ry` by `standard_library::attach` and declared in the stub as plain types.
  Under GNU R, `crates/ry/src/load.rs` reads which `ry.Rtypes` declarations are not function types
  and binds each as an active binding that stops with the "only ry's runtime" error.
- **UTF-16 and UTF-32** (`Str.to_utf16`, `from_utf16`, `to_utf32`, ...) follow Roc's newer `Str`
  builtins, with `:BadUtf16` and `:BadUtf32`; NUL is bad because an R string cannot hold it.
- **Changed:** `Num.ceiling`, `floor` and `round` keep the number type (R integers are 32-bit, so a
  rounded large double cannot become an integer); `Num.to_integer_checked` converts with
  `:Err(:OutOfBounds)`. Saturating integer arithmetic clamps to plus or minus 2147483647.
- **Hover docs** come from `#'` lines directly above a declaration in a `.Rtypes` file
  (`StubLibrary::documentation`); a later source's first declaration of a name replaces or clears
  them, the same override rule as the type.

## A call's result drops unconstrained type-parameter members of a union

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

## The standard library runs only on ry's runtime

- A pure-R build for GNU R was declined. Many functions return result tags and `Dict`/`Set`
  values, which GNU R cannot represent, and a partial build covering only `Str` and `Num` would
  split the library into two dialects with different availability.
- Instead, a diagnostic should flag library calls once a project can declare that it targets GNU R
  (`backlog.md`).

## The benchmark harness compares test by test through `ry load`

- **Shape.** `oracle bench` (`crates/oracle/src/bench.rs`) runs a generated package's testthat suite as `ry load <package> --file bench.R`, once with `--runtime gnu-r` and once with `--runtime ry`. The loader plan is the same on both sides, so a difference is an evaluation difference, not a loading one. The driver (`bench.R`) writes one JSON line per `test_that()` block with its status, problems, a deparse of its value and its time.
- **Why per test, not per file.** A whole-file pass rate hides progress: one missing native fails a file of 30 tests. Per-test results also give the failure messages that the tally groups into fix classes.
- **GNU R runs twice.** A test whose value or problems GNU R does not reproduce (a UUID, a timestamp, a temp path) is compared by status alone, so only what GNU R itself reproduces counts against ry, and no hand-maintained ignore list is needed.

## `[`, `[[`, `[<-` and `[[<-` dispatch before evaluating their other arguments

- **Previous shape.** The four were builtins: every argument was evaluated before S3 dispatch. data.table's `[.data.table` needs `j` and `by` unevaluated (`.()`, `:=`, column names), so `dt[, .(x)]` failed with "could not find function `.`".
- **Target shape.** GNU R's DispatchOrEval. The definition flag `BuiltinDef::dispatch_or_eval()` (a special with internal generic) makes `eval::apply_call` call `dispatch::dispatch_or_eval`: it evaluates only the first argument, and for an object passes the rest to the S4 method, or else the S3 method, as promises. Otherwise it evaluates them and calls the function with values, so the subset code itself never sees unevaluated arguments. A symbol bound to a missing argument evaluates to the empty argument, as GNU R's `R_isMissing` check does, so `f <- function(m, i, j) m[i, j]; f(m, , 2)` selects every row. `do.call` (`eval::apply`) hands such a special forced values.
- **Bytecode.** A call of a dispatch-or-eval special (`bytecode.rs`, `subject_call`) evaluates the subject into a register, branches on `BranchIfObject` to `DispatchSubject` (which runs `dispatch::dispatch_evaluated_subject`, so a method still gets promises and the subject's expression), and otherwise evaluates the rest into registers and runs `CallBuiltin`, which the JIT turns into its element intrinsic. A `BranchUnlessPrimitive` guard sends a rebound `[` to the generic call. A formal in an index position takes the generic path too, because a register cannot hold the empty argument a missing formal stands for. Without this shape `x[i]` in a loop ran as a generic call and compiled loops fell to tier-0 speed.
- **NextMethod.** As in GNU R, NextMethod reaching any primitive names the call `<generic>.default`, which error messages show and which a dispatching special reads as "do not dispatch again". Without that, `[.simple.list` re-dispatched to itself forever.

## libr reads options from base's `.Options`

- **Previous shape.** libr's `Runtime` held its own options environment that nothing wrote, so `Rf_GetOption1` always returned `NULL` and packages (data.table's `datatable.*` options) saw defaults only.
- **Target shape.** `Rf_GetOption1` walks `interp::envir::options()`, the interpreter's `.Options` pairlist, which is the single store `options()` writes.

## parallel's fork routines

- `crates/interp/src/parallel.rs` implements all of parallel's `.Call` routines: the L'Ecuyer stream jumps and the `mc_*` fork routines behind `mcfork`, `mclapply` and `mcparallel`. The pipe protocol is GNU R's (a native-endian `R_xlen_t` length, then the bytes; a zero length means the child is exiting), because parallel's R code drives it unchanged.
- **Fork safety.** The runtime starts no helper threads (no GC, JIT or I/O threads), so `fork()` copies a consistent interpreter and no lock can be held by a thread that does not exist in the child. What would be copied wrongly is buffered output, so `mc_fork` first flushes standard output and error and every file connection's write buffer (`connections::flush_pending_writes`). The child closes its siblings' pipes, so a sibling's standard input still reaches its end. `mc_exit` ends the child with `_exit`, so the master's exit handlers and buffers are not run twice.
- **Reaping without SIGCHLD.** GNU R reaps from a SIGCHLD handler, and its children wait for SIGUSR1 before exiting. ry does neither: a child exits at once, and the master calls `waitpid(WNOHANG)` on released children at the start of each routine. A child is released (its pipes closed) only once its pipe reaches its end or it is removed, detached or estranged, so its pid cannot be reused while it still names a tended child. `mc_children` lists tended children only, which `mccollect`'s loop needs in order to end.
- **Cleanup scopes.** `mc_prepare_cleanup` pushes a mark, and each child records the innermost mark at its fork. `mc_cleanup` acts on the children forked since the innermost mark (all children when there is none) and pops the mark only when it signals or detaches, because `mccollect`'s `cleanup(FALSE, FALSE)` runs inside `mclapply`'s scope and must not end it.

## Corpus package tests: compatibility details that are easy to get wrong

- **Hashed frames list in GNU R's order.** `ls()`, `as.list(env)` and `mget` over a hashed environment follow GNU R's bucket order, and packages' snapshot tests record that order. The order depends on the frame's whole history, because GNU R's table never shrinks on `rm()` and its chain counter (`HASHPRI`, `env.profile`'s `nchains`) falls only when a bucket empties, so replaying the inserts of the live bindings cannot reproduce it once anything has been removed. A hashed frame therefore keeps GNU R's chained table itself (`heap::env::HashTable`, boxed beside the frame), updated on every insert, removal and compaction: buckets from the PJW hash of the name, an insert prepends and counts one (GNU R's `R_HashSet` counts every insert, since its empty-chain test runs after the scan), and past 0.85 of the size the table grows to `size * 1.2 + 1`, moving buckets in ascending order and recounting occupied buckets. The table is the single source of truth for listing and `env.profile`; lookups still go through the address-keyed index. The global environment is hashed from its creation, as in GNU R. `crates/heap/tests/test_frames.rs` checks it against a plain model.
- **String encodings.** Strings intern by pointer, and `mkchar` marks non-ASCII text UTF-8. `Encoding<-` and `iconv` mark text that is valid UTF-8 as UTF-8 even when asked for "unknown", because the session is UTF-8 and a second pointer for the same text would make `==` fail (desc's `read_dcf` broke on duplicate fields). Latin-1 strings stay a separate pointer, and `==` compares them by text through `values::same_text`; `match()` and `unique()` do not yet (`backlog.md`).
- **A sink's connection is a GC root** (`connections::Table::trace`), because GNU R preserves the connection object while output goes to it, and the collector otherwise finalized and closed it under `capture.output`.
- **S4 dispatch on an unsupplied argument dispatches on NULL**, as GNU R's `GetObject` does, even when the argument has a default (`dispatch::dispatch_object`).
- **Queued finalizers root their weak reference's key, value and finalizer** (`libr::boundary::root_finalizable`), because the collector never traces a weak reference's fields and an earlier finalizer can allocate. `RY_GC_VERIFY=1` checks the write barrier's invariant before every minor collection, which is the quickest way to tell a missing barrier from a missing root.

## One serializer writer, in libr

- **Previous shape.** The interpreter (`crates/interp/src/serialize.rs`) had its own writer behind `.Internal(serialize)`, `serializeToConn` and `saveToConn`, and libr (`crates/libr/src/serialize.rs`) had the full `R_Serialize` writer that `saveRDS` and native code used. The interpreter's copy wrote only XDR version 3, so `serialize(x, NULL, version = 2)` gave version-3 bytes and digest's hashes of version-2 serializations were wrong.
- **What was duplicated.** Two writers for one format, which drift: the interpreter's lacked versions, ASCII, native byte order and refhooks.
- **Target shape.** libr's writer is the only one. libr registers the four internals (`serialize`, `serializeb`, `serializeToConn`, `saveToConn`) through `interp::register_builtins`, the same way the loader's builtins arrive, because the interpreter cannot depend on libr. The header is written one field per output call, as GNU R does, since digest's `spooky` algorithm counts the calls through a custom output function.
- **Impact.** One source of truth for the format, version-2 output and refhooks for free, and digest's suite passes. The readers are still duplicated (`interp::serialize`'s `unserialize` and libr's `R_Unserialize`); `backlog.md` tracks merging them the same way.
## The cold check searches green trees and caches globals per item check

- **Before.** Every whole-file tree reader (parse-stage diagnostics, annotation sites, attached namespaces, used tokens) walked the red tree with `descendants()`, building a red node per element, about 8 ns each. One item check read `frame_definition` and `package_name` through salsa for every reference, and each read interned a key tuple and verified a memo. `map_child_types` re-interned every type it visited, even when nothing changed.
- **Shape.** `syntax::matching_nodes` filters with borrowed `NodeRef` cursors and makes owned `SyntaxNode`s only for matches; `NodeRef::tokens` reads token text with no owned cursors. `testing::check_parse_invariants` pins both to the owned-cursor walks. (With the flat tree an owned cursor is only a refcount bump, so the gap is now small.) `SalsaGlobals` holds per-check `RefCell` maps for the two lookups, and `map_child_types` returns the input type when no child changed.
- **Why it is safe for salsa.** The first read inside a query records the dependency edge, and repeats only dedupe, so caching a read within one query execution changes no edge set. The cache lives as long as one `item_check` execution and is never shared.
- **Allocator.** mimalloc's eager arena commit made the kernel zero transparent huge pages the process never used. `crates/ry/src/main.rs` turns it off from an `.init_array` constructor, before the first allocation. It cut peak RSS on the benchmark code bases from about 790 MiB to about 520 MiB.
- **Counters.** `infer::RESOLVE_CALLS` is thread-local: a shared atomic bumped on every resolve step was a contended cache line across the worker threads.

## Call checking follows R's lazy argument and matching rules

- **Required formals.** Previous source of truth: a defaultless formal was required unless the body called `missing()` on it, which rejected `f(1)` for `function(x, unused) x`, valid R. Target shape: a formal is optional unless the body may force it. `check::FormalUses` walks the body in evaluation order (a read that no write dominates forces: a branch's writes count only when every non-diverging arm makes them, a loop body's or a `&&` operand's never, and `<<-` never assigns the local formal; `missing()`, `substitute()` and `quote()` arguments do not; reads in nested functions count, since the closure may run; a forced default forces what it reads, to a fixpoint). Inferred and declared types share the walk, so a declared `[name]` is legal over an unforced formal. Laziness only relaxes inferred signatures: a declared signature is honored as written, so `fn(a: integer, b: character)` over `function(a, b) a` keeps `b` required, because the annotation is the contract callers read. The rendered message names the missing formals instead of an arity count.
- **Partial matching.** `argument_targets` runs R's middle pass: an unclaimed name that is a prefix of exactly one unclaimed formal before `...` claims it. Matching stays case-sensitive, so `x =` goes to `lapply`'s `...`, not `X`.
- **`match.fun` strings.** A character argument at one of `check::MATCH_FUN_CALLERS` (base callees only, never a project function of the same name) checks as the named function when it is a literal that names a known scheme, else as `Unknown`. The resolved type rides on `CallArgument::named_function` and is used only where the expected type is a function, so the same argument still reads as `character` elsewhere.
- **Variadic actuals.** `infer::pair_parameters` lets a variadic function serve a fixed interface (its rest requires nothing). Base generics such as `mean` and `as.numeric` declare `...` so method arguments (`as.numeric(d, units = "secs")`) pass, and without this the apply family rejected them as callbacks.
- **Forwarded `...` to a callback parameter** adds no parameter to the demanded function type, because it may be empty.
- **`trailing-comma` is a warning.** R fails on the empty argument only when the callee forces it, so it is not a certain run-time error.

## Called names are looked up as functions

- **Previous shape.** A call head resolved like any read, to the nearest binding, so `names <- toupper(names(x))` made `names(x)` a call of a character vector, and cli's `.Call <- NULL` made every `.Call` an error.
- **Target shape.** R's `findFun`: `Checker::function_lookup` skips a binding whose type is definitely no function and continues through the enclosing slots, the project namespace (or, past a test file's or a script's top level, `GlobalEnv::enclosing_scheme`) and the library. An untyped binding that the body only ever calls is a callback and stays the function, so `function(format) format(1L)` types its parameter as a function as before. An untyped binding that is also read as a value somewhere (`dir <- path; dir(dir)`, `x[names(x) %in% names]`), next to an outer function of its name, types the call as `Unknown`, so neither is constrained. A unification probe was considered instead of the value-read test and rejected: at the call head the binding is still an open variable that any function type unifies with, and the conflict only appears at a later value use. The reached binding is stored once, in `ItemCheck.callee_targets`, and the IDE reads it there rather than recomputing the lookup.
- **Qualified names.** `pkg::name` never takes this lookup: a foreign package reads only its stubs, and the project's own package only its namespace (`GlobalEnv::namespace_scheme`), never the reading file's bindings.

## testthat files mirror testthat's environments

- **Previous shape.** Every file under `tests/testthat/` was a package file, so test bindings leaked into the namespace and showed up as duplicates or shadowed package functions; a first fix pooled every test file into one shared test namespace, which let one test file resolve another's names.
- **Target shape.** `DocumentKind::{Test, TestHelper}`. Helper and setup files fill the shared test namespace (`test_definitions`), each test file keeps its own bindings, and a closure in a test file prefers the binder its own top-level statement writes, because each `test_that()` block is its own environment. The kind is derived from the path in one place per surface (`cli::document_kind`, `semantics::test_document_kind`).
- **Impact.** One more per-name layer (`test_name`) in the global lookup, read through the same tracked projection as `package_name`, so an edit to a test file does not invalidate package items. The unresolved-name diagnostics read `test_name` too, and their typo suggestions come from the per-name `project_typo_suggestion` firewall, so a new helper definition re-diagnoses no test file whose answer it does not change (`test_incremental.rs` pins it).

## useDynLib native symbols resolve only as a native routine's name

- `metadata::NativeSymbols` parses `useDynLib` (explicit names, `.registration = TRUE` with its `.fixes` prefix). Explicitly listed names and names carrying the `.fixes` prefix resolve anywhere, because `useDynLib` binds them in the namespace. A registering `useDynLib` binds the routines its compiled code registers, which ry cannot see, so any other bare name resolves only where naming recorded it as the `.NAME` (or first positional) argument of `.Call`, `.External`, `.External2`, `.C` or `.Fortran` (`naming.native_routine_reads`), and a typo elsewhere still reports.

## Comparisons coerce across atomic types

- **Previous shape.** `<`, `==` and the rest required both operands in one family (numeric or `character`), and an open operand compared against a number was constrained to `numeric`.
- **Weakness.** Both rules rejected ordinary R. `Sys.getenv("RSTUDIO") == 1`, `type == "a"` after `if (is.logical(type))`, and `x > 0` on a flag all failed, and the numeric constraint leaked through exported schemes into every caller (zoo's `na.fill`, cli, lubridate).
- **Target shape.** A comparison accepts any mix of `logical`, `integer`, `double` and `character`, because R coerces both sides to a common type. `complex` and `raw` stay unsupported. An open operand is never constrained. `comparison_operand_shape` in `check.rs` owns this.
- **Cost.** `while (n && out)` now pins a counter `n` to `logical`, because the comparison that used to say "numeric" no longer does (rlang `stack.R`).

## A test-guarded write to a formal normalizes it; inferred types admit the default

- **Previous shape.** An `if` without `else` joined a branch write with the pre-if type by unification, so `if (is.logical(type)) type <- "q"` pinned `type: character`, and every caller passing the logical the code tested for was reported. A union join here instead produced `T | logical` everywhere, which cascaded through zoo, digest and xts.
- **Target shape.** In `infer_if`, a non-`NULL` write to a slot whose pre-if type is an open formal (`is_open_parameter`), under a condition that reads the same slot (`reads_slot`), *normalizes*: the slot takes the written type after the `if`, and the formal's variable is not unified. Other writes still go through `join_writes`.
- **Default widening.** After inference, a defaulted formal whose settled type does not admit its default's concrete type is widened to `settled | default`, probed with `compatible` under a snapshot. A call passing the default's own value is always fine.
- **Defaulted flags.** A condition or `!` on an unannotated defaulted formal does not constrain it to `logical` (`reads_open_defaulted_formal`), because option flags are passed `1`/`0` as often as `TRUE`.
- **Approximation.** After a normalizing write, the body sees only the written type, not a union with the input. This is unsound when the test does not cover every non-normalized input, and is accepted because the code after such a test assumes the normalized type.

## An open `T | NULL` parameter binds `T` to the argument's non-`NULL` part

- **Previous shape.** `is.null(x)` on an open formal shapes it to `T | NULL`, and a caller's open argument then unified with the whole union, so the caller's variable became `U | NULL` and every later use of it was reported (httr2's `check_request` pattern).
- **Target shape.** `compatible_inner` in `infer.rs`: when the expected type is exactly a two-member union of an open variable and `NULL`, and the argument is a variable or a union with no other tag, the variable member unifies with the argument minus its `NULL` members, under a snapshot. The rule is deliberately this narrow; a broader one (any union with a variable member) produced `logical[][]` cascades.

## `switch(typeof(x), ...)` narrows `x` per arm

- An arm runs only for the types its labels (plus fall-through labels) name, so `infer_switch` narrows the subject slot before each named arm (`narrow_to_typeof_labels`, `typeof_label_membership`): a union keeps the matching members, an open variable gets a fresh one so that `logical = if (x) ...` does not pin the parameter for the other arms. The narrowing is a refinement, not a write, so it does not join back.
- `NULL = ...` is a legal label in R's grammar and names the string `"NULL"` (`hir.rs`).

## Nested replacement rewrites the spine

- `x[[1]]$a <- v` used to leave `x` as it was. `replacement_written_type` now walks the accessor spine: each intermediate container is projected (`project_replacement`), the innermost takes `v`, and each enclosing container takes the rewritten child through the same `write_replacement_step` that single-level writes use. An undescribable intermediate projects as `Unknown`; a `[` or `@` step leaves its container unchanged.

## data.table queries run on ry's engine through a declining bridge at `[` dispatch

- **Previous shape.** Under ry's runtime, every `dt[i, j, by]` on a data.table went to data.table's own R and C code; only `frame` objects reached ry's engine, so benchmark code that calls data.table directly got none of the speed.
- **Target shape.** `crates/interp/src/data_table.rs`. `dispatch_evaluated_subject` offers `[` on a data.table to `data_table::query` before S3 dispatch. The bridge imports the columns zero-copy, lowers the call with the frame lowering in strict mode (pure expressions only, evaluated with every condition caught by `conditions::quietly`), runs the plan, and builds the result with data.table's attribute order and `setalloccol`. `:=` computes on the engine and writes through `data.table::set`, then records `.global$print` and returns visibly, as data.table does. Any doubt declines and data.table answers, so the bridge never has to be complete, only exact.
- **Why not a native data.table namespace or masking `[.data.table`.** Both would have to reimplement data.table entirely before any code could rely on them, including keys, indices, `.SD` semantics and every printing quirk. A declining hook gives exact output from the first query it accepts and grows by accepting more shapes, and the real package stays the fallback.
- **What declines, and why.** A query without `by` whose `j` only picks whole columns (`x[i]`, `x[order(a, -b)]`, `x[a > 0]`, `x[, .(a, b)]`): it computes nothing beyond a gather, and importing every column and converting the result back held both copies, so at 1e6 rows it ran two to three times slower than data.table's C and peaked four times higher. Keyed subjects and extra attributes (key semantics), auto-indexing filters (data.table adds an `index` attribute to the source as a side effect), joins without `on` (key joins), `x[]` (returns `x` itself), and reductions whose result depends on summation order: data.table's GForce (`by`, no `:=`, j in `.N`/`f(col)` form) adds doubles sequentially in plain double, while outside GForce base R uses long double, so only GForce-form `sum` and `mean` without `na.rm` are accepted. To match GForce, the engine sums doubles in row order per group (`table::group` dense path single-threaded for double sums, plan path via `cum_sum().last()`).
- **Masking.** A `packageEvent("data.table", "attach")` hook copies `package:ry`'s `setkey`, `setkeyv`, `key` and `copy` into `package:data.table`, so the bindings are identical and `library(data.table)` prints no masking message; ry's versions call data.table's on a non-frame.
- **Impact.** Correctness: every accepted shape is compared against GNU R in `crates/oracle/cases/datatable__*.R`. `options(ry.datatable = FALSE)` disables the bridge, `options(ry.datatable.trace = TRUE)` reports which engine answered. No analysis impact.

## Delimited files: one sniffer for the reader and the checker

- **Previous shape.** `fread` was declared to return the `data.table` nominal, so a read file had no column knowledge, and the table engine had no reader or writer of its own.
- **Target shape.** `crates/sniff` decides a delimited file's layout (separator, header, column names, and each column's type out of logical, integer, double and character) with fread's rules, from the head plus jump points spread over the file. It is the single source of truth for that layout: `table::io::read_csv` parses with it, and the hosts sniff every project `.csv`/`.tsv` head (64 KiB, `sniff_head`) into the `DataFiles` salsa input, which `fread("path")` reads through the per-path projection `data_file_layout`. A file whose sample missed a wider value rereads that column from all its values, so the reader never gives a narrower type than the data holds.
- **fread returns a frame.** ry's table replaces data.table, so `fread`, `rbindlist`, `melt` and `dcast` return `frame`, and a frame satisfies a `data.table` parameter. `data.table()`, `as.data.table` and `setDT` keep the nominal. The checker refines these calls after the ordinary call has checked their arguments (`refine_table_call` in `check.rs`), reading only the recorded argument types, so nothing infers twice. `setnames` and `setcolorder` reuse the `:=` slot-write model (`naming::record_column_update`).
- **Writer.** `fwrite` output matches data.table's text: shortest round-trip doubles (ryu, `1` not `1.0`), `TRUE`/`FALSE`, `NA` as an empty field, and quoting only where needed. Batches format in parallel and write in order.
- **Known gaps.** integer64 reads as double, dates read as character, `dec = ","` and `strip.white` on character columns are not supported, and a ragged line is an error rather than fread's fill. The checker resolves a data path against the project root only, because `SourceFile` carries no path.
- **Impact.** Editing a data file invalidates only the checks that read its path (salsa backdates every other path's layout). The CLI walks data files without `.gitignore`, since data directories are commonly ignored.

## The table engine's sort and distinct kernel

- **Previous shape.** `setkey`, `setorder`, `unique`, `uniqueN` and `dcast` called Polars' multi-column sort and `unique_stable`, which compare row by row through the column types, cannot match data.table's NA, NaN and `-0` rules without extra passes, and lost to data.table at 1e7 rows.
- **Target shape.** `ops::order_digits` codes each key column as one `u64` per row whose unsigned order is the sort's order: `NA` is 0 and `NaN` is 1, both first in either direction as data.table places them; integers are offset from their minimum; doubles go through `ordered_bits`, so `-0` ties with `0`; factors use their codes; strings use their rank in byte order (C locale) from `string_ranks`. `sorted_rows` packs a row's digits and its row number into one `u64` or `u128` (`Packed`), and `radix_sort` sorts only the digit bits: up to three stable LSD counting passes, or for wider keys one MSD pass on the top 16 bits followed by an in-cache sort of each bucket. The row number makes every key distinct, so the result is stable. Keys wider than 96 bits fall back to Polars.
- **Distinct rows.** Keys of 24 bits or fewer skip sorting: `uniqueN` sets bits in a shared atomic bitmap and `unique` keeps a first-row table per task. Wider keys sort and compare neighbours.
- **String ranks.** Each thread numbers the strings of its slice in a hash map, keyed by the 16-byte view itself for strings of 12 bytes or fewer (no string read), with a small direct-mapped cache in front while the slice has few distinct strings. The distinct strings of all threads sort once by `(first 16 bytes as a big-endian integer, string)`, which is byte order with almost only integer comparisons.
- **Gathers.** Reordering a frame gathers every column in parallel; a string column gathers its views and shares the original data buffers, so no string bytes move.
- **Impact.** One kernel serves every ordering operation, so all of them agree on NA placement and ties, and `crates/table/tests/test_table.rs` (`fuzz_sort_unique_and_runs_against_a_reference`) checks them against a reference comparator over random NA, NaN, `-0`, infinities, factors and long shared-prefix strings.

## `object-systems = false` stops class definitions in project code at run time

- **Previous shape.** Only the lint read the option, so `ry run` happily defined S4, Reference and R6 classes in a project that had turned them off. A first runtime check covered only code whose top environment is the global one, so the package under `ry load` still defined classes.
- **Target shape.** `ry run`, `ry load` and the console on ry's runtime pass the option to the interpreter as a run flag (`RUN_FLAG_OBJECT_SYSTEMS`). With it off, `run_closure` calls `refuse_object_system`, which raises the lint's message (one text, `syntax::native_values::object_systems_disabled_message`, and one table of defining functions, `object_system_function`, shared with the lint and strict mode's top-level rule) when a call named after a class definition form (bare or after `::`) runs a closure owned by `methods` or `R6` and the caller is project code.
- **Scope.** Project code is whatever has the global environment as its top level (scripts, the console, functions defined there), or runs in a namespace the loader built, which `runtime.R` marks with `.__ry_loader__.`. Installed packages are exempt, because they must keep loading (`stats4` defines S4 classes when it loads). Matching by call name keeps the hot path to one flag test when the option is on and one symbol-name lookup per closure call when it is off; a renamed alias (`f <- setClass`) slips through, which is acceptable for a guardrail whose job is to stop deliberate use. GNU R has no such check, so there only the lint applies.

## A strict library file's top level holds only definitions, and purity is declared

- **Previous shape.** Strict mode checked types only, so a strict package file whose top level called `cat()` or `options()` checked clean and ran the call on every load.
- **Target shape.** `semantics::top_level::item_top_level` classifies each top-level item of a library file as a definition, a constant, or an effect with its range, and `strict` reports the first effect of each statement (`reference/type-system.mdx`, "A package file's top level"). A library file is a `DocumentKind::Package` file (`R/`); scripts, tests, helpers and the console are entries and unrestricted. Class definition forms are read from `object_system_function`, so the lint, the runtime refusal and this rule share one table.
- **Purity is declared, never inferred** (Roc-style effect inference was rejected: R's higher-order functions take pure and effectful callbacks under one name, and an inferred effect in a type would let one added `cat()` change every transitive caller's interface). `item_declared_pure` is a per-item `bool` read from `#: @pure` or the inline `@pure` marker (`PURE_MARKER` in the syntax tree), so a body edit backdates it and callers never re-check; stubs mark names with `@pure`. The marker is trusted and means "no effect whatever the arguments", which is why `lapply` is not pure. A project definition shadows the stub, and a name with a conditional writer is never pure (`CalleeOrigin::Conditional`), because which definition runs depends on the condition.
- **Lazy constants.** In a strict file, a single-writer constant is bound lazily by the loader (`load_index::package_index`) only when every package name its initializer mentions has one writer earlier in collation order; otherwise a later or second write would change what a late evaluation reads. `loader::diff` rebinds a lazy constant when anything its initializer reaches changes, and because laziness now depends on other files, it checks every name, not only the dirty items' names, for a flip. The loader installs the stubs and sets `ProjectFiles` only for a strict package, so non-strict loads never parse the stubs. The loader cannot read `ry.toml`, so each host passes strictness in with `Loader::set_strict`: the GNU R hosts read the planned root's config, and ry's runtime gets the project's through the `RUN_FLAG_STRICT` bit of `ry_run`, because libR links its own copy of `loader` and no process-global registration reaches it. Classification resolves callees through the project-wide definition maps. Their conditional-writers half skips `name <- function(...)` items without naming them, since a function body cannot write a top-level name, so strict planning names only the few non-function items and plans faster than a non-strict load (on finbench, about 0.8 s against 0.9 s, and a full load about 150 ms faster). A `<<-` anywhere in a package function body (`top_level::item_super_assigned_names`, a scan of the item's flat expression arena) counts as a writer of its target placed after every top-level statement: the namespace locks once loading finishes, so such a write can only land during `.onLoad` or something it calls, and counting every function over-approximates that call graph without building it. A bare stub call takes its purity from the namespace of the name's winning declaration (`StubLibrary::declarations`), the same one its type comes from, instead of a flat name set. A replacement inside `local()` of an outer name is accepted when the outer binding is a literal or `c()`/`list()` of literals (`item_binds_plain_value`), because R replaces in a local copy; reading the outer constant's inferred type would put type inference on the load-planning path.
- **Lazy R6 generators.** `name <- R6Class(...)` (bare or `R6::`) is bound lazily in any file, not only strict ones, when its arguments are literals, names, functions, and `list()` or `c()` of them (`top_level::TopLevel::Generator`, `generator_part`). The rule is syntactic on purpose: non-strict loads install neither the stubs nor `ProjectFiles`, so a purity-based rule would classify differently under `ry load` and `ry check`. Unlike a strict constant, whose laziness counts every name its value mentions because a pure function it calls may read them, a generator counts only its immediate reads (`IndexedItem::reads`): its methods read the namespace when they run, lazy or not, and R6 already evaluates `inherit` late. A general purity rule for non-strict constants was rejected, because a pure function such as `toupper` still reads the locale, which an earlier eager statement in a non-strict file may have changed. On the finance code base all 199 generators stay promises after load, and the load on an index-cache hit drops from about 0.4 s to about 0.29 s.
- **Impact.** Loading a strict package binds names and registers classes only, so evaluation order between constants no longer matters and a reload reruns less. Incrementality holds: `test_incremental.rs` pins that adding `cat()` to a pure function re-derives only that function, and removing a marker re-classifies only its callers.

# Decision record: one Rmath, one codec stack, and the numerical routines

- **One Rmath.** The interpreter's distributions and libR's Rmath were two independent
  implementations of the same functions, so a precision fix landed in one and not the other.
  `crates/interp/src/rmath.rs` is now the single copy (libR's version, the one the conformance
  suite holds to GNU R): the interpreter's builtins vectorize it, libR only converts C flags,
  raises the collected warnings and draws variates, and `gamma`, `lgamma`, `beta` and `lbeta` call
  it instead of libm's `tgamma`, which is not exact for whole arguments.
- **One compression stack.** libR decoded bzip2 and xz with `bzip2-rs` and `lzma-rs` while the
  interpreter used `bzip2` and `lzma-rust2`, which also write. libR now calls
  `interp::connections::decompress` (file magic) and `interp::serialize::decompress_record`
  (lazy-load records), so one set of codecs reads every file and both crates drop the others.
- **Time zones come from jiff** reading the system's tz database (`system.rs`, `Zone`). A name the
  database does not know acts as UTC under the abbreviation glibc gives it (the name's leading
  letters when there are three or more, else empty). Two glibc behaviors are copied deliberately:
  an `isdst` that contradicts the zone's rule shifts the reading by an hour, even in a zone with
  no daylight saving time, except for `"UTC"` and `"GMT"`, which GNU R converts itself; and a
  year outside jiff's range moves by whole 400-year cycles, which keeps the calendar and weekdays.
- **A repeated (fall-back) wall time takes the offset in force at the wall time read as UTC**,
  which is what glibc's `mktime` does with `isdst = -1`, so Berlin's 02:30 reads as CET while New
  York's 01:30 reads as EDT. `strptime` keeps a spring-forward gap's wall-clock fields.
- **The locale comes from the C library.** The session calls `setlocale` for the categories GNU R
  sets at startup (numbers stay C), `Sys.setlocale`/`Sys.getlocale` go through it, and `sort()`
  reads `LC_COLLATE` from it (`sort::refresh_collation`) instead of a fixed C.UTF-8 answer, which
  is what withr's `local_locale` tests and C-collation sorts observe.
- **`nlm`, `nlminb` and `polyroot` follow the published algorithms**: Dennis and Schnabel's
  UNCMIN for `nlm`, whose line search and trust-region updates translate GNU R's `uncmin.c`
  (see "licensing"), netlib's PORT reference for `nlminb` (see "`nlminb` runs PORT's own
  drivers"), and
  Jenkins and Traub's three-stage algorithm for `polyroot`, which also fixes the order of the roots
  GNU R returns.
- **One UNCMIN, one QUADPACK, one interval search, all in `interp`.** libR used to carry its own
  copies of the routines behind `optif9`/`fdhess`, `Rdqags`/`Rdqagi` and `findInterval2`, next to
  the interpreter's `nlm`, `integrate` and `findInterval`. The single copies are
  `interp::stats_nlm` (all three methods, analytic derivatives, the `msg` bits, `fdhess`),
  `interp::stats_optimize::{qags, qagi}` (writing into caller-owned `Subintervals`, so the C
  entry points pass `work`/`iwork` straight through) and `interp::arith::find_interval`; libR
  only converts C callbacks and arguments, as `lbfgsb` does. GNU R black box decided the details
  the copies disagreed on: the cubic backtrack groups its coefficient as `3 * t3 * (...)`, and a
  non-finite value (which nlm turns into the largest double) shrinks the step by ten. nlm's call
  protocol is also GNU R's, as observed through call counts: an uncached probe at the start whose
  value is ignored and which alone decides whether `gradient` and `hessian` attributes count
  (wrong length or mode warns and ignores them), a ring of the five latest evaluations, and the
  error "non-finite value supplied by 'nlm'" instead of a call at a non-finite point. Where GNU R
  reads past a later attribute that is too short or missing, ry errors or uses zeros.
  `stats__uncmin_quadpack.R` pins the iterates at full precision and the call protocol.
  Methods 2 (double dogleg) and 3 (More-Hebdon hook) match GNU R bit for bit, which took these
  departures from the textbook, all found by tracing every evaluation in `%a`: the trust-region
  update retries with a doubled radius only for a Newton step whose decrease the model predicted
  to within a tenth; after the switch to central differences an analytic gradient builds the
  Hessian from gradient calls; the hook's mu update uses the derivative at mu = 0 (`phip0`); both
  `phip0` and `phip` square `sqrt(dot)` instead of using the dot product; and the unfactored
  secant update sums its two corrections before adding them to H. The libR case
  `optif9_trust_region` compares at tolerance zero, so it assumes reference-BLAS rounding in the
  GNU R that recorded it (GNU R's `ddot_` and `dnrm2_` come from the system BLAS).
- **`parse_Rd` is a hand-written parser** in `crates/interp/src/rd.rs`, built from the Rd format's
  documentation and GNU R's observed output (`tools__parse_rd`). It tracks the three lexical modes
  (LaTeX-like, R-like and verbatim) per macro, keeps srcrefs, and reproduces GNU R's warnings and
  errors for unterminated strings. `deparseRd` reads the same tree.
- **`iconv` goes through the C library's `iconv`** rather than a Rust crate, because GNU R's
  results (which names it accepts, how `sub` substitutes, transliteration) are glibc's.
- **Impact:** each fact has one implementation, so the oracle and GNU R's own tests exercise the
  code that libR and the interpreter both run. No effect on static analysis.

## Cold-check constant costs: one green walk per file, immortal interning, high durability

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

- **Previous shape.** Calling a value typed `fn | NULL` was a `type-mismatch` finding wherever the `NULL` came from, including a local set by a `switch` without a default (testthat's `load <- switch(style, ...)` after `arg_match(style)`) or by an `if` whose other branch is correlated with the call's guard (nloptr's `eval_g_ineq_wrapper`). A union of functions and plain values (shiny's `ns <- NS(namespace)`) was "not a function".
- **Target shape.** `infer_ordinary_call` drops `NULL` beside function members unless the callee names a parameter, and `dispatch_call` checks a union of functions and values against its function members only, with an `Unknown` result. A parameter's `NULL` comes from its default or its declared type, so the caller supplies it and the call is still reported.
- **Why.** The rule in "The checker presumes R code is correct": the `NULL` path of a computed function is one the code presumably rules out, and R fails only on that path.
- **Impact.** Loses the true positive of a `switch` on an unvalidated value. Incremental analysis is unchanged: the rule reads the item's own naming.

# Decision record: editor features for ry's own syntax

- **The checker records pattern types.** `ItemCheck::pattern_types` maps each `match` pattern to
  the type of the value it is matched against, filled while the arms are checked. Before this the
  IDE had no type for a pattern, so hover and completion there had nothing to read. It is a
  per-item query result, so it adds no project-wide dependency.
- **A tag is a project-wide structural symbol.** Tags have no single declaration: `:Ok` in one type
  and `:Ok` in another are the same tag. References and rename therefore cover every spelling of the
  name in the project, and go-to-definition goes to its listing in a named type definition, or in
  the standard library stubs. A tag the standard library
  spells (in `types/ry.Rtypes`) refuses to rename, because the stub would no longer match.
- **Type parameters navigate to their binder**, scoped to the definition or function that binds
  them, so `T` in two functions is two symbols.
- **Semantic tokens are an `ide` query** (`semantic_tokens`, `classify_type_token`), and the server
  only maps `SemanticClass` to its legend. Before this the classification lived in the server and
  covered annotations only, which left `match`, `=>` and tags uncolored.
- **Semantic tokens also color R names an R grammar cannot classify**: parameters (declarations and
  reads that resolve to a parameter binding), call heads as functions, the package of `pkg::name` as a
  namespace, and the left side of a function assignment. Everything else stays with the editor's R
  grammar, so tokens add facts rather than replace the grammar. The server also answers range
  requests, so a large file only pays for the viewport.
- **Member names are `property` or `method` tokens.** `x$name` and `x@name` decide from the
  syntax alone (a called member is a method), and so do record-literal and pattern fields. R6
  members and S4 slots are named by call arguments, so `semantic_tokens` reads them from the
  outline's `classify_symbol_call` rather than a second classifier: one definition of what an R6
  member or S4 slot is serves the outline and the highlighting.
- **A quick fix lives on the diagnostic** (`Diagnostic.fix` in `crates/semantics/src/diagnostics.rs`,
  `push_lint`'s fix argument in `lints.rs`). Before this, a fix would have had to re-derive the
  finding in `ide`, a second source of truth that drifts. The server reads fixes from the diagnostics
  it publishes, after suppressions, so a suppressed finding offers no fix. `ry check --fix` can reuse
  the same field.
- **Outline sections and folds come from RStudio section comments** (a comment alone on its line
  ending in four or more `-`, `=` or `#`). A section ends at the next header of the same or a higher
  level in the same block, which matches RStudio's outline, so files written there look the same.
- **An inlay hint states what the code does not.** A payload-free tag or an `@Name(...)` constructor
  already spells its type, so it gets no hint and no "add annotation" action, and a vector of
  records, tuples or tags (`Position[]`) is dropped from a hinted union because no such value exists.
- **A type error on a native literal blames the one differing component** (`native_component_mismatch`
  in `crates/semantics/src/check.rs`), recursing into nested literals and tag payloads, and keeps the
  whole literal when more than one component differs, because then no single spot is the cause.
- **Impact:** every feature reads per-item query results, so incremental analysis is unchanged; the
  fixtures in `crates/ide/tests/ide/records-and-tags.R.test`, `frames.R.test` and the typing suite's
  `mismatch_blame` group pin the behavior.

## The DataFrame API is Polars', and ry's own types are UpperCamelCase

- **Previous shape:** the built-in table was `frame`, queried only through data.table's
  `x[i, j, by]`, and ry's own types were lowercase (`frame{...}`, `record{...}`, `tuple{...}`), so
  nothing in a type told R's types from ry's.
- **Names:** every type and type constructor ry adds to R is UpperCamelCase (`DataFrame`,
  `LazyFrame`, `GroupBy`, `LazyGroupBy`, `Expr`, `Record{...}`, `Tuple{...}`, alongside the
  standard library's `Result`, `List`, `Str`, `Dict`, `Set`), and R's own types stay lowercase.
  The case is the visible line between R's types and ry's. No lowercase alias was kept: the old
  names had no users outside this repository, so a deprecation path would only add a second
  spelling to every renderer and fixture.
- **API:** Polars' verbs over expressions, as qualified functions used with `|>`:
  `DataFrame.select`, `with_columns`, `filter`, `group_by` + `GroupBy.agg`, `sort`, `join`,
  `head`/`tail`, `lazy`, and the `LazyFrame.*` mirror with `collect` and `explain`. Expressions are
  `col("x")`, `lit(v)`, R's operators through `Ops.Expr`, and `Expr.*` functions (`alias`, the
  aggregations, `len`, `is_null`, `if_else`, `cor`). There is no method dispatch: a qualified name
  always means one function, which keeps the checker's and the IDE's view a plain name lookup. A
  string names a column wherever a verb takes an expression, as in Polars.
- **One plan for eager and lazy:** every verb appends a `table::lazy::Step` to a `Plan`, and an
  eager verb collects at once. The optimizer pushes filters below `with_columns` and projections
  and into a scan, prunes scan columns, and turns a leading `head` into a scan row limit;
  `crates/table/tests/test_lazy.rs` fuzzes that optimizing never changes a result. Errors name the
  schema of the longest valid prefix (`Plan::valid_schema`), so a misspelled column lists the
  columns that exist at that step.
- **Typing:** the checker refines a verb call after the ordinary call has checked the arguments
  against `types/ry.Rtypes` (`refine_dataframe_call` in `crates/semantics/src/check.rs`), reading
  each argument's syntax for its output name and column type. The refinement applies only when the
  name reaches the library (`reaches_library`), so a project's own `DataFrame.filter` is an
  ordinary function. An argument the checker cannot read (an `Expr` in a variable) leaves the
  columns unknown rather than guessing. `as.data.table` and `as.DataFrame` keep the columns.
- **data.table syntax stays as compatibility, typing included.** The runtime still answers
  `x[i, j, by]` on a DataFrame and routes plain data.table queries to the same engine, so existing
  code keeps working; the bracket typing already existed and costs nothing to keep. The docs teach
  the verbs and present the brackets only as compatibility.
- **R-side classes:** the value's class is `"DataFrame"` alone; `as.data.frame` and
  `as.data.table` methods are registered with `registerS3method` into the generics' namespaces, so
  they work whether or not the package is attached first.
- **Impact:** column checks and completion cover the whole query rather than one bracket;
  incremental analysis is unchanged, since the refinement reads only the item's own recorded types.

# Decision record: positioning

- **Audience.** Teams with large, old R code bases that now want agents to write and change that
  code. Code an agent writes needs verification that does not depend on a human reading every line,
  and a type checker gives it on every edit. The pitch leads with this, because "a language server
  for R" undersells why the project exists.
- **The shape of the pitch: one toolchain, R plus types, then R++ and R--.** ry is one toolchain for
  R (checker, language server, formatter, loader, console, runtime). Its type system sits on top of
  R and checks the code a project already has. ry's runtime adds features plain R does not have
  (records, tags and `match`, frames, the standard library, inline annotations), which the pitch
  calls R++ together with plain R (everything ry runs). Strict mode keeps only typed code, which
  the pitch calls R--. A two-circle Venn
  diagram shows this: R on the left, ry's types and features on the right, typed R (`#:` comments)
  in the overlap, the union is R++ and the right circle is strict mode. The diagram has one
  source, `docs/src/components/Venn.astro`, used by the landing page and `getting-started.mdx`.
- **One tagline: "A type checker and a fast runtime for R, written in Rust."** It is the README's
  line, and the landing page's subhead and meta description, the Starlight site description,
  `getting-started.mdx`, `ry.mdx`, the Zed extension and the `ry-lang` crate repeat it word for
  word (user direction). It names the two things a reader cannot get elsewhere; the linter,
  formatter and language server follow in the body. The hero heading "A toolchain for R." is drawn
  by the user-owned particle animation, so it keeps its own wording.
- **The migration is sold as moving your code into the future, not as switching to ry.** The
  destination is the future of the code base (typed, checked on every edit, safe for agents to
  change, fast), and ry is the tool that takes it there, usable today on the code it already has.
  The pitch avoids "migrate to ry", and step 4 is called "Go fully typed" rather than "Move to ry's
  types". Step 3 is "Switch to the ry runtime" (user direction), because that is the one step that
  changes what runs the code, and it names the gain plainly: inline types and ry's own features.
  The blue-to-purple Venn story reads as the code moving forward.
- **The landing page sells features first and the migration last.** Below the hero come three
  tabbed sections: one toolchain (type checker, formatter, language server: what works today), then
  the backwards-compatible runtime (tagged unions, nominal types, records, the standard library),
  then the migration as the closing pitch, with the Venn diagram ("from the blue bubble to the
  purple one"). On wide screens the four steps and the selected step's code fill the left column
  and the diagram sits on the right, so the code stays directly under the step that chose it while
  the diagram follows along; on phones the diagram comes first. A reader has to want the features before a path to
  them matters, which is why the migration is the last part of the sell and not the opening.
- **The landing page carries almost no prose.** Each section is a headline, at most one sentence,
  and tabs of real code beside real output. No eyebrow labels, step labels, notes or hints, because
  people skim a landing page and the snippets carry the argument; detail lives in `getting-started.mdx`.
- **Motion shows the story; it never decorates.** The migration section sets the Venn beside the
  step tabs, and each step moves a "your code" marker into its region and dims the others (the
  `data-step` attribute on `Venn.astro`; without it the diagram stays static, as on `getting-started.mdx`).
  Runtime and migration snippets reveal code first and output after, once on first view and again
  on each tab switch, and one neutral pill slides between tabs (no colored outline or glow). Every
  switch (pill, panel fade, line reveal, Venn marker) shares one symmetric ease-in-out curve and
  duration from `--switch-ease` and `--switch-duration`, because an ease-out curve read as abrupt.
  Every snippet function uses a `{}` body. All motion is off under `prefers-reduced-motion`.
- **Each landing snippet teaches exactly its tab's concept.** It must pass two tests: an R user who
  has never seen ry understands it in five seconds, and nothing besides the tab's concept makes a
  newcomer stop and wonder. So snippets use `type`, never `alias` (the distinction lives in the
  custom-types docs), avoid incidental syntax (`@new`, one-field records that need a trailing
  comma, generics, open unions, `eval(parse())` tricks), lay unions out one variant per line with
  a leading `|`, and use the fewest annotations that make the point. If a snippet needs a sentence
  to be understood, simplify the snippet instead.
- **The checker tab opens untyped.** Plain R with a real `ry check` finding comes first, and an
  Untyped/Typed toggle in the frame header shows the same file with that bug fixed and one `#:`
  line that finds the next, with a one-line caption saying R reads it as a comment. Each mode shows
  exactly one diagnostic, so the two outputs are the same height. Each toolchain view ends with one
  quiet docs link. Leading with annotations assumed the reader already knew what they were.
- **Tabs in one frame share the tallest tab's height, so the content is balanced to make that
  height small.** On wide screens a switch must not move the page, so a showcase's panels share one
  grid cell. A short tab then leaves a visibly empty frame, so the snippets are sized to land near
  the same height (toolchain tabs about 320px at 1440, typing steps within about 100px of each
  other), and the language server's feature tabs sit in the frame's header row like the checker
  toggle. At 900px and below, code and output stack and the heights differ by hundreds of pixels, so
  there each frame fits its own panel and a switch may move the page.
- **The landing Venn has a step indicator and a caption; why-ry's static copy does not.** Four
  clickable bars repeat the step tabs for pointer users, and one plain caption per step says what
  the highlighted region means, which also defines R++ and R-- in context. The Venn is centred
  against the steps column.
- **A tab appears only once its feature has a real snippet.** The runtime's Speed tab charts the
  why-ry Speed table as speedup over GNU R on one linear axis from zero with a 1x line, sorted so
  the strongest wins lead, and keeps every program, including the slowdown (drawn in grey), because
  hiding a loss would cost more trust than it buys; it links to the full table and method. The DataFrame tab shows the Polars-style API, never
  data.table's `x[i, j, by]` (kept for compatibility only), with one line for the
  `tools/bench/table` result. No placeholder tabs.
- **The migration has four steps, each with a real snippet and real output:** check the code you
  have, add types as `#:` comments, switch to the ry runtime (inline types, which it also enforces
  at run time, and ry's own features), then go fully typed under strict mode. Each step is optional and reversible, which is what makes adopting
  ry safe for an existing code base. `getting-started.mdx` tells it in full.
- **GNU R stays a first-class target.** `#:` comments keep a file plain R, so a team that keeps
  GNU R loses only the runtime features. The pitch says so in one sentence and does not dwell.
- **The benchmark is honest about typing.** Its three columns are GNU R, ry on the same untyped
  code, and ry on typed strict code. Typed code is not faster yet (`backlog.md`), and the pitch
  says so instead of implying that types buy speed. When typed runtime features do get faster, the
  third column shows it without a rewrite of the copy.
- **The Tutorial follows the four steps** (`docs/astro.config.ts`): one page per step, "1. Check
  your code" and "2. Type your code" for what works on GNU R and ry's runtime alike, "3. Run on
  ry's runtime" and "4. Go fully typed" for what needs ry's runtime. See "Docs information
  architecture" for why it is one page per step.
- **Rejected:** opening the landing page with the Venn diagram and the migration (it explains a
  path before the reader wants the destination), and leading with "a reimplementation of R" (true, but a reader cannot use the
  incomplete runtime for real work today, so it is the destination, not the pitch).
- **Rationale for the user's framing being kept:** the user proposed this structure. It was kept
  because each step maps to a real, shipped feature with real output, and because R++/R-- is a
  compact way to say "a superset you can opt into, and a safe subset you can enforce".

## Nominal and structural types cover both product and sum types

- **Shape:** every record, tuple or tag union is structural by default (`alias` only names it), and
  `type Name = ...` (or `#: @type`) wraps it in a nominal type. That gives a 2x2 grid, structural or
  nominal for product and for sum types, with one mechanism instead of a separate "declared record"
  or "declared union" concept. The user guide teaches it in
  `tutorial/fully-typed.mdx` (a record pair and a union); the contract is the reference section "Nominal records,
  tuples and tags".
- **Construction:** `@Name(...)` is the only constructor, and its parentheses are the value's own,
  so `@Point(x = 1, y = 2)` is a record and `@UserId(id = 1L,)` a one-field record. A bare value of
  the right shape is refused, which keeps construction in one checked place, like Roc's opaque
  `@Name`. A separate constructor syntax (`Point(x = 1)`) was rejected because it collides with R
  calls. `@Point((x = 1, y = 2))` still works, because the inner parentheses are only R grouping, but the docs never write it.
- **Previous weakness:** the checker handled the nominal wrapper only at assignment. `match`
  exhaustiveness, pattern checks, field writes, `$` completion and the native-value-to-R check all
  looked at the nominal head and saw no shape, so a nominal union was never checked for coverage,
  bindings lost their nominal type, and field writes went unchecked.
- **Target:** each of those reads projects the nominal value to its representation through the
  visible type definitions (`exhaustiveness::structural`, `Checker::structural`, `ide::representation`),
  so the definition stays the single source of truth for the shape. Binding patterns keep the
  nominal type. Exhaustiveness splits on constructors only while a row has a constructor head, which
  is what lets a recursive `type Tree<T>` terminate.
- **Impact:** no new stored state, and the projection reads per-name definitions that each item
  already depends on, so incremental analysis is unchanged. The `nominal_grid` typing group, the
  nominal cases in `crates/ide/tests/ide/records-and-tags.R.test` and `crates/interp/tests/test_records.rs`
  pin the behavior.

## Per-group top rows and as-of joins reuse the engine's kernels

- **Previous shape.** The Polars-style API had no way to keep each group's best rows or to join on
  the nearest key, so the table bench answered db-benchmark's top-two question and the keyed
  rolling join with data.table syntax (`x[order(-v3), head(v3, 2L), by = id6]`,
  `prices[trades, roll = TRUE]`).
- **`Expr.head(n)` inside `agg` keeps up to `n` values per group, as rows.** Polars gives a list
  per group there, but ry has no list columns, so the values are exploded into rows, which is also
  data.table's layout. The checker tracks an expression's shape (`Shape` in
  `crates/semantics/src/check.rs`: per row, per group, or up to `n` per group) and accepts `Head`
  only as a whole `agg` item, because `head(v, 2) + 1` would mix shapes. After a sort, the planner
  fuses sort and aggregation into one query, which hits the engine's top-k kernel
  (`group.rs::top_k_shape`) instead of sorting the whole table.
- **`join_asof` is `How::Asof(Strategy)` in `table::lazy`, lowered to the rolling join.** The left
  frame is data.table's `i`, so `"backward"` is `roll = TRUE`, `"forward"` is `roll = -Inf` and
  `"nearest"` is `roll = "nearest"`. The `by` columns become the leading equality keys and `on` the
  rolled key. One kernel (`join.rs::rolling`, with Polars' asof as the fallback) serves both APIs.
- **A grouped median holds `(row, slot)` pairs, one list per run of rows.** It used to hold
  `(slot, value)` pairs and concatenate the threads' lists, 240 MB of fresh memory per 1e7-row
  query; page-faulting that in made groupby q6 from R two to ten times slower than the same kernel
  measured from Rust. Reading the value from the input column during the counting sort halves the
  lists and drops the concatenation.
- **Impact.** No new kernel or stored state; both verbs are thin plan steps. The typing fixtures in
  `frames.R.test` and `crates/table/tests/test_lazy.rs` pin the semantics.

## data.table's helpers run data.table's own C, and the C API is what makes them fast

- **Previous shape.** The plan was to route R calls to `fread`, `fwrite`, `setkey`, `rbindlist`, `melt`, `dcast`, `shift`, `fifelse`, `tstrsplit` and friends to the engine's kernels in `table::ops` and `table::io`, which beat data.table when measured from Rust. Under ry's runtime those calls ran data.table's own C through libr, at up to 2.5 times GNU R's time, because every C API call (`TRUELENGTH`, `STRING_ELT`, `LENGTH`, `INTEGER`, the type predicates, `PROTECT`) entered the full error boundary.
- **What measurement showed.** With the boundary skipped on those calls, data.table's C under ry runs at GNU R's speed or faster: `fread` of the 1e7-row groupby file takes 2.2 to 2.3 s against GNU R's 2.8 to 3.8 s (best of two, across separate runs), and `transpose` (the core of `tstrsplit`) 0.062 s against 0.055 to 0.068 s at 1e6 rows. Routing `fread` to the engine's reader made it slower, 3.2 to 3.7 s: the reader itself takes 1.4 s, but turning its columns into R vectors (one CHARSXP per string) costs about 1.9 s. The numeric helpers are already a `memmove` or a single loop in data.table's C, so the engine has nothing to win there.
- **Target shape.** No routing hook for the helpers. They stay data.table's own code, so their output is data.table's by construction. Their speed comes from the C API (`crates/libr/src/vectors.rs` and `memory.rs`): an accessor whose argument is a plain, non-ALTREP object of the right type reads it directly, and enters the boundary only to raise an error. Every package with compiled code gains from this, not only data.table. The engine is fastest where data stays inside it: frames (`frame_read_csv` reads the same file in 1.4 s) and the `[` bridge, whose results are small.
- **The `[` bridge imports only what a query reads.** It plans against a schema read from the R vectors' types, `query::referenced_columns` names the columns the plan reads, and only those convert. A character column converts each distinct CHARSXP once, because R interns its strings, and then gathers the views. Converting every column used to make `x[, sum(v1), by = id4]` on the groupby table 20 times slower than data.table.
- **Impact.** One source of truth for each helper's semantics, data.table's own code. The checker still types `fread`, `rbindlist`, `melt` and `dcast` as returning frames, which is not true at run time (`backlog.md`, "The checker and the runtime disagree about data.table's readers").

## Frames and the checker share one CSV reader, and data.table keeps its own

- **Rejected alternative.** A faster frame read path could have been a second, lighter CSV reader beside the engine's, with its own type inference and quoting rules, so that the checker's schema and the runtime's columns could drift apart.
- **Target shape.** The engine has one CSV reader: `table::io::read_csv` / `read_csv_with`, through `read_bytes` and `read_sliced` (`crates/table/src/io.rs`). The `sniff` crate decides the layout and the column types for both that reader and the checker (`semantics/src/check.rs`). A lighter or faster path is a change inside this reader, not a new one, and character columns read as factors go through the reader's own factor path (`io::strings_as_factors`, read back with `convert::factor_levels`).
- **data.table's helpers stay on data.table's own C.** No helper is routed to the engine, `fread` included (see "data.table's helpers run data.table's own C"). Measured at 1e6 and 1e7 rows, `fread`, `fwrite`, `rbindlist`, `setkey`, `setorder`, `unique`, `uniqueN`, `melt` and `dcast` all run at or below GNU R's time under ry. Each figure is the best of five runs of the call `tools/bench/table/ops.R` makes; `ops.R` itself keeps the faster of two runs, which can show `setorder`, `unique` or `uniqueN` up to 1.25 times slower on either engine, and repeated runs do not confirm that. A helper moves to the engine only if a measurement shows the engine winning end to end, including the conversion back to R vectors.
- **Impact.** One source of truth for CSV parsing and type inference on the engine side, and one for each data.table helper's semantics: data.table's own code.

## An escape lasts until the outermost native call returns

- **Previous shape.** A vector whose writable element pointer native code took (`STRING_PTR`, `DATAPTR`, `VECTOR_PTR` on a character vector or list) joined the escaped set for good, and every minor collection rescanned all of its elements. data.table calls `STRING_PTR` on its input columns, so after one `unique(x)` every later minor collection reread the 3e7 string elements of the groupby table, each read a cache miss on a CHARSXP header. `shift(x$v1)` went from 0.024 s to 0.14 s once any such call had run.
- **Target shape.** `Heap::retire_escaped` runs when the outermost native call returns (`crates/libr/src/boundary.rs`). Native code may store through the pointer only while that call runs, so each old escaped vector moves to the remembered set: the next minor collection rescans it once, then it is an ordinary object. GNU R's collector never sees those stores at all, so this is strictly safer than GNU R. While a call still runs, an old escaped character vector is not rescanned: a minor collection marks every young CHARSXP instead, which is sound because a character vector holds only CHARSXPs and they point at nothing. A young CHARSXP that is garbage then lives until the next major collection.
- **Minor collection budget.** The young budget is the larger of `GcPolicy::young_limit` (8 MB) and half the old small objects, as GNU R's cons-cell trigger grows with its heap ("Collections run on GNU R's schedule" says why old large vectors do not count).
- **Impact.** The single-loop data.table helpers run at GNU R's speed in a session that has called any data.table function, and `rbindlist` of two 1e7-row tables drops from 1.75 s to 1.26 s; `setkey`, `dcast` and `unique` now match or beat GNU R. `crates/heap/tests/test_gc.rs` pins that stores through an escaped pointer survive minor collections, before and after the escape is retired.

## data.table's C runs at or below GNU R's time: barrier, pacing and string kernels

- **Previous shape.** Inside `tools/bench/table/ops.R` several helpers trailed GNU R although they ran faster alone. The cost was the collector inside the timed region. Native code fills a fresh character vector one `SET_STRING_ELT` at a time, and each store looked up the parent's mark bit. `Rf_duplicate` of a character vector or list paid one barrier and one refcount check per element. Large vectors counted against the 8 MB nursery, so `fread`, which allocates every column before filling any, ran a minor collection per column, and each rescanned the 1e6-element string columns already allocated. After every major collection the old-generation limit fell straight to twice what survived, so the explicit `gc()` between two questions bought a full collection in the middle of the next one (31 ms of `rbindlist`'s 90 ms at 1e6 rows).
- **Target shape.** All of it lives in `crates/heap/src/heap.rs`:
  - The write barrier reads the parent's header OLD bit, so a store into a young vector costs one load and one compare ("The young generation allocates by bumping a cursor").
  - `Heap::copy_elements` copies a range between vectors with one barrier decision for the whole range, and `Rf_duplicate` uses it.
  - Large vectors have their own young budget, `GcPolicy::young_large_limit`: 16 MB, or half the old generation if that is larger ("Peak memory comes from what the heap keeps alive" says why not GNU R's 64 MB).
  - The major-collection limit shrinks by a fifth at most per collection instead of falling to what survived ("Collections run on GNU R's schedule").
- **Kernels.** `duplicated`/`unique` without incomparables hash a plain key per type, and for an all-ASCII character vector they hash the CHARSXP pointer, because interning makes equal ASCII text one object (`crates/interp/src/sort.rs`). `strsplit` with `fixed = TRUE` builds its `memchr::memmem` finder once per pattern and collects byte ranges instead of `String`s (`crates/interp/src/strings.rs`). `char_str` validates ASCII-flagged strings strictly, which runs a word at a time, but only as a hint: native code can set any gp bit through `SETLEVELS`, so the flag never licenses `from_utf8_unchecked`.
- **Impact.** At 1e7 rows every helper in `ops.R` is as fast as on GNU R or faster (table in `development/design/r-runtime.md`, section 7). `tstrsplit` takes 3.71 s against 5.73 s, `fread` 2.36 s against 3.16 s, and `rbindlist` 0.71 s against 0.90 s. The first `dcast` of a session no longer warms up slowly: 0.028 s against 0.047 s at 1e6 rows. These timings were taken with a 64 MB large-vector budget; at 16 MB, `fread` of a 1e7-row, five-column CSV, `tstrsplit` and `rbindlist` measured the same as at 64 MB (fread 1.31 s against GNU R's 1.27 s on one thread). The cost is memory: up to the budget in dead large vectors, and a collection limit that shrinks over eight collections. `RY_GC_VERIFY=1` passes the oracle suite, and `crates/oracle/cases/vectors__duplicated_fast_paths.R` pins the typed `duplicated` keys and fixed `strsplit` against GNU R.

## What keeps a dropped object alive: registers, handles, caches and dying finalizers

GNU R finalizes an object at the first full `gc()` after its last reference goes, and R6, lgr's buffered appenders and processx all depend on that timing. Four places in ry's runtime held a reference GNU R does not have.

- **The register file and the handle stack.** Both are roots. Tier-0 bytecode released handles only on a backward jump and never cleared a statement's temporaries, so a straight-line body kept everything it had touched until it returned. A statement of a `{` now ends with `Op::EndStatement dst start end`, which clears the block's result register and the statement's temporaries (registers are allocated in stack order, so they are exactly `start..end`), and `bytecode::execute_code` releases the activation's handles after every instruction. That is safe because between instructions every live value sits in a register. The AST interpreter's `do_begin` roots only the block's own value on the way out for the same reason. Compiled code shares the same register window, so the JIT lowers the op to inline `NULL` stores (`Action::Clear`), kills those registers in its liveness, and leaves handle release to tier 0 and the activation's exit. `evaluation__finalizer_run_points` pins the timing, and the bytecode, closure, promise and S3 cases give the same output under `gctorture(TRUE)`.
- **Bytecode inline caches.** A cache holds the environment a lookup started from and the cell it found, so a body shared between closures (R6's `clone()` rebinds copied methods with `environment<-`) kept the original object's environment alive. The cache list carries the heap flag `WEAK_PAIRS` (`Heap::mark_weak_pairs`): the marker does not trace its elements, and after weak references settle `clear_dead_weak_pairs` sets a pair whose key (the enclosure) or value (the cell) is unmarked to `NULL`, which is a cache miss, releasing the counts the barriered fill took. Minor collections need no special case: an old list that gained a young key is in the remembered set and is traced, and one that did not can only point at old objects. Emptying every cache at each collection was rejected, because each site then pays a full lookup after every collection.
- **Tier-1 code.** A compiled body embeds its enclosure and the cells its lookups speculated on, and the body outlives any one closure (R6 gives every object's methods the same body). The TIER1 external pointer therefore protects a weak reference keyed on the enclosure, whose value lists what the machine code embeds. While the enclosure lives everything stays reachable; once it dies, `compiled_for` frees the code and clears TIER1, which is safe because a running activation keeps its own enclosure reachable.
- **Removed binding cells.** A cell can outlive its frame entry while a cache holds it, so `remove_binding` sets the cell's value to the unbound marker and a stale cell reaches nothing.
- **Dying finalizers.** Marking a dying weak reference's key, value and finalizer can make another weak reference's key reachable after the ephemeron fixpoint ended, and that reference's value and finalizer were then swept while it stayed registered (processx's exit finalizers ran freed closures). `process_weak_refs` now takes every dying reference off the list before marking any of them, then queues and marks them all. What they reach can no longer be a registered reference's key that missed the fixpoint, because every reference still registered has a key the fixpoint already marked, as GNU R's `CheckFinalizers` does.
- **Visibility of a variable read.** Reading a variable can force a promise, whose expression then decides whether the value prints (`withCallingHandlers(f(), ...)` stays invisible when `f()` is). Compiled code sets the visible flag before the load, not after it, and the analysis treats the flag as unknown after a load that may force.
- **Expression vectors.** `eval()` of an expression vector (`eval::eval_all`, which is how testthat runs a test file) releases each element's handles before the next element runs, exactly as `{` does, so a `gc()` in one top-level statement of a test file finalizes what an earlier statement dropped (lgr's `AppenderBuffer` flushes on destruction this way).
- **Impact:** finalizer timing matches GNU R in the oracle cases `evaluation__block_finalizers`, `evaluation__expression_vector_finalizers` and `evaluation__finalizers_in_compiled_code`. The cost is one instruction per block statement in tier 0 and a handle-stack truncation per instruction. No effect on static analysis.

## ALTREP elements go through the class methods everywhere

- **Previous shape.** libr's C entry points dispatched ALTREP `Elt` methods, but the interpreter read and wrote vector elements through `Sexp::element` and `Heap::set_element`, which read raw storage. About 370 interpreter call sites then saw an ALTREP instance's private triple instead of its elements (nanoarrow's character columns crashed DBI's arrow round trip).
- **Target shape.** `heap::object::install_altrep_elements` takes a read and a write function, which libr installs at startup, and `Sexp::element` and `Heap::set_element` dispatch to them for an ALTREP object, so there is one access path and no call site can forget it. The accessor cannot fail, so an unwind from the method (an error, a condition handler's jump, an interrupt) is parked in the interpreter (`interp::unwind::park`) and raised as soon as the builtin that made the read returns (`call_builtin`), before its value reaches any R code, so `tryCatch(x[[2]], ...)` catches it; a read outside a builtin raises it at the evaluator's next poll instead. Meanwhile the read returns `NA` (strings) or `NULL`. `length()` asks the class's `Length` method, and `[[` on a string or list instance reads only the element it selects, so an R caller runs `Elt` exactly where GNU R does (`r_code_reads_only_the_string_elements_it_asks_for` in `crates/libr/tests/test_altrep.rs`). Numeric instances are still materialized whole for slice access. Making `element` fallible would touch every call site for a case only ALTREP classes reach. An ALTREP instance is never marked mutable, so the interpreter copies it before modifying it in place, as GNU R's `DATAPTR` contract implies.

## POSIX regular expressions run on an automaton when they need the longest match

- **Previous shape.** TRE patterns (`perl = FALSE`) ran on fancy-regex with `leftmost_longest`, which sends every variable-width pattern to the backtracker. lgr's `(.*z.*){10}` never finished.
- **Target shape.** `Matcher::Automaton` keeps two regex-automata `meta::Regex` beside the fancy one: a leftmost-first search gives the match start, and an anchored `MatchKind::All` search from there gives the longest end, which is POSIX's leftmost-longest rule in linear time. Capture groups still come from fancy-regex, and the callers that do not report groups (`regexpr` and `gregexpr` without `perl`, `sub` and `gsub` without backreferences) never ask for them. Patterns regex-automata cannot compile, such as ones with backreferences, stay on fancy-regex.

## Smaller runtime choices

- **PostScript output reuses the PDF device's drawing operators** with a prolog that defines them in PostScript, so the two devices share one renderer. The page is placed on the paper as GNU R places it (centred, turned a quarter for `horizontal = TRUE` with `%%Orientation: Landscape`), Only `onefile = FALSE` writes the `EPSF-3.0` header (one file per page through the `%d` name), and an EPS file has no `%%DocumentMedia` or `%%Orientation`. Otherwise `%%DocumentMedia` names the paper as given with its truncated size, `paper = "special"` is the device size (turned for `horizontal = TRUE`) and omits `%%Orientation`, and the bounding box is rounded; `graphics__device_files` pins these headers against GNU R. JPEG goes through `jpeg-encoder` from the same raster the PNG device draws.
- **The native codeset follows `LC_CTYPE`** (`l10n_info`, `iconv("")`), so the C locale reports ASCII as GNU R does and utf8's formatting under `LC_ALL=C` escapes what GNU R escapes.

## libR commits mimalloc arenas lazily, and handle vectors are not zeroed before their fill

- **Previous shape.** `libR.so` declares its own mimalloc, separate from the `ry` binary's, and only the binary turned off eager arena commit. In `libR` an eagerly committed arena is backed by transparent huge pages, so after earlier work had purged parts of it, the next large vector faulted them back in and the kernel zeroed and compacted whole huge pages. Every `Rf_allocVector` also zeroed the whole vector, which GNU R does not do for numbers, and a character vector or list was zeroed and then filled.
- **Target shape.** `libR` sets `mi_option_arena_eager_commit` to 0 from an `.init_array` constructor (`crates/libr/src/libr.rs`). `Rf_allocVector` allocates numeric vectors uninitialized through `Heap::alloc_vector_uninit`, and `Heap::alloc_vector` zeroes only the header of a vector it fills with handles. Purging stays on (`MIMALLOC_PURGE_DELAY=-1` shaves a little more, but resident memory would only grow).
- **Impact.** In ops.R's sequence at 1e7 rows, `rbindlist`'s slow first calls dropped from 2 to 6 s to about 1.3 s (GNU R: 0.9 to 2.1 s). Run-to-run noise on the other questions is larger than any change. Package loading gains too: it plans on every core, every planning thread's arena paid the zeroing, and the plan of a 326,000-line package went from 680-990 ms to 330-440 ms.

# Decision record: everything is fuzzable, simulatable and scriptable

This is a user directive: robustness is the long-run priority, so the whole toolchain is designed to
be fuzzed, simulated and driven by scenario tests. It widens the per-stage fuzzing record above from
the analysis pipeline to every component. `docs/src/content/docs/development/robustness.md` is the
contract.

- **Previous shape.** Each analysis stage had a seeded fuzz battery, but the language server could be
  tested only over a real socket with a real worker thread, so its interleavings (edits racing
  queries, cancellations, delayed watcher events) were untestable and unreproducible. The worker
  iterated a `HashSet` of open documents when refreshing, so the order of its publishes was
  nondeterministic. No harness compared a long editor session with a fresh start, and no test
  scripted a user journey across the editor, the CLI and the runtime.
- **Target shape.** Nondeterminism lives behind seams a seed drives. The server's worker sends
  through an `Outbox` (the socket, or a recorder) and its loop body is `run_job`, so
  `server::simulation` runs the real server single-threaded with a seeded schedule and injects
  cancellation at the Nth cancellable checkpoint (`cancel_at_checkpoint`), always followed by a real
  notification because the server assumes a newer edit is queued whenever it is cancelled. The
  oracle is a fresh server on the same disk and buffers, compared by input state, diagnostics, random
  requests, range geometry and the push contract. Expensive incremental cross-checks run behind
  `syntax::testing::enable_invariants()`. Scenario files script journeys through the simulator and
  the real binary. The file-system seam is a directory only the simulator writes, not an in-memory
  file system; injected IO errors are a gap.
- **Why a recorder and a step function, not a mock server.** The simulator must exercise the code an
  editor talks to. Two narrow seams keep production and simulation on one code path, and the
  simulated client sends real `lsp_types` notifications through the real `ServerState` handlers.
- **Harness modules ship in the library, not behind a cargo feature.** `server::simulation` and the
  crates' `testing` modules compile into release builds. A package's own integration tests see only
  its default features, so a feature gate would make the bounded passes opt-in, which breaks the
  rule that every harness runs in plain `cargo test`. They panic on a broken contract by design,
  since that is how a harness reports.
- **Impact.** Correctness: the simulator found divergences that unit tests and the existing fuzzers
  missed (a closed test file staying in the project, a lost `library()` rescan after cancellation,
  a formatting edit measured in bytes instead of UTF-16 units, and two splice reparses that
  disagreed with a full parse: an edit that opened a string the suffix closed, and one that left a
  `function(...)` whose body a full parse found in the suffix). Differential fuzzing against GNU R
  found three runtime bugs (NA against NaN propagation in `mean`, `sum`, `prod` and the cumulative
  functions, a zero-length replacement into a zero-length vector, and a top-level `nargs()`). Simplicity: two seams, no
  parallel implementation. Performance: none in production; the cross-checks are off unless a
  harness enables them. Incrementality: the from-scratch oracle now covers the server's own
  bookkeeping (open documents, attach sets, config, metadata), not only salsa queries.

## The presented R version comes from the base package the runtime runs

- **Previous shape.** Three sources: `Rversion.h` and `libr`'s `R_VERSION_STRING` said 4.6.2, `rcmd::R_VERSION` said 4.6.2, and the interpreter's `R.version` said 4.3.3, while `packageVersion("base")` came from the linked GNU R library. A package compiled from source took its R 4.6 C branches while its R code took the 4.3 branches: rlang's C made `ns_registry_env()` defunct and its R code still called it, which broke pkgload, generics and downlit; presenting 4.6.2 in R as well made `is_installed("base", version = getRversion())` false and sent R code to 4.4+ base functions that a 4.3 base lacks.
- **Target shape.** One source of truth: `library/base/DESCRIPTION` of the R home (`Version`, `Revision`, `Last Changed Date`). The interpreter reads it for `R.version` and `getRversion()` (`interp::system::r_version`), `serialize` writes it as the writer version, and `rcmd::assemble` generates the home's `include/Rversion.h` from the same file. The revision is raised to at least 89194, because ry implements the API data.table guards on it. The checked-in `crates/libr/include/Rversion.h` (4.6.2) serves only code built directly against `crates/libr/include`.
- **Why not always 4.6.** The runtime runs the base R code of the linked installation, so a version claim above it is false at the R level. With an R 4.6 library linked, as the packaging design intends, ry presents 4.6 and packages take their modern-API branches; with 4.3.3 linked, they take the 4.3 branches, which match the GNU R baseline the corpus is measured against.
- **Impact.** Packages installed from source must be reinstalled after a home's base version changes. `tests/test_install.rs` pins that `getRversion()`, `packageVersion("base")`, `Rversion.h` and `R --version` agree.

## An Array is a flagged R vector, written `[...]`

- **Previous shape.** ry had no strict vector. Typed code used `c()`, whose result R coerces (`c(1, "a")` is character), recycles, and lets hold `NA`, so neither the checker nor a DataFrame column could promise one item type without missing values.
- **Target shape.** `[a, b, c]` is ry syntax for `Array<T>`, the standard library's strict vector: one exact item type (a single `logical`, `integer`, `double` or `character`, or a ry value), no `NA`, no attributes, no recycling and no implicit conversion. The contract is `reference/type-system.mdx`, "Arrays"; the tutorial mentions it in `tutorial/runtime.mdx`.
- **Runtime representation.** An Array is an ordinary R vector carrying the heap flag `flag_bits::ARRAY` (`crates/heap/src/object.rs`): atomic storage for scalar items, a VECSXP for ry-value items. A separate object type was rejected because every C API accessor, `sum()`, and the JIT already read the unboxed storage, so converting to R is free (drop the flag) and nothing has to learn a new layout. Setting any attribute clears the flag, which keeps "no attributes" true without checks at every attribute write. `values::duplicate` and `shallow_duplicate` drop the flag on the copy (nested Arrays keep theirs), because a copier is about to write and a builtin or C code may write an `NA`; only `arrays::writable` re-flags a copy, after checking the write.
- **One place enforces the rules.** `crates/interp/src/arrays.rs` validates construction, operators (`arith.rs` wraps each builtin in `strict`), indexing and replacement, then flags the result. The bytecode fast paths and the JIT's type guard refuse a flagged operand, so they can never skip a check; a dedicated Array feedback kind is backlog work.
- **R code keeps working on Arrays.** Reads follow R's positions wherever R gives no `NA` (negative positions, zeros), and everything that would produce `NA` is an error. A replacement that is not an item of the item type (an `NA`, an R vector, another type) does not error: the result is the plain R vector R would build, so base functions such as `ifelse()` that assign into an argument keep working. An operator works the same way: an operand that is not an Array and not a single item of the item type makes R's operator run and return a plain vector (`arrays::check_operands`), because base `str()` computes `ob - signif(ob, digits)` and `signif()` returns a plain vector. Typed code cannot reach that path because the checker rejects the value; the rules the checker cannot see (bounds, no recycling) stay runtime errors, so a typed `Array<T>` never holds a plain vector.
- **`List` results.** A `List` function returns an Array only when its collection argument (`list`, `first`, `lists`) is one, never because an item argument is. A callback's result type is bound by the `item` constraint (`types/std.Rtypes`), so the checker rejects `List.map` to a non-item instead of failing at run time.
- **Checker.** `Array<T>` fits `T[]` (atomic items) or `list[T]` (ry-value items), never the reverse, so Arrays pass to R code and R vectors need `as.Array()`. A DataFrame stores an Array column as its plain vector, so `column_type` maps `Array<T>` to `T[]`.
- **Impact.** No duplicated state: the flag is the single source of truth for strictness, and the checker's type and the runtime flag agree by construction. Pinned by `crates/semantics/tests/typing/arrays.R.test`, `crates/interp/tests/test_arrays.rs` (including a generated round trip) and Array seeds in both fuzzers.

## Array storage stays row-major; record Arrays go columnar only with JIT field fusion

- **Scalar Arrays already have the data-oriented layout.** An `Array<double>` is one contiguous `double` buffer with no NA and no attributes, which is exactly what a hot loop wants: `x[[i]]` is a bounds check and a load, and the JIT's Array guard can drop the NA and attribute checks (`decisions.md`, "An Array is a flagged R vector"). Nothing in that layout changes.
- **The open case is an Array of records.** `Array<{x: double, y: double}>` is a list of record objects, one 48-byte object per item whose length-one numbers are already unboxed into slots (`crates/heap/src/object.rs`, `Slot`). A columnar shape (one atomic buffer per field) would save the per-item header and pointer, cut collector work from one object per item to one per field, and make `List.map(xs, \(p) p$x)` a column read.
- **Why not now.** Every `xs[[i]]` on a columnar Array must allocate a record to hand out, so the interpreter's element loop trades a pointer load for an allocation, and a list a builtin or `.Call` code reads with `VECTOR_ELT` would need a materialized copy. The layout pays only once the JIT fuses `xs[[i]]$field` into a column load, so it waits for that, and DataFrame already serves the columnar-records workload.
- **Trigger to revisit.** When the JIT compiles `xs[[i]]$field` on Arrays, measure a million-item record Array loop row-major against columnar (`backlog.md`, "Columnar record Arrays"), and switch only if it pays.

# Decision record: editor responsiveness at 300,000 lines

- **Previous shape.** Completion walked every item in the project per request, reading each item's
  salsa fields, re-walked every file's syntax tree for untyped `$` fields, resolved a scheme (and
  so possibly a type check) for every matching global, and sorted every candidate with an
  allocated key before keeping 128. References for a global named and lowered every item in the
  project. On finbench that was 30 ms p50 for completion and 300 ms p99 for references.
- **Target shape.** Two per-file tracked indexes in `crates/ide/src/ide.rs`: `global_names` (the
  top-level names a file writes, with their item) and `spelled_names` (the names spelled after
  `$`, `@` and `::`). They are derived from the item tree and the syntax, so they cannot drift, and
  an edit recomputes only the edited file's entry. Ranking selects the top results with
  `select_nth_unstable_by` and compares labels case-insensitively without allocating, and call
  shapes are resolved only for the results returned. References and rename skip a file, then an
  item, whose source text does not contain the name, because every occurrence spells it.
- **Requests preempt idle work.** A request cancels the idle unit in flight once the request is
  queued (`CancelHandle::preempt_idle` in `crates/ry/src/server.rs`); it never cancels a sync.
- **Durability.** A setter that names no durability keeps the field's current one, so the old
  `didOpen` rewrote every opened file at `MEDIUM` and invalidated every memo in the project. It now
  skips text that matches the disk and lowers the file to `LOW` in idle time, because salsa never
  backdates a memo whose durability drops, and the first `LOW` write therefore re-derives the
  file's dependents once.
- **Project-wide maps fold per-file memos.** `package_definitions`, `conditional_slot_items`,
  `interface_sccs` and their test-file twins read every item's projections directly. Once the
  edited file was `LOW`, each map was too, and confirming it after a keystroke checked about 56,000
  item memos, about 35 ms before any request could answer. They now read `file_definitions` and
  `file_conditional_writers` (`crates/semantics/src/semantics.rs`; `interface_sccs` is gone), so
  the check touches about 500 file memos and every untouched one passes on durability.
  `project_arithmetic_classes` needed no change: it reads only `package_definitions`.
- **Every host input but the open documents and the file list is `MEDIUM`.** The server and
  `analysis-stats` created the stubs, settings, metadata and data files at the default `LOW`, and
  nearly every memo reads the stubs, so no memo was ever `MEDIUM` and the file durabilities had no
  effect. With those inputs at `MEDIUM`, the maps must be `LOW` before any file's first edit, since
  a memo whose durability drops cannot backdate and the drop would re-execute every item check
  (about 2 s on finbench). `ProjectFiles` is therefore `LOW`, which keeps the maps `LOW`.
  `test_incremental.rs` pins both: an edit verifies only the edited file's items, and a file's
  first edit re-checks only its own.
- **Impact.** Settled requests are about 1 ms p50 except completion at about 4 ms, and a request
  right after a keystroke now costs about the same (0.5 ms hover p50 in `analysis-stats` on
  finbench, from 30 ms). Edited-file diagnostics fell from 36 ms to 1.5 ms median.

# Decision record: a parallel startup warm-up in the server

- **Previous shape.** The server primed the workspace one file at a time in idle units, and each
  unit also scanned the file's `library()` calls and applied the grown attach union at once. The
  first file's check derived every item's naming for the project-wide maps on one thread, about
  1.3 s on a 300,000-line project, and the first wave's lints read those maps, so a file opened at
  startup waited that long for any diagnostics. Each union change re-ran the open files'
  diagnostics, so an open file was republished about six times, with counts that flickered.
- **Target shape.** `Worker::warm_up` in `crates/ry/src/server.rs` runs before any other idle
  work: it scans every file's attach facts across all cores and applies the union once, then
  derives the per-file folds behind the project-wide maps (`warm_project_folds`) across all cores.
  It used to name every item as well, which the next record removed. Until it ends, the first wave
  holds only parse-stage diagnostics (the lints need the maps), pull diagnostics answer the same,
  and settled publishes wait; its end refreshes every open file. The rest of the cold check runs
  in parallel batches of 64 files. `Worker::in_parallel` is the one helper for both.
- **Stopping parallel work.** The threads run on cloned handles whose tokens `CancelHandle`
  cancels too, and they also poll a queued-job counter between files, because salsa resets a
  handle's cancellation flag when a top-level query returns (`attach.rs`, `DbGuard::drop`), which
  drops a flip that lands at a query's end. Without the counter an open sometimes waited for the
  whole warm-up.
- **Impact.** On dsbench a file opened at startup gets its first wave in under 15 ms (from about
  1.3 s) and its settled diagnostics about 0.55 s after startup (from 1.3 to 1.9 s), published
  once instead of six times. Analysis is unchanged; only the order and parallelism of work moved.

## ry run and ry R: the command chooses the runtime

- **Previous shape.** Two commands each carried a `--runtime` flag with its own default: `ry run script.R` and the `ry repl` console defaulted to GNU R, `ry load` to ry's runtime, and `RY_RUNTIME` overrode all three. Which runtime ran a file depended on the command, a flag and an environment variable, and the docs showed `ry run --runtime ry` everywhere. `ry run` also took FILE as its own positional, so clap still parsed `--help` or `--no-load` written after it.
- **Target shape.** The command names the runtime (`crates/ry/src/command_line.rs`). `ry run [FILE] [ARGS]` runs on ry's runtime, `ry R [FILE] [ARGS]` on GNU R, and without FILE each opens the console. `-i FILE` runs FILE and then opens the console, which is the home of `ry repl -f`. Both consoles load the package the working directory belongs to, and `--no-load` turns that off. `Command::session` resolves every spelling to one `Session { runtime, mode }`, so `main` dispatches on what to run.
- **Compatibility.** `ry repl` stays as a hidden alias with its GNU R default and its `-f`; `--runtime` on `run`, `repl` and `load` and `RY_RUNTIME` stay honoured but undocumented. The flag wins over the variable; `ry R` ignores the variable, because a command named after GNU R must run GNU R. Like every option, `--runtime` must come before FILE now: `ry run script.R --runtime gnu-r` hands `--runtime gnu-r` to the script on ry's runtime. That follows `Rscript` and was accepted deliberately.
- **One trailing positional.** FILE and its arguments are one `trailing_var_arg` vector: clap stops reading options only once a trailing positional has its first value, so everything after FILE reaches the script verbatim, as with `Rscript`. On GNU R the arguments follow `--args` in the embedded argv, so `commandArgs(trailingOnly = TRUE)` reads them on both runtimes. `crates/ry/tests/test_command_line.rs` fuzzes the parser against this property. `--keybindings` and `--no-load` conflict with a script, because they would be silently ignored there.
- **Exit codes.** 0 for success, 1 for a top-level error or a strict-mode refusal, 2 for a usage error, an unreadable script (read up front, before any R starts) or a runtime that cannot start; `q(status = n)` exits n. A batch run on GNU R starts R non-interactive with `--no-echo`, as `Rscript` does: a script sees `interactive()` as FALSE, a top-level error prints "Execution halted" and exits 1 through R's own path, and R prints no trailing newline when the session ends.
- **`-i FILE`** is checked under its own name with the same `Preflight` the console then continues (`Preflight::rename`), so a refusal exits 1 before the console opens, later commands see the file's definitions, and the file is fed as already-checked console input.
- **Impact.** One rule for users (the command is the runtime) and one resolution function, `resolve_runtime`, which `Command::session` and `ry load` both call; the property tests pin it. No analysis state changes.

## Source-change detection lives in the loader, so both runtimes reload the same way

- **Previous shape.** The ry binary kept a per-root fingerprint map beside its `Loader` (`crates/ry/src/load.rs`), updated by the GNU R `.Call` routines, and its `ReloadWatcher` compared it before each command. `libR.so` has its own `Loader` (`crates/loader/src/ry_runtime.rs`) that the binary cannot see, so ry's console had no watcher, no `load_all()` and no package autoload. The fingerprint was duplicated state outside the loader that owns what is loaded.
- **Target shape.** `Loader` records the fingerprint of the sources a plan read (taken before reading, so an edit made mid-read counts later) and promotes it on `commit`. `Loader::changed_roots` reports each committed package whose fingerprint moved, once per change. Both backends ask it: the binary's `ReloadWatcher` for GNU R, and for ry's runtime `libR.so` exports `ry_loader_changed`, which `repl`'s `RuntimeReloadWatcher` calls before each command. `ry_repl` attaches `tools:ry` and runs a startup load, so `ry run` in a package loads it as `ry R` does.
- **Impact.** One source of truth per loaded package, one console loop on both runtimes, and the reload is the same `.ry_autoreload()`. The check stats `DESCRIPTION`, `NAMESPACE` and the code files in `R/` (the same extension test `collated_r_files` uses, so editor swap files do not reload) without reading them. It overlaps the per-file `stamps` that `Loader::read` keeps, on purpose: `stamps` decides which texts a plan rereads, the fingerprint decides whether to plan at all, and merging them would make the per-command check walk `Collate` too. `crates/loader/tests/test_reload_fuzz.rs` and `test_repl_e2e.rs` pin it.

## A reload re-derives one file and rewrites only the bindings it changed

- **Previous shape.** `package_index` built the package's name table (`names`) in its own body from every file's items, so any edit re-ran a loop over all 25,000 items of a large package; `loader::diff` then rescanned every name for a laziness flip, rebuilt the eager and constant lists, and compared the S3 directive sets in full. Each item's load facts were a tracked query (`item_load`, with `item_syntax` and `item_node` behind it), and on the R side every reload resynced the namespace's exports and the attached package environment by rescanning all 3,100 exports. A body edit on the 326,000-line package took 35 to 60 ms to reach output, 18 to 40 ms of it planning.
- **Target shape.** The binding facts are a firewall of their own: `file_binds` per file and `package_bindings` joining them into `PackageBindings` (laziness per name, each lazy name's item position, the eager and constant positions in collation order), returned behind an `Arc`. A body edit leaves a file's binding facts equal, so `package_bindings` keeps its value and `diff` sees the same `Arc` (`Arc::ptr_eq`) and skips the laziness flip scan. An item's load facts are computed inside `file_items` from its node rather than memoized per item. The plan carries `keeps_names` (every binding it writes was bound before and none goes away), which lets `runtime.R` skip the exports resync and copy only the touched names into the package environment.
- **Why per file, not per item.** Salsa re-verifies every memo whose durability an edit's input had: the package's files start at high durability, so the first edit of each file re-verifies every memo the plan reaches, and only later edits of the same file (now low durability) take the durability shortcut. One memo per file is far cheaper to re-verify than several per item; the first edit of a new file still re-verifies about 8 memos per file and 5 per eager item. Starting files at low durability would make every edit pay that cost, so it was rejected.
- **Impact.** On the 326,000-line package a repeated body edit reaches output in 9 to 12 ms (plan 6 to 8 ms), and the first edit of each file in about 30 ms. `test_incremental.rs` pins that a body edit re-runs one file's layer and stops at `package_bindings`. The cold load also warms `file_binds` across cores.

## A cold load is analyzed while base R starts

- **Previous shape.** The first `load_all()` of a console or `ry load` session planned from nothing after base R had loaded, so parsing and indexing the package (about 400 ms on the 326,000-line package) ran strictly after R's own startup (about 140 ms). The plan also deep-cloned every indexed item into the diff's working lists, re-erased each item's text a second time only to hash it, and matched erasure marks through a hash lookup at every token boundary.
- **Target shape.** `Loader::prepare` reads the sources and derives the whole index, `package_index` included, without making a plan; the host calls it on a thread before starting R (`ry_loader_prepare` exported from libR for ry's runtime, `load::prepare` for GNU R). The plan that follows takes the loader's lock, finds everything memoized, and only diffs. Strict mode moved to an atomic beside ry's runtime's session, so applying the session's flags never waits for the prepare to release the lock. The diff borrows items instead of cloning them, an item's hash comes from its slice of the file's erased text, and the eraser walks marks sorted in a single pass.
- **Impact.** Cold `ry run` on the 326,000-line package went from about 0.95 s to about 0.74 s (median of seven), and the analysis has no second source of truth: prepare and plan share one salsa database, so a file edited between them is read again and re-derived as usual.

# Decision record: a file's check names only what its interface reaches

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

# Decision record: data-oriented design and explicit allocators

Performance comes first (user directive, `AGENTS.md` "Goals"), so hot data is laid out for the
cache and allocated in bulk. The open audit list is `backlog.md`, "Open: data-oriented layouts and
explicit allocators".

- **The rule.** Hot data is a set of dense arrays indexed by small `Copy` ids (`u32`), not a graph
  of individually boxed nodes. Split a record into parallel arrays when a hot loop reads only some
  of its fields, keep variable-length children in one flat side array addressed by ranges, and use
  bitsets for per-element flags. Memory that lives exactly as long as one phase (one item check,
  one parse, one JIT compilation, one query plan) comes from an arena or bump allocator that is
  reset or dropped as a whole, so the phase pays neither per-node `malloc` nor per-node `free`.
  Long-lived objects of one shape come from pools of that shape. A new hot structure that holds
  `Box`, `Vec` or `String` per node needs a reason.
- **Where it applies first.** The checker's per-item inference (types built mid-check), HIR
  lowering, the JIT's analysis plan, and the heap's mark and sweep. The table engine already keeps
  columns in Arrow buffers and accumulators as struct-of-arrays, so it changes only at the edges.
- **The toolchain is a pinned nightly (user directive), and std's allocator API is the interface.**
  `rust-toolchain.toml` pins a dated nightly. Rust 1.100.0 (stable on 12 November 2026)
  stabilizes a subset of `allocator_api` (rust-lang/rust#156882): the `Allocator` trait, `Global`,
  `System`, and the allocator parameter of `Box<T, A>` and `Vec<T, A>` with their `new_in` and
  `with_capacity_in` families. The pinned nightly already has that subset with no feature gate;
  `#![feature(allocator_api)]` now only warns that it was partially stabilized, and the unstable
  remainder lives under `allocator_ext`. Verified on the pinned nightly: `Vec::new_in(&bump)` and
  `Box::new_in(x, &bump)` build against `bumpalo` 3.20 with its `allocator_api` feature, and
  `hashbrown::HashMap::new_in(&bump)` builds with hashbrown 0.16's `nightly` feature, which makes
  it take std's trait. So arenas are `bumpalo` through std's `Vec` and `Box`, maps stay on
  `hashbrown` (std's `HashMap`, `String`, `Rc` and `Arc` take no allocator parameter), and the
  earlier `allocator-api2` polyfill route is dropped. One trap: in any build that also links
  Cranelift, `bumpalo`'s `allocator_api` feature must stay off, because Cargo unifies it with the
  `allocator-api2` feature `regalloc2` uses, and with both on `bumpalo` drops the `allocator-api2`
  impl and `regalloc2` fails to compile. The JIT therefore wraps `&Bump` in its own std
  `Allocator` (`Arena` in `crates/jit/src/analysis.rs`).
- **The pin moves forward deliberately.** Bump the date in one commit that also fixes what the new
  nightly breaks (new lints, rustfmt changes), never by floating `nightly`, so a build is
  reproducible. `flake.nix` reads the channel from `rust-toolchain.toml`, so a bump also needs
  `nix flake update rust-overlay` when the locked overlay predates the new date. `cargo install
  --git` does not read the file, so the installation page tells users to pass `+nightly`.
- **A nightly feature is enabled only for a clear performance or design payoff,** with a
  `#![feature(...)]` at the crate root and a one-line reason beside it. Likely candidates:
  `allocator_ext` (the unstable rest of the allocator API, if an arena needs it), `portable_simd`
  (table kernels), `likely_unlikely` (branch hints on interpreter and heap fast paths),
  `never_type`, `try_blocks` and `trait_alias`. Features marked incomplete (`specialization`,
  `generic_const_exprs`) stay out. Nightly rustfmt also applies the unstable `group_imports` and
  `imports_granularity` settings in `rustfmt.toml`, the style the original code used, so each file
  has one `use { ... }` tree.
- **The global allocator stays mimalloc.** The `ry` binary sets it as `#[global_allocator]`
  (default feature `mimalloc`, lazy arena commit in `crates/ry/src/main.rs`), and `libR.so` has its
  own (`crates/libr/src/libr.rs`), pinned to `features = ["local_dynamic_tls", "v2"]`: the default
  TLS model fails `dlopen`, and v3 crashes in `mi_thread_init` on the second thread that allocates
  after the first one exits. v2 stays: under `ry run`, data.table's OpenMP workers and Polars'
  thread pool run beside it without a crash (both keep their threads alive). The v3 crash needs
  an allocating thread to exit before another starts allocating, which the libr test binaries do
  and a package that starts its own short-lived threads could too, so v2 removes the risk. Move to v3 only once a `libmimalloc-sys` release passes the libr test binaries with it.
  Cargo unifies features across the packages of one build, so `cargo build -p ry-lang -p libr`
  gives the binary v2 as well; `mi_option_arena_eager_commit` is option 4 in both versions, so
  `main.rs` is correct either way. Arenas sit on top of the global allocator, not in place of it.
- **Salsa.** Values a tracked query returns, and interned data, must own their memory and compare
  by value, because salsa stores them across revisions and backdates by equality. So an arena
  never leaks into a memoized result: a query does its work in a scratch arena and copies the
  result out into a compact owned form (boxed slices, dense tables indexed by `ExprId`). Interned
  ids (`Ty`, `Name`) are already dense indices, but every intern goes through salsa's shared
  table. Types an item check builds and throws away belong in a per-check store, and only
  exported schemes and recorded expression types get interned.
- **R's C API.** Every `SEXP` lives in `heap`'s collector, because native code holds it for an
  unknown time, through `PROTECT`, preserved objects and external pointers, and R's semantics
  decide when it dies. A phase arena must never back an R object that can escape to C. The heap's
  own size-class pages are already per-shape pools, and data-oriented design applies inside it
  (side mark bitmaps, allocation fast paths) as long as a `SEXP` stays a stable, non-moving
  pointer to its header. `R_alloc` is the one arena R itself defines (`crates/heap/src/arena.rs`).
  Arenas suit analysis, the parser, and the JIT's compile-time data.
- **Measured: the JIT's analysis plan.** Operands borrowed from the bytecode words, abstract
  states as rows of one flat matrix, liveness as bitsets, `block_at` as a dense array, all in one
  `Bump` per compilation (`crates/jit/src/analysis.rs`). Over the seven `tools/bench/why`
  programs, analysis time per program fell from about 6.0 ms to about 4.5 ms (min of five runs,
  roughly 100 compiled bodies each). The first cut was 25% slower than the old layout, because the
  state matrix grew by doubling inside the arena, and a bump arena cannot reuse a vector's old
  buffer, so every regrowth copied the whole matrix: reserve arena lists whose final size is
  roughly known. Analysis is about 5% of compile time; Cranelift's code generation is the rest.
- **Impact.** Fewer allocations and frees per phase, smaller memos, and scans that read contiguous
  memory. Incremental analysis is unaffected as long as memoized results stay owned and
  value-compared, which this rule requires. Each audit item is measured before and after, and a
  layout change that does not pay is reverted.

# Decision record: every host keeps every tree

The server still keeps every tree; `ry check` now releases them once (see "Decision record: `ry
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
  contended on hot types.
- **Correctness.** Findings are identical on the three benchmark code bases (2337, 1853, 784).
- **Simplicity.** The batching driver, `limit_resident_trees` and the eviction-only comments are
  gone.
- **Performance.** Cold `ry check` went from about 2.2 to 2.5 s to about 1.6 to 1.8 s on the
  benchmark code bases; peak RSS rose from about 485 to 565 MB. mimalloc's remaining time is
  fresh-page setup, and its purge and huge-page options measured no difference.
- **Incremental.** Unchanged: the server never evicted. The remaining waits in the cold check are
  salsa claims on project-wide queries (`duplicate_binding_map`, `project_arithmetic_classes`,
  `project_type_definitions`) that one thread computes while others need them.

# Decision record: `ry check` releases its trees once, and memoized records are compact

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

# Decision record: HIR as struct-of-arrays, lowered from the green tree

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

# Decision record: the interpreter's call path, lookups and allocation

- **Goal.** Per-test time on the three benchmark code bases, without the JIT, at or below GNU R's.
  The cost was spread over the interpreter core (`step`, frame lookup, argument matching, an
  allocation per binding and promise), so the fixes are many small cuts rather than one redesign.
- **A closure call's heap objects are the environment, one cell per formal, and the argument
  promises with their pairlist; the bookkeeping stays off the Rust heap.** Arguments and matched
  formals live in inline `SmallVec`s (`eval::Args` holds 10, `eval::Matched` 8, so a call with
  more spills to one Rust allocation), callers fill a vector they own (`promise_args_into`,
  `evaluate_args_into`), and the frame lives inside the environment object. A formal whose default
  is atomic or `NULL` binds that value directly with `BINDING_MISSING` set, because a promise of a
  constant costs an allocation and a force for the same value; `missing()` reads the bit, not the
  binding's type. A typed closure turns such a binding back into a promise on entry, because an
  inline annotation checks a default only when the body uses it.
- **Duplicate formals keep the first binding.** `formals<-` can make them, and `define_formals`
  skips the probe that would catch them, so the frame (scanned or indexed) answers with the first
  cell, as GNU R's frame list does.
- **Frames scan up to 16 cells before building a hash index**, and hold nine bindings inline as
  symbol and cell pairs, so a lookup reads one cache line per four bindings. `identical` has nine
  formals, and building a map per call cost more than scanning. Separate symbol and cell arrays
  were measured and cost 3% more instructions, because inserts then check two spills.
- **A call binds its formals in one batch** (`Heap::define_formals`): one collection check that
  keeps every value and the environment alive, then the cells, then one write barrier.
- **Compiled code inlines more of base**: `.Internal(f(...))` of a non-dispatching builtin
  compiles to register loads plus `CallBuiltin`; `local(expr)` compiles to a closure creation plus
  a call, as GNU R's compiler does, behind the usual guard that `local` is still base's (base binds
  it as a lazy-load promise, so the guard compares against the forced value); and a function
  expression compiles its body once per site, shared by every closure the site creates.
- **Scalar `x[i]` and `x[[i]]` on a plain vector skip general subscript resolution**, in the
  subset builtins (`subset.rs`) rather than the compiler, so both evaluators take it.
- **A compiled read of a promise-bound variable forces the promise it found** instead of
  restarting the lookup through `eval_symbol`; only active, missing and unbound cells take the slow
  path.
- **A for loop reuses its scalar** when only the loop binding references it, as GNU R's `STEPFOR`
  does, and the AST loop releases each iteration's handles instead of rooting them until the end.
- **Minor collections mark only roots added since the last one** (new symbols, newly preserved
  objects), because older ones are already marked old. The nursery stays at 8 MB: 32 MB was
  measured slower, because first touches of fresh pages cost more than the collections saved.
- **Compiled regular expressions are cached by pattern and flags**, and fixed argument names
  (`PACKAGE`, `NAOK`, `.Environment` and friends) are interned once in `values::symbols()`.
- **`inherits()` follows `class()`**: an object with no class attribute inherits from the implicit
  class `class()` reports (`matrix`/`array`, else the basic class), not the dispatch chain, so
  `inherits(1L, "numeric")` is `FALSE`, as in GNU R. It compares bytes and allocates nothing.
- **A hashed frame's table is flat head and next arrays indexed by slot**, and callers
  that only need the set (`length(env)`, sorted `ls()`, `lockEnvironment`) use
  `Heap::bound_symbols`, which skips the walk.
- **`step` is inlined into the dispatch loop** and reads the activation through a `Decoded` struct
  (code, constants, caches, feedback pointer), so a simple instruction costs no call; type
  feedback writes through a raw pointer that is null when no tier is collecting it. This cut
  `mean.default` by about a quarter of its instructions.
- **`GetFun` always looks in the local frame first.** Starting a function lookup in the
  enclosure's cached cell when the compiler sees no local binding of the name was tried and
  rejected: a frame gains bindings the compiler cannot see (`assign(..., envir =
  parent.frame())` as zeallot's `%<-%` does, `<-` inside a default argument, `base::assign`, the
  locals `UseMethod` copies into a method's frame), and a frame binding does not move the symbol
  epoch that guards the cache. `Guard` keeps that shortcut for operators only, as GNU R's
  compiler does.
- **S3 dispatch allocates no implicit class vector**: `Globals::implicit_classes` caches one shared
  (not-mutable) vector per dimension shape and type, except for calls and records, whose class
  depends on the value. Method names are built in a stack buffer and interned directly.
- **A `.Call` routine's external pointer holds the `Routine` entry's address**, so `try_call`
  finds the implementation by index in a table built once; a pointer that comes back null from
  serialization falls back to the lookup by name. C code that calls `R_ExternalPtrAddr` on such
  a symbol gets that entry's address, not a function, but it got null before, which fails the
  same way; every libr foreign-call path goes through `try_call` first.
- **`c()` of untagged, unnamed atomic vectors copies slice by slice** into the result
  (`combine_unnamed_atomic`), because the general path records every element and its name first
  and was the largest source of Rust allocations in `sd()`.
- **The random generators skip work GNU R repeats per call, and stay bit-identical to it.**
  Measured on the Vasicek simulation (one `rnorm(200)` and one `m[i, ] <- ...` per step), these
  cut ry from 142.5k to 87.3k instructions per step, against GNU R's 117.7k.
  - `GetRNGstate` returns early when `.Random.seed` still holds the session's state (same kind
    code, Mersenne position in range, same words), because rebuilding the generator from the
    vector is the dominant cost of a short draw. `PutRNGstate` writes into an uninitialized vector.
  - `rnorm` with scalar, finite `mean` and positive finite `sd` under Mersenne-Twister and
    Inversion fills the result in two passes (all uniforms, then `qnorm` of each), so the twister
    and the AS 241 central branch run as tight loops. The arithmetic is GNU R's exactly, with no
    fused multiply-add, so streams match `set.seed` runs in GNU R. Every other case draws one at a
    time through a closure that `random_vector` monomorphizes per generator.
  - `m[i, ]` and `m[i, ] <- v` on a 2-D double, integer or logical matrix with a plain scalar row
    copy the strided row directly instead of resolving both subscripts.

## `just ry` builds ry's runtime; ry-lang does not depend on libr

- `ry run` loads ry's runtime (`libR.so`) beside the executable at run time, and libr is a separate
  cdylib, so `cargo run -p ry-lang` never builds it. `just ry` builds `-p ry-lang -p libr` first,
  and `just build` builds the whole workspace; the not-found error names both.
- A nightly artifact dependency (`artifact = "cdylib"`) was rejected: every `ry-lang` build,
  including checker-only and LSP work, would compile libr and Polars, and `cargo install` would
  still lose the library, because the artifact path points into the build directory. Embedding
  the library in the binary would double its size for users who never run code.

## A DataFrame column reads back as a plain vector, not an Array

- `DataFrame(price = [12.5, 9.99])` checks each column as an Array, then stores it as the plain
  vector, and `df$price` reads back a plain `double[]`, statically and at run time.
- Typing the read as `Array<T>` was rejected: a frame's later steps can put `NA` into a column (a
  left join's unmatched rows), so the column type would have to be tracked per verb and an
  `NA`-holding column would need a second representation. One column type keeps the schema the
  single source of truth. `as.Array(df$x)` is the checked way back, and it fails on an `NA`.
- A narrower rule was also rejected: reading an Array only when the checker can prove no step put
  an `NA` in (no left or outer join, no `NA`-producing expression). An Array changes how R
  arithmetic behaves (no recycling, no `integer`/`double` mixing), so the same `df$units *
  df$price` would pass or stop with an error depending on which verbs ran before it, and a
  refactor that adds a join would break unrelated arithmetic. The rule is in `type-system.mdx`
  ("DataFrames").

## The engine's workers return freed memory at large frame boundaries

- **Before.** Nothing returned memory that the engine's worker threads had allocated. libR's
  allocator is mimalloc, which queues a block freed on another thread for its owning thread and
  processes the queue only when that thread allocates again. Polars' and rayon's workers allocate
  a table's columns, the R thread frees them, and the pools then sit idle, so a 5e7-row
  `DataFrame.read_csv(strings_as_factors = TRUE)` left 9 GB resident for a 1.7 GB table, and the
  db-benchmark group-by script ran out of memory on a 15 GB machine where data.table peaked at
  6.4 GB.
- **Shape.** `table::on_every_worker` runs a closure on every thread of both pools;
  `interp::frame::release_worker_memory` runs `mi_collect(true)` there and on the R thread. It runs
  when a frame of at least 2^22 cells is wrapped or finalized, and in `gc()`. The size floor keeps
  loops over smaller frames from paying for a broadcast per iteration.
- **Trap.** A broadcast waits for every pool thread, and a forked child has none of its parent's
  threads, so a child that drops a large frame or calls `gc()` would hang. Whoever implements
  `fork` (`mcparallel`, `mclapply`) must skip the broadcast in the child.
- **`mi_collect(true)` drains each thread's queue of blocks freed elsewhere,** so their pages
  become reusable, and that reuse is what bounds the peak. It also returns the pages whose purge
  is scheduled ("libR keeps freed memory until `gc()`").
- **Rejected.** `MIMALLOC_PURGE_DELAY=0` only purges pages already freed, so it left 5.9 GB; a
  per-query arena would also fix it but needs every kernel to allocate through it.

## Group codes wider than 64 bits are ranked, not handed to Polars

- The group-by kernels code the keys as one mixed-radix u64 per row. db-benchmark's six-key
  question at 5e7 rows needs about 2.6e19 codes, so the kernel used to decline, and Polars' own
  multi-key group-by then took more than 12 GB.
- `row_codes` (`crates/table/src/group.rs`) now replaces the codes so far by their rank among the
  distinct codes before a key column that would overflow. A rank keeps the code order, so `keyby`
  stays sorted, and it is below the row count, so any number of key columns fits as long as each
  fits on top of the row count. The question takes 7 s at a 6.2 GB peak; data.table takes 10.5 s.

## Double sums group by code range, one scan per thread

- A double `sum` or `mean` adds each group's values in row order, as data.table does, so the
  dense group-by kernel cannot split the rows into chunks and merge partial sums (the result would
  round differently and depend on the thread count). It used to run such a query on one thread,
  which at 5e7 rows and 500,000 groups missed cache on almost every row and lost db-benchmark's
  q3 and q5 to data.table.
- `dense_by_code` (`crates/table/src/group.rs`) gives each thread a contiguous range of codes; the
  thread scans every row, keeps its own in ascending order, and the threads' states concatenate in
  code order. The same path serves any dense grouping whose per-thread states would outnumber
  the rows.
- Rejected: scattering the rows into many code-range partitions first, as the hashed path does.
  A partition's rows are spread over the whole table either way, so each partition reads every
  cache line of every value column, and the scatter is a pass of its own. Measured from the Rust
  driver at 5e7: q3 1.42 s scattered against 1.18 s scanned, q4 1.71 s against 1.19 s. Keeping
  one thread below a cache-sized slot count was slower too (q4 1.42 s).

## Factors are ry's own codes plus one level table, outside Polars

- **Previous shape.** A factor column was a Polars `Enum`; `string_to_factor` built the levels
  (parallel hash partitions, byte-order sort) and then called `FrozenCategories::new`, which
  interned every level a second time at roughly 135 bytes per level. The CSV reader parsed factor
  columns as `Categorical` with a shared mapping and re-read a column whole as text when it held
  an `NA`. At db-benchmark's 1e8 join inputs (up to 1e8 distinct levels) the Enum alone kept
  `join_x` from fitting next to `join_big` in 15 GB.
- **Target shape.** `crates/table/src/factor.rs`: a factor is a Polars extension type
  (`ry.factor`, feature `dtype-extension`) whose storage is a dense zero-based code column
  (u8, u16 or u32 by level count) and whose dtype holds one `Arc<Levels>`: a `Utf8ViewArray` of
  labels in R's order plus a lazily built label-to-code hash index. Extension series filter,
  take, sort, group and append through their storage, and `to_physical_repr` gives the codes, so
  `kernel::Codes::of` and the row encoder need no factor case. Casting an extension column is
  refused by Polars, so every conversion goes through `factor::cast`, `factor::text` or their
  lazy forms `cast_expr`/`text_expr` (Polars `map` UDFs).
- **Reader.** A factor column parses as String slice by slice, and each slice is interned into a
  persistent `Interner` with 256 hash partitions, each owning its labels' views and byte arena (a
  label of up to 12 bytes lives in its view). Codes during the read are `index << 8 | partition`.
  A partition that the first slice fills with mostly distinct labels is sized for the whole file
  from that slice, because growing it later copies and rehashes every label and holds both tables
  at once. `finish` sorts one 16-byte key per level (the label's first 12 bytes read big-endian, then
  its code), stored in the view type so that the sorted keys become the label views in place, re-sorts only the runs
  whose prefix ties, remaps the codes in parallel, and turns the arenas into the label buffers
  without copying a byte. `NA` never enters the levels, so the whole-column text re-read is gone.
- **Joins.** `join::factor_codes` translates one side's codes into the other's levels through
  `Levels::position`; identical level tables (the same `Arc`) translate by identity.
- **Impact.** One source of truth for a column's levels, about 16 bytes of view plus the label
  bytes per level, and no second interning pass.

## Releases are built natively per platform, with the runtime beside the binary

- **Before.** `just release` cross-built `ry` alone for three targets with nix and zig on one
  machine, and the archives held a single `ry`. That was fine while ry was only a checker, but
  ry's runtime is a second artifact (`libR.so`/`libR.dylib`) that links zlib, bzip2 and xz on
  Linux, which zig's glibc 2.17 sysroot does not carry, and macOS cross-linking has only
  libSystem stubs.
- **Shape.** `.github/workflows/release.yml` (staged in `.github/pending/workflows/` until a
  maintainer moves it) builds each platform on its own runner with `just package VERSION`,
  smoke-tests the archive with `tools/smoke-test.sh`, and publishes. `just package` is the one
  recipe CI and `just release-dry-run` share, so a local dry run exercises the same packaging. An
  archive is flat (`ry`, `LICENSE`, `THIRD_PARTY_NOTICES`), because the Zed extension expects `ry`
  at the archive root; the runtime is embedded in `ry` (see "A release is `ry` alone").
- **Targets.** Linux x86_64, macOS arm64 and Windows x86_64, the three the VS Code extension has
  always bundled. Linux arm64 and Intel macOS were dropped for the first runtime release, because
  their runtimes had never run anywhere; `ry.vsix` without a binary still serves them, and adding
  a matrix row brings them back.
- **Windows is `x86_64-pc-windows-gnullvm`, not MSVC.** The archive carries `ry.exe`, `R.dll` and
  `ry-rcmd.exe`, built with MSYS2's clang64 toolchain, so all three link the UCRT as Rtools-built
  package DLLs do and can share one C runtime with them. `just package` refuses a Windows binary
  that imports a toolchain runtime the archive does not carry.
- **The VS Code extension bundles the archive's `ry`.** `just build-platform-vsix` copies it into
  `editors/code/bin`, and `just build-extension` adds the combined `LICENSE` and the notices.
- **Publishing.** The GitHub release uses the workflow's `GITHUB_TOKEN`. The extension goes to the
  VS Code Marketplace only, by user decision; Positron and VSCodium users install the `.vsix` files
  attached to the GitHub release, and there is no Open VSX step or token. The Marketplace step uses
  trusted publishing (`vsce publish --oidc`, which needs vsce 4.1 and a policy on the publisher; packaging stays on the locked vsce 3.6, because 4.1 rejects bun's `node_modules` layout),
  because the only token the Marketplace accepts is an Azure DevOps global PAT, and those are
  retired on December 1, 2026. A `VSCE_PAT` secret, when set, is used instead, as a fallback.
- **One version per release.** `just bump-version` sets the CLI to `X.Y.Z-beta` and both
  extensions to `X.Y.Z`; the Zed extension's version used to be bumped by hand, which let it drift.
  The news page is the release record, and the stale `CHANGELOG.md` was removed so there is one.
- **Linux glibc floor.** Native builds on `ubuntu-22.04` need glibc 2.35, up from 2.17. Accepted
  for the beta; a manylinux-style container is the fix if users on older distributions ask.
- **Nix.** The flake's toolchain comes from `rust-toolchain.toml` (rust-overlay's
  `fromRustupToolchain`). `nix build` builds `ry` and libR, installs the library at
  `lib/ry/libR.so`, and wraps `ry` with `R_HOME` defaulting to nixpkgs' R, because a Nix-built
  binary cannot load another distribution's libR; a user-set `R_HOME` still wins. `ry-unwrapped` skips the wrapper. The zig cross outputs
  remain for the checker-only binaries.

## In-place updates through nested targets

- **Before.** Only `x <- f(x, ...)` with a builtin flagged `reusing_first_argument` updated in
  place, so `state$seen <- Dict.insert(state$seen, k, v)` copied every step: the list element was
  a second holder. Compiled code had a hole as well: registers hold values without counting as
  references, so an update from outside the body (`eval.parent()`, a forced promise) could change
  a value a register was about to use, and `r <- list(x, h())` with `h` running
  `eval.parent(quote(x[[1]] <- 99))` gave `r[[1]][[1]] == 99`.
- **Nested targets.** `rebind` accepts a target that is a chain of `$` or `[[` with a constant
  name, string or number (`same_expression` compares both sides). `solely_held` walks the chain
  requiring one reference at each plain list, and no other argument may be the value or a list
  on the chain, because argument values are not counted (`holds_on_chain`).
- **Bytecode.** `CallRebinding` carries the target so compiled code takes the same path. It is
  appended last because `OPS` is indexed by discriminant. The JIT treats it as a generic call and
  inlines it only when the target's slot type is plain.
- **Register rule.** `gp::COMPILED_FRAME` marks an environment while a compiled body runs on it.
  `reuse_permitted(env, value)` allows an update when the flag is clear, when the innermost frame
  is that environment's own compiled closure (whose updates the compiler orders with
  `may_modify_in_place`), or when no live register holds the value. A stale register only costs a
  copy. Applies to `complex_assign` too, so it fixes plain R as well as the library.
- **Moving a value into a helper's parameter** was first rejected as unsound and is now built
  with escape checks and an undo journal; see "Moving a collection into a helper's parameter".
- **Collection storage stays as it is.** A dictionary or set is already struct-of-arrays: a key
  vector, a value vector and an open-addressing index in an integer vector, all in the collector's
  heap, which a `SEXP`-holding value must be (`decisions.md`, "Data-oriented design and explicit
  allocators"). Hashing and probing were at most about 15% of a call, and the rest was call
  overhead, which "Library leaves resolve at compile time" removes.
- **Impact.** Value semantics are unchanged (pinned in
  `crates/interp/tests/test_standard_library.rs`, including the cases that broke the move), nested
  accumulator loops become linear, and incremental analysis is unaffected.

## Library leaves resolve at compile time

- **Before.** A call such as `Dict.len(d)` in a loop looked `Dict.len` up through the
  environment chain, built an argument list with a promise per argument, and dispatched through
  the builtin table on every iteration: about 0.27 us for `Dict.len` and 0.45 us for `Dict.get`
  on the profiling build (0.6 us on an older build), against 0.001 us for an empty iteration.
- **Tier 0.** The bytecode compiler resolves a qualified library name (`library_function` in
  `bytecode.rs`) to its builtin when the name resolves to a native-accepting builtin and every
  argument is positional with no `...`, and emits a guard on the symbol's binding, the builtin as
  a constant and a direct `CallBuiltin`. A failed guard (someone rebound `Dict.len`) falls back to
  the general call, so redefinition still works.
- **Leaves.** `standard_library::is_leaf` names the functions that never call back into R:
  every `Result`, `List`, `Dict` and `Set` function that takes no callback and does not update
  its first argument in place, and every `Num`, `Bool` and `Str` function except the `Str` walks.
  The JIT treats a leaf as an opaque step with `Effect::PureUnlessObserved`, so it flushes nothing
  around it unless an argument is observed: every boxed argument's object and ALTREP flags are
  checked at run time, because reading an ALTREP vector (`List.from_vector` materializes one)
  runs its class's methods, which a package may write in R. A set flag flushes state exactly as
  an opaque call does. An in-place update is excluded because compiled code may hold its argument in a
  register that no reference count sees, so the update could change a value still in use. The
  test `leaves_are_the_functions_without_callbacks` derives the rule from the stub signatures.
- **Intrinsics.** `Dict.len`, `Set.len`, their `is_empty`, `Dict.contains`, `Set.contains` and
  `Dict.get` compile to calls of small runtime helpers (`jit_collection_count`,
  `jit_collection_position`, `jit_dict_get`) that take the key unboxed and return an unboxed
  count or position. Anything they do not handle (a wrong type, `NA`, a mismatched key kind, a
  one-entry collection stored without an index) hands the instruction to tier 0, which raises the
  same error as before. `Dict.get` still allocates its `:Ok(value)` record; `:Err(:KeyNotFound)`
  is one shared value (`standard_library::Tags`).
- **Hash.** `key_hash` applies MurmurHash3's 64-bit finalizer after FxHash. FxHash's low bits are
  constant for doubles that hold whole numbers, which the open-addressing index uses to pick a
  slot, so integer-valued keys clustered and a miss probed linearly (0.80 us; 0.09 us with the
  finalizer alone, before the intrinsics). The
  index is not serialized, so the change needs no migration.
- **Measured** (`ry run`, profiling build, minimum of 7 runs of 1,000,000 calls on a 1,000-entry
  dictionary, us per call, before and after): `Dict.len` 0.271 to 0.009, `Dict.get` 0.450 to
  0.053, `Dict.contains` 0.320 to 0.018, a missing key 0.800 to 0.025.
- **Impact.** Library calls in loops are as cheap as `length()`, value semantics and errors are
  unchanged (pinned in `test_standard_library.rs`, including compiled-loop error cases), and the
  analysis is unaffected.

## Library traversals compile to loops

- **Before.** `List.walk`, `List.map` and the other callback-taking library functions called the
  callback through `eval::apply` once per element, which built a call, matched arguments, made a
  promise per argument and entered the callee's compiled body: about 5,000 instructions per
  element under callgrind, of which the callback's own `a + x` was about 1,000. Compiled code
  treated the whole library call as one opaque step.
- **Interpreter.** `standard_library::Callback` checks a closure callback once per library
  call: when it has no `...` and takes exactly the arguments the library passes, each item binds
  positionally with `bind_arguments` and `run_closure`, skipping argument matching. The frame's
  call still quotes the values, `(f)(5)`, as `eval::apply` does. A call naming the formals, like
  GNU R's `FUN(X[[i]], ...)`, would be wrong: `sys.call()` and `match.call()` users evaluate it
  in the caller's frame, where the formal names find the caller's own variables. Any other
  callback goes through `eval::apply` as before.
- **JIT.** `call_builtin` turns `List.walk`, `List.map`, `List.keep_if`, `List.drop_if`,
  `List.any`, `List.all` and `List.count_if` into `Action::Traverse` when the callback inlines:
  a closure literal at the call site (`Ty::Closure`), a function constant (`Ty::Fun`), or a name
  whose variable lookup from the enclosure finds a closure now. The element's type is the join
  of the feedback of the callback's own reads of its last formal (or a declared scalar
  annotation), and the callback body lowers through `lower_expression`, the same inliner direct
  calls use, so it must be scalar arithmetic, comparisons and intrinsics over its formals and
  loaded variables.
- **Guards.** The loop hands the whole call back to tier 0, which reruns it from the start,
  whenever the list is not a plain `VECSXP` (an ALTREP or array list), an element is not the
  expected unboxed scalar, a predicate returns `NA`, or the arithmetic overflows. A named
  callback adds an identity check of the register against the inlined closure, plus a
  speculation on its binding that keeps the closure alive (so a recycled address cannot pass
  the check) and recompiles once the binding changes. A hand-off before the loop has written
  anything visible is invisible; `map` and `keep_if` build their result in the destination
  register, which tier 0 overwrites.
- **Measured** (`ry run`, profiling build, best of 3). A hot caller that traverses a 100-element
  double list 10,000 times: `List.walk` 0.014 s (0.008 s with a named callback), `List.map`
  0.036 s, `List.keep_if` 0.013 s, `List.any` 0.008 s, `List.count_if` 0.008 s, against
  0.34-0.46 s with the JIT off and 0.57-0.94 s for the `Reduce`/`lapply`/`Filter`/`vapply`
  equivalents on GNU R. A cold caller over 1,000,000 elements only gets positional binding:
  `List.walk` 0.734 to 0.463 s, `List.map` 0.516 to 0.406 s, `List.keep_if` 0.488 to 0.350 s,
  `List.find_first` 0.501 to 0.324 s, `Dict.walk` (x10 over 100,000 entries) 0.670 to 0.598 s.
  The typed benchmark folds use no library callbacks and are unchanged.
- **One loop, described by a struct.** `analysis::Traversal` is a fold (`Walk { until }`, `Map`,
  `Filter`, `Any`, `All`, `CountIf`, `Find { index }`), a source (`List`, `Dict`, `Set`) and
  three flags (`backwards`, `indexed`, `from`), and `Traversal::of` decodes every looped name
  into it, so `List.walk_backwards_until` or `List.find_last_index` is a combination rather than
  a new code path. Codegen emits one loop: a list reads the `VECSXP`, a `Dict` or `Set` gets its
  keys vector and values list from `jit_collection_parts` (count `-1` hands off), `backwards`
  counts down, `indexed` passes the 1-based position as an integer, `from` takes an integer or
  whole double start of at least 1, and a find stops at the first match and builds its
  `Result` through `jit_found` after the loop.
- **Until steps.** A `walk_until` body lowers through `lower_step` into a (break, value) pair:
  `:Break(s)` and `:Continue(s)` give a constant flag, `{`/`(` unwrap, and `if` merges both arms
  with `Action::Select`. Any other body shape refuses, so the call stays generic.
- **`if` in inlined bodies.** `lower_expression` lowers `if (c) a else b` with a logical `c` to
  `Action::Select`, which evaluates both arms and picks one; that is sound because an inlined
  body has no effects, and an `NA` condition hands off so tier 0 raises R's error.
- **Unused formals.** A callback formal whose value cannot be unboxed (the key of a
  string-keyed `Dict`, say) binds to `UNUSED`, so a body that reads it refuses instead of
  resolving the name to a frame variable of the caller.
- **Impact.** Values, errors and warnings are unchanged (`traversals_agree_across_tiers` in
  `crates/jit/tests/test_tiers.rs` runs each shape in both tiers, including the hand-off cases;
  `fuzz_stdlib_traversals_agree_with_model` compares random traversals in every tier against a
  Rust model of the library), and analysis is unaffected. What still runs each callback through
  `standard_library::invoke` is in `backlog.md`.

## The standard library is measured against GNU R, function by function

- **Previous state.** Nothing compared a library function with what R code would write instead,
  so several functions were quadratic without anyone noticing: `List.len`, `List.get`,
  `List.first` and the other accessors copied the whole list into a `Vec` per call (8.6 s for
  100,000 `List.is_empty` calls), and `Dict.update` rebuilt the dictionary per call.
- **Target.** `just bench-stdlib` (`tools/bench/stdlib`) times every `List`, `Str`, `Dict` and
  `Result` function at 1e6 against the fastest of base R, purrr, stringr and stringi, with
  checksums, and `tools/bench/stdlib/results.md` holds the last full run. A row slower than R
  is either fixed or listed in `backlog.md` ("Open: standard library rows slower than GNU R")
  with its reason. Where the input is a vector, ry's side also runs on an Array, because R's
  vectorized functions work on unboxed storage too and the fair comparison is ry's unboxed
  vector.
- **Accessors read in place.** `list_count`, `list_item` and `list_slice` read a list or an
  Array without materializing it, so the accessors, `take_*`/`drop_*`, `sublist`, `split_at`
  and `starts_with`/`ends_with` cost what they return. `List.contains` compares a number by
  value against an Array's storage or a list's plain scalars, which is what `identical`
  decides for a number that is not `NaN`. A length-one Array is never such a plain scalar,
  because `identical([1], 1)` is `FALSE`.
- **Leaf callbacks skip the call.** A callback that is a leaf builtin (`Str.trim` passed to
  `List.map`) is called through its `BuiltinFn` directly (`Callback::leaf`, `call_leaf`), not
  through `eval::apply`, which built a call and matched arguments per element. A leaf reports
  errors against its own function object, and `call_leaf` swaps in the call `eval::apply` would
  have built only when an error escapes, so the condition is unchanged.
- **Updates in place.** `Dict.update` and `List.update` reuse their first argument
  (`reusing_first_argument`) and update an owned value in place after the callback returns,
  re-checking ownership then, because the callback may have stored the collection.
  `List.update` holds a reference on the list while the callback runs, as an R closure's
  argument would, so a `<<-` assignment into the list inside the callback copies it, and a
  callback that returns the list itself takes the copying path so the list never holds itself. Removal
  through `Dict.update` and `Dict.remove` still rebuild, because a `Dict` keeps insertion order
  and an ordered removal shifts every later entry.
- **Impact.** Values and errors are unchanged (the docs tests run every library example, and
  `fuzz_stdlib_traversals_agree_with_model` feeds Arrays as well as lists); the accessors went
  from O(n) to O(1) per call and `Dict.update` from O(n) to amortized O(1). Analysis is
  unaffected.

## Moving a collection into a helper's parameter

- **Before.** `d <- helper(d, k)` shared the dictionary between the caller's `d` and the
  parameter, so the helper's `Dict.insert` copied it and a loop of such calls was quadratic. A
  first version that cleared the caller's binding was removed, because a second argument naming
  `d`, a nested closure, `get(..., parent.frame())` and an error unwinding through the assignment
  all read the cleared variable.
- **Target shape** (`crates/interp/src/moves.rs`). `rebind_call` offers a closure call to
  `moves::call_moving`. A static check (`movable`) requires positional arguments without `...`,
  and that the other arguments, the callee's defaults and its body call only closures and inert
  builtins (`inert_builtin`, plus native-accepting library builtins), never naming the caller's
  variable unless it is the callee's own local. Then the caller's binding is set to `NULL` for
  the call. At run time any code the check did not cover ends the move first and restores the
  binding: entering a closure other than the callee, forcing a promise the check did not see, a
  library callback that is not a closure, and the default error handler.
- **Undo journal.** An in-place update of a dictionary or set either appends or replaces a value
  (`insert_in_place` reports replacements to `moves::replacing`), so the original is the first
  `entries` entries with the replaced values put back (`collection_prefix`). A failed call, or an
  early end, rebuilds the caller's value from it.
- **Dead frame release.** When the call returns without the move having ended and the callee
  creates no closure, its frame is unreachable, so `release_parameter` clears the parameter's
  binding. Without that the dead frame keeps the result shared and the next call copies. The
  heap does not count an environment's enclosure as a reference, so the environment's refcount
  cannot decide this the way GNU R's `R_CleanupEnvir` does.
- **Holes the static check cannot see.** A variable that holds an environment can reach any
  frame, so `$`, `[[` and their assignments on an environment end every move
  (`moves::reaching_environment`, in `subset.rs`). A local assignment of the caller's variable's
  name does not shadow it, since a read before the assignment, or on a path that skips it,
  finds the caller's variable, so such a body is not movable; only a parameter shadows. A closure
  created in the body captures the frame without entering a closure, so every live move records
  each closure created (`moves::created_closure`), and the frame release and the tail update
  both require that each is referenced only from the move's private frames (`closures_dead`).
- **A shared start does not stick.** A value something else also holds, such as `.Last.value`
  holding the dict a top-level `d <- Dict.empty()` created, cannot move, but the call still runs
  tracked (`Move::shared`, binding untouched) so the dead frame is released and the next call
  moves. Before this, one shared call made every later call copy, because the dead frame kept
  each result shared.
- **Tail update.** A call `f(p, ...)` in tail position, `f` a library builtin that reuses its
  first argument and `p` the first parameter, compiles to `Op::CallTailUpdate`
  (`bytecode::tail_updates`). Tail position is the body, the last statement of a `{` or a branch
  of an `if` in tail position, or the argument of a `return` in a statement outside any function
  literal or `local`. It updates `p`'s value in place only when, after its arguments are
  evaluated, a move into this frame still lasts and every closure created while it lasted is
  dead once the frame is, with none among the call's other arguments, whose values the heap does
  not count (`moves::frame_private`): then nothing outlives the frame, and nothing in it runs
  after the call, so no one sees the parameter change. A body that creates closures used to be
  refused statically; the dynamic rule turned 20,000 calls of `h <- function(d, k) { inc <-
  function(x) x + 1L; Dict.insert(d, k, inc(k)) }` from 30.6 s into 0.85 s (debug build). The AST evaluator never does this, so the first two calls, before tier 0 compiles the
  body, copy.
- **Why static types do not prove it.** A type says what a value is, not which frames a
  function reads or whether it captures its environment, so a typed helper needs the same body
  check as an untyped one. Effect annotations would be the typed route, and ry has none.
- **Measured.** 40,000 calls of `d <- add(d, i)`, profiling build: with `add <- function(d, k)
  { d <- Dict.insert(d, k, k); d }` inside a function, 16.9 s before and 0.10 s after; at top
  level inside `system.time`, 16 s before the shared-start fix and 0.066 s after; with the tail
  form `add <- function(d, k) Dict.insert(d, k, k)`, 16.4 s before and 0.053 s after.
- **Lists.** A plain list (no attributes, not an Array) moves like a dictionary: `moves::extent`
  gives its length as the journal's `entries`, and `List.set` and `List.swap` report replaced
  elements to `moves::replacing`, so `original` rebuilds it. Growth past capacity copies and
  leaves the original intact. 40,000 calls of `xs <- helper(xs, i)` with
  `helper <- function(xs, i) List.append(xs, i)` take 0.046 s on the profiling build.
- **Closure-creating helpers.** `movable` accepts a `function` literal whose defaults and body pass
  the same check (`check_literal`) and returns those bodies. `eval::make_closure` reports every
  closure to `moves::created_closure`, which marks it checked when its body is one of them, and
  entering a checked closure no longer ends the move; its frame joins the move's tracked frames,
  so forcing a promise made there is covered too. At return, `closures_dead` releases the
  parameter only when no created closure is the result and each one's refcount is covered by the
  tracked frames' own cells, which means none escaped. 40,000 calls of
  `{ inc <- function(x) x + 1; xs <- List.append(xs, inc(i)); xs }` take 0.14 s, and the tail
  form `{ inc <- ...; List.append(xs, inc(i)) }` scales linearly too (0.06, 0.14 and 0.26 s at
  20,000, 40,000 and 80,000 calls), although `Op::CallTailUpdate` stays off for a body that may
  capture its frame.
- **Base replacements end the move.** `xs[[i]] <- v`, `xs$a <- v`, `xs[i] <- v` and nested forms
  on a moved list modify it in place without reporting to the journal, so `subset::modifiable` and
  complex assignment in `eval.rs` call `moves::modifying` first, which ends a move whose value is
  the object being changed. It must run before the sharing check, because ending the move makes
  the object shared again and forces the copy. A copied closure (`values` duplicate) is reported
  to `created_closure` like a fresh one.
- **Limits.** The tail update covers only the last statement and stays off in a body that creates
  a closure. Both are in `backlog.md`.

## A key of the wrong type is an error, not a missing key

- A dictionary or set holds keys of one kind. A lookup with a key of another kind used to return
  `:Err(:KeyNotFound)`, which hid a bug the checker reports statically. It now stops with "the
  dict's keys are character, and this one is integer; a collection holds one type", through the
  same `joined_key_kind` check that insertion uses. Integer and double keys still join as numbers,
  and an empty collection has no kind yet, so any key misses.

## A function literal in a call argument is typed in context

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

## ry refuses a `libR.so` built from other sources

- Cargo builds `libR.so` separately from `ry`, so after an edit to a shared crate (the parser, the
  interpreter) a leftover library silently ran old code. `libr` and `repl` both hash the sources
  (`src/`, `include/`, `build.rs`) of `libr` and every workspace crate it links, found by following
  `path` dependencies (`crates/libr/build_identity.rs`, included by both build scripts).
  `libR.so` exports the hash as `ry_runtime_id`, and `ry` compares it right after `dlopen`, then
  exits 2 naming `just build` and `cargo build -p libr`.
- Contents, not timestamps or a git commit: a rebuild or `touch` that changes nothing keeps the
  identity, git is not needed, and the two binaries of one `cargo install` or release build always
  agree. Edits to crates `libr` does not link (the formatter, the IDE layer) leave it alone.
- Not covered on purpose: `Cargo.toml` files (adding a test target must not look like a runtime
  change), `Cargo.lock` and the compiler version. A dependency bump alone does not trip the check.

## Character group keys get first-appearance codes during import, and the bridge widens to `dcast`

- **Previous shape.** The grouping kernels (`crates/table/src/group.rs`) built mixed-radix digits for factors, integers and logicals only, so `by` on a character column went through Polars' general grouping, and at 1e6 rows the `[` bridge lost to data.table's C on string keys (`by = id3` 0.021 s against 0.017 s). Ranking the distinct strings with `ops::string_ranks` to make a digit was tried and was slower, because it sorts them. The bridge also declined every query from data.table's own namespace, any `j = eval(e)`, and any call whose head is a primitive object, so `dcast`'s aggregation `dat[, eval(fun.call), by = c(varnames)]`, with `fun.call` being `list(v1 = .Primitive("sum")(v1))`, always ran on data.table.
- **Target shape.** Importing a character column into the engine (`import_column` in `crates/interp/src/data_table.rs`) already visits every CHARSXP, so it numbers each distinct string by first appearance through the CHARSXP's truelength, as data.table does, and restores the truelengths afterwards. The column is built from the distinct strings and the codes (`convert::strings_from_codes`, a parallel gather of views), and `Table::set_string_codes` stores the codes beside it (`StringCodes`). Any change to that column drops them (`forget_column`), and `set_key` drops all of them, so they cannot drift from the data. A code is a digit with radix `distinct + 1` (0 is `NA`). `keyby` ranks only the distinct strings, by byte order, so it sorts `k` strings, not `n` rows. In the bridge, `Piece::expand_eval` replaces `j = eval(e)` with the call `e` evaluates to, as `[.data.table` does, `frame::call_head` reads a primitive head by its name, and data.table's own namespace qualifies for grouped queries without `:=`.
- **Exactness guards.** Outside GForce, base `sum` accumulates in `long double`, so the bridge still declines it, except `sum` of one integer or logical column (`sums_exactly`), which is exact in any order. GForce's integer `sum` widens to doubles with a warning as soon as a group's running total leaves the integer range, while the engine widens on the final total. The bridge therefore declines a GForce `sum` unless the absolute values of every integer column the query reads add up to at most `INT_MAX` (`running_sums_fit`), which rules out any running overflow. The same bound guards every grouped query that mentions `sum`, because outside GForce base `sum` widens only the overflowing group's total and data.table then rejects the mixed column types. `expand_eval` follows `[.data.table` exactly: it leaves `eval(e)` alone when a variable of `e` names a column, and declines when the expansion reads no column, since data.table decides `with = FALSE` before expanding (`eval(quote(1:2))` is `1:2` there, not columns 1 and 2). An evaluated `by` that names no column (`by = character()`, which `rollup` and `groupingsets` pass for the grand total) declines, because data.table answers it as one group. Strings intern by encoding, so when the distinct non-ASCII strings of a column carry different encoding marks, the column gets no codes and groups by its text, as data.table does. A negative truelength the import loop did not set (one left behind by C code that stopped midway) declines rather than aliasing two strings. An unnamed `by` expression is named as data.table names it (`by_expression_name`: the first suitable variable of `all.vars(key, functions = TRUE)`), and the case where data.table falls back to `deparse(key)` declines. A `j` that picks whole columns of every row also declines, because data.table copies those vectors while the engine would convert each string column in and back out (0.048 s against 0.006 s at 1e6 rows).
- **Impact.** At 1e6 rows, best of 15: `by = c("id1", "id4")` takes 0.007 s against 0.011 s on GNU R, `by = id3` 0.007 s against 0.013 s, `keyby = id3` 0.015 s against 0.016 s, and `dcast(x, id1 ~ id4, value.var = "v1", fun.aggregate = sum)` 0.018 s against 0.027 s. All `tools/bench/table/ops.R` checksums match GNU R at 1e6 and 1e7. `fuzz_kernels_agree_with_the_polars_plans` covers string keys with `NA`s and up to 5,000 distinct values, and `crates/oracle/cases/datatable__dynamic.R` pins `eval`, spliced primitives, `dcast` and the running-overflow rule against GNU R.

# Decision record: what typing finbench in strict mode settled

## Shipped stubs are whatever `types/` holds

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

- `stub_library` is a HIGH-durability memo. Computed before the `PackageMetadata` singleton input
  existed, it saw no active namespaces and was never re-verified, so a project with any
  `stubs/*.Rtypes` lost every shipped conditional namespace. `cli.rs` creates the metadata input
  first; `check_keeps_declared_packages_active_beside_override_stubs` in `test_cli.rs` pins it.

## Strict mode implies type errors

- `[check] strict = true` without `typing` used to report the strict findings but no type errors,
  which made a strict project look clean while its calls did not type. Strict now turns typing on
  (`diagnostics.rs`); `typing = false` beside it no longer turns type errors off. Strict code is
  fully typed by definition, so there is no useful strict-but-untyped mode.

## An all-`Any` overload does not veto agreement among unchecked fits

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

- `~` (unary and binary) types as `formula` (`infer_formula` in `check.rs`). Its operands are
  inferred for hover and goto-definition, but their errors and strict origins are dropped, because
  they name model variables (data-frame columns, `.`, `I(x^2)`) that are not bindings in scope.
  Previously `~` was `Unknown`, so every model call and every `dcast(dt, id ~ n)` was a strict
  finding that could not even be annotated inline. Rejected: typing operands as ordinary
  expressions, which would report every column name as unresolved.

## `$`, `[` and `[[` dispatch to a class's subset method

- A stub or project method named `$.class`, `[.class` or `[[.class` types the access on a value of
  nominal type `class` (`subset_method_result` in `check.rs`), as R's S3 dispatch would run it. It
  is consulted before the structural read, so an opaque type gets typed field reads without
  exposing a representation (`nominals.R.test`, `a_dollar_method_types_field_reads_on_its_class`).

## The C headers declare the API of the presented R version

- ry presents the R version of its base `DESCRIPTION` (4.3.3), and packages branch on `R_VERSION`
  to backport newer API. If the headers declared every name ry implements, those backports would
  collide (rlang's `ANY_ATTRIB`, `CLEAR_ATTRIB` and `R_mapAttrib`, yaml's `R_mkClosure`), and names
  newer R removed would be missing (Rcpp's `VECTOR_PTR` below 4.5).
- So every declaration GNU R added in 4.4, 4.5 or 4.6 sits behind `#if R_VERSION >= R_Version(...)`,
  with the version taken from GNU R's own headers, and `VECTOR_PTR` is declared below 4.5. The
  headers include `Rversion.h`, which `ry-rcmd assemble` generates for the home.
- The crate's fallback `Rversion.h` says 4.6, so ry's own tests and code built straight against
  `crates/libr/include` still see every name. `libR.so` exports all of them regardless.

## macOS packages bind to ry's libR

- **Before.** `crates/libr/build.rs` set the export list and soname for Linux only, so a macOS
  `libR.dylib` exported nothing from the C layer and had no stable install name, and nothing made
  a GNU R package binary call into it. On Linux the problem never arises: a package records
  `libR.so` by soname, and ry's library, loaded first with `RTLD_GLOBAL`, satisfies it.
- **The macOS problem.** Mach-O uses a two-level namespace: every undefined symbol in a package
  binary is bound to one named library. CRAN's binaries link `-framework R` and name
  `/Library/Frameworks/R.framework/Versions/4.x-<arch>/Resources/lib/libR.dylib`; Homebrew's
  link `-lR` and name Homebrew's `libR.dylib`. dyld would load GNU R's libR beside ry's, and the
  package would run against GNU R's heap.
- **Options weighed.** Forcing a flat namespace (`DYLD_FORCE_FLAT_NAMESPACE`) needs an environment
  variable set before ry starts, is stripped by SIP for protected binaries, and changes binding for
  every library in the process. Interposing (`DYLD_INSERT_LIBRARIES`) has the same launch problem.
  Rebuilding every package from source gives up CRAN's binaries.
- **Shape.** ry's `libR.dylib` has the install name `@rpath/libR.dylib`, and its exports are
  rustc's list plus a second `-exported_symbols_list` naming the `RY_EXPORT` C symbols (ld64 and
  ld64.lld merge the two lists). When `dyn.load` opens a Mach-O library whose load commands name
  GNU R's libR (`crates/libr/src/macho.rs`), it opens a copy instead, with that load command
  rewritten in place to `@rpath/libR.dylib` and re-signed ad hoc with `codesign --force --sign -`,
  because arm64 refuses to map a modified, unsigned image. The rewrite also zeroes the compatibility version the
  load command records (GNU R links libR with its own version, and ry's libR has none, so dyld
  would refuse it). dyld satisfies a load command from an
  already loaded image whose install name matches, so the copy binds to ry's library. The copy is
  cached under the user cache directory keyed by the rewrite's version (`REBIND_VERSION` in
  `dynload.rs`, bumped whenever the rewrite changes), path, size and mtime, and a library that names no
  libR is recorded with a marker so it is read once. The DLL record keeps the original path.
- **ry-built packages.** `rcmd` links with `-undefined dynamic_lookup` and an empty `LIBR`, so the
  binary records no libR and resolves R's API in the flat lookup that falls back to every loaded
  image. The same build then loads under ry and under GNU R, and needs no rewriting.
- **Relative dependencies.** The copy lives in the cache, outside the package's `libs/`, so the
  cache mirrors the original's surroundings with links (`mirror_surroundings` in `dynload.rs`):
  the copy's directory links every file beside the original, and `macho::Rebound::climb`, the
  most `..` components any `@loader_path` load command or rpath starts with, decides how many
  ancestor directories are mirrored too. Rewriting `@loader_path` to the original's absolute
  directory was rejected: the longer string does not fit in the load command in place, and
  growing load commands needs header padding and moves offsets. `.target` in the cache entry
  records where the copy sits in the mirror.
- **Known gaps.** CRAN's own `libRblas.dylib` could pull in GNU R's libR if a package links it
  directly; unobserved. An `@rpath/../x` name combined with an `@loader_path` rpath is not counted
  in the climb. The install-name match is reasoned from dyld's source and verified only once the
  release smoke test runs on a Mac.

## Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld

- **Before.** The runtime called libc directly (`dlopen`, `umask`, `utimensat`, `nl_langinfo`,
  `iconv`, `kill` and signal numbers, process priorities, `getpwnam`), so it built only on Unix, and the Windows archive
  carried `ry.exe` alone.
- **Target.** `x86_64-pc-windows-gnullvm` for both binaries: the mingw-w64 ABI Rtools builds
  packages with, on the UCRT that Rtools 4.2 and newer link. `windows-gnu` links `msvcrt.dll`, so
  `errno`, `FILE *` and `malloc`/`free` pairs crossing the API would hit two C runtimes, and an MSVC
  `ry.exe` loading a mingw `R.dll` mixes runtimes the same way, so neither is shipped. Package DLLs
  import from `R.dll` by module name, and the loader reuses an already loaded module with the same
  base name, so naming ry's library `R.dll` binds them with no rewriting. Exports come from
  `__declspec(dllexport)` in `native.c` plus rustc's own exports, so there is no `.def` file to
  drift from the code; the linker writes `libR.dll.a` for `rcmd`. `LibExtern` is
  `__declspec(dllimport) extern` under `_WIN32`.
- **One platform seam.** Every operating-system call the interpreter makes goes through
  `crates/interp/src/platform.rs`, which has a Unix and a Windows `imp` with the same functions.
  Operating-system *calls* never happen outside it, so a new libc use shows up as a Windows build
  error in one file; platform *conventions* (`.Platform`, the platform string, `libs/x64`, the
  routine table) still branch on `cfg!` where they are used. The heap's address-space
  reservation has its own small seam (`virtual_memory` in `crates/heap/src/space.rs`: `mmap` or
  `VirtualAlloc`). Windows has no `iconv(3)`, so `PortableIconv` converts with `encoding_rs` behind the
  same interface `Riconv` exports.
- **lld is required.** GNU ld 2.41 (Ubuntu's mingw-w64) emits a `HIGHLOW` base relocation for
  each `SECREL32` thread-local offset, so a relocated `R.dll` (ASLR, or a base-address collision)
  computes a garbage address on every `thread_local` access. lld emits none. mingw's `gcc` is
  configured `--with-ld` and ignores `-fuse-ld=lld`, so `clang --target=x86_64-w64-mingw32
  -fuse-ld=lld` is the linker driver (`development/design/r-runtime.md`, section 11, has the
  command).
- **A 64 MiB main-thread stack** for `ry.exe` (`crates/ry/build.rs`): the session evaluates on the
  main thread, and base R's startup overflows Windows' 1 MiB default. The UTF-8 code-page
  manifest the MSVC build embedded through linker flags goes in as a `windres` resource object,
  so the gnullvm build needs `windres` (`WINDRES`, else `llvm-windres` or `windres` on `PATH`).
- **Jumps use `_setjmp(buffer, NULL)` and `longjmp`.** mingw's `setjmp` macro records the frame
  and `longjmp` then unwinds through SEH. clang's `__builtin_setjmp` stores the canonical frame
  address, but its landing code uses it as the frame pointer, so `R_UnwindProtect`, whose frame
  pointer sits above its stack pointer, resumed with garbage locals when an R error landed in it.
  A null frame makes the CRT's `longjmp` a plain register restore, as on Unix.
- **`xerbla_` never jumps over Rust.** Windows' R.dll exports it for R's `Rblas.dll`/`Rlapack.dll`.
  It raises an R error for a package's call, but returns while the interpreter's `DirectFortran`
  guard (`linalg.rs`) is alive, and the routine's negative `info` reaches the Rust caller.
- **Two modules named `R.dll`.** ry's runtime beside `ry.exe` shares GNU R's DLL name, so `ry R`
  loads GNU R's `R.dll` first by full path; later by-name imports bind to the loaded module.
- **Toolchain.** The release runner uses MSYS2's clang64 environment (`msys2/setup-msys2`).
  Cross-building from Linux works with the host clang and lld over MSYS2's clang64 sysroot
  packages; packages for tests are built with the Rtools ABI from MSYS2's ucrt64 GCC 14 headers
  and static `libstdc++` (GCC 16's needs a newer clang than 18).
- **Windows-only routines** that `utils` and `grDevices` reference at load time are registered in
  `WINDOWS_ROUTINES` (`builtin_libraries.rs`), chained in only on Windows so Unix sessions keep
  GNU R's Unix routine table.
- **Impact.** `ry run` runs base R and Rtools-ABI package DLLs (jsonlite, data.table, digest, cli,
  uuid, Rcpp with exceptions crossing R errors) under Wine with no GNU R binary executed, and the
  release workflow ships `R.dll` beside `ry.exe`. Open items are in `r-runtime.md`, section 11.

# Decision record: a new top-level name costs one project fold

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

## ry ships its own base R, and GNU R through `R_HOME` is the opt-in

- **Before.** `ry run` loaded the base packages' R code, lazy-load databases, help and `share/`
  files from an installed GNU R (found through `R_HOME`, the usual install paths, then `R RHOME`),
  which was the only reason a user needed GNU R for ry's runtime.
- **Now.** ry pins one GNU R release, R 4.6.2 (`tools/base-r/VERSION`, SHA-256 in
  `tools/base-r/SHA256SUMS`, patches in `tools/base-r/patches/`). `just base-r` downloads the
  tarball, checks it and runs `ry-rcmd build-base` (`crates/rcmd/src/base.rs`), which replays GNU
  R's `src/library/Makefile` on ry's own interpreter: each step is a separate `ry-rcmd R --vanilla`
  process against the staging tree, as GNU R's make runs `R_EXE`. The output (`target/base-r`, about
  23 MB) is GNU R's layout, so `system.file`, `R.home()`, help and `tools:::` code work unchanged.
  `just package` embeds it zstd-compressed (`RY_EMBED_BASE`, `crates/repl/build.rs`) and `ry`
  extracts it to `cache/ry/base/<key>` on first use, like libR.
- **Lookup order** (`repl::base_r`, `interp::system::find_r_home`): a non-empty `R_HOME` is
  authoritative and must hold base R (no fallback, so a typo is an error, not silent R 4.6.2);
  then `RY_BASE_R`; then a complete tree (`ry-base.txt` present) at `base-r/`, `../base-r`,
  `../../base-r` or `../lib/ry/base-r` beside the executable; then the embedded copy. The GNU R
  search paths and `R RHOME` are only for `ry R` (`crates/repl/src/gnu_r_home.rs`).
- **No GNU R compiled code.** The default packages' routines are ry's (`builtin_libraries.rs`), so
  those packages get a placeholder `libs/<pkg>.so` for `library.dynam` to find. grid and splines
  are compiled from the tarball's C sources against ry's headers (`compile_native`), as before.
- **No precompiled bytecode** in the lazy-load databases (coordinated with the bytecode work):
  ry's interpreter compiles on its own terms.
- **The makebasedb check is rewritten,** because every ry session has `tools:ry`, `package:std`
  and `package:ry` attached: the step ignores them instead of demanding a bare search path.
- **BLAS/LAPACK** come from the system (Linux `libblas.so.3`/`liblapack.so.3`, macOS
  Accelerate). Windows has no system LAPACK and no bundled reference build yet, so linear algebra
  there needs `R_HOME` naming GNU R (`backlog.md`).
- **Embedded archive format:** sorted entries of a kind byte (0 directory, 1 file, 2 executable),
  a u32 LE path length and path, and for files a u64 LE length and the bytes, zstd-compressed as a
  whole and keyed by the FNV hash of the compressed bytes. Paths with empty or `..` parts are
  rejected on unpack.
- **Version emulation is kept for the opt-in:** `R_HOME` naming GNU R 4.3 or later makes ry
  behave like that release ("The runtime emulates the GNU R version it loads base R from" below).
  The oracle's expectations are GNU R 4.6.2 output, so CI sets `R_HOME` to 4.6.2.
- **Impact.** One source of truth for the R version a user gets, with the same tree shape as
  GNU R's. Start-up from that tree is cut by the start-up image ("The start-up image is written
  in process and keyed by what start-up reads" below).

## The runtime emulates the GNU R version it loads base R from

- **Status:** the default is ry's own base R 4.6.2 ("ry ships its own base R" above); this
  applies when `R_HOME` names a GNU R installation.
- **Before.** ry's runtime always presented as R 4.6 at the R level, while its internals followed
  R 4.3's behavior, so base R 4.6's own R code failed its `Built:` check or called internals that
  had changed or did not exist yet.
- **Now.** The version comes from the base package in `R_HOME` (`interp::system::r_version`:
  `library/base/DESCRIPTION` for the version, `SVN-REVISION` of the GNU R home that base links to
  for the revision and date, clamped to at least 89194). `R.version`, `getRversion()`, the `Built:`
  check, the serialization writer version and the `Rversion.h` that `rcmd` generates all report it. Behavior that GNU R changed between versions is gated with
  `interp::system::r_at_least(major, minor)` at the one place it differs (strptime in a DST gap,
  `psignrank` rounding, `%*%` argument counts, `rbind`/`cbind` passing `deparse.level` to S3
  methods, `is.atomic(NULL)`, complex formatting, short hex colours, the graphics engine version).
  Additions such as `Tailcall`, `Exec`, `declare` and zstd compression exist under every version,
  because no older base R calls them.
- **Range.** GNU R 4.3 and later; an older `R_HOME` stops `ry run` with an error naming the version.
  The oracle's expectations are recorded with GNU R 4.6.2; ry with `R_HOME` at GNU R 4.3.3 is
  checked against a 4.3.3 recording of the same cases (`ORACLE_R` and `R_HOME` select the
  installation). Gates for 4.4 and 4.5 were placed from GNU R's sources and NEWS, not from
  running those versions.
- **The default packages' routine table is the union over versions** (`builtin_libraries.rs`),
  but a routine a release does not register is hidden from lookup and from `getDLLRegisteredRoutines`
  (`registered_in_release`: stats' `pKolmogorov2x` and `pKS2` before 4.4, `pkolmogorov_two_exact`
  and `pkolmogorov_two_limit` from 4.4). The table's positions stay fixed, because routine
  pointers and image tokens index it.
- **The C API surface stays R 4.6's whatever the version,** because a newer API is a superset; only
  the version macros in the generated `Rversion.h` follow base. A package compiled from source must
  be reinstalled when the linked base version changes.
- **`Tailcall` re-evaluates the call in the caller's frame without reusing it,** so it gives the
  right result but not constant stack depth; true tail calls are in `backlog.md`.

## `perl = TRUE` runs on PCRE2, linked statically

- **Before.** Both regex dialects were translated into fancy-regex syntax
  (`strings::translate`, with a Perl branch). PCRE features fancy-regex lacks failed to compile:
  rcmdcheck's `(?<q>...)\g{q}`, lubridate's quantified lookahead `(?=\D+)?`, possessive and
  atomic forms, and verbs beyond `(*UCP)`. Error messages were fancy-regex's, not PCRE's.
- **Options weighed.** Extending the translator or fancy-regex would chase PCRE one construct at
  a time and never match its backtracking semantics or messages exactly. Linking PCRE2 gives
  GNU R's own engine for `perl = TRUE`, so behavior and messages are GNU R's by construction.
- **Chosen.** The `pcre2` crate, compiled as `R_pcre2_prepare` compiles (UTF mode, `CASELESS`
  for `ignore.case`, JIT when available), in `strings::compile_perl`. A compile failure warns
  `PCRE pattern compilation error` with PCRE2's message and the rest of the pattern, then errors,
  as GNU R does. `.cargo/config.toml` sets `PCRE2_SYS_STATIC=1`, so the bundled sources are built
  and a release binary needs no system PCRE2.
- **The TRE dialect stays on fancy-regex** behind the translator, which is now Extended-only.
  Moving it to a real TRE build is the same trade and is open in `backlog.md`.

## Native code sees a compiled closure's body as its expression

- `BODY` and `R_ClosureBody` return the source expression of a compiled body, as `R_ClosureExpr`
  does. ry's bytecode holds binding cells and type feedback that have no GNU R type, so native
  code that walks objects (`lobstr::obj_size`) failed on them.

## TRE approximate regular expressions

- **Previous shape.** A TRE bound with settings (`{+}`, `{~1}`, `{2i+1d<3}`) was read by a
  separate scanner (`approximate_settings`) and either dropped or rejected as unsupported, and
  plain bounds went through `parse_interval`, which missed TRE's quirks. So `grepl("^a{+}$",
  "aaa")` was FALSE instead of TRUE.
- **Target shape.** One reader, `strings::parse_bound`, ports `tre_parse_bound`, so counts,
  settings and errors agree with GNU R by construction. It also reproduces TRE's expansion quirks:
  `{,n}` allows `n + 1` copies (`{,1}` exactly one), and an approximate bound that needs copies
  (`{2+}`, `{1,2#}`) is "Invalid contents of {}".
- A pattern with settings and no backreference compiles to `Matcher::Approximate`
  (`approximate::ApproximateRegex`), a port of `tre_tnfa_run_approx` over a position automaton.
  The translator marks each scope as a named group (`SCOPE_GROUP` plus its index), and the
  automaton turns those groups into parameter changes on transitions. With a backreference TRE
  uses its backtracking matcher, which ignores the settings, so ry does the same.
- Details that only show up against GNU R: the nesting depth is counted in `{` order and more than
  three scopes trip TRE's `value <= 3` assertion, but only when a match reaches the fourth; an
  unset cost is free inside its scope yet counts 1 toward the whole match; ties go to the earlier
  start; minimal repetition is ignored; a later search in `gsub` or `gregexpr` runs on the rest of
  the text with `REG_NOTBOL`.
- `strings__tre_bounds` pins the behavior.

## TRE minimal repetitions over alternatives

- GNU R's TRE, when no submatches are asked for (`regexpr`, `gregexpr`, `grepl`), minimizes the
  lazy repetitions over all top-level alternatives: `regexpr("a.*?Z|a.*?Y", "aXbYcXdZ")` has
  length 4. A pattern with a minimal repetition compiles to `Matcher::Automaton` with
  `shortest`, and a plain find takes the earliest end among the matches that start leftmost.
- Submatch queries (`sub`, `regexec`) keep the leftmost-first match of the whole pattern, so
  `sub("(a).*?Z|(a)(.*?)Y", ...)` takes the first alternative, as TRE does. TRE's submatch
  results for these patterns are irregular in general (`regexec` can even skip the leftmost
  match); leftmost-first agrees with it on the shapes the corpus uses.
- `connections__message_sink_and_flush` pins both.

## The message sink and connection flush points

- `sink(type = "message")` sets the connection table's `message_slot` (GNU R's `R_ErrorCon`).
  Messages, warnings and writes to `stderr()` go there through `connections::write_message`,
  which flushes after every write as `REvprintf` does. Closing that connection resets it to 2.
- GNU R flushes a file connection after `cat`, after each message-sink write, and when an output
  sink is removed, but not after `writeLines`; ry matches each point, so a second reader of the
  same file sees exactly what GNU R's would. `connections__message_sink_and_flush` pins it.
- Native code printing through `Rprintf` and `REprintf` goes through the same table
  (`libr::boundary::write_console`), so a sink diverts C output too.

## The C API draws from the interpreter's generator

- **Before.** `libr` kept its own Mersenne Twister for `unif_rand`, `norm_rand`, `exp_rand` and
  `R_unif_index`, beside the interpreter's `interp::random`, which implements every RNG kind. The
  two shared only `.Random.seed`, so a package calling `unif_rand` after `RNGkind("Marsaglia-Multicarry")`
  read a seed its copy could not parse (doParallel's stress test failed with "'.Random.seed' is not
  a valid integer vector"), and `R_sample_kind` always answered `Rejection`.
- **Now.** `unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` call `interp::random`, the
  `Rf_r*` deviates are `interp::distributions`' functions drawn through `SessionDeviates`, so no
  deviate algorithm exists twice, and `GetRNGstate`/`PutRNGstate` are the interpreter's
  `get_rng_state`/`put_rng_state`.
  `R_sample_kind` reports the method `RNGkind(sample.kind =)` chose. `R_binom_kind` still
  hard-codes the pre-fix BTPE, which is the only `rbinom` stream ry implements.
  `RNGkind("user-supplied")` finds `user_unif_rand` and its seed routines through the
  `find_native_symbol` interpreter hook, which `libr` fills with `dynload::find_symbol`
  (randtoolbox's WELL tests need it).
- **Impact.** One generator means one state and one stream, so a C draw and an R draw interleave
  exactly as under GNU R, and every kind the interpreter supports works from C at no extra cost.

## Native code reads an environment as a cell

- **Before.** `CAR`, `CDR` and `TAG` refused an environment ("bad CAR: a 'environment' has no
  cells"), and `R_NewEnv` refused a `NULL` enclosure.
- **Now.** GNU R lays an environment out as a cell (frame in `CAR`, enclosure in `CDR`, hash table
  in `TAG`), and nanonext looks bindings up by walking `CAR(env)` because `findVarInFrame` is not
  API. `CAR(env)` returns the same read-only frame snapshot as `FRAME`, `CDR(env)` the enclosure
  and `TAG(env)` `NULL`, as `HASHTAB` does. `R_NewEnv(R_NilValue, ...)` gets the empty
  environment as its enclosure, because GNU R stores the enclosure unchecked.
- **Impact.** mirai, and purrr's `in_parallel` on top of it, run. A snapshot costs one cell per
  binding on each `CAR(env)`, which is fine for nanonext's small aio environments.

## Typed checking costs per name, not per file

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

# Decision record: matching GNU R's own tests where its behavior is an artifact

GNU R's `tests/*.R` pin several behaviors that come from GNU R's machine or its implementation, not from the language. Each is reproduced from observed output (black-box against Rscript), never from GNU R's source.

- **x87 long double sums.** GNU R accumulates `sum`, `mean`, `rowSums`, `colMeans`, `var` and `cor` in an 80-bit long double. A software model (64-bit significand, round to nearest even, exact sqrt) replaced the compensated-sum stand-ins, because only an exact model agrees bit for bit (`mean(c(1:9, -1e300, 1e300))` is 0). The data.table bridge deliberately keeps plain double sums for GForce shapes. On x86_64 the slice loops (`long_double::add_all`, `add_differences`, `add_products`, behind `sum`, `mean`, `var`, `cov`, `cor` and `List.sum`) run on the x87 unit through inline assembly, which rounds identically because System V leaves x87 precision control at 64 bits; the software model is about 10x slower (13 ns an element) and stays the fallback on other targets and on Windows, whose x64 CRT sets x87 precision to 53 bits, and for values outside the x87 exponent range.
- **`%%` and `%/%`** correct the remainder in extended precision, handle a divisor too large for the quotient, and warn past 2^63, matching GNU R's observed results on huge operands.
- **Invalid UTF-8 bytes** survive text processing as private-use code points (`U+10FF00 + byte`, one per byte) and print as `\xNN`; `useBytes` regex functions match byte by byte. The heap itself keeps a string's exact bytes and its encoding flag (`gp::BYTES`, `LATIN1`, `UTF8`), so the stand-ins exist only while `interp` handles a string as a Rust `str`. Carrying bytes through every string builtin instead would touch about 230 `char_str`/`string_at` readers, for no observable gain, because the range is the tail of the last private-use plane: a genuine character there is the one thing the scheme misreads (it becomes a byte when text holding it is interned again). The earlier range, `U+F780..U+F7FF`, sat where symbol fonts map glyphs, so it moved.
- **A string declared as bytes is refused where GNU R translates it.** `values::refuse_bytes` raises "translating strings with \"bytes\" encoding is not allowed" for a non-ASCII bytes string in the builtins whose GNU R code calls `translateChar`: case changes, `chartr`, `sprintf`, `make.names`, `abbreviate`, string `sort`/`order` with more than one element, and `as.vector(mode = "symbol")`. `nchar` raises its own "not computable in \"bytes\" encoding" messages. Functions GNU R runs on the bytes (`paste`, `substr`, `strsplit`, `format`, `useBytes` regex) keep working (`oracle/cases/strings__bytes_encoding.R`).
- **`agrep` and `adist`** use an approximate-match NFA over edit costs; `adist`'s `counts` traceback prefers the order GNU R reports (observed, not derived).
- **Scripts run as console input**: one top-level expression at a time, with `stdin()` reading the rest of the script, because GNU R's tests read data from their own source that way.
- **Message sinks and abort restarts**: `sink(type = "message")` diverts every stderr writer, reaching top level invokes an R-level `abort` restart, and messages are cut to `warning.length`.
- **`.Last.value` holds no reference count**, so binding it never forces a copy on the next modification, as in GNU R.
- **Replacement primitives called directly** (`` `names<-`(x, v) `` outside an assignment) copy their target, because only a complex assignment owns it.
- **`pretty()` and plot axes near the double range**: the 4% axis extension is clamped to ±`DBL_MAX`; a log axis stays inside `log10(0.99*DBL_MAX)` and `log10(1.01*DBL_MIN)`; a zero log limit becomes -320 with GNU R's "nonfinite axis limits -- corrected now" warning. `GEPretty` and `Rf_CreateAtVector` in libR reuse the interpreter's `linear_axis` and `at_vector`, so grid and base graphics share one tick algorithm.
- **Deparse parenthesizes dangling operands**: an `if`, loop or low-precedence unary operator that ends a binary operator's left operand gets parentheses (GNU R's PR#18232 fix). GNU R leaves `function` bare there, which reparses differently; ry keeps the parentheses on purpose.
- **`smooth.spline`** (`rbart`, `bvalus` in `crates/interp/src/stats_spline.rs`) is written from the penalized B-spline method. It copies GNU R's 0.333 in the roughness integral because the printed matrices and fitted values depend on it. Near-interpolating GCV fits can settle on a different `spar`, since the criterion there is rounding noise; that divergence is accepted.
- **`Fexact`, `parseLatex`, `gzcon`, `OutDec`, classed `parseError` conditions and `file(encoding=)` re-encoding** are native implementations of the documented interfaces, checked against GNU R's tests.
- **Bytes-encoded strings force byte matching**: when any pattern or text element of `regexpr`, `gregexpr`, `regexec`, `grep`, `sub`, `gsub` or `strsplit` is marked `"bytes"`, the call behaves as `useBytes = TRUE` (byte positions, `index.type = "bytes"`, non-ASCII results marked `"bytes"`), as GNU R does.
- **TRE compile errors** reject PCRE-only groups (`(?<`, `(?=`, `(?!`, `(?>`, `(?P<`) with "Invalid regexp" and an inverted interval with "Invalid contents of {}". The warning and error wording follows GNU R's three observed paths (wide-character `regexpr`/`grep`, `regexec`, everything else).
- **Accepted divergences**: `fisher.test` on tables whose network exceeds GNU R's FEXACT hash computes the p-value instead of erroring "hash key ... > INT_MAX"; `gsub("\\b", ...)` does not reproduce TRE's re-match of `\b` after an empty match; printing a bytes string pads by its escaped width, not GNU R's single-escaped width.
- **`nlminb` control ranges** are PORT's own `DPARCK` checks: an out-of-range `v` slot sets `iv[1]` to the last failing position, so `nlminb`'s R code prints GNU R's "is out of range" message. `nls(algorithm = "port")` keeps ry's simplified range check.
- **`qbeta` polishes its last digits** with a neighbour walk over adjacent doubles that minimizes `|pbeta(x) - p|`, because GNU R's tests require a mean relative round-trip error below 8e-16.
- **`traceOnOff`** keeps a real tracing state (default on), so `trace()` tracers run.
- **`qbeta` past the log-scale solve**: Newton steps on the caller's own probability scale recover the digits a log-scale solve loses when `|log p|` is large; a NaN from pbeta's asymptotic expansion bisects toward 0; pbeta warnings raised while searching are dropped; a subnormal result snaps to 0 or 2^-1024, whichever probability is nearer the target, because GNU R never returns a subnormal quantile and `d-p-q-r-tst-2` relies on it.
- **`dpois` deviance in double-double**: `bd0_extended` in `rmath.rs` computes `x log(x/np) + np - x` as an unevaluated sum, with its own atanh-series logarithm, because GNU R's tests pin densities like `dpois(1400, 1000)` to a few ulps.
- **Discrete quantile slack**: a probability within 64 epsilon of the target, measured from the nearer end of [0, 1], counts as reached (16 epsilon of the log probability on the log scale). A slack relative to `p` alone returned the quantile below near 1, and a narrower one exposed `ppois` digits that differ from GNU R's.
- **Deparse of braced `if` in calls**: a braced `if` stays on one line as an argument of a primitive call (`c`, `list`, `quote`, `return`, ...) and breaks inside closure calls, `[` and list elements; `else` after a braced consequent starts its own line.
- **S4 arguments to `cbind` and `rbind`** hand the call to `methods:::cbind`/`methods:::rbind` with `deparse.level` tagged; a deparse level of -1 marks a call from methods' own `cbind2`/`rbind2` defaults and binds directly.
- **`LC_CTYPE` is C.UTF-8** at session start, so `iconv(..., "ASCII//TRANSLIT")` transliterates as GNU R does in a UTF-8 locale.
- **`qnchisq` stops at a relative accuracy of 1e-13**, as GNU R's results do. A full-precision search round-trips some quantiles exactly, and `all.equal` leaves exact matches out of its mean, so `d-p-q-r-tests` line 188 failed on a more precise answer.
- **Accepted numeric divergence**: `pf(x, 1, 1, ncp = 20, lower = FALSE, log = TRUE)` is -17.1486, which an independent mixture sum confirms, while GNU R's less precise noncentral beta gives -17.1318 and `d-p-q-r-tst-2` line 302 asserts GNU R's range.
- **Native `loess`** (`crates/interp/src/stats_loess.rs`) is written from the documented method and black-box checks against GNU R. Fits, robustness weights and `predict` match GNU R; the kd-tree vertex interpolation is a cubic Hermite blend along edges and, in two dimensions, a Coons patch (edge curves plus cross-edge slopes, minus the corner tensor product), found by fitting candidates to GNU R's predictions. Three or more predictors use the tensor product alone, unverified. With `span > 1` the neighbourhood radius is the largest distance times `sqrt(span)`. The approximate statistics (`enp`, `one.delta`, `two.delta`) are computed exactly rather than with GNU R's interpolation table, so they differ in the third digit; above 1000 observations the deltas use a cheaper approximation.
- **URL transfers use `reqwest`, not libcurl** (`crates/interp/src/http.rs`). The blocking client was already linked through polars, honors the proxy variables and reads the platform's roots (`SSL_CERT_FILE`). A `url()` connection fetches its whole body when it opens and then reads from memory, like `unz()`. Failures are reported with libcurl's codes and wording, because R code and GNU R's tests match on them. Each request runs on a scoped thread (`on_transfer_thread`), since the blocking client panics inside an async runtime an embedder may own; a transfer thread that dies reports libcurl's "Failed initialization" instead of unwinding into R. `curlGetHeaders` follows redirects hop by hop, so it reports each hop's headers, but not the proxy's CONNECT response that libcurl adds. `libcurlVersion()` reports 8.5.0, the libcurl release whose wording and behavior the transfers follow, so version checks in packages pass; its attributes say what ry really uses (`ssl_version` "rustls", no `libssh_version`, protocols file, http and https).
- **Socket connections are `std::net` streams in blocking mode** (`Device::Socket` and `Device::ServerSocket` in `crates/interp/src/connections.rs`). A non-blocking R connection does not switch the OS socket to non-blocking mode: its read first polls with a zero timeout and, with nothing to read, marks the connection incomplete and returns no bytes, which is how `readLines` holds back a partial line. A blocking read waits up to `socketTimeout()` seconds through the stream's read timeout. Each read refills a 64 KiB buffer with one receive, so `socketSelect()` counts buffered input as ready without asking the OS. Polling is `platform::poll_sockets` (`poll(2)`, `WSAPoll` on Windows). Messages follow GNU R's, with two deliberate differences: `serverSocket(70000)` is an "invalid 'port' argument" where GNU R silently truncates the port to 16 bits, and `socketAccept(timeout =)` gives up after the timeout where GNU R waits forever. With these, `parallel::makeCluster()` works (oracle case `parallel__socket_cluster`). Workers start from `R.home("bin")/Rscript`, so under ry's own R home (`just ry-home`) they run on ry, while under plain `ry run`, whose `R.home()` is GNU R's, they are GNU R processes speaking the same serialization.
- **Unserializing from a connection reads one object** (`unserialize_connection` in `crates/interp/src/serialize.rs`), so several objects on one connection, or a peer's next message on a socket, stay unread. An ASCII string reads exactly its length, which leaves the newline after it unread as GNU R does; an ASCII object that ends in a string is therefore followed by that newline, which the next ASCII header (`\nA\n`) accepts and a binary one does not.
- **`warning(immediate. = TRUE)` signals like any warning**; a flag on the interpreter makes only the default handler print it at once, so calling handlers and `assertWarning` still see it.
- **`Cstack_info()`** reports a fixed size of 7969177 bytes, current use from the remaining native stack, and the evaluation depth from the frame stack, since scripts only compare these against each other.
- **`sort.list` is never an S3 method**: dispatch skips base's own `sort.list` closure when looking for a method of `sort`, because GNU R treats it as a plain function and `sort(x)` on a classed `x` must not reach it.
- **A generic called without its first argument dispatches as on NULL**, matching GNU R's `UseMethod` instead of erroring on the missing argument.
- **An S4 value assigned into a non-list vector** goes through `as.vector` (which dispatches) before `[<-`; into a list it is embedded with GNU R's "implicit list embedding of S4 objects is deprecated" warning.
- **`zeroin2` reports a precision after hitting the iteration limit**: the bracket width `|c - b|`, not -1, so `uniroot`'s `estim.prec` matches GNU R on non-converged searches.
- **JIT function speculation skips frame-bound symbols**: a call of a symbol the frame can bind (a formal or a local assignment) is never speculated from the enclosure, because a formal holding a function shadows a global of the same name (`uniroot(f, ...)` with a global `f`). Pinned by `formal_shadows_enclosing_function` in `crates/jit/tests/test_tiers.rs`.
- **One implementation where two were written in parallel.** `sum` over doubles totals each argument in the long double model, rounds it to a double, and adds it to a long double total, which is what GNU R's observed results across arguments need; main's compensated sum was removed. Parse data comes from `crates/interp/src/parse_data.rs`, which replays GNU R's lexer contexts, instead of a second builder in `lower.rs`. A hashed frame's chains live in one place (`HashTable` in `crates/heap/src/env.rs`), which both `env.profile` and the listing order read. Graphics devices keep the graphics engine's implementations.
- **Impact:** no effect on static analysis. Each behavior has one implementation in `crates/interp`, and the GNU test runner (`development/corpus-and-oracle.md`) is the regression check.

## A fix says whether it may be applied unseen

- `Fix.applicability` (`diagnostics.rs`) is `Safe` when the edit keeps what the code means (the lint
  rewrites, syncing a `#:` comment to the inline type that governs, removing an unreachable arm) and
  `Unsafe` when it guesses intent or silences a finding without handling it (spelling suggestions,
  `stop()` arms for missing cases). It follows ruff's model. `ry check --fix`, the editor's preferred
  quick fix and the `source.fixAll` action apply only safe fixes, because nobody reviews those edits;
  `--unsafe-fixes` and a click opt into the rest. The fix lives on the diagnostic, so the CLI and the
  server share one source of truth (`applicable_fixes`, `apply_fixes`).
- `ry check --fix` re-analyzes and repeats until no fix changes a file (at most `MAX_FIX_PASSES`),
  because overlapping fixes are skipped within a pass and one fix can expose another. It refuses
  to write a file whose text on disk no longer matches the analyzed text, and leaves a file with a
  syntax error alone.
- Safe means safe in context, not by syntax alone: `boolean-shorthand` holds only while the file
  binds no `T`/`F` (it rides the lints' `unless_redefined` channel), and the `#:` comment fix is
  withheld when the inline type names a type parameter the comment would not bind.
- The server keeps each open document's settled fix-bearing findings with the text they were computed
  for (`published_fixes` in `server.rs`). `codeAction` reuses them while the text matches, so a
  request per cursor move costs no analysis, and the offered fixes are the findings on screen.

## Parameter hints and call hierarchy follow R's own call rules

- Parameter-name inlay hints (`inlay_hints` in `ide.rs`) map arguments to parameters with the
  checker's own `argument_targets` (`check.rs`), so the hint and the type check never disagree on
  where an argument goes. Hints stop at a forwarded `...` and skip a piped value. They show only in calls with at least two positional arguments, and skip one-letter
  parameters and arguments whose text already names the parameter (`years` or `x$years`), because a
  hint that repeats what is on screen is noise.
- The call hierarchy is limited to project top-level functions (the `package_definitions` index),
  because those are the items a reader navigates between and the index is already per-name tracked.
  Only call heads count as calls, so passing `f` to `lapply` is a reference, not a call. A caller
  that is a top-level statement is grouped as "(top level)". The server re-resolves a function item from
  its uri and selection start instead of serializing ids into `data`, so a stale item never names
  a wrong function; a "(top level)" item does not re-resolve, because its start is whatever the
  statement calls first.

## The debugger lives inside the interpreter

- **Previous shape**: none; `browser()` was the only way to stop R code, and only in a console.
- **Shape**: the Debug Adapter Protocol adapter is `interp::debugger`, inside the interpreter, not
  a separate process attaching to it. `ry debug-adapter` (`crates/ry/src/debug_adapter.rs`) only
  frames messages on stdio and reaches the adapter through libR's `ry_debug_adapter`, because the
  interpreter lives in `libR.so`. A reader thread feeds a channel so the runtime can poll for a
  `pause` between statements without blocking on stdin.
- **Hooks**: statements are located by the srcrefs `{` already sets (`Op::SetSrcref` in
  compiled code, `do_begin`/`eval_all` in the tree walker). Without an attached adapter the hook
  costs one `Option` check per statement. `SetSrcref` carries a flag, because the one that
  restores the outer srcref after a block starts no statement and must not stop. `browser()` and
  the uncaught-error handler stop through the same entry point.
- **Rooting**: every object the variables view hands out a reference for is rooted until the
  program resumes, because the debug console can drop its last other reference.
- **Stdout**: the protocol owns stdout, so the adapter keeps a duplicate of it and points fd 1 at
  stderr; native code calling `printf` cannot corrupt the protocol.
- **Package code**: the loader lowers lazy definitions without srcrefs for speed, and only while a
  debugger is attached does it lower against a whole-file srcfile (`interp::lower::file_srcfile`),
  held as the tree handle's protected value. Loading stays as fast as before outside the debugger.
- **Testing**: `interp` `test_debugger` (transcripts and a seeded random client) and the
  `debug-*.scenario` journeys, which run the real `ry debug-adapter`.

## A conditional write of a formal covers the paths that run it

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

- **Previous shape.** An overload probe stands an unchecked argument in as a `Named` type. Its
  name counted as an undeclared nominal, and undeclared nominals are compatible with everything,
  so `months(unknown)` picked the `Date | POSIXct` candidate and returned `character`.
- **Target shape.** The witness has one reserved name, `UNCHECKED_WITNESS` in `infer.rs`, which no
  source annotation can spell, and `undeclared_nominal` excludes it. Only `Any` and an
  unconstrained variable accept it, so a call with an unchecked argument picks no class candidate
  and stays `Unknown`.

## A call on a parameter whose arity the body tests is unchecked, and a later sibling does not force

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

## Native scalar functions: frameless calls with checks proven at compile time

- **Previous shape.** A compiled call of a closure that could not be inlined (any body with an
  `if`, so every recursive function) went through `runtime::jit_call`: a frame, promises holding
  the evaluated arguments, the callee's own compiled entry, and boxed results. Annotation checks
  were skipped only when `entry_for` proved them, and the result was refined by a guard. Typed and
  untyped code ran at the same speed, because the profiles already knew what the types said.
- **Target shape.** `crates/jit/src/native.rs` compiles a closure whose body is a pure scalar
  computation (constants, parameters, locals, `if`, `&&`/`||`, arithmetic, comparisons, `abs`,
  `sqrt`, `exp`, `log`, `floor`, `ceiling`, and calls of itself or other such closures) to a
  machine function `fn(budget, args...) -> (status, value)` over unboxed `i32`/`f64` values,
  specialized per closure and argument types. A compiled caller with unboxed arguments emits
  `Action::NativeCall` instead of `Action::Call`; recursion stays in machine code.
- **Why no frame is observable.** The body reads only its parameters and locals, has no effect,
  and calls nothing that could look at a frame. Everything that would warn or raise in R (an `NA`
  condition, integer overflow, `sqrt`/`log` of a negative, the expression-depth limit, a pending
  interrupt) returns `FAILED` before anything visible has happened, and the caller hands the call
  to tier 0, which reruns it and reports the problem exactly as R does. The depth budget is
  `max_depth - frames.len()` at the outermost call, decremented per nested call, so the limit
  matches tier 0's call for call. A left `NA` operand of `&&`/`||` also fails, rather than
  reproducing R's three-valued rule. Closure arguments are evaluated eagerly, which R's promises
  make observable only through an assignment or a `return` inside the argument, so those refuse.
- **Annotations.** Argument checks are decided by `entry_for` and the return check by
  `annotations::return_admits` when the native compiles, from the specialization's types; a
  function whose checks would fail is never compiled, so its calls go the checked way.
  `entry_for` and `admits_inline` now refuse an `Array` mask for a plain scalar.
- **Invalidation.** A native's function lookups (its callees' included) join every caller's
  speculations, and the cache revalidates them on each lookup, recompiling when a callee was
  rebound. Compiled natives keep their closure rooted (the cache key is its address) and are
  capped at 4096 per session.
- **Impact.** why-ry `fib` (`fib(30)`) runs in 0.007 s against GNU R's 1.12 s; `fib(25)` went
  from 0.072 s through `jit_call` to under 0.001 s. It speeds typed and untyped code equally: types make no scalar function faster than its
  profile already does, they only decide at compile time what an untyped function never checks.
  Next: unboxed records, tuples and tagged unions as native arguments and results.

## Range formatting is a cut of the whole-file format

- Previously the formatter had only a whole-file entry point, and an unfinished range formatter
  formatted the selected text on its own, so a selection and the file could disagree about a line
  (indentation, a brace added by auto-bracing, a comment's attachment).
- Range formatting (`format::format_range`) formats the whole file, pairs source and formatted trees
  statement by statement, and returns the formatted text of the statements the selection touches.
  The formatted file is the single source of truth, so restriction and additivity hold by
  construction and the range invariant battery (`check_range_format_invariants`, also a fuzz target)
  can check them. Cost is a whole-file format per request, which is cheap next to analysis.
- On-type formatting reuses it: a line break formats the line just finished, `}` the construct it
  closes. The language server advertises both unconditionally; the experimental-features registry
  that gated them was removed, and its config key still parses with a warning.

## Bug lints are on by default, style lints are opt-in

- lintr parity is not a goal. A lint is added only when it catches real bugs (as `length-test`
  did: every corpus hit was a bug) or when it fits the existing lint architecture and costs little.
  Lints already ported stay; nobody works through lintr's list for completeness.
- A lint is on by default when the code it flags behaves differently from how it reads
  (`equals-na`, `vector-logic`, `length-sequence`, `class-equals`, `trailing-comma`, `T`/`F`), and
  off when it only flags taste (`redundant-return`, `commented-code`, naming, shadowing). lintr turns
  both kinds on, which teaches users to ignore it; ry's defaults must stay clean on a tidy codebase.
- A fix that changes behavior on purpose (`x == NA` to `is.na(x)`, `&` to `&&`) is unsafe even when
  it is almost certainly what the author meant, because `ry check --fix` applies safe fixes unseen.
  `redundant-return` is safe: `return()` keeps its argument's visibility, so
  `return(x <- 1)` and a trailing `x <- 1` are both invisible.
- The lint fixtures render each fix (`safe fix a..b "text"`), so a fix's range and text are part of
  the contract.
- `unreachable-code`, `sprintf-arguments` and `length-test` are on by default (each is a bug; a
  CRAN corpus sweep found only true positives). `duplicate-argument` is opt-in although a repeated name is often a
  bug, because a callee that collects `...` keeps both on purpose (cli bullets in `c()`, repeated
  HTML `class` attributes) and a syntax lint cannot see the callee's formals. The semantic
  version already exists: with `[check] typing = true`, a call that gives a known function's
  formal twice is a `type-mismatch` (`named_argument_mismatch` in `check.rs`), so no second
  semantic lint is needed.
- The other lintr ports (`any-is-na`, `paste-sep`, `lengths`, `literal-coercion`,
  `redundant-equals`, `single-quotes`, `testthat-expectation`, `condition-message`,
  `comparison-negation`, `outer-negation`, `fixed-regex`, `which-grepl`, `length-levels`,
  `browser-call`, `numeric-leading-zero`, `line-length`, `implicit-assignment`, `while-true`,
  `scalar-in`, `order-subset`, `matrix-apply`, `empty-assignment`, `unnecessary-lambda`,
  `yoda-test`, `nzchar`, `sample-int`, `unnecessary-concatenation`) are opt-in taste or speed lints. `implicit-assignment` is opt-in because
  `while ((line <- f()) != "")` is idiomatic R; lintr's `cyclocomp`, `object_length` and
  `nested_ifelse` were skipped as low value without a fix.
  `line-length` is the only lintr layout linter ported, because the formatter never splits a
  line, so nothing else keeps lines short. lintr's whitespace and
  layout linters are not ported, because `ry format` owns layout.
- `trailing-comma` stays on inside rlang's dynamic dots (`mutate(d, y = 1, )`), where the comma
  is harmless: it is a warning whose safe fix only removes the comma, and stub `@masked` cannot
  mark dynamic dots because it also covers data.table verbs, whose `...` is not rlang's.
- lintr's `# nolint` comments are honored (`apply_suppressions` in `crates/ry/src/diagnostics.rs`),
  because 15 packages in the CRAN corpus carry them and the mandate is that existing code keeps
  working. A bare `# nolint` keeps syntax errors, and lintr names map to ry codes in `lintr_codes`;
  keep that table in step when a lint is ported. A `.lintr` file's `linters` field is read too
  (`apply_lintr` in `crates/ry/src/config.rs`): it fills only the levels `ry.toml` leaves at
  `Default`, so `ry.toml` always wins, and an unreadable field is a warning, never an error,
  because the file is lintr's. Only the `.lintr` at the project root (nearest `ry.toml`,
  `roughly.toml` or `DESCRIPTION`) applies, so the answer never depends on how a path is spelled.
  `lint_levels_mut` in `lints.rs` is the one name-to-field table, and
  a unit test pins it against the struct's fields.
- Call lints are gated in the green pre-check by `callee_may_lint`, which knows which enabled lint
  reads each bare callee, so common callees (`length`, `stop`, `which`) cost nothing while their
  opt-in lints are off. A fix whose replacement binds more loosely than what it replaces goes
  through `in_place_of`, which adds parentheses unless the position stands alone.

## `ry check` keeps an on-disk record of its findings

- **Measured first.** On 4 cores, a cold check of the 300,000-line finance bench takes about 1.45 s
  wall and 5 CPU seconds. Every pass already spreads over the cores: deriving the syntax facts used
  3.8 cores, naming 3.6, and the item check 3.7. Releasing the trees (0.05 s) and setup (0.05 s) are
  the only serial parts. More parallelism therefore has no headroom on this machine, and a cold run
  under 1 s needs about a third less CPU work, not more threads.
- **Previous shape.** Each run started from nothing. The salsa memos live only in memory, and
  salsa 0.28 neither exposes its dependency graph nor persists memos without serde on every value,
  including the global `Ty` table.
- **Target shape.** `.ry_cache/check.json` (`crates/ry/src/check_cache.rs`) records the findings,
  not the memos. Each file's record holds its content fingerprint, its surface fingerprint (the
  text without top-level function bodies, unless a body can write or declare outside its frame),
  the files its check read through interface edges, and the `NAMESPACE` imports it uses. The whole
  record is keyed by the ry binary (hashed by its bytes), the configuration, `NAMESPACE`,
  `DESCRIPTION`, the data files and the stubs. With no changed file, every finding replays. When
  every changed file keeps its surface, only the changed files and their dependents are checked.
  Anything else checks all files. A directory with no `ry.toml` or `DESCRIPTION` gets no record.
  The architecture page's "The check cache" section has the details.
- **Why the surface rule is sound.** One file's check reads other files without an interface edge
  only through name-keyed project-wide maps. Those are folds of per-file shares, so the surface
  fingerprints each share as `diagnostics::project_fold_shares` spells it, beside the text outside
  function bodies. A text rule alone was not enough: a `UseMethod` call or a `#: @type` inside a
  body feeds the S3-generic and type-declaration folds, and a review reproduced both as stale
  findings before the shares were added. Inferred schemes are the remaining body-dependent fact,
  and the recorded interface dependencies cover them. On all three benches, a file's dependency
  closure has a median size of 0, a 90th percentile of 1 and a maximum of 1.
- **Measured.** On the finance bench, a replay takes 0.10 s (57 MB peak), a body edit 0.45 s, and a
  cold run with the record written 0.1 s more than `--no-cache`. The data-science and biostatistics
  benches replay in 0.09 s and 0.07 s. The output is byte-identical to a run with `--no-cache`.
- **Floor of a partial run.** It still parses every file, because the project-wide folds read every
  file's item tree. It warms those folds on all cores, and keeps its trees instead of releasing
  them: releasing walks every item serially and cost more than it saved.
- **Checked.** With invariants on, every run that reuses findings is compared against a fresh run
  and panics on a difference. A seeded simulation drives that comparison with body and signature
  edits, `<<-`, `library`, `assign`, `setClass`, `UseMethod` and S3 methods, `#: @type` in and
  outside bodies, script files, and added and removed files
  (`cached_checks_match_fresh_checks_across_seeded_edits` in `cli.rs`).

## Corpus-driven stub and overload fixes

- A name the item writes with `<<-` reads as `Unknown` unless the project defines it (`read_non_local` in `check.rs`, via `top_level::super_assigned_names` over the item's own HIR, so no project-wide map is read). Before, openxlsx's Reference Class field `t <<- t[-inds]` resolved to base `t` and indexing it was a false report.
- `grepl`, `grep`, `regexpr` and `gregexpr` take `pattern: character[]` as a second overload, not a union: a union parameter makes an untyped caller parameter infer the union, which then fails at `paste0(collapse = p)` (gt). `sub` and `gsub` keep `character` because no corpus code needed more.
- Overload resolution settles one narrowing disagreement: a scalar narrowing wins over the same atom's vector. A general "most specific by `compatible`" rule was tried and rejected, because it picked list shapes (`list[named: ...]`) that later reads rejected, adding ten corpus false reports.
- `menu`'s `choices` is one union including `list[Any]`, because a list and a character overload disagree and would make an untyped caller `Unknown`.
- The cost of the second `grepl` candidate: `grepl(1L, x)` now reports "no overload matches" over the whole call instead of the argument.

## Strict-friction rules for matrices, unions, `...: Any` and `$` members

- `m[i, j]` on a `matrix` types in `matrix_index` (`check.rs`): elements are `Any` because a matrix carries no element type, two vector or empty slots give `matrix`, a scalar slot gives `Any` (R drops dimensions), and `drop = FALSE` keeps `matrix`. Any other named argument falls back to the general rules rather than guessing.
- An operator on a union beside a class dispatches per member, and a member with no class takes the ordinary operator rules (`ordinary_operator_result`). The per-member path applies only when some member is a class and commits only if it adds no error, so plain unions keep their old messages.
- An expected `fn(...: Any)` pads a lambda's parameter hints with `Any` past the declared formals: R would forward any arguments through `...`, so `Any` is the honest type, and `Unknown` would make every callback in plumber or shiny a strict report.
- An annotation covers strict origins anywhere inside its value except inside a nested function literal or braced block (`annotation_covers`), because those have their own bindings to annotate. Before, only an `Unknown` right-hand side as a whole was covered.
- A stub declares an environment member as `Class$member : T` (`is_stub_name`, `infer_field`). This is how polars types `frame$select(...)`; it is looked up before `$.class` dispatch and keyed by the class name, so no new table exists beside the global scheme map.

## Inferred annotations follow the project's annotation style

- `ide::inferred_annotations` is the one source of the edits behind the per-binding code action, the whole-file action and `ry check --annotate`, so the editor and the CLI cannot write different annotations.
- With inline annotations on (the default) the type goes where inline syntax puts it: binders after `function` or `\\`, each formal's type after its name, ` -> R` after the parameter list, and `name: T` for any other binding. A right assignment, a quoted name, or a generic type on a value that is not a function literal has no inline place, so it falls back to a `#:` line.
- The whole-file action and `--annotate` write top-level bindings only, because locals carry no interface and annotating each one would bury the signatures; the per-binding action still offers a local.
- `--annotate` is its own flag rather than part of `--fix`: a fix repairs a finding, while an annotation records what inference found and rewrites every signature, which `--fix` users have not asked for.
- A formal that is optional only because the body may never read it (`[value]: T`) also falls back to `#:`: inline syntax spells optionality only as a default, so the inline form would make a caller that leaves the argument out an error.

## The strict load check overlaps the runtime start

- Before, `ry load` on a strict package ran the whole-project strict check, test files included, and only then started the runtime, so a cold typed load cost the check plus the load (4.8 s against 2.4 s untyped on finbench).
- On ry's runtime the check now runs on a thread (`run_load` in `crates/ry/src/main.rs`) while the runtime starts. The loader receives a verdict callback (`ry_loader_set_verdict`) and calls it before any statement that is not effect-free, before compiled code and `.onLoad` (`.ry_await_verdict()` in `runtime.R`), after the load returns, and before an S4 statement held back for a later lookup (`do_run_deferred`) that is not effect-free; a refusal exits with the verdict's status. GNU R still checks before it starts, because its loader cannot call back into ry.
- An eager item is effect-free (`EagerItem.effect_free`, `load_index.rs`) when it binds a function literal, or when `item_effect_free` (`top_level.rs`) holds: a strict constant, or a definition whose declaring calls' arguments have no effect. Strict mode's `Definition` alone is not enough, because it leaves a declaring call's arguments to R, so `setRefClass(..., methods = list(m = { cat(1); function() 1 }))` passes strict mode yet prints. Loading an imported package (NAMESPACE imports, `library()` with effect-free arguments) is allowed before the verdict; this package's own code with an effect never is.
- `.ry_await_verdict()` sits before `sysdata.rda` and `data/*.R`, because a data file is ordinary code.
- A constant may call a user `@pure` function before the verdict. That purity is trusted, not checked, which is the documented contract of `@pure`.
- The check computes diagnostics only for the `R/` files (`analyze_project`'s requested paths): a name that only a test file defines is unbound in the loaded package anyway, and the tests' diagnostics were never reported. It walks the whole root so it can share the check record ("The strict load check shares the check record").
- `package_verdict` is `extern "C"`, so a panic in the check exits 101 instead of unwinding into libR, which would abort.
- The check thread lowers its own priority (`lower_priority`), and the analysis workers inherit it, so on a loaded machine the runtime start keeps the cores it needs; the check has the whole load to finish.

## Every load of a strict package is checked

- Before, only `ry load`'s first load ran the strict check; `load_all()`, a console reload and the load a console makes at start ran a package strict mode would refuse.
- The load index records whether strict mode covers any file (`PackageIndex::strict`, the same test as `prepare_purity`), and every plan carries it as `Plan::strict`, so a host decides without reading ry's configuration twice.
- A host checks a plan that changes something (`!is_noop()`): GNU R's `.Call` routine and `ry load --emit-plan` through `load::strict_value`, ry's runtime through a host callback (`ry_runtime::set_check`, registered by every command that runs the runtime via `repl::set_package_check_on_ry_runtime`). The loader's crate cannot depend on the CLI's analysis, which is why it is a callback. A refusal turns the plan into an error before R sees it, so nothing runs and the old bindings stay.
- Whether a package is strict is decided per package root on both runtimes, from that package's own `ry.toml` (`load::strict_config`): GNU R's routine reads it directly, and ry's runtime asks a second host callback, the strict query, before each plan. Taking it from the command's configuration instead would let a lax script `load_all()` a strict package unchecked.
- The first plan of a session that has a verdict (`ry load` on ry's runtime) skips the callback: the concurrent check already covers that load, and checking again would serialize what the verdict overlaps.
- The check is the whole-package `preflight::check_package` with its fingerprint-cached clean verdict, so a reload of a large strict package pays a full analysis; checking only the changed files is in `backlog.md`.

## Lazy definitions keep source references

- Before, a lazy definition on ry's runtime lowered its node without a srcfile, so `print(f)` deparsed and tracebacks named no file, except under the debugger, which built a whole-file srcfile per force.
- A plan's files become per-file handles (`FileSource`) holding the source and the `keep.source` option read when the handle is made. The first force from a file that keeps source builds its `srcfile` (`srcfilecopy` semantics) and stores it in the handle, so a file costs nothing unless one of its definitions is used, and the srcfile is shared by all of them.
- The definition is reparsed alone after as many spaces as its line has bytes before it, and its srcrefs are computed against the line's real text (bytes and character columns both match GNU R's parser), lowered with a line offset (`interp::lower::lower_node_at_line`), which gives exact line and column srcrefs without parsing or lowering the whole file. The tree in the handle is used directly when no srcref is wanted.

## GNU R's public C API is a checked list, not a measured subset

- **Previous shape.** `test_headers.rs` checked that what ry's headers declare is exported, so a
  public name missing from both passed. Diffing GNU R 4.5's `R.dll` export table found such names
  (`R_findVar`, `R_findVarInFrame`, the `R_GetVarLoc*` family, `Rf_formatReal` and its siblings,
  `R_AllocStringBuffer`, `utf8clen`, `Rf_mbrtowc`, `R_MissingArgError`).
- **Target shape.** `crates/libr/tests/headers/r_api_names.txt` lists every name GNU R's public
  headers declare *and* its `R.dll` exports (declared-only names vary by platform and are often
  macros), plus an internal section of non-header names packages link. The test fails when ry's
  headers miss one or `libR` does not export it. Regenerate against a new GNU R release with
  `RY_GNU_R_INCLUDE`, `RY_GNU_R_EXPORTS` and `RY_UPDATE_API_NAMES=1` (`development/testing.md`).
  GNU R's front-end headers (`ga.h`, `graphapp.h`, `iconv.h`, `libintl.h`) are excluded: packages
  get gettext's header from Rtools, and the rest is RGui.
- `R_varloc_t` is a struct holding one pointer, as in GNU R, so `R_findVarLoc` returns the binding
  cell by value through the same ABI.
- `VECTOR_PTR` is declared without `NORET`: GNU R declares it as an error stub, ry returns the data
  pointer, and a `noreturn` declaration would let a compiler drop the code after the call.
- `NORET` is `[[noreturn]]` only for C23. C++ gets `__attribute__((noreturn))`, because Rcpp writes
  `inline void NORET stop(...)`, where clang rejects the standard attribute; `Rf_error` and the
  other no-return entry points carry `[[noreturn]]` directly in C++. A package's own `NORET` wins.
- **Impact.** A header name GNU R exports can no longer go missing silently on any platform. The
  265 `R.dll` exports ry still lacks are GNU R internals and RGui entry points that no package in
  the measured closure uses (`tools/research/data/symbols.csv`).

## Windows `R CMD INSTALL`: the home's `Makeconf` decides the platform

- **Shape.** `rcmd::Platform::of_home` reads `SHLIB_EXT` (and `DYLIB_EXT`) from the R home's
  `etc/Makeconf`, so the home stays the single source of truth for how its packages build, and a
  Linux-hosted test can drive the Windows conventions with a Windows `Makeconf`. From it follow
  the shared-object suffix, `libs/x64/`, `R_ARCH`, and build-file precedence (`.ucrt`, then
  `.win`, then the portable `Makevars`; `configure` and `Makefile` have no Unix fallback, as in GNU
  R). `makeconf(root, toolchain, platform)` writes the Rtools ABI for Windows: `-shared
  -static-libgcc`, no PIC flags, forward-slash paths, `-L"$(R_HOME)/bin/x64" -lR`.
- **The SHLIB is `<package><ext>`**, the name unchanged, as GNU R links it; data.table's Makevars
  renames `data.table.dll` to `data_table.dll` itself. Every shared object `src/` holds is
  installed, so a renamed one is found.
- **Windows home layout.** `R.dll` and its import library `libR.dll.a` in `bin/x64/`; `bin/R.exe`,
  `bin/Rscript.exe` and `bin/x64/{R,Rscript,Rcmd}.exe` are copies of `ry-rcmd.exe`, which picks the
  front end from its file stem and the home from its location (Windows runs no `#!` scripts).
  Directories are symlinked where Windows allows it and copied otherwise; Wine reports a
  successful symlink without creating it, so the link is checked for existence.
- `ry-rcmd` uses `crates/ry/build.rs` as its build script, so both executables get the 64 MiB
  stack and the UTF-8 manifest from one place; the manifest is `include_str!`-ed so the script works
  from either package.
- **Verification limit.** Running MSYS2's `gcc`, `make` or `sh` under Wine is not permitted in the
  agent environment, so the six packages were installed cross-hosted (`r-runtime.md` section 11)
  and only loaded under Wine. The release smoke test is the first native Rtools run.

## The Windows session's native encoding is UTF-8

- `init_locale` sets every category from the environment and then, on Windows, swaps a non-UTF-8
  codeset for UTF-8 (`use_utf8_codeset` in `crates/interp/src/system.rs`), as GNU R from 4.2 does
  under its manifest. The manifest alone does not suffice where it is ignored (Wine).
- Locale names report the UTF-8 codeset as `.utf8`, GNU R's spelling, including in the composite
  `LC_ALL` string; Wine's CRT spells it `.65001`. An explicit `Sys.setlocale` to another code page
  is honored.
- `.Internal(Sys.which)` exists because GNU R's Windows base code calls it (Unix base R uses
  `command -v`); it searches `PATH` with `PATHEXT` and returns forward-slash paths.

## DataFrame expression operands are checked against the engine's own typing

- **Previous shape.** `frame_expression` in `check.rs` computed each expression's column type but
  never looked at its operands, so `col("region") * 2` on a `character[]` column passed the
  checker and failed only when the query ran.
- **Target shape.** The same walk checks operands with the rules of the engine's `Typer`
  (`crates/table/src/query.rs`): arithmetic, unary `-` and the numeric aggregations refuse text
  (`character` or `factor`), `&`/`|`/`!` refuse text, and the test of `Expr.if_else` must be
  `logical`. The class comes from `join_class`, so a type outside logical/number/text (unknown,
  `Any`) stays silent. Two error kinds, `ExpressionOperand` and `ExpressionComparison`, name the
  column when the operand is a bare `col("x")`.
- **Deliberately stricter than the engine.** The engine compares an `integer` or `logical` with a
  string by formatting the number, as R does; the checker reports text against `integer` or
  `double` all the same, because with a known schema that pairing is almost always the wrong
  column. Text against `logical` stays allowed, so `col("x") == NA` is not flagged.
- **Impact.** No new state: the checks run inside the existing refinement of a verb call, which is
  per item, so incremental analysis is unchanged.

## Strict mode reports a DataFrame query over unknown columns

- **Previous shape.** A `DataFrame` whose columns are unknown (a computed `read_csv` path, a bare
  `DataFrame` parameter, a verb argument the checker cannot read) is a known type, not `Unknown`,
  so strict mode let every `col("x")` on it through unchecked, and "a clean strict run checks
  every column" did not hold.
- **Target shape.** `note_unchecked_columns` in `check.rs` records a strict origin
  (`StrictOriginKind::UnknownColumns`) on the subject of a verb that names a column, through a
  string argument or a `col("x")` anywhere inside one, when the subject's columns are unknown. A
  verb whose chain already has an earlier step over unknown columns that names a column is
  skipped, so a chain is reported once, at its first step that names a column; steps that name
  none (`head`, `lazy`) are looked through. The message suggests the annotation with the first column the query names.
- **Why at the query, not at the read.** The unknown schema has many origins (readers,
  parameters, pivots, unreadable arguments), and a frame nobody queries by column needs no schema.
  Reporting the first unchecked query is precise and always actionable; an annotation on the
  binding gives the frame columns, so the origin disappears rather than needing to be cleared.
- **`read_csv` has its own reader typing.** `read_csv_type` matches `read_csv`'s own formals
  (`path`, `strings_as_factors`) instead of borrowing `fread_type`, whose argument whitelist made
  `strings_as_factors = TRUE` fall back to unknown columns. Both share `sniffed_columns`, and the
  file read goes through the per-path `data_file_layout` query, so a data-file edit re-checks only
  the reads of that file (`a_data_file_edit_rechecks_only_the_reads_of_that_file`).
- **Impact.** No new state; the check reads recorded types inside the existing per-item
  refinement.

## The data-table lint is opt-in and marks only constructors

- **Shape.** `[lint] data-table` (off by default, `data_table_lint` in `lints.rs`) flags
  `data.table()`, `fread()`, `as.data.table()` and `setDT()`, bare or `data.table::`, and names the
  DataFrame equivalent with a link to `reference/from-data-table.md`. A bare call is dropped when the
  file defines that name, exactly as `object-system` does.
- **Why constructors.** Like the object-system lint, which flags definitions rather than every
  `new()`, every data.table operation needs a table first, so the constructors mark where
  data.table enters the code without flagging each query.
- **Why off.** S4/R6 warn by default by user mandate; data.table has no such mandate, the
  benchmark code bases import it everywhere, and the guide says migrating is optional.

## CSV files are read in released slices straight into the final column buffers

- **Previous shape.** `read_csv_with` handed the whole mmapped file to one Polars parse, then
  converted each column to its R type as a second copy.
  At 1e8 group-by rows the file (5.2 GB), the parsed frame and the converted columns were all
  resident together: the load peaked at 9.9 GB and the run did not fit in 15 GB.
- **Target shape.** `read_bytes` (`crates/table/src/io.rs`) parses the file in about 256 MB slices
  that end on a line break outside quotes (`slices`, which runs the quote state machine
  `Quoting` over each stretch in parallel from all three start states and chains them), appends
  each slice's columns into one `Builder` per column (a `Vec<i32>`, `Vec<f64>` or `Vec<u32>` of
  factor codes plus a validity bitmap, reserved from the first slice's bytes per row), and
  `madvise(DONTNEED)`s the parsed slice. A factor column's slices are interned into one
  `factor::Interner` (see "Factors are ry's own codes plus one level table, outside Polars").
- **Fallbacks.** A Polars parse error on
  a slice retries the whole file as text, so a row a slice boundary breaks never changes the
  answer. `check_csv_input` (the fuzz target) compares a 16-byte sliced read with a whole read.
- **Impact.** 1e8 load peak 5.6 GB (was 9.9 GB). No duplicated state: the builders are the final
  buffers. The rest of the 1e8 peak is q10, which has nearly one group per row, so `aggregate` and
  `hashed` in `group.rs` drop each per-row and per-group buffer as soon as the next step no longer
  needs it; the whole group-by run then peaks at 13.4 GB from R and 12.6 GB from Rust, against
  data.table's 13.7 GB.

## The Rust table benchmark runs on libR's allocator and the same lazy plans as the R path

- **Previous shape.** `db_benchmark` used glibc malloc and answered joins through the data.table
  `x[i]` IR, while `ry run` used libR's mimalloc and lazy `Plan`s, so "ry from Rust" (9.2 s at
  1e7) lost to "ry from R" (6.3 s) for reasons that had nothing to do with the API.
- **Target shape.** `benches/allocator.rs` installs mimalloc with libR's settings (eager commit
  off, purge delay -1) as the global allocator, and the joins run the same `Plan`s as `ry.R`. The
  driver now measures the engine with no R in the way: 4.9 s against 5.5 s from R at 1e7.

## A load holds back S4 definitions and caches its index across processes

- **Deferred S4** (`loader::deferred_s4`, `.ry_force_s4` in `runtime.R`). Previously every top-level
  `setClass`, `setGeneric`, `setMethod` and `setValidity` ran eagerly through the `methods` package,
  about 300 ms on the 326,000-line bench package. On ry's runtime a package's S4 definitions are now
  held back as one unit: each name they bind becomes a placeholder promise, and the interpreter's
  class-cache miss and `R_getGeneric` hooks (`interp::methods`) force them. A force during the load
  runs only the definitions before the current statement, so order is GNU R's. The unit is
  all-or-nothing because S4 registration is global state (class tables, generic method tables) that
  no per-name promise could reproduce; the rules that keep it eager (a method on a generic the package
  does not define, a reference class, a definition reading a name bound later, a generator name that
  is also written elsewhere) are listed in `reference/package-loading.md`, "S4 definitions", and
  pinned in `crates/loader/tests/reload/deferred_s4.test`. GNU R's backend never defers, since it has
  no hook to force on; `options(ry.load.lazy_s4 = FALSE)` turns it off.
- **Held-back methods for other packages' generics** (`DeferredS4::foreign_methods`). Previously
  any `setMethod` on a generic the package did not define kept the whole unit eager, and nearly
  every S4 package defines a `show` method, so on the synthetic finance code base no S4 was held
  back. Such a method now qualifies when its signature names one of the package's own classes,
  which means it can only be selected for objects of that class. Each kind of generic needs its
  own door, because forcing on every dispatch of `initialize` or `length` would fire during the
  load itself (`.ry_foreign_generic_kind`, `.ry_defer_s4`):
  - A generic function (`show`, `initialize`, any `standardGeneric`): the placeholders are per
    dispatch label, one promise per method in the generic's `.MTable`/`.AllMTable`, labelled as
    `methods` labels them (`.ry_s4_label`: `matchSignature` padded with `ANY` to `.SigLength`).
    A dispatch for another class never reads them, so it never forces; `getMethod`,
    `existsMethod` and a dispatch for a held-back class read the label and force. A signature
    longer than `.SigLength` keeps the unit eager. Another package's `setMethod()` on the same generic
    reads every label (`methods:::.resetInheritedMethods`), so it forces the unit; a force during
    this load from a method after the cursor reads the label with `get0`. Forcing restores each table's previous value
    before the definitions run (`.ry_restore_s4_labels`).
  - A primitive (`length`, `[`): primitives dispatch from C without `standardGeneric`, so
    `s4_dispatch` forces only when a non-promise S4 argument's class is one of the unit's
    classes (`interp::methods::Lookup::Primitive`). A held-back method can apply to nothing
    else, so this is exact.
  - A function that is not yet generic from another package (`as.list`, `toString`; one of the package's own
    functions keeps the unit eager, since a placeholder would replace it): `setMethod` would make it generic
    in the package namespace, so that name and `.__T__name:pkg` become placeholders and the name
    joins the forcing generics.
  - Group generics keep the unit eager: a group member dispatches through the group's table,
    which no per-name placeholder covers.
- **Class unions** (`S4Definition::Union`). A union of only the package's held-back classes is
  held back with them. A union that names other packages' classes edits the global class-cache
  entry of each such member (GNU R changes `.getClassesFromCache("numeric")@contains`, not
  `methods::.__C__numeric`), which no placeholder can stand in for, so a mixed union
  (`c("Cls", "NULL")`) used to keep the unit eager and a union of only other packages' classes
  (`c("numeric", "NULL")`) ran in place at about 10 ms each, which on a real S4 package meant no
  deferral at all or 150 ms of load for 15 such unions. Both are held back, because every
  membership question names the union (`is(NULL, "U")`, a method for `U`) and so forces the unit,
  and `possibleExtends` falls back on the union definition's `@subclasses`. What stays wrong until
  the force is the reverse listing (`extends("NULL")`, `is(NULL)`), documented under Limits.
  Forcing on any lookup of a member class (a hook in `R_getClassFromCache`) would make the
  listing exact but was tried and rejected: the methods package itself calls `is(NULL, ...)`
  constantly (`setOldClass()` does at load), so the unit ran at once and nothing stayed deferred.
  A method on another package's generic
  for a mixed union keeps the unit eager, because dispatch on `NULL` can hit the generic's table
  without any class lookup. A restore must add the unions to those members in definition order,
  because that is the order `extends("NULL")` lists them in after an eager load
  (`cacheMetaData()` only replaces the entries in place), not `getClasses()` order.
- **Coercions** (`S4Definition::Coerce`). `setAs(from, to, def)` is a method on `coerce` (and on
  `coerce<-` with `replace`), so it is held back as a foreign method with signature `(from, to)`
  when either side is a held-back class; `as()` reaches it through `.quickCoerceSelect`, which
  reads the label placeholder. A coercion between other packages' classes runs in place. Before,
  any `setAs` made the unit eager because `setAs` was not in `PURE_S4_READS`.
- **No `cacheMetaData` while held back** (`.ry_apply`). The sync after a load called
  `cacheMetaData`, which reads every `.__C__`/`.__T__` binding and so forced the unit. Each
  definition already caches what it defines as it runs, and `.ry_force_s4` ends with the same
  call, so the sync skips it while a unit is held back.
- **Cross-check against eager.** Laziness changes when definitions run, not what they compute,
  so the check is a whole-process comparison rather than an in-process invariant:
  `crates/ry/tests/test_load.rs`
  (`a_held_back_method_for_another_packages_generic_is_there_when_that_generic_needs_it`) runs
  seven first-use probes (`show`, primitives, `as.list`, `print`, union membership,
  `initialize` with `callNextMethod`, `readRDS` objects) lazily and eagerly and asserts identical
  output, and `tools/loader-bench/s4_check.R` compares lazy, eager, cache-hit and GNU R results on
  the generated package. The deferred-S4 rules are pinned in
  `crates/loader/tests/reload/deferred_s4.test`.
- **Measured** on a seeded S4-heavy package (`just bench-s4-load`, `tools/loader-bench/s4_package.py`:
  2,863 S4 definitions). Eager, the first load took 5.7 to 5.9 s under `ry run`, 92% of it S4
  (setMethod 2.8 s, setClass 2.3 s, setClassUnion 0.4 s, setGeneric 0.2 s), at about 1.3x GNU R per
  call (`pkgload::load_all` 6.3 to 7.1 s, `library()` of an installed copy 0.6 s). Held back, the
  load takes about 0.27 s; first S4 use costs the full eager run on a miss (see the metadata cache
  below for a hit).
- **The S4 metadata cache** (`DeferredS4::cache_key`, `s4_cache_path` in `loader.rs`;
  `.ry_restore_s4`, `.ry_save_s4` in `runtime.R`). Previously every process ran the held-back
  definitions through the `methods` package (about 330 ms on the bench package, 1.7x GNU R, the gap
  being interpreter speed in `slot<-`, `list()` and `possibleExtends`). A full force now restores the
  bindings the unit made (`.__C__*`, `.__T__*`, generics, generators) from the cache and caches
  their metadata as `cacheMetaData` would, which is how GNU R loads an installed package's S4
  objects, so the restored state is the same one an installed package would have. That is valid only
  when the bindings are a pure function of the definitions' text: the key is set only when every
  definition reads its own names or `PURE_S4_READS` (constructors and literals), and it covers the
  definitions' text, package name and version, and the build id; the R version and imported packages'
  versions are checked at restore. A run that signalled a message or warning is not saved, so a
  diagnostic is never silenced. A force during the load never uses the cache, because only a full
  force produces the whole unit's bindings. A miss still costs the full run.
- **The S4 cache is a lazy-load database, and a restore replaces `cacheMetaData`**
  (`.ry_save_s4`, `.ry_restore_s4`, `.ry_cache_restored_s4`, `.ry_s4_metadata` in `runtime.R`).
  Previously the cache was one RDS file that a restore read whole (`readRDS` 170 to 370 ms for
  2,900 definitions, 8 MB unserialized) and then ran `methods::cacheMetaData` (about 190 ms, which
  touches every class and generic), so the first S4 use on a hit cost 380 to 460 ms. Now:
  - The save runs after `cacheMetaData`, so the package's own generics are stored with their
    method tables already merged, and writes `tools:::makeLazyLoadDB` into a fresh directory that
    replaces the old one. A lazy-load database keeps environments shared between definitions (a
    generic and its `.__T__` table), which per-value serialization would split. The restore binds
    promises with `makeLazy` and checks the key first; `lazyLoadDBfetch` reads the whole file into
    memory on the first fetch, so a later replacement cannot tear a session's promises.
  - Instead of `cacheMetaData`, the restore puts the package's classes and plain generics into
    `methods:::.classTable` and `.genericTable` as bindings that share the namespace's promises
    (`importIntoEnv`), so no definition is read until something looks it up. This is exact because
    the save records, per class and generic, whether `cacheMetaData` would do anything else: a
    union's own members must already contain it (then `.recacheSubclasses` is a no-op for them,
    and only foreign members such as `NULL` are recached, in order), and a plain generic is the
    package's own non-group, non-primitive generic, for which `cacheMetaData` only merges tables
    the save already merged. Anything else (a class name already in the table, a non-plain class)
    runs the full class loop in `cacheMetaData`'s order, since then the order of updates decides
    the table's contents; other packages' generics (`show`, `initialize`, `coerce`, primitives)
    always run `cacheMetaData`'s per-generic steps (`.ry_cache_generic`). Packages with load actions
    or `.requireCachedGenerics` are not saved, since a restore would not repeat what they do.
  - Measured on the generated S4 package (2,877 definitions, Imports lubridate, data.table, xts):
    first `new()` on a hit fell from 380 to 460 ms to about 30 to 60 ms; the save on a miss costs about
    0.4 s.
- **The load-index cache** (`index_cache`, `read_index_cache`, `write_index_cache` in `loader.rs`).
  Previously every cold start parsed all files and derived their facts (about 280 ms on 4 cores).
  The whole `PackageIndex` is now cached per package root, keyed by an xxh3-128 over the build id
  and every input (strictness, DESCRIPTION, NAMESPACE, each path and text). The cache is package-level,
  not per file, because the per-file salsa queries all chain off the parse and cannot be fed a stored
  result without re-parsing; a hit therefore skips salsa analysis entirely and the runtime parses
  each item's slice of text when it is first forced (`Source::Text`). The build id is a hash of the
  analysis crates' sources from `crates/loader/build.rs`, so any change to the facts' derivation
  invalidates old entries without a hand-maintained version number. A hit warms the salsa parse on a
  detached, cancellable db clone so the first reload does not pay for it.

## Runtime semantics settled by the corpus package tests

- **A loop accepts a jump from another call only when GNU R's compiler would give it a context**
  (`eval.rs`): its body calls `eval`, `evalq` or `source`, or writes `break` or `next` inside an
  ordinary call. Previously any `break` evaluated in the loop's environment ended it, so
  `h <- function() eval(quote(break), parent.frame())` broke a caller's loop where GNU R reports
  "no loop for break/next". Each loop records its call depth and whether it has a context, and
  compiled closures apply the same test. Pinned by `base__loop_jumps_from_other_calls`.
- **One implementation per attribute setter.** libr's `Rf_dimnamesgets` and the `row.names` arm of
  `Rf_setAttrib` call the interpreter's `set_dimnames` and `set_row_names` (`subset.rs`). The libr
  copies had drifted: libr's `row_names_gets` missed the double `c(NA, -n)` form, so readr's
  chunked reader saw two-row frames forever.
- **A user interrupt is a condition first.** `Rf_onintr`, `R_CheckUserInterrupt` and the
  evaluator's interrupt poll call `conditions::signal_interrupt`, which offers an
  `interrupt` condition to calling and exiting handlers and unwinds to top level only when none
  takes it, as GNU R's `onintr` does. Previously `Unwind::Interrupt` bypassed the handler stack,
  so `tryCatch(rlang::interrupt(), interrupt = )` halted the script (later's tests).
- **A pending interrupt waits for the next interrupt check** (`interp::unwind`).
  - *Previous shape.* `R_interrupts_pending` and `R_interrupts_suspended` were libr globals the
    interpreter never read, `.Internal(interruptsSuspended())` always answered `FALSE` and set
    nothing, and `INTERRUPT_REQUESTED` alone recorded a Ctrl-C, which the very next call raised.
    So `suspendInterrupts(rlang::interrupt())` fired as soon as `rlang::interrupt()` ran.
  - *Shape.* The interpreter owns both globals, exported under GNU R's names
    (`INTERRUPTS_PENDING`, `INTERRUPTS_SUSPENDED`), so C code, `suspendInterrupts()` and the
    evaluator share one record. `INTERRUPT_REQUESTED` only says "attend at the next poll", and stays
    raised while an interrupt is pending. The evaluator's poll (calls, loop iterations, compiled back
    edges through deoptimization) delivers a pending interrupt on every 1000th poll, as GNU R's
    `eval` checks every 1000 evaluations; `R_CheckUserInterrupt` (`check_user_interrupt`) delivers
    at once unless suspended; `Sys.sleep()` and socket waits deliver even while suspended, because
    GNU R waits in `R_SelectEx`, which lifts the suspension. Leaving `suspendInterrupts()` delivers
    nothing by itself. A C context saves and restores the suspension, as GNU R's contexts do, and
    `ry run` installs the SIGINT handler, as `Rscript` does.
  - *Known gap.* A host that sets `R_interrupts_pending` directly, without the signal handler, is
    seen only by `R_CheckUserInterrupt`, not by the evaluator's poll, which reads the attention flag.
  - Pinned by `crates/libr/tests/test_interrupts.rs`, which runs in its own process because it loads
    base R, and the oracle cases `conditions__suspended_interrupts` and
    `conditions__interrupt_handlers`.
- **Long integer sequences are compact; short ones are plain vectors.** `:` and `seq_len` build
  sequences of 2^20 or more elements through the `compact_intseq` host hook, which libr answers
  with its `compact_intseq` ALTREP class (the interpreter cannot name libr, so a host without
  ALTREP gets `None` and allocates). Previously every sequence was expanded, so vctrs's
  `seq_len(2^31 - 1)`, built only to read its length, allocated 8 GB and the parallel package run
  was killed for memory. Short sequences stay plain because reading one through ALTREP dispatch
  costs more than filling it once, and the evaluator's and the JIT's fast paths read plain
  integer data directly. Pinned by `base__long_sequences`.

## utils' sockets, internal downloads and allocation profiling

- `make.socket` and friends keep their `TcpStream`s and `TcpListener`s in one locked map in
  `crates/interp/src/utils.rs`, keyed by the OS handle, because that handle is the `socket`
  number R code sees and passes back.
- `download.file(method = "internal")` and `url(method = "internal")` read only `file://`
  URLs and give GNU R's "defunct" or "not supported" errors for the rest, as R 4.2 and later do.
- `Rprofmem` logs a vector only when its data exceeds 128 bytes, sized as GNU R counts it (data
  rounded up to 8 bytes plus a 48-byte header), because GNU R takes smaller vectors from pages of
  small cells. ry has no such pages, so it writes no `new page:` lines. The heap calls a plain
  `fn(usize) -> bool` reporter (`Heap::report_allocations`), which costs one compare per vector
  while no profile runs. The reporter returns false to end the profile (a failed write), since
  it runs while the heap is mutably borrowed and must not reach the heap itself. The bytecode compiler hides its own vectors with `Heap::swap_report_threshold`.
- `Rprof` raises intervals below 10 ms to 10 ms with GNU R's warning, since GNU R's timer cannot
  sample faster.
- `system(intern = FALSE)` and `system2` return invisibly, as in GNU R.

## One session temporary directory, removed at exit

- **Previous shape.** The interpreter's `tempdir()` made `RtmpRy<pid>` and libR's front end
  made a second `Rtmp<pid>` for native code's `R_TempDir`. Neither was ever removed, so every
  run leaked two directories into `/tmp`.
- **Target shape.** `interp::system::session_temp_dir` is the only one: a `mkdtemp`
  `RtmpXXXXXX` under the first writable `R_TMPDIR`, `TMPDIR`, `TMP` or `TEMP`, as in GNU R.
  libR passes it to native code. `session::finish` removes it, but only in the process that
  created it, so a forked `mcparallel` child that ends never deletes its parent's directory.
- **Order at exit.** `session::finish` runs the exit finalizers (the `run_exit_finalizers`
  hook, so `quit()` runs them too) and only then removes the directory, because a finalizer
  may still write there. The embedding exits (`R_CleanUp`, `Rf_endEmbeddedR`) remove it
  through `R_CleanTempDir` after their finalizers.
- **`tempdir(check = TRUE)` recreates the same path**, not a fresh name as GNU R does,
  because native code already copied it as `R_TempDir`. It is created private, and an
  existing directory at that path must belong to the current user.

## Base R kernels: radix sorting, gather subsetting, x87 accumulation and shortest-digit formatting

- **Previous shape:** sort and order compared elements one pair at a time, subsetting went through a per-element generic path, `rowSums`, `rowMeans`, `cumsum` and the moments accumulated in a software long double model, and doubles turned into text through the general formatter, which searched for the digit count by trial formatting.
- **Target shape:** `sort.rs` sorts integers, doubles and strings with radix and counting sorts (strings by their interned order), `subset.rs` gathers plain vectors in one typed loop, and `long_double.rs` is the one home for GNU R's long double accumulation, run on x87 instructions where the target has them: `add_all`, `add_differences` and `add_products` for sums and moments, block row sums for `rowSums` and `rowMeans`, and `running_sums` for `cumsum`. cov and cor round each column mean to double before the products, as GNU R does. `format.rs` builds a double's text straight from Rust's shortest round-trip digits when the value is normal and needs at most 15 significant digits; past 15 digits, or when zero padding would pass 15 integer digits, GNU R prints exact digits that the shortest form does not have, so those values take the general path.
- **Strings stored at once skip the root handle.** `values::mkchar` pushes a root handle that lives until the builtin returns, so a kernel that builds a long string result held one handle per element (8 MB at 1e6 rows, 16 MB while the handle vector doubled). `values::set_mkchar` and `set_mkchar_bytes` intern and store into a vector the caller keeps rooted, with no handle; use them in any per-element loop. `Heap::mk_char` reads a string's bytes once, a word at a time, for the NUL test, the ASCII test and the intern hash together (`strings::scan`), and compares a candidate of 8 to 16 bytes with two word loads instead of a `memcmp` call; allocating a new string is a separate cold function, so the hit path keeps its registers. Re-interning an 8-byte string that is already interned costs about 126 instructions, down from about 167, and the time per hit (about 30 ns with 200,000 strings) is dominated by the two cache misses on the table and the candidate. `order` computes each radix key per pass from the column rather than storing every key (`SortKey::get`), so two keys at 1e6 rows hold one `u64` per row of scratch instead of all of them, and the result is written straight into an uninitialized integer vector: the benchmark's peak went from 206 MB to 171 MB at the same speed.
- **Impact:** results match GNU R to the last digit (the `stats__long_double_moments`, `vectors__radix_sort_and_gather`, `strings__double_to_text` and `arith__cumulative` oracle cases pin them), and the sorts, subsetting, `rowSums`, `cumsum` and `as.character(double)` run faster than GNU R. No heap layout changed: every kernel reads and writes existing vectors.

## Base R kernels: position tables, a most significant digit radix order, and a per-call string memo

- **Previous shape:** `match`, `duplicated` and `unique` hashed elements into an `FxHashMap` keyed by value, `order` with several keys sorted rows with a comparison sort over all keys, every radix key was a `u64` in 16-byte `(key, row)` pairs, a dense key always took a counting sort however wide its range, and string kernels interned each piece through the heap's session-wide string table.
- **Target shape:** `sort.rs` keeps one `PositionTable` (open addressing over 1-based `u32` positions, keys left in their vector, slot from the hash's high bits) for `match` and `duplicated`, and an integer range at most about four slots per element uses a direct-address array instead (`DenseRange`). `radix_order` sorts by the first key, then sorts each run of ties by the rest, so a later key is only sorted inside a run that sits in cache. One key sorts by a fused min, max and sortedness pass, then a counting sort when the range has fewer than about a million buckets, a pair sort under 1024 rows, and otherwise an MSD radix sort with 11-bit digits that ping-pongs between two buffers. Keys that fit 32 bits (integers, factor codes, string ranks) sort as `u32` with 8-byte pairs; only doubles need `u64`. `values::StringMemo` fronts the heap's string table for one vectorized call (`substr`, `formatC`), and switches itself off after a window of 65,536 lookups with fewer than one hit in eight. `formatC(format = "f")` formats a value whose scaled magnitude is below 2^52 with `fixed_fast` in `format.rs`: an `fma` gives the exact error of `value * 10^digits`, so the half-to-even rounding glibc applies is decided exactly, and only larger values take Rust's arbitrary-precision formatter (about 1,100 instructions per value). That took the kernel from 6.4 s to 2.7 s at 1e7 rows (GNU R 5.6 s); a unit test cross-checks it against the exact formatter.
- **Why the memo is per call:** at 1e7 strings the heap's table misses cache on every probe, while the distinct pieces one call produces (a code inside ids, a rounded number) fit in cache. `substr` at 1e7 fell from 2.3 s to 1.0 s (GNU R 1.8 s). A faster session-wide table would make the memo unnecessary; that is the heap's to decide.
- **Why the counting sort stops at a million buckets:** past that, its count and scatter passes miss cache on every row, and on a 1e7 permutation the radix sort's sequential scatters win (0.34 s against 0.43 s).
- **Impact:** the `vectors__radix_paths` oracle case pins the radix and match paths, including integer `NA` at either end in either direction. No heap layout changed.

## The language server reads the check record

- **Previous shape.** The server started from nothing. Until the startup warm-up ended, opening a file
  showed only its parse-stage findings, so a large project looked clean for seconds.
- **Target shape.** At initialize the worker loads `.ry_cache/check.json` and keys it with
  `check_cache::key`, the one function `ry check` uses too. The key canonicalizes the root and the
  configuration's directory, because `ry check .` and the server spell them differently. The worker
  then runs `check_cache::plan` over `collect_r_files`, hashing loaded files from the database and
  reading the others from disk, and restores the findings of every loaded file the plan reuses
  (`Worker::restore_recorded_diagnostics`). `fast_diagnostics` serves them until the settled wave
  replaces them. Any input change clears them all, because one file's change can change another's
  findings.
- **Why not per-file invalidation.** Partial invalidation would need the dependency closure at every
  edit. The record is only a cold-start aid: after the first edit the live analysis is warm anyway.
- **Known difference.** The server analyses the package files and the open documents while `ry check`
  analyses every file, so a finding that depends on a script can differ until the settled wave.
- **Identity.** A record is keyed by the binary that wrote it. `WorkerSeed::executable` is
  `current_exe` in production; `Simulation::start_as` passes the scenario's `ry` binary, since the
  simulated server runs inside the test binary. `ClientOptions::cold` starts a simulated session
  before the warm-up, and the seeded sessions draw it.

## Compiled function lookups also check the activation's own frame

- **Previous shape.** A compiled body's speculations resolve a called name from the closure's
  enclosure, because the compiler proves the body never binds the name itself. Validation
  (`runtime::valid`) re-resolved from the same place whenever the binding epoch moved.
- **What was weak.** R can bind a name in a function frame without any assignment in the body:
  `assign` into `parent.frame()`, a default argument, or the locals `UseMethod` copies into the
  method's frame. The epoch moved, but the re-resolution skipped the frame, so the cached global
  function kept being called (oracle case `environments__hidden_function_bindings`).
- **Target shape.** `valid` takes the activation's frame and refuses when a speculation that starts
  at the body's enclosure finds a binding in that frame that may hold a function. Defining a
  watched name in a function frame already moves the epoch, so the fast path is unchanged. A loop
  entry, and the first run after a compile, walk the speculations regardless of the epoch, because
  that frame may have bound the name before the compiler watched it.
- **Impact.** Correct shadowing at no cost on the hot path; pinned by the oracle case and
  `run_time_binding_shadows_cached_global_function` in `crates/jit/tests/test_tiers.rs`.

## Literate documents keep their offsets, and the server keeps their original text

- **Previous shape.** `check` read `.Rmd`/`.qmd`/`.Rnw` through `syntax::literate`, which blanks prose to spaces byte for byte, but the server and `ry fmt` skipped them.
- **Analysis.** The engine still sees only the blanked text, so every byte offset is the document's own and no range table exists to drift. The chunks of one document form one script.
- **Positions.** The server keeps the original text per literate file (`Worker::literate_documents`) and converts LSP positions against it, because a blanked multibyte character has a different UTF-16 width. Each `did_change` applies the edits to the original and converts the whole buffer again; conversion is a linear scan, so per-edit conversion would add a second source of truth for no measurable gain.
- **Formatting** is per chunk (`format::format_chunks`), returning one edit per changed chunk, so prose, fences and inline code are never rewritten. An indented chunk (inside a list item) is skipped, because re-indenting it would need the list's indentation. Range formatting is off for literate files.
- **Completion** answers only inside an R region (`syntax::literate::r_regions`), so typing prose shows nothing. Code lenses other than reference counts are dropped, because "Run file" and "Run test" mean nothing for a report.
- **VS Code** matches literate files by glob, because their language id depends on which extension is installed.

## A running console is a debug adapter, a data source, and a live-value source on one socket

- **Previous shape.** The debugger existed only as `ry debug-adapter`, which launches a fresh script; a `ry run` console had no editor connection, so `debug()`, `View()` and the editor's hover knew nothing of the session.
- **Transport.** `ry run` listens on a Unix socket in `$TMPDIR/ry-<uid>` (0700, owner and mode verified by both ends) and writes `console-<pid>.json` naming its cwd and socket. Discovery is a directory read, so nothing extra runs at start-up beyond one accept thread; a reader thread per connection queues events, and the interpreter thread answers them (`interp::console_server`). Framing is DAP's `Content-Length`, so one parser serves the debugger, the viewer and the language server.
- **When requests are answered.** At the prompt, input is read on a persistent input thread (`console_server::read_line`) while the interpreter thread answers every request. The thread owns the console's reader and reads one line per prompt that asks; a line stays owed until the interpreter takes it, so a prompt whose wait ended early (an editor took a `Browse[1]>` stop over) hands its pending line to the next prompt instead of reading ahead; that next prompt is then written to the terminal, since the thread is still showing the old one. A line typed while a command runs goes to `STASHED_INPUT` and the queue keeps draining, so an editor's `pause` behind it is still answered. `browser()` inside R code run for an editor's request at the idle prompt does not stop (`SERVING_REQUEST`), because the top-level read owns the terminal. An editor that attaches while a Browse stop's expression runs keeps its adapter: `with_adapter` drops the finished stop's adapter instead of writing it back. A per-prompt scoped helper thread could not do that, because the scope had to join before the prompt returned. Every request queue is bounded at 64 queued requests per connection: the socket thread blocks, so a flooding client stalls on its own socket buffer and nothing is dropped.
- **Viewer sort and filter are row numbers, not copies.** A `ryTable` request carries `filter` and `sort`; the view caches the zero-based row numbers they leave (`viewer::Order`) and pages through them, recomputing only when the pair changes. A ry `DataFrame` computes them in the table engine (`table::ops::view_rows`, which reuses `sort_frame`'s radix path on a two-column frame of the key and the row numbers) and gathers a page with `take_rows`; an R object computes them with `grepl()` and `order(na.last = FALSE)` in a base-scoped environment. Both put `NA` first in either direction, keep ties in row order, and let `NA` match no search, so the two engines show the same rows.
- **`browser()` with no editor drives the same adapter from the terminal.** An interactive console installs `Adapter::for_console` over `console_server::Browse`, a transport that turns GNU R's browser commands into DAP requests (`c` is a disconnect, `n`/`s`/`f` are next/stepIn/stepOut, `Q` is terminate, other lines are an `evaluate` with context `ryBrowse`, which autoprints) and renders responses as text. Stepping rules therefore exist once. An editor `initialize` arriving at the prompt replaces the terminal inside the transport, and `configurationDone` re-sends the `stopped` event. The adapter ends with its command while the terminal drives it (`Adapter::is_browse`), but stays attached once an editor took over. A forced `browser()` stop needs no srcref, and only error stops hide sourceless frames. Scripts (`ry run file.R`) install no reader, so `browser()` stays a no-op there, as in non-interactive GNU R. While a command runs, `poll_interrupt` services the queue: debugger requests are answered (pause, breakpoints), anything that would evaluate R is refused as busy, because it would interleave with the user's code. While stopped, the debugger's own receive loop answers other connections inline.
- **Attach** installs the same `debugger::Adapter` the launch path uses, with `console = true` so disconnect detaches instead of exiting, and reloads the workspace packages with srcrefs so package breakpoints hit. Only one debugger connection at a time.
- **`debug`/`debugonce`** set closure gp bits and enter `Mode::FunctionEntry`, stopping with reason "function breakpoint". Code typed at the prompt has no file, so its frames get a `sourceReference` the client fetches with `source`.
- **Data viewer.** `ryView` evaluates an expression (in a frame when stopped, else in the global env) and registers a view that preserves the object; `ryTable` returns one page of formatted rows, at most 1000; `ryRelease` drops it, and at most 32 views live at once. The data never leaves the runtime whole, so a million-row table costs one page. Variables carry `evaluateName` so the editor's "View Data" names the exact value. `View(x)` sends a `ryView` event to connections that sent `rySubscribe`; VS Code subscribes when it opens or attaches to a console.
- **Live values in the language server** come from `ryGlobals` (names, class, summary, columns, promises unforced) with a 150 ms timeout (`ry::console_link`). Analysis stays the source of truth: the console only adds hover lines and completion items analysis lacks, and the simulator disables the link to stay deterministic.

## A parameter and its named arguments are one editor binding

- Before, a named argument (`f(rate = 1)`) was no navigation target: hover widened to the whole
  call, goto and references found nothing, and renaming the parameter left every call site's
  `rate =` behind, so the rename broke the code.
- Now `ide::target_at` maps a named argument to the parameter's `Target::Slot` in the callee's
  item (the callee resolves the way any name read does, `expression_target`), and the occurrences
  of a parameter slot add the named arguments at every call of its function. Those calls come
  from the function binding's own occurrences (the global walk for a top-level function, the slot
  walk for a local one), so a different function with a parameter of the same name is untouched.
- Nothing new is stored: call sites are derived on request from the existing occurrence walks, so
  no project-wide map gains a dependency and incremental behavior is unchanged.
- A library callee has no parameter site, so its named argument goes to the stub's function and
  hover shows the signature-help label of the parameter. Partial matching is not followed, on
  purpose: it would make a rename guess.

## The cold check's per-call work: a bump arena, borrowed schemes, a token-only green builder

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

## The strict load check shares the check record

- **Previous shape.** `preflight::check_package` stored a clean verdict in `<cache dir>/ry/strict/<hash of the root>` under one fingerprint of every input, beside `ry check`'s own record in `.ry_cache/check.json`. Two mechanisms recorded the same fact, any edit invalidated the whole verdict, and a package that `ry check` or the editor had just checked still paid a full check on load.
- **Target shape.** The load check is `ry check`'s analysis (`cli::analyze_project`, walking the whole root) asked about `R/` alone, reading and writing the same record. A recorded file's `diagnostics` is `None` when the run that wrote it did not check that file, and `check_cache::plan` treats such a file as stale, so a later `ry check` still checks the tests a load skipped. Kind isolation makes the sharing sound: a name only a test file defines stays unresolved in `R/`, and a test's `library()` does not attach in `R/`, so the `R/` verdict is the same as from an `R/`-only walk.
- **A verdict-only pass was measured and rejected.** A lint may be configured as an error, and `is_strict_error` counts it, so lints cannot be skipped. What a verdict could skip (rendering, annotation agreement) is about 0.3 of 5.3 CPU-s on typed finbench.
- **Impact.** On typed finbench (4 cores, release) a load after `ry check` takes 0.55 to 0.64 s, the same as an untyped load, and an edit inside one body rechecks one file. A verdict-cold load costs 2.5 to 2.7 s, about 0.15 s more than the old `R/`-only walk because the record needs the whole root walked; the rest is analysis CPU (parse about 1 s, item HIR 0.9 s, folds 0.5 s, type check 2.2 s), which belongs to the type checker's own speed.

## The bench templates are inline-typed

- **Previous shape.** The templates carried `#:` comment annotations, and `generate.py` stripped them with a regex for untyped runs. Comment annotations never reach ry's runtime, so the typed benches could not show what types buy at run time, and records and tags (which have no R spelling) could not appear at all.
- **Target shape.** The templates use inline syntax. An untyped run erases each generated file with `ry debug erase` (`syntax::erase_inline_annotations`, the same erasure `ry R` runs) and then drops the `#: @type` lines the erasure leaves, so there is one source of truth for the program and the erasure is exercised on 180,000 lines per run. Where typed code models data better with a record or a tag union than with S4, R6 or a named list, the template keeps both versions in `#@ typed` / `#@ untyped` / `#@ end` blocks; the untyped version stays the plain R the dependency set expects, and `generate.py` counts lines on it so both runs instantiate the same templates.
- **Impact.** The typed and untyped programs differ only where a record or tag replaces an R object system; everything else is the same program. `return()` had to accept native values (it passes its argument through, like `invisible()`).

## `$` fields resolve through the values that built the receiver

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

- **Previous shape.** `ry check --no-cache` ran on the same database shape as the language server:
  every cross-item name lookup interned a `QueryName` and went through the tracked `package_name` or
  `test_name` firewall, and every item found its cyclic group with its own `interface_group` walk.
  That recorded about 340 k salsa edges for name lookups alone, which a check that never edits
  never replays. Separately, the token-only green builder had stopped sharing nodes, which cost
  about 70 MiB of peak memory.
- **Target shape.**
  - `Db::one_shot()` (true only for `RootDatabase::one_shot`, which `ry check` uses) switches
    `name_layer` to read `package_definitions` and the conditional-writer maps directly, and
    `item_group` to read `project_interface_groups`, one iterative Tarjan walk over every item.
    Every read still goes through salsa, so the answers are the same facts at a coarser
    dependency granularity. Under `enable_invariants()` both are compared against the per-name and
    per-item answers, and `test_incremental.rs` pins that the one-shot path runs no firewall
    query, reports the same findings, and that the incremental path keeps its firewalls.
  - `ry check` runs the group walk once on a throwaway database handle. It records one edge per
    item, salsa keeps that capacity in the handle's reused query-stack frame, and every later query
    at the same depth then cleared a table sized for the project (about 0.75 G instructions,
    depending on which thread ran the walk).
  - Node sharing in the tree builder is gone with rowan; the flat tree (the record "The syntax
    tree is ry's own flat preorder arena") stores each file once without it.
  - A one-shot check reads each name's project layer once per lookup: `NameLookup` resolves the
    project maps (`package_definitions` and the conditional-writer maps, or their test
    counterparts) once per query instead of two salsa fetches per name, and the incremental path
    keeps the `projected_name_layer` firewall, cross-checked under `enable_invariants()`.
  - On a one-shot check an unlisted frame binder (a name the item interface did not list, looked up
    in an enclosing function's frame) is answered by building `FrameBinders::of` once per item
    instead of one `frame_binder` query execution per name, which recorded about 216 k query
    executions on the finance bench.
  - The global allocator enters mimalloc through `mi_malloc` and friends when the alignment is at
    most 16 and no larger than the size; the `mimalloc` crate always took the aligned path.
- **Impact.** Callgrind on the finance bench went from 22.8 G to 22.0 G instructions, and
  interleaved medians from 1.60 s to 1.48 s wall, 5.2 s to 4.8 s of CPU and 445 to 431 MiB, with
  byte-identical findings. The 1 s target is not met (`backlog.md`, "Check cache follow-ups").

## R's dynamic help server runs on the interpreter thread, only while R waits

- **Previous shape.** `startHTTPD` failed on purpose, so `help(help_type = "html")`, `help.start()` and `tools::startDynamicHelp()` reported that the help server was not implemented.
- **Shape.** `interp::help_server` binds the listener and runs an accept thread plus one thread per connection. Those threads only parse HTTP and queue an exchange; the R handler (`tools:::httpd`, or `tools:::.httpd.handlers.env[[name]]` for `/custom/<name>`) runs on the interpreter thread, which answers the queue and hands the response back over a channel. No R object crosses a thread. The handler runs as under GNU R's `R_ToplevelExec`, with none of the waiting code's condition handlers or restarts in reach (they are parked in `Interp::suspended_handlers`, where the collector sees them), and a Ctrl-C during it reaches the waiting code once the response is sent. `stopHTTPD` shuts every open connection down, and the server bounds request heads (1 MiB) and bodies (64 MiB) and closes idle connections.
- **When requests are answered** follows GNU R, which serves help only from its event loop: at the console prompt (the `console_server` idle loop, woken through `console_server::wake`), in `Sys.sleep`, in `process.events`, and while an HTTP download runs on its transfer thread, so a session can fetch its own help pages through `url()` or `download.file`. A running command never interleaves with a handler. Off Unix the console idle loop does not serve yet.
- **Wire format.** Request decoding (path percent-decoding that stops at the first malformed escape, the query vector, the raw body with its `content-type`, the `Request-Method:` header block) and the response rules (error 500 forms, status reason "Code N", HEAD, HTTP version echo, `startHTTPD` return codes 0, -1 and -2) were probed against GNU R and are pinned by `crates/oracle/cases/tools__help_server.R`.
- **Impact.** The handler runs exactly as on GNU R, so every Rd page renders through R's own `tools` code; a sweep of every Rd file in 14 default packages through `Rd2txt`, `Rd2HTML`, `Rd2latex` and `Rd2ex` matches GNU R byte for byte apart from the render timestamps.

## Inferred signatures carry body facts that a call reads

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

## `ry test` is `ry load` plus testthat, with affected tests first

- **Previous shape.** `load_all()` loaded a package, but running its tests meant `devtools::test()` under GNU R, which reloads the whole package (about a minute on finbench) before every run.
- **Shape.** `ry test` is `ry load` running an R driver (`crates/ry/src/test.R`) that calls `testthat::test_dir(load_package = "none", package =)` against the namespace `load_all()` built, so testthat's own reporters print the output and helpers, setup files and the package environment behave as under `devtools::test()`. Nothing in the reporter path is reimplemented, which keeps the output identical for CI.
- **Watch.** `--watch` waits on operating-system notifications for `R/`, `data/`, `src/`, `inst/`, `tests/`, `DESCRIPTION` and `NAMESPACE` (`loader::watch`, `Loader::wait_edits`; edits under `_snaps/` are dropped because testthat writes there during a run), reloads with `load_all()`, and reads the reload's touched names from `.ry_packages[[root]]$changed` (`NULL` after a full load or one that ran code). `Loader::callers` closes those names over the index's reverse mentions and reads, and a test file is affected when it changed or its `all.names()` meet that closure. Affected files run first, then the rest; a changed helper or setup file, or a `NULL` change set, reruns everything.
- **Why not a test index in the loader.** Test files are parsed with `all.names()` and cached by stamp; that is a few milliseconds per file and keeps the loader ignorant of testthat.

## Source edits arrive by OS notification, ordered by a barrier file

- **Previous shape.** The console compared a fingerprint of names, sizes and mtimes of `R/`, `data/` and the metadata before every command, and `ry test --watch` polled stamps every 0.2 s. Neither saw `src/` or `inst/`.
- **Shape.** One `notify` watcher per loaded package (`crates/loader/src/watch.rs`) files relevant paths into an inbox. `SourceWatcher::take` writes a barrier file into a private watched directory and waits for its event; since one watcher delivers events in order, every edit made before the call is then in the inbox, so a reload depends on the order of writes and calls, not on thread timing, and the reload fuzz test stays deterministic. The fingerprint stays as the fallback when notifications are unavailable or lost.
- **Compiled code.** A `src/` edit makes the console and `--watch` reload, and `load_all()` rebuilds. On ry's runtime `dyn.unload` retires the library record into `unloaded_dlls` (kept mapped, so a stale pointer cannot crash), and the next `dyn.load` of the same path maps a fresh copy from a temp directory, because `dlopen` returns the already open handle for a path it has seen.

## `ry document` is roxygen2 with block and topic caches and linear topic ordering

- **Previous shape.** Documentation meant `devtools::document()`, which reloads the package and reparses every block; on a 22,000-topic package roxygen2's `RoxyTopics$topo_order` (run 15 times by `@inherit` processing) and `RoxyTopics$add` are quadratic and dominate.
- **Shape.** `crates/ry/src/document.R` calls `roxygen2::roxygenise(load_code = )` with ry's `load_all()`, so output is roxygen2's own, byte for byte. For the run it swaps `roxygen2:::parse_package` for a version that caches each file's tokenized blocks by md5 (keyed by roxygen2 version, R version and roxygen options; `package_cache_path(root, "roxygen")`), and replaces roxygen2's quadratic steps with linear equivalents (`roclet_process.roclet_rd`, whose topic list grows by copying, and `RoxyTopics$topo_apply`, `topo_order` and `drop_invalid`, which find topics by scanning names), each only when the installed function's deparsed body equals the embedded original, so another roxygen2 version runs unpatched. Everything is restored on exit. The session runs with `keep.source = TRUE` because roxygen2 reads R6 methods' srcrefs.
- **Topic cache.** A one-file edit on finbench (22,000 topics) still took about 2 minutes in roxygen2's per-topic work, so `<cache>-topics` keeps each topic's finished Rd text, aliases, family and `@inherit*` sources, plus each block's namespace directives. A block is *clean* when its file's md5 is unchanged and it is trackable: a `name <- function` block whose loaded object's srcref names the same file and line (`defined_here`), with no dot in the name (an S3 method's dispatch is not tracked) and none of the `uncacheable_tags` (`@eval`, `@template`, `@example`, `@describeIn`, ...). A clean block skips `block_find_object`. The rebuilt set is every topic touching a dirty or untrackable block, plus the package topic, closed under reverse `@inherit*` edges and family co-membership; the blocks needed to rebuild it add the forward closure. Every other topic is emitted from the cache through a stub that `roclet_output`'s `write_if_different` skips when the file's size and mtime match the record. `namespace_imports` is cached per file md5 (`<cache>-imports`). On finbench: 385 s cold, about 11 s for a no-op rerun, 20 s for a one-file edit (50 topics rebuilt), byte-identical to `devtools::document()`. Most of the warm time is `readRDS` of the 29 MB compressed block cache (about 3 s); uncompressed it is 214 MB, so it stays compressed.
- **Why the dependency tracking is in R and conservative.** roxygen2's `@inherit*`, `@family`, `@describeIn` and `@rdname` merging make one block's Rd depend on others. Rather than reimplement that, the cache only decides *which* topics roxygen2 rebuilds, over-approximating with closures; anything it cannot attribute to a file (generated blocks, S4, R6, data) is always rebuilt.
- **Known staleness.** A cached block keeps markdown links resolved against installed packages at parse time, and a cached topic keeps documentation inherited from another package.

## A primitive's S4 cache lookup matches arguments by name first

- **Previous shape:** `s4_dispatch` (`crates/interp/src/dispatch.rs`) built the dispatch label from the primitive's supplied arguments in order, as GNU R's `R_quick_dispatch` does. `x[i, drop = FALSE]` then took the class of `drop` for `j`, keyed as `x#logical#logical#missing`, and ran the method cached for `x[i, j]`. Matrix's `indexing.R` hit this: after `D[TRUE, TRUE]`, `D[L, drop = FALSE]` failed with "logical subscript too long".
- **Why GNU R gets away with it:** the quick table GNU R consults for a primitive is often a stale environment that lacks the inherited entries (as for Matrix), so the lookup misses and full dispatch, which matches by name, runs. A small class defined at top level does show the quirk in GNU R (`a[TRUE, TRUE]` then `a[TRUE, drop = FALSE]` both run the `j = "logical"` method).
- **Chosen shape:** `methods::signature_arguments` matches the supplied arguments to the generic's formals (exact names, then positions up to `...`) and orders them by `.SigArgs` before the cache lookup. A partial or unknown name skips the cache and leaves matching to the generic. ry deliberately differs from GNU R's quirk here; an oracle case cannot pin it, the Matrix test suite does.

## Package-test compatibility details that look optional but are not

- **Warnings always go through `.signalSimpleWarning`.** GNU R's `vsignalWarning` calls `.signalSimpleWarning(msg, base::quote(call))` whenever that function is bound, even with no handler established, and rlang reads the frames it leaves. ry used to call it only when a handler existed (`crates/interp/src/conditions.rs`, `warning_call`).
- **Compiled closure calls pass constant arguments as values.** GNU R's byte code pushes a constant argument with `PUSHCONSTARG`, without a promise, and rlang's `env_dot_type` tells the two apart. `compiler::cmpfun` compiles at once, as GNU R's does (`crates/interp/src/bytecode.rs`, `bcClose`; `eval::compiled_args_into`).
- **`dyn.load` refuses GNU R's `tcltk.so`.** GNU R builds its base packages' shared objects with R's internals, so they read `SEXPREC` fields directly and crash on ry's heap layout; the other base packages' code is built into libR (`builtin_libraries.rs`), and tcltk is refused with "Tcl/Tk support is not available on this system", which sessioninfo's `external_info()` catches. `capabilities("tcltk")` is `FALSE` to match.
- **Parse errors use GNU R's exact text.** testthat and other packages match on it ("unexpected symbol", the quoted line). The message names the token GNU R's LALR parser stops on (`SyntaxError::found`) with bison's token names, the location is that token's 1-based column with tabs to multiples of 8 (end of input is column 0 on the line after the last), and the context is the last 255 bytes read, split into lines, the last two quoted with a caret. `Rscript` and the console quote only the failing top-level expression, `Error: ... in "..."`, and nothing at the end of input (`lower.rs`, `grammar_error_report`; oracle case `parse__error_messages`).
- **`list.dirs` names the directory itself only when `recursive = TRUE`.** testthat reads snapshot variants with `list.dirs(recursive = FALSE, full.names = FALSE)`, and a stray `""` entry became a phantom variant.
- **Base binds the missing argument in the C API.** `R_MissingArg` is a symbol whose base value is itself, so `R_existsVarInFrame`, `R_GetBindingType`, `R_BindingIsActive` and `R_BindingIsLocked` answer for it in the base frames instead of erroring (`crates/libr/src/environments.rs`, `base_binds_missing_arg`).
- **A compiled `local(expr)` is `(function() expr)()` with a four-element `function` call**, whose srcref slot is NULL, as GNU R's compiler writes it. rlang's `trace_back` writes into that slot of every frame call.
- **`stdout()` is the current sink target.** GNU R's `stdout()` returns `R_OutputCon`, so a call inside `sink()` names the sink, while a `stdout()` kept from before writes past it to the console (connection 1 is the console itself, never redirected). vctrs' reporter tests broke on a stray `.` the reporter wrote through a saved `stdout()` (`crates/interp/src/connections.rs`, `stdout`, `device_write`, `print`).
- **A connection's `conn_id` address is never compared.** GNU R only checks that the external pointer is not NULL (closing clears it); curl stores its own handle there, so comparing addresses made `isOpen` and `close` reject curl connections (`connections.rs`, `resolve_number`).
- **TRE's lazy quantifiers with capture groups take the leftmost-first match.** Without groups TRE returns the shortest overall end; with groups its tagged matcher follows the leftmost-first rule (`x(.+?)(a+)` on `xaaab` is `xaaa`, `x.+?a+` is `xaa`). openssl's PEM splitting depends on it (`crates/interp/src/strings.rs`, `Matcher::find_at`). `\Q...\E` quotes a literal run.
- **`attach(env)` attaches a copy.** GNU R copies the bindings into a fresh environment and leaves the original's enclosure alone; attaching the environment itself cut shiny's namespace off from its imports (`crates/interp/src/envir.rs`, `do_attach`).
- **A user interrupt is signalled as a condition first.** `Rf_onintr` and `R_CheckUserInterrupt` run the handler stack for class `c("interrupt", "condition")`: calling handlers run, an exiting handler (`tryCatch(interrupt = )`) catches it, and only an unhandled interrupt unwinds to top level. later and rlang's `interrupt()` rely on it (`crates/interp/src/conditions.rs`, `signal_interrupt`).
- **`rmultinom` carries the remaining probability as a double-double.** GNU R uses a `long double`; in plain `f64` ten probabilities of 0.1 leave a conditional probability just above 0.5, `rbinom` then draws the complement, and the counts differ from GNU R's for the same seed. RcppArmadillo compares R's draws with its own (`crates/interp/src/distributions.rs`, `rmultinom`).
- **`[` subsets a parsed expression's `srcref` list.** plumber locates each endpoint's comment block with `attr(exprs[i], "srcref")` (`crates/interp/src/subset.rs`, `subset`).
- **`Rf_setAttrib(R_NilValue, name, R_NilValue)` succeeds.** GNU R checks for removal before it rejects a `NULL` target, and data.table's `C_unlock` runs on a `j` result that is often `NULL` (`crates/libr/src/attributes.rs`, `set_attrib`).
- **Integer sequences of at least 2^20 elements are compact ALTREP objects** (`colon`, `seq_len`, `seq_along` in `crates/interp/src/arith.rs`, through the `compact_intseq` interpreter hook). GNU R compacts every `1:n`; ry keeps short ones ordinary because the evaluator reads them without an indirection, and only long ones matter for memory. vctrs' tests build `seq_len(2^31 - 1)` twice and ran out of memory when it was materialized.
- **`row.names<-` stores a real compact form `c(NA, n)` as integers**, as GNU R's `row_names_gets` does. cpp11's `data_frame::nrow` reads only the integer form, and readr's chunked reader loops forever on the real one.
- **`R CMD INSTALL` of a directory leaves the shared objects in its `src/`.** The build runs in a copy outside the source (so a Cargo workspace never adopts it), but `pkgbuild::compile_dll` and `pkgload::load_all` expect GNU R's in-place result, so the objects are copied back (`crates/rcmd/src/rcmd.rs`, `install_source`).

## Records, tuples and tags compile natively

- **Previous shape.** `Op::Construct` was an opaque step in the JIT (a helper call that re-read registers through the activation), a guarded `match` fell back to the `match` special for the whole expression, and `l[[i]]` on a plain list returned a boxed element, so arithmetic on it ran in tier 0.
- **Construct.** `Action::Construct` writes `(kind, bits)` pairs into a stack buffer using `runtime::representation`'s encoding and calls `runtime::jit_construct`, which builds the slots and allocates through `Heap::alloc_record_slots`. Unboxed doubles, integers and logicals go straight into unboxed slots (`Slot::Real`, `Slot::Integer`, `Slot::Logical`); a double whose upper word collides with an integer or logical prefix stays a handle, and slots past `UNBOXED_SLOTS` are boxed in codegen.
- **Guarded match.** The guard compiles in place with the arm's bound names read from registers (`Compiler::bound`), and `Op::MatchCondition` applies the special's condition rules (a record gets the boundary error). Anything that would reach the frame by name evaluates in a scratch child frame holding the bindings, through `Op::BoundEval`: a call the compiler cannot inline (a closure's promises would read the frame), `[[`-style subject dispatch, and every runtime fallback. A guard that assigns, creates a closure, nests a `match`, leaves a loop or calls a bound name (`guard_compiles_in_place`) runs whole through `Op::BoundEval`, which keeps the special's semantics: an assignment in a guard never reaches the function's frame. The JIT turns `MatchCondition` on a logical into a move, so a guard of plain arithmetic and comparisons runs natively.
- **`MatchFail`** always signals an error, so the JIT treats it as `PureUnlessObserved` and leaves every register `Bottom` after it. As an opaque step that fell through with its registers' stale types, it widened the join after each `match` (a reused temporary still typed as the scrutinee's record) and left the loop's arithmetic in tier 0.
- **List element reads** speculate on the site's type feedback exactly like tuple items and record fields: a check on the loaded element hands off to tier 0 on a mismatch.
- **`List.range` and `List.repeat`** stop with an error past 2,147,483,647 elements (R's longest integer-indexable vector), so a typo cannot ask for an unbounded allocation.
- **Benchmark.** In `tools/bench`, `#@ typed` / `#@ untyped` variant blocks ("The bench templates are inline-typed") let the finance `ledger`, data-science `readings` and biostatistics `followup` templates use tags, tuples and records when typed and `list(kind=)` when untyped, and both draw the same instances.

## Completion, signature help and import fixes reach untyped and installed code

- **Installed formals are a callback.** `ide::completion_with` and `ide::signature_help_with` take `&mut dyn FnMut(&PackageTarget) -> Option<Vec<Formal>>`; the server and `analysis-stats` pass `InstalledPackages::formals`, which parses the definition out of the generated package source (`crates/ry/src/installed.rs`). Reading the R library is I/O and not salsa-tracked, so it stays out of `ide`. Untyped project functions and R6 methods (`fields::field_function`) read formals and default text from the HIR and syntax instead, and a function with a type or stub scheme keeps the typed signature.
- **Use counts are chunked.** Completion ranks by project use counts. One tracked query over all files re-summed every file per keystroke (p99 about 60 ms on finbench), so `chunk_uses` counts 32 files per tracked query and completion sums the chunks: an edit recomputes one chunk.
- **Import fixes read the export manifests, not `stubs()`.** `stubs()` skips conditional and qualified-only namespaces the project never uses, which are exactly the ones an "unresolved name" fix must suggest. `stubs::exporting_namespaces` indexes every manifest (`types/*.exports`) by name. The fix offers `pkg::name` and, outside a package, `library(pkg)` after the last top-level attach call.
- **Header inlay hints reuse the annotate edits.** A function literal's hints are the `inline_annotation_edits` the "Add inferred type annotation" action inserts, so the hint and the inserted text cannot disagree.

## References are checked against an independent resolver, and a replacement copy is its source

- **The oracle shares no code with the analysis.** `crates/ry/src/scope_oracle.rs` resolves names from the syntax tree with R's lexical rules (parameters, locals visible from the end of their assignment, enclosing functions seeing all their locals, `<<-` falling through to package level). A check against the analysis's own resolver would agree with every bug. Where scoping does not decide (formula, quoted code, `:=` target, an argument of a call with no visible function literal, a `<<-` target defined by nothing lexical such as R6's generator environment) the site is undecided and left out.
- **Each `for` loop variable is its own binding.** ry links a loop variable to its loop, an assignment after a loop joins that loop's binding, and a read links to the nearest preceding definition, because each loop overwrites the value. The oracle models the same split; reassignments with no loop between them stay one binding.
- **A replacement assignment's local copy refers to its source.** Previous shape: `the$x <- v` inside a function minted a fresh local slot (R semantics: `the <- \`$<-\`(the, ...)`), and the editor showed it as an unrelated variable, so renaming the global `the` left the function's later uses pointing at nothing. Naming now records `ItemNaming::replacement_copies` (slot to `CopySource`: an enclosing slot or a non-local name), and `ide::target_at`, hover's definition and `occurrences` follow `copy_origin`. The checker still types the copy as its own variable, so typing and narrowing are unchanged.
- **Refused renames are not misses to fix.** A parameter of a function stored in a field whose reads ry cannot all trace (`generator_funs$set`, an R6 `private$` method, `list(set = function(token))`) is refused, because a named argument at an untraced call would break. These are the rename samples left below 100% on the corpora.

## A strict reload re-checks through a persistent salsa database

- **Previous shape.** Every strict check of a changed package ran `analyze_project` from scratch over all of `R/`, about 2 s on the 326,000-line bench package, and `package_index` re-joined every file's names on each reload (about 28 ms) before a 10 ms diff.
- **Shape.** `preflight::PACKAGE_CHECKS` keeps one `cli::PackageCheck` (a salsa database plus its file inputs) per canonical package root for the life of the process, and `check_package_code` updates only the inputs whose text changed, so salsa re-derives what the edit affects. The database lives in the ry binary, not the loader, because the loader runs inside libR on ry's runtime and cannot link the analysis.
- **Loader side.** `namespace_spec` is a tracked query in `load_index.rs`, the spec is shared as `Arc<NamespaceSpec>`, and the reload fingerprint is taken from the read stamps the plan already holds, so a one-file reload plans in about 3 ms and prepares in 4 to 5 ms.
- **With the shared check record.** The first check of a package in a process still goes through `analyze_project` with `CacheUse::ReadWrite` ("The strict load check shares the check record"), so a cold load reuses `ry check`'s findings. That database replays findings it never derived, so it is not kept (`PACKAGE_CHECKS` holds `None`). The first reload builds the kept database, and every reload after it is incremental.
- **Impact.** Measured on the typed finance bench package in a debug build, a one-file reload with only the record takes about 22 s. With the kept database, the first reload takes about 20 s and later ones under 1 s, load included. The remaining costs are listed in `backlog.md`, "Open: the package loader".

## Compiled code under `ry load` builds with ry's own R home

- **Previous shape.** `src/` was compiled by pkgbuild through `callr`, so `R CMD SHLIB` ran GNU R's `bin/R` and linked GNU R's `libR`; the result loaded only because ry's `libR.so` shares the soname, and a package without pkgbuild installed could not load at all.
- **Shape.** On ry's runtime, `.ry_compile` calls `.Internal(ryLoaderCompile(root, .libPaths()))`, which runs `rcmd::compile` in process against an R home from `rcmd::cached_home`. That home lives under the user cache (`ry/home/<hash>`), keyed by ry's version, libR's and `ry-rcmd`'s stamps and GNU R's home, and is built from headers embedded in `rcmd` at build time (`crates/ry/build.rs`, `embed_headers`), so an installed ry needs no source tree. libR is hard-linked into the home (copied only across file systems), and a new home prunes older homes made from the same libR. `ry-rcmd` must sit beside libR, which is why the release archives and the Nix package ship it there.
- **Unload.** `dyn.unload` is real on ry's runtime (`libr::dynload::unload_library`): it calls `R_unload_<pkg>` and moves the library to `unloaded_dlls`, which keeps it mapped, because R objects may still hold pointers into it. Since `dlopen` returns the existing mapping for a path already open, a reload copies the rebuilt library into a fresh temp directory under its own file name (which names `R_init_<pkg>`) before loading it.
- **Symbols.** A registered routine whose R name is already bound in the namespace is skipped with GNU R's `loadNamespace` warning, and on reload only the symbols the previous load bound (and that are still `identical`) are removed, so a user function with a routine's name survives.
- **GNU R runtime** keeps pkgbuild, matching `load_all()`.

## `if` in NAMESPACE is evaluated by the host, then resolved in text

- **Previous shape.** The loader's `NamespaceSpec` put every `if` directive in `unsupported`, and the load warned and ignored it; the checker's `namespace_directives` skipped it too.
- **Shape.** R evaluates conditions, Rust parses directives. `.ry_namespace_conditions` (runtime.R) walks each `if` at top level or inside a top-level `{` as `parseNamespaceFile` does (global environment, only the taken branch, depth first) and encodes the values; every transport carries them as one string (`loader::parse_namespace_conditions`), because the embedded `.Call` routines take only strings. `Loader::read` replaces each `if` with the directives on its taken branches (`metadata::resolve_namespace_conditionals`) before anything parses `NAMESPACE`, so the spec, the diff, the index cache key and the analysis all see the NAMESPACE R would build, with directive order kept.
- **Unknown directives.** Values are keyed by the directive's source text. A directive without values fails the read with `LoadError::Conditionals` before any stamp is taken (a stamp taken for a discarded read would hide the edit from the next one), the host returns `list(unresolved = TRUE, namespace_conditionals =)`, and R evaluates and plans again, repeating while the plan names new directives. The first request of every load sends no values, because evaluating the conditions the last load had would run conditions the file may no longer hold, which GNU R never does; a package with an `if` in NAMESPACE therefore plans twice per load.
- **Checker.** Analysis cannot evaluate R, so it reads both branches, which avoids false unresolved reports for platform-conditional imports.

## An integer and a double joined stay unboxed

- **Previous shape.** `Ty::join(Int, Dbl)` was `Boxed`, so a variable that is an integer on one path and a double on another (a `match` whose arms return `v` and `0`, `x <- if (c) i else 0.5`) was boxed on every edge into the join, and arithmetic on it ran as an opaque tier-0 step: 0.67 s per 2M iterations against 0.10 s when both arms agree.
- **Shape.** The join is `Ty::Num`: the value as an `f64` in the register's or slot's double variable and an "is integer" flag in its integer variable. An edge from `Int` or `Dbl` converts (an integer `NA` becomes the double `NA`), and boxing, write-back and deoptimization rebuild exactly the R value the flag names (`runtime::jit_box_num`, `call_flush`), so `typeof`, integer overflow and `NA` behave as in tier 0. Widening to a plain double instead would change `typeof` and integer arithmetic, which R code observes.
- **Who reads it unboxed.** Only `Move`, `SetVar` and arithmetic whose result is a double whatever the flag says: a comparison, `/`, `^`, or `+ - *` with a double operand. `Builder::box_operands` retypes every other reader's `Num` operand as boxed and `Plan::boxings` tells code generation to box it first, so no other action ever sees a `Num`. An inlined body refuses a `Num` slot, and a direct call passes one boxed from its slot cell after the flush.
- **Impact.** The mixed `match` loop runs compiled; correctness is pinned by `test_tiers` seeds that observe `typeof`, `NA` and integer arithmetic after the join, and the fuzzer's final statement can print the types.

## data.table's own test suite is the bridge's acceptance test

- **Previous shape.** The bridge and the base builtins under data.table were checked by the `datatable__*` oracle cases only, and `test.data.table()` stopped after test 471.3 under ry.
- **Target shape.** `test.data.table()` passes in full (all 9863 tests of data.table 1.14.10, the same count GNU R runs), so a change to the bridge, the C API or a base builtin that data.table reaches reruns it. The suite lives in the installed package (`tests/tests.Rraw.bz2`) and runs with `library(data.table); test.data.table()` under `ry run --runtime ry`; `crates/oracle/cases/datatable__suite_shapes.R` pins the shapes it caught.
- **What the suite forced on the bridge.** It declines `roll = "nearest"` (Polars breaks ties toward the later row, data.table toward the earlier), `.EACHI` over a non-equi join or with a `j` that reads a join column (data.table reports `i`'s bounds there), a non-equi join whose join columns differ in type between `x` and `i` (data.table keeps `x`'s types), `:=` with `nomatch`, and a table with a `_nqgrp_` column. `.EACHI` is recognized while parsing the call (`frame::parse_parts`), so every check after it sees `each_i`. A rolling join answers exact matches with an equi-join filtered by `mult` and rolls only the rows of `i` that matched nothing (`table::query`), as data.table does.
- **What it forced on the runtime.** A comparison of two plain scalars and `Rf_ScalarLogical` return the shared `TRUE`, `FALSE` and `NA` (they are marked not mutable, so assignment copies them), because data.table's `setattr` compares addresses against them. Outside a UTF-8 locale a non-ASCII string of unknown encoding compares unequal to a marked string with the same text, and `Rf_translateCharUTF8` writes its non-ASCII bytes as `<xx>`, as GNU R's conversion from the C locale does. `read.table(fill = TRUE)` pads a short record by converting empty fields, so character columns get `""`.
- **Impact.** Correctness only: every new decline costs data.table's own speed on rare shapes, and the shared logicals save an allocation per scalar comparison.

## A hashed frame counts complex assignment's `*tmp*`

- GNU R binds `*tmp*` in the frame for the length of a complex assignment, and in a hashed frame that insert and removal move the chain count (`env.profile` as the black box), which can grow the table and so change the order `ls(sorted = FALSE)` lists. ry never binds `*tmp*`, so `complex_assign` calls `Heap::count_transient_binding`, which applies the insert and the removal to the frame's `HashTable` without a slot (`crates/heap/src/env.rs`, `HashTable::count_transient`). Pinned by `crates/oracle/cases/environments__hashed_order_after_removal.R`.

## Base kernels pick their data structure by the value range

- **Previous shape.** `as.character()` on integers formatted and interned every element, the cumulative functions zeroed their result before filling it, `strsplit()` probed the session-wide string table for every piece, and `substr()` recycled `start` and `stop` with a division per element. Each was 1.2x to 2.7x GNU R's time.
- **Shape.** `as.character()` on integers (`coerce.rs::integers_as_strings`) interns each distinct value once when the values span a small range, which `factor()`, `table()` and `cut()` inherit. `cumsum` and friends allocate with `values::alloc_uninit`, and real `cummax`/`cummin` run a branch-light loop until the first NaN. `strsplit()` interns ASCII pieces through the call's `values::StringMemo`, and `substr()` recycles by counters. Each falls back to the general path when the range is wide, so no input is slower than before.
- **Impact** (`tools/bench/base`, ry time over GNU R's): at 1e6 rows `table` 0.28 (one factor) and 0.36 (two), `cut` 0.77; at 1e7 rows `cumsum` 0.90, `cummax` 1.00, `strsplit` 0.90, `substr` 0.79.

## Runtime error checks follow GNU R's order and blame

- **Rule.** Where GNU R checks a condition before matching arguments or dispatching, ry checks it at the same point and blames the same call. `NextMethod()` errors through the innermost closure call ("called from outside a function", "called from an anonymous function", "object not specified" when `.Class` is unbound and the object has no class, "generic function not specified", "no method to invoke"), with no fallback to the call's name. `round()` and `signif()` count their arguments before matching them and blame the calling closure, so `eval.rs` leaves their formals check to them.
- **Why.** Packages and GNU R's own tests compare these messages, and a lenient fallback hid real bugs (a `NextMethod` that silently picked the wrong generic).

## A formula's dot expands to the sum GNU R prints

- `stats_models.rs::expand_frame_dot` replaces `.` in a formula's right-hand side with the sum of the frame's other columns only when the dot is reached through formula operators. The sum is wrapped in parentheses unless the dot stands at the top, inside `(`, as a call argument, or as the left operand of `+`, which reproduces GNU R's `terms()` output and deparse exactly.

## The strict load check analyses only the package

- **Previous shape.** `ry load`'s strict check asked about `R/` but built the whole project: every test and script file was parsed and folded, about a quarter of the derive work on the benches, and the verdict thread then freed the analysis (85 ms on typed finbench at 60K lines) before the loader could proceed. The check record's key hashed the ry executable on the same thread before any analysis started.
- **Shape.** A run whose requested files are all package files (`ry check R`, the load's check) analyses only package files, because tests and scripts read the namespace but add nothing to it. The record keeps the unanalysed files: an unchanged one keeps its recorded content, surface, findings and imports used, and when one changed, the record stores no surface for it (`RecordedFile.surface` is an `Option`) and marks every unanalysed file unchecked, so the next full run checks them. `check_cache::plan` compares surfaces only for analysed files. The `NAMESPACE` findings are recomputed on every run that builds an analysis (a few milliseconds) from the analysed files' imports used, and a replay recomputes them when the package files' imports differ from all files' imports, since the record does not say which kind of run wrote it. `preflight::check_package` frees the analysis on its own thread, and the build identity is hashed on a thread that overlaps the project walk.
- **Impact.** The verdict-cold typed load on finbench at 60K lines fell from 0.52-0.55 s to 0.43-0.45 s; the rest of the gap is checker CPU. Package findings are unchanged by construction, and the seeded simulation in `cli.rs` now mixes package-only runs into its edits, comparing each with a fresh run (`tests/scenarios/check-cache-package-only.scenario` pins the two traps it found: a test helper edited between a load and a full check, and the `NAMESPACE` verdict across the two kinds of run).

## The data.table migration is one engine in `ide`, read off the checker's types

- **Where it lives.** `crates/ide/src/migrate.rs` (`data_table_rewrites`, `apply_data_table_rewrites`) is a pure read of the analysis database, like every editor feature. `code_actions` offers each rewrite that overlaps the viewport plus one whole-file action, and `ry migrate data-table` (`cli::migrate_data_table`) analyzes the requested files exactly as `ry check` does and applies the same list, so the editor and the CLI cannot disagree. It is not a lint fix: a lint's fix is local to its diagnostic and needs no types, while a rewrite needs the target's type and the columns it knows.
- **What counts as a table.** A query `x[...]` is rewritten only when `item_check`'s type of `x` is a `Frame` (data.table or DataFrame) or the `data.table` named type, so a vector index is never touched.
- **Column or variable.** With the frame's columns known, a bare name is a column exactly when it is one of them; data.table also looks columns up first, so a same-named variable loses there too. With unknown columns, a name is a column unless it resolves to a local binding (`item_naming().resolutions`), is a project global name (`project_global_names`), is a base constant (`T`, `F`, `pi`, `letters`, ...), or is one of data.table's own symbols. A guess cannot be wrong silently in the other direction: a name taken for a column that is not one makes the DataFrame query fail with a missing-column error at run time.
- **Nothing approximate.** Every unsupported shape returns `None` and produces no rewrite. Updates in place (`:=`, `setorder`, `setDT`) are rewritten only as top-level statements outside any function, as `x <- x |> ...`, because inside a function the caller would stop seeing the change. `dt[i, col := v]` needs known columns: an existing column is rewritten only for a literal of its own type, as `Expr.if_else(test & !Expr.is_null(test), v, col("col"))`, since data.table leaves a row whose test is `NA` unchanged. A keyed join (`setkey` then `X[Y]`) is skipped because it needs statement order.
- **A rewritten table never escapes to unrewritten code.** Every rewrite turns a data.table into a DataFrame. `[.DataFrame` still answers `x[i, j, by]` and `:=`, but data.table's own functions refuse one (`setorder(df, -x)` stops with "x must be a data.frame or data.table"), and so does any package code that checks `is.data.table`, so the whole-file migration (`DataTableRewrite.replacement`) keeps a form only where its table flows into other kept rewrites, into `$`, `[[` or a shape function (`nrow`, `names`, ...), or is discarded. A query that is its function's return value is wrapped in `data.table::as.data.table()`, so a function's interface keeps its classes (parameters already enter through `as.DataFrame()`). Every other form gets `replacement: None`; dropping one can expose another, so `data_table_rewrites` iterates to a fixpoint (`fate`, `uses_are_safe`). Uses are matched by name across the whole scope, which can only leave more forms alone. A single code action (`standalone`) is user-directed and stays offered. Found by `tools/bench/migrate/fuzz.py`: the previous engine rewrote the `data.table()` constructors in the benchmark templates while their callers stayed data.table code.
- **Accepted differences.** DataFrame aggregations skip `NA` (so `na.rm = TRUE` is dropped and `sum(x)` without it no longer gives `NA`), `DataFrame.sort` puts `NA` last where `setorder` puts it first, and a join lists the left table's columns first. The guide's own mapping makes the same choices; `docs/.../reference/from-data-table.md` states them.

## Dates, windows, ranks, cross joins and pivots in the table engine

- **Date is a column type.** `ColumnType::Date` maps to Polars' `Date` (whole days since 1970-01-01) and to an R double with class `"Date"` both ways (`table::convert::RVector::Date`, `libr::frame_altrep::date_vector`, `interp::frame::literal`). A fractional R date floors to its day, as `as.Date` printing does. Typing follows R's own rules for days: date ± number is a date, date - date is `integer[]` days, dates compare only with dates, and any other operator on a date is a `DateOperation` error (`check.rs`, `date_operation`). R itself would give a `difftime` for date - date; the engine has no duration column, so the integer day count is the deliberate shape.
- **`Ops.Expr` beats `Ops.Date`.** `col("day") >= as.Date(...)` has two S3 `Ops` methods, which R refuses as incompatible. `chooseOpsMethod.Expr` returns `TRUE` (R >= 4.3 protocol, honored by `interp::dispatch`), so the expression method always wins; the stub's `Ops.Expr` accepts `Date` on either side.
- **Windows and ranks lower to Polars.** `Expr.over(e, keys)` is `Expr::Window` and `Expr.rank` is `Expr::Rank` (ordinal, ties broken by row, `NA` last). The fast kernels decline both, so they run in Polars; the checker types `over` as per-row with its argument's type and `rank` as `integer[]`. The ordinal row number is `is_null().cum_count()`, because a constant `TRUE` mask is folded by Polars into a scalar of length one.
- **Cross join is a `How`.** `How::Cross` takes no keys (`check_join_keys` rejects any), renames clashing right columns with the suffix, and keeps left-major order; the checker's `join_type` treats it as a join with empty keys.
- **Pivot is eager.** `DataFrame.pivot` and `LazyFrame.pivot` collect first and run `table::ops::dcast`, because the output columns depend on the data, so no lazy plan can know its schema. The checker types the result as an unrefined `DataFrame` for the same reason.
- **A pivot addresses cells by dense ids.** `dcast_dense` in `crates/table/src/ops.rs` turns the row keys and the spread column into dense ids from their order digits (the same per-column sort codes `sort_frame` uses), so each cell is `row_id * labels + label_id` and one pass fills or aggregates it. The earlier path grouped by every row key and the label and then looked each group up by its label text, which took 4.5 s for a 1e7-row pivot against 0.7 s now. The grouped path stays as the fallback for keys whose digits do not fit a u64 or a cell count past `i32`.
- **Pushdown respects order and groups.** A filter that reads a window, rank or aggregate depends on which rows it sees, so it moves only below a sort, and a filter that also reads row order (`first`, `last`, `head`, `rank`: `lazy::reads_order`) does not move below a sort either. Every pass that needs the columns an expression reads goes through `query::column_references`, which includes window keys, so projection pushdown never drops a key.

## Seeded batteries replay through one loop

- **Previous shape.** Each battery looped over its own seed range and printed its own replay hint, if any, so replaying a failure meant reading the test to find how its seed was formed.
- **Shape.** `syntax::testing::for_each_seed(default, |seed| ...)` runs seeds `0..default` scaled by `FUZZ_ITERS`, runs only `FUZZ_SEED` when it is set, and on a panic prints `FUZZ_SEED=<seed> cargo test -p <package> <target> <test> -- --exact`. A battery whose seeds are offset (`1000 + seed`) keeps the offset inside the closure, so the printed seed is still the one to pass.

## A reload diffs S3 registrations per table entry

- **Previous shape.** The reload diff compared `S3method` directives as a set, and registered a directive whenever its function was rebound.
- **Why that was wrong.** R registers directives in order into one entry per generic and class, so a later directive for the same pair wins. Rebinding the function of a losing directive re-registered it and made it win, and dropping one of two directives for a pair unregistered an entry that still exists (found by `test_reload_fuzz`, pinned in `crates/loader/tests/reload/edits.test`).
- **Shape.** `loader::diff` maps each pair to its winning directive and registers or unregisters only winners. The same diff's `keeps_names` claim (no binding appears or disappears) now also requires every name a rerun statement may write to be bound already, and a statement whose writes are unknown rules the claim out.

## `R CMD` make variables expand as GNU make expands them

- **Shape.** `make_assignments` and `expand_make` (`crates/rcmd/src/makeconf.rs`) follow GNU make 4.3: recursive (`=`) values expand at use, simple (`:=`) values expand at assignment, `?=` assigns once, `+=` keeps the flavor of the original and adds no space to an empty value, a backslash-newline run collapses to one space, `$$` is a literal dollar, a lone trailing `$` stays, and a variable that references itself expands to nothing at the inner reference instead of recursing (GNU make stops there with an error). There is no depth bound, so a non-cyclic doubling chain expands exponentially, exactly as it does under GNU make. `R_HOME` is ry's home whatever the makefile assigns, because R passes it on make's command line.
- **Why.** A package's `Makevars` is written against GNU make, and any difference in expansion changes compiler flags silently. `make_expansion_agrees_with_gnu_make` checks generated makefiles against the real `make`, so the rule is GNU make's behavior, not a reading of its manual.
- **Includes.** An `include`, `-include` or `sinclude` line expands its file list and reads each file through a caller-supplied `read`, applying its assignments at that point, as make does; a file `read` cannot supply is skipped, and nesting stops at 32 levels so a self-include ends. GNU R's `Makeconf` includes `vars.mk`, and the differential generates includes too, so the rule is checked against make.

## The JIT leaves a lossy double modulus to tier 0

- **Previous shape.** Compiled `%%` on doubles computed `a - floor(a / b) * b` directly.
- **Why that was wrong.** GNU R warns "probable complete loss of accuracy in modulus" when the finite quotient `|a / b|` exceeds 2^63, and compiled code skipped the warning.
- **Shape.** Both the bytecode codegen and native code call `runtime::jit_modulus_loses_accuracy` (`interp::arith::modulus_loses_accuracy`, the single rule) and hand the operation to tier 0 when it is true, so the warning has one implementation.

## The young generation allocates by bumping a cursor

- **Previous shape.** Each small allocation went through `allocate_with`, a non-inlined call that ran the collection check, claimed a bitmap word on a miss and took the next free bit by trailing-zero count, then wrote the header. The write barrier looked up the parent's mark bit in its page's side bitmap and cached the last unmarked parent in `Heap::young_parent`. `sd` on 20 doubles cost 121.5k instructions per call against GNU R's 70.6k, and a loop of `cons` + `root` + a scalar vector cost 298 instructions per iteration, 191 of them in `allocate_with`.
- **Target shape** (`crates/heap/src/space.rs`, `heap.rs`):
  - Each size class has a cursor `{next, end, page, next_slot}` over a run of consecutive free slots, claimed from the allocated bitmap by `claim_run` (marked as allocated up front, so a slot never handed out reads as unmarked and is swept like a dead one). The fast path is `next != end`, then a bump. `refill` is cold and takes the next run in the current page, then an available page, an empty page, or a new one. A collection first calls `release_cursors`, which clears the unhanded tail of each run so it is free, not dead.
  - `Heap::allocate` is `#[inline(always)]`: `young_bytes + size <= small_trigger`, the bump, word-wise zeroing of the requested prefix and the header store. The collection check and the `keep` slice live on the cold `allocate_small_slow`, so a caller's `keep` array costs nothing on the fast path.
  - `small_trigger` is derived state: `young_limit.max(old_bytes / 2)`, or 0 under gc stress, refreshed in `Heap::new`, after each collection, and in `update_policy`, which replaced `policy_mut` so nothing can change the policy without refreshing it. Recomputing it per allocation was two loads, a shift and a max on the hottest path; it changes only where it is refreshed.
  - `gc_bits::OLD` is set in the header the first time an object is marked and never cleared. The barrier's fast path is `parent & (OLD | REMEMBERED) != OLD`, one load from a header the store is about to touch anyway, so `young_parent` and its side-bitmap lookup are gone. Since a non-moving heap never promotes by copying, "marked once" is exactly "old". `verify_barrier` cross-checks OLD against the mark bit.
  - `Heap::reserve(bytes, keep)` runs one collection check for a batch, and the `_uncollected` allocators (`alloc_triple_uncollected`, `cons_uncollected`, `alloc_environment_uncollected`, `define_formals_uncollected`, `alloc_compiled_promise_uncollected`) never collect, so a closure call that knows its counts after argument matching pays one check and roots nothing while it builds the frame. `TRIPLE_BYTES` and `ENVIRONMENT_BYTES` size the request. The reservation is a budget, not memory: going past it only moves the next collection earlier.
- **Pools per object type.** The data-oriented mandate asks for a pool per object type, and the size classes already are that: every triple-shaped type (pairlist cells, promises, closures, binding cells, language objects) has one 40-byte layout and shares the 40-byte class, environments are the only 192-byte objects, and scalar vectors fill the 32- and 48-byte classes. Splitting the triple class by `SEXPTYPE` would give each type its own cursor and pages without changing any layout, and it would cost locality: a call allocates promises, cells and conses interleaved, and one cursor puts them in consecutive slots.
- **Impact.** The microbenchmark loop costs 116 instructions per iteration (298 before): the inlined `cons` about 54, a scalar `alloc_vector` 46 plus a 12-instruction zeroing. `sd` costs 110.3k instructions per call (121.5k before), measured as the callgrind difference between 150 and 50 calls on the profiling build. `root`, `allocate_with` and `write_barrier_slow` dropped out of the profile; `refill` runs about eight times per call for about 1k instructions. What is left of the gap to GNU R is the interpreter's call path (`frame_cell`, `Frame::insert`, argument matching), which the batch `reserve` lets it shrink further. `crates/heap/tests/test_gc.rs` pins that bump runs refill the holes a minor collection leaves and that a reserved batch survives the collections after it.

## The string intern table is a packed open-addressed array

- **Previous shape.** `heap::strings` kept interned `CHARSXP`s in a `hashbrown` table of pointers, and every probe compared the candidate's bytes through its pointer, so a lookup cost one cache miss per candidate into objects scattered over the heap. With 1e7 live strings `formatC` went from 5.9 s to 7.8 s while GNU R stayed level. `values::StringMemo` hid this per call in `substr` and `formatC` only. A `young: Vec<Sexp>` listed every string interned since the last collection, so a minor collection could remove the dead ones, which cost 8 bytes per new string at the peak.
- **Shape** (`crates/heap/src/strings.rs`). One `Vec<u64>` of slots, linear probing over a power-of-two length. A slot packs the object's address divided by 8 (44 bits, so 47-bit user addresses fit) with 20 bits of its hash, so a probe compares the tag inside the slot and touches the object only on a tag match, about once per million wrong candidates. Deletion leaves tombstones. The table grows at 7/8 load counting tombstones and rebuilds to between 5/16 and 5/8 load, which keeps probe sequences short and its memory near GNU R's (whose table resizes at 0.85).
- **Which young strings to check.** The small-object space keeps an `interned` bitmap beside its mark bits (`Space::set_interned`). A minor collection removes `interned & !marks` on the pages it touched, and the sweep clears the bit with the slot, so no list of new strings is kept. Strings too large for the space (over the largest size class) still go on `young_large`, which is short.
- **A `CHARSXP` is aligned to 8, not 16.** Its header plus up to 7 bytes fits the 40-byte class instead of 48, which most short strings (`paste0("id", i)`, formatted numbers) are.
- **Impact.** `formatC` at 1e7 rows spends about 0.8 s outside the collector with no live strings and with 1e7 of them, so the time no longer depends on heap state. `values::StringMemo` stays in `substr` and `formatC`, whose results repeat, because a small per-call table stays in cache where even one probe into the session-wide slot array misses it; it is not needed for the time to stay level. `test_gc.rs`'s intern fuzz test mixes in large strings so `young_large` is exercised.

## `gc()`, `gcinfo()` and `gc.time()` report the collector's real counters

- **Previous shape.** `gc.time()` returned zeros and `gc()`'s table was made up, so collector time could not be attributed from R.
- **Shape.** `heap::GcStats` keeps live objects (GNU R's `Ncells`), live vector data in 8-byte cells (`Vcells`; a major collection counts it afresh, a minor one adds what it marked), their maxima since `gc(reset = TRUE)`, and user, system and elapsed collection time while `GcPolicy::timing` is on, which `gc.time(TRUE)` sets as in GNU R. `Heap::usage` gives `gc()`'s table, with the trigger columns scaled from the heap's single byte limit, since ry triggers on total bytes rather than on two separate counts. `gcinfo(TRUE)` installs `GcPolicy::reporter`, which prints GNU R's one-line report after each collection while collection is still inhibited.

## `as.character` of a long number vector is deferred

- **Shape.** As in GNU R, `as.character` of an integer or double vector of at least 256 elements without attributes returns a deferred string vector (libr's `deferred_string` ALTREP class through the `defer_strings` hook), holding the source and the `scipen` in force at the call. Reading one element formats only that element; materializing with no element read formats the whole vector in one batch, as the eager path would, and then drops the source. Subsetting reads elements without materializing.
- **Why it fits.** ALTREP is already a first-class part of the object model, so no new object kind was needed. Below 256 elements the deferred vector's own allocations cost more than converting.

## Collections run on GNU R's schedule, and dead large vectors go back to the kernel

- **Previous shape.** A major collection ran only when the old generation passed twice the survivors of the last one, and a large vector ran a minor collection at half the old generation. A minor collection promotes everything it marks, so temporaries live at a minor collection piled up as old garbage: a CI report's loop (`keep <- v > 0.5; s <- list(id[keep], v[keep])` over `runif(5e7)`) peaked at 2.6 GB against GNU R's 1.4 GB. Large vectors came from libR's mimalloc, whose purge delay (`crates/libr/src/libr.rs`) keeps freed memory resident until `gc()`, so blocks of other sizes stayed resident on top of the live data. `rep()` built a `Vec<usize>` of output positions, 8 bytes per element, which tripled its peak.
- **Target shape.**
  - `GcPolicy::heap_limit` bounds all of the heap's bytes (old, young small, young large). A large allocation past it, or a minor collection that leaves less than a fifth of `min_heap_limit` (64 MB, GNU R's `R_VSIZE`) free, runs a major collection (GNU R's `R_MinFreeFrac` escalation). After a major one, `HeapGrowth::next_limit` applies GNU R's `AdjustHeapSize` fractions, and the limit never sits closer than that fifth to the need, or every following minor collection would escalate. `HeapGrowth::from_environment` reads `R_GC_MEM_GROW`, `R_GC_GROWFRAC`, `R_GC_GROWINCRFRAC` and `R_GC_VGROWINCRFRAC` with GNU R's presets and ranges (user docs: `reference/cli.md`, "Memory").
  - The small-object trigger is half the old *small* bytes (`old_small_bytes` and `old_large_bytes` are counted apart), as GNU R's cons-cell trigger ignores vector memory. Counting old large vectors let a session holding 300 MB of vectors pile up 150 MB of small garbage (vapply over a 1e6 list: 230 MB peak, now 112 MB, GNU R 58 MB).
  - Vectors of at least `large::MAPPED_BYTES` (1 MB) get an anonymous mapping each, unmapped when they die (`crates/heap/src/large.rs`). `MappedPool` keeps freed mappings up to a quarter of the limit and resizes them with `mremap`; when no kept mapping is long enough it grows the longest and unmaps the rest, so the pool never holds memory idle while pages are added; otherwise up to its limit stays resident while idle. It is a pool of address space (the allocation pools the performance principles call for), not a cache of derived data: nothing in it can go stale, and the redesign that would remove it is fewer large temporaries, not a faster lookup. A sixteenth of the limit cost the subset loop 4.2 s of system time against 2.2 s; `MADV_HUGEPAGE` raised it to 9.9 s (huge-page zeroing and compaction) and was rejected.
  - libR's global allocator is `heap::large::Mapped<MiMalloc>`: blocks of 1 MB and more are mapped the same way, so a builtin's scratch buffer goes back to the kernel instead of staying in mimalloc where the mapped vectors cannot reuse it (`sample(x)` on 2e7 doubles: 305 MB peak with mapped vectors alone, 229 MB with the allocator, GNU R 229 MB). Growing such a block is an `mremap`, not a copy.
  - `rep`, `rep.int` and `rep_len` stream positions (`RepPlan` in `crates/interp/src/arith.rs`, `subset::select_positions`), and a plain vector's negative subscripts stream through a keep mask (`gather_kept` in `crates/interp/src/subset.rs`: `x[-(1:10)]` on 2e7 doubles, 305 MB peak, now 171 MB, GNU R 229 MB).
- **Impact.** Peaks against GNU R 4.3.3: rep 242/239 MB, the subset loop 1386/1386 MB, data.table's `dt[v > 0.5]` loop 329/330 MB (`runtime_peak_memory_stays_near_gnu_r` in `crates/ry/tests/test_perf.rs`). The subset loop takes about 18 s against 17-19 s before and GNU R's 28.6 s. On the generated 60,000-line suites the collector's share grows from 0.44-0.58 s to 0.68-0.82 s of 20-28 s suites. `R_GC_MEM_GROW=0` costs far more on ry than on GNU R (finbench: 66 majors and 4.8 s in the collector against GNU R's 1.6 s), because GNU R's second old generation frees promoted temporaries without a full mark and ry has one old generation (`backlog.md`, "Open: collector throughput on the benchmark suites").

## Large temporaries: freed from their reference counts, tracing stays for cycles

- **Previous source of truth.** Only tracing freed memory. A temporary that a minor collection saw live was promoted and died only in a major collection, so a loop's dead large vectors stayed resident until the heap limit forced one. The copy-on-modify count in each header (`Sexp::refcount`) was kept but freed nothing.
- **Shape.** A per-type hybrid in `crates/heap/src/heap.rs` (`note_zero`, `reconcile`, `shallow_roots`, `free_released`):
  - Every counted store releases the old value through `release_reference`, which reports a count that fell from one to zero. Such an object goes into the zero-count table (`Heap::zero_count`, a flat `Vec<Sexp>` with the `ZERO_LISTED` header bit against duplicates) when it can be reclaimed: a non-ALTREP list, or a non-ALTREP large logical, integer, double, complex or raw vector. Strings and small vectors are left to tracing, since their pages are not returned one at a time.
  - The table is reconciled only inside `maybe_collect`, at an allocation, because C code may hold an unprotected `SEXP` until its next allocation, as GNU R allows. A reconcile runs once the candidates add up to 4 MiB (`RECONCILE_BYTES`) or the table reaches 65,536 entries beyond those held. It sets `HELD` on the shallow roots: the protect stack, precious set, handles, handouts and globals, everything the root sources trace (interpreter frames, the bytecode register file, pending `.Call` arguments, condition handlers), one level deep, weak references, the fields of every object whose fields do not count (`untrack_references`, R's `DISABLE_REFCNT`), and the elements of every list that native code has seen (escaped lists, whose `SET_VECTOR_ELT` stores do not count). A candidate still at zero and not held is released: a list gives up its elements, which may cascade, and a large atomic vector's pages are freed at once.
  - Environments, closures, promises and external pointers stay with the tracing collector, which handles the cyclic subset (the shape of CPython's cycle collector and Nim's ORC). Counts only ever overcount, because sweeping a dead object does not decrement its children, so a missed decrement keeps an object for tracing and never frees one early.
  - A freed vector's bytes stay counted until the next collection recounts them, so the collection schedule matches tracing alone. Uncounting them put collections off, and the dead temporaries only a collection finds (forced promise values) piled up: `rep` peaked about a quarter higher.
- **Soundness check.** `RY_GC_VERIFY=1` makes a reconcile free nothing; it runs a full collection instead and panics if anything it would have freed is reachable. The heap fuzzers (`fuzz_reconciles_free_only_unreachable_vectors`, `fuzz_temporaries_are_freed_and_live_vectors_kept`) run that cross-check, and the oracle cases pass under `RY_GC_STRESS=1 RY_GC_VERIFY=1` with the same results as the default run.
- **Measured.** Peak RSS in MiB from `wait4`, ry against GNU R 4.3.3, for the scripts in `reference/cli.md` ("Memory"): subset loop 1097 against 1386 (tracing alone reached 1100 only with `R_GC_MEM_GROW=0`), data.table loop 277 against 340, `rep` 239 against 239 (its live data). Suite CPU is unchanged against tracing alone (finbench 18.4 s, dsbench 20.7 s against 21.6 s, biobench 25.3 s, collector time 0.57 to 0.74 s either way).
- **Not done.** Freeing a closure's frame at return (GNU R's `R_CleanupEnvir`) would need counted enclosures; environments stay traced (`backlog.md`).

## Start-up memory: release relocation tables and preflight code pages

- **Previous shape.** ry started about 13 MB above GNU R, all in clean file-backed pages: `libR.so`'s `.rela.dyn` (3.7 MB) stays resident after the dynamic loader has applied it, and the `ry` executable's code for the preflight analysis (about 6.7 MB) stays resident while the script runs.
- **Shape.** An `.init_array` constructor in `crates/libr/src/embed.rs` walks `dl_iterate_phdr` and applies `MADV_DONTNEED` to every loaded object's `DT_RELA` table of 64 KiB or more, aligned inward to pages. `ry run` drops the preflight's state and applies `MADV_DONTNEED` to the executable's executable segments (`release_executable_code` in `crates/ry/src/main.rs`). Both are file-backed and clean, so anything touched again faults back in from the file; nothing reads the relocation tables after load except lazy `DT_JMPREL` binding, which is left alone.
- **Rejected:** RELR relocations (`-z pack-relative-relocs`) would shrink `.rela.dyn` at link time, but they need glibc 2.36 and release binaries promise glibc 2.35.
- **Impact.** Start-up peak 63.7 MB to 53.7 MB against GNU R's 50.4 MB. What remains is `libR.so`'s larger text and read-only data.

## `format()` of numbers streams into the result

- `do_format` formats logical, integer and double vectors element by element straight into the result's `CHARSXP`s once the common width is known (`format_numbers` in `crates/interp/src/format.rs`), instead of building a `Vec<String>` of every element first, which held about 40 MB of Rust strings at 1e6 elements. Complex numbers and doubles whose items differ in width fall back to the general path.

## BLAS and LAPACK come from the system, then from GNU R's Makeconf

- **Previous shape.** The interpreter opened BLAS and LAPACK by bare soname (`libblas.so.3`, `liblapack.so.3`, then Accelerate on macOS), and `rcmd` linked Linux packages with a fixed `-lblas -llapack`. On NixOS there is no global library path, so linear algebra failed even though GNU R's `Makeconf` recorded exactly where its libraries are.
- **Shape.** `library_candidates` (`crates/interp/src/linalg.rs`) tries, in order: the sonames, so `LD_LIBRARY_PATH` stays the user's override and a distribution's R gets the library it links; Accelerate on macOS; each library `BLAS_LIBS` (and for LAPACK, `LAPACK_LIBS` then `BLAS_LIBS`) names, in its `-L` directories and then by soname; and `R_HOME/lib/libRblas`/`libRlapack` last, because R's own libraries leave `xerbla_` to GNU R's `libR`. A candidate must define every routine of the table, so a BLAS tried for LAPACK is passed over. Opening LAPACK loads BLAS first, so a LAPACK whose BLAS sits in a directory of its own resolves the dependency by soname. A failure lists every place searched and the fix.
- **One Makeconf reader.** `make_assignments` and `expand_make` live in `crates/rcmd/src/makeconf.rs`, which `interp` includes by path, as `repl` includes `gnu_r_home.rs`. `Toolchain::for_gnu_r_home` on Linux keeps the default compilers but takes `BLAS_LIBS` and `LAPACK_LIBS` from GNU R's `Makeconf` (`Toolchain::with_makeconf_libraries`), so compiled packages link the same libraries the interpreter loads.
- **Impact.** No change where the sonames load. `La_library()` already reports the file the routines came from; `extSoftVersion()["BLAS"]` now does too. The candidate order is pinned by unit tests in `linalg.rs` and `rcmd.rs`.

## Reference Class and R6 method bodies resolve object members in naming

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
  `a_reference_class_edit_rechecks_only_methods_whose_member_answers_change`). The index reads
  `item_hir` only, never naming, so there is no cycle.
- **Unknown classes bind everything.** A superclass the project does not define, or a generator the
  project does not bind to `setRefClass`, hides the members, so every name binds rather than
  guessing. The file-level R6 guard stays for helper functions placed in an R6 list by name.

## A literal `source()` path is followed for resolution

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

- An inactive conditional namespace stays out of the stub library, so its names never resolve
  bare reads. `stubs::dormant_namespace_exports` reads the shipped manifest plus that package's
  stub declarations directly from `StubSources`, without activating anything, and
  `diagnostics::namespace_read_message` reports a missing name as "not exported".
- A package with stubs but no shipped manifest is not checked, since partial stubs would report
  real exports. `:::` reads stay unchecked as everywhere.

## A name inside a string's `{...}` is a read for liveness

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

- **Before.** `join_types` unified the branch values, and `Any` unifies with anything, so
  `if (is.null(f)) order else f` with `f: Any` typed as `order`'s signature and the `Any` path was
  then checked as a function (sets `as.list.cset`).
- **Shape.** `join_branch_values` returns `Any` when either side is `Any`, matching the union rule.
  `join_written_values` keeps unifying, so a variable written `Any` on one path keeps the typed
  path's type and a later misuse is still reported; applying the rule there too dropped NlcOptim's
  two true positives (a matrix called as a function).

## tidyr and tibble ship masked-verb stubs beside their manifests

- `types/tidyr.Rtypes` and `types/tibble.Rtypes` declare only the `@masked` verbs; every other
  export still comes from the `.exports` manifest. Masking is keyed by stub declarations, so a
  manifest-only package could not mask, and `pivot_longer(d, cols = c(a, b))` reported its columns.
  Reshaping verbs return `Any` because their columns depend on the data; row-preserving ones
  (`drop_na`, `fill`, `complete`) keep `<T>`. tidyr re-exports `tibble`, so it declares it too.

## A `NULL` local inside a query of unknown columns is the column

- **Before.** `time <- NULL` (the R CMD check "no visible binding" idiom) followed by
  `dt[...][order(-time)]` read the lexical `NULL` whenever the columns were unknown, and `-time`
  was reported (data.table's own test runner).
- **Shape.** `read_column` types a resolved `NULL` local as an `Unknown` masked column when the
  scope's columns are unknown. A plain `[` on a subject that may be a data.table (declared, with
  data.table syntax, or `Unknown`/`Any`/an open variable) counts `unseen_frames`, and `infer_read`
  turns a `NULL` local read into `Unknown` there. A column scope was not pushed for the plain case
  because it masks every read, which hid a true unresolved name in `m[not_a_column]`.

## `length(x)` guards drop `NULL`

- `recognize_guard` treats `length(x)` as a condition and `length(x) <op> literal` (either side) as a
  test whose edge a zero length cannot reach is non-`NULL` (`non_null_edge`). It only drops `NULL`
  from a union, never shapes an open variable, because a length test says nothing else about the
  value. usethis `if (length(r) != 2) return(...)` then `r[1]:r[2]`.

## A numeric binding yields to a matrix or a generic argument

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

## `ry test --watch` selects callers through S3 dispatch

- **Shape.** `Loader::callers` (`crates/loader/src/loader.rs`) walks the reverse mention graph from the changed names, and every name it reaches also reaches the generics its `S3method()` registrations name and every dotted prefix of the name (`format.shape` reaches `format`). The plan's `changed` list (`Plan::changed_names`) is the touched names plus the function and generic of each registration the reload made or removed.
- **Why.** A test calls the generic, never the method, so following mentions alone ran the test that dispatches to an edited method last. The dotted prefix covers a method registered by naming convention or by `registerS3method`, and the registration edge covers `S3method(gen, cls, fun)` with any function name. `check_reload` (`crates/loader/src/testing.rs`) models both, and the `test-watch` scenario edits a helper that only a three-argument registration ties to `format`.

## Any save refreshes every open document's diagnostics

- **Shape.** `Worker::did_save` (`crates/ry/src/server.rs`) refreshes every open document, whether or not the saved path is open.
- **Why.** A save of `NAMESPACE`, a stub, or a file no buffer shows changes what other documents see, and the server had skipped the refresh for a path it did not hold open, so open documents kept stale diagnostics until their next edit. The simulator found it; `any_save_republishes_documents_that_an_edit_elsewhere_changed` pins it.

## A call-hierarchy item answers only in its own document

- **Shape.** Incoming and outgoing calls resolve the item and answer nothing if the name resolves to a function in another file (`server.rs`, the hierarchy item lookup).
- **Why.** An item's call ranges are relative to its own document; an edit can move the definition its name resolves to, and answering from the other file returned ranges in the wrong text.

## The console test waits on a count of handled closes

- **Shape.** `console_server::closed_connections` and `wait_for_closed_connections` (`crates/interp/src/console_server.rs`) count the debugger connections the console has finished closing, behind a mutex and condition variable. `test_console_server` waits for its own count instead of polling with a sleep.
- **Why.** Polling bounded by a wall-clock budget fails on a loaded machine and breaks the rule that no harness reads the clock.

## A splice reparse refuses a prefix with an unclosed delimiter

- **Shape.** `reparse::try_splice` (`crates/syntax/src/reparse.rs`) falls back to a full parse when the old tree reports an unclosed delimiter or unterminated string before the edit, or a missing closer (an error whose message says "to close", as for a `[[` closed by a single `]`, which never says "unclosed"). The closer phrase also matches other recoveries, such as a type argument list's `>`; that only costs a full parse.
- **Why.** Recovery ends such a group early, so a closer the edit adds anywhere after it closes the group in a full parse and reshapes the tree up to it. Files with such an error are already broken, so the fallback costs little.

## Every seeded harness rotates through `FUZZ_SEED`

- **Shape.** Each case of a harness is one seed of `for_each_seed`, and a harness that shares one generator across cases builds it as `SplitMix64(CONST ^ seed)`. `tools/fuzz-deep.py` finds harnesses by reading the sources, resolves each against its binary's `--list`, and runs it with `FUZZ_SEED` at a fresh start and `FUZZ_DEPTH`, which scales each loop's own default count. A scaled depth, rather than one `FUZZ_ITERS` count, keeps the proportions of a test with several loops of different sizes and needs no budget read out of the source.
- **Why.** A single stream drawn across iterations neither replays one case nor moves to new inputs, so a deep campaign over it repeats the default run. Discovery from the sources means a new harness joins the campaign without being registered.

## CI runs `just gate` against GNU R 4.6.2, and nothing ignored

- **Previous shape.** The live `ci.yml` used stable Rust, which cannot build the workspace (the toolchain is a pinned nightly for `allocator_api`), and tested only `crates/ry` through `default-members`. Its second job ran `-- --ignored` in a release build; once widened to the workspace that pulls in `ide::fuzz_deep` (about 55 minutes), the corpus benchmarks (which need a fetched corpus) and `format_directory` (which needs `RY_FORMAT_DIR`).
- **Shape.** One job runs `just fmt-check`, `just clippy` and `just battery`, the recipes `just gate` runs, with the toolchain read from `rust-toolchain.toml`, so CI and the local gate cannot diverge. GNU R is installed because the runtime's suites skip without an installation, which would leave the runtime untested. It is 4.6.2, the release the oracle's expectations are recorded with and ry bundles as its base R (the live `ci.yml` still installs 4.3.3 until `.github/pending/ci.yml` is accepted). The zlib, bzip2 and xz development packages are installed because libr links them. The ignored suites stay manual (`just fuzz-deep`, `just perf`), because none of them is a pass/fail gate that fits a push.
- **Triggers (user decision).** CI runs only on `workflow_dispatch`, never on push or pull request. The orchestrator starts it on main at its hourly check when main has new commits.
- **Impact.** CI covers every crate except `zed_ry` (wasm), at most once an hour on main, or on any ref when started by hand.

## Alias cycles are reported at the declaration, and short names get narrow suggestions

- **Previous shape.** An alias cycle was found only when an annotation used the alias, and reported on that use with a whole-statement range; an unused cyclic alias was never reported. `nearest_name` returned nothing for a name under four characters, so `person$nam` got no suggestion.
- **Shape.** `alias_cycle_diagnostics` (`crates/semantics/src/diagnostics.rs`) walks each alias the file declares with `check::alias_cycle_walk`, seeded with the alias itself, and reports every alias on a cycle at its declaration. A use of a cyclic alias drops the annotation silently, so one mistake is reported once, where the fix goes. A name of two or three characters is suggested a candidate only when exactly one candidate is that name with one character inserted (`nam` and `nme` find `name`; `df` never finds `dt`), because a substitution budget on a short name matches unrelated names.
- **Impact.** The fix site is the declaration, and the check runs per file over that file's declarations, so it adds no project-wide read.

## Refactorings refuse rather than guess, and a seeded property check holds them to it

- `ide::refactors` (`crates/ide/src/refactor.rs`) offers extract variable, extract function,
  inline variable and annotation conversion. Each one is offered only where the rewrite keeps
  evaluation: extract variable refuses a statement that would evaluate the expression repeatedly,
  conditionally or never (loop or function bodies, `if` branches, the right of `&&`/`||`), and
  `is_quoted` refuses code under a formula or a quoting call. Extract function refuses `return()`,
  `<<-`, `...` and frame-inspecting calls. Inline refuses when a read name is reassigned before the
  last use or a call would run more than once.
- Calls that do not evaluate their arguments once, in order, in the caller are classified by
  `argument_evaluation`: *captured* (quoting, data masking such as `with()` and `dplyr` verbs,
  and the index of a `[`, for data.table) blocks every refactoring; *guarded* (condition
  handlers, `switch`, `withr::with_*`) blocks hoisting a variable out but allows extracting a
  function that is called in place. `evaluated_first` is the one rule for hoisting and for
  inlining a value with a call: every node between the expression and its statement evaluates it
  exactly once, and nothing before it in the statement calls or assigns. A value that assigns is
  never inlined (`x <- y <- 0`). Extract function treats a read anywhere in the enclosing loop as
  later code, because the next pass runs it. These came from review, and the property check
  cannot see them (the result parses and resolves), so each is pinned in
  `crates/ide/tests/refactor`.
- Text that only parses as one expression inside its old context is parenthesized
  (`needs_grouping`: a `?`, or a reparse that is not exactly one node), because `value <- d?f`
  and a multi-line operand chain change meaning when moved out.
- Two edits at one offset are merged into one: LSP clients disagree on the order of same-offset
  inserts, so a definition inserted at the statement start plus a replacement there must be one
  edit.
- `ide::testing::check_refactor_invariants` (driven by `crates/ide/tests/test_fuzz.rs`) applies
  every action at every node range and asserts determinism, non-overlapping edits, a clean reparse
  and no new unresolved names. It runs without stubs: with them one seed took 17 s instead of
  1.3 s, and unresolved-name reporting does not need them. The parse and resolution invariants run
  only on input that parsed cleanly, because mutated input already has errors the rewrite cannot
  be blamed for.

## Call-head completion ranks by call counts, without test files

- **Before.** Completion ranked equally good matches by how often the name is spelled in the
  project, so at two characters on a call head testthat's `expect_*` (spelled in every test)
  beat the package's own functions, and S3 methods (`print.purrr_rate`) crowded out functions.
- **Shape.** The per-file name-use fold (`SpelledNames`) also counts names in call-head position
  (directly or as `pkg::name`). A call-head completion ranks project S3 methods (a `generic.class`
  beside a `generic` candidate) last, then by calls, then spellings. Call counts leave out test
  files unless the cursor is in one (`chunk_uses` takes `with_tests`); spelling counts keep them,
  since tests show which `pkg::` members a project uses (dropping them cost httr2's `pkg::name`
  role 9 points). Two characters on call heads went from 68% to 72% on httr2 and 74% to 76% on
  purrr.
- **Rejected:** ranking by words shared with the enclosing function (`check_rate` in code that
  has a `rate`). It gained a point or two at half the name and lost six to eight at two
  characters, where long names share some word with almost any function.

## Call hierarchy and workspace symbols are measured against the reference oracle

- `ry debug analysis-stats` checks incoming and outgoing calls of a sample of project top-level
  functions (defined exactly once) against the independent lexical resolver that already checks
  references: every oracle global site that is a call head is an expected call, grouped by the
  enclosing top-level definition. Workspace symbols are checked by querying each sampled
  definition's exact name and a half-length prefix (at least 3 characters); a prefix that
  matches more names than a search lists is left out of the share, since a user types on.
- Workspace search reads a memoized per-file list (`searchable_symbols`), because rebuilding
  every file's outline per query took about half a second on finbench.

# Decision record: the syntax tree is ry's own flat preorder arena

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

## Compiled bodies share one deoptimization exit

- **Previous shape.** Every guard's cold block boxed the live unboxed registers and slots itself,
  with one runtime call per value, then called `jit_deopt`. A body with many guards carried that
  boxing code once per guard, so the cold blocks were most of the IR Cranelift compiled.
- **Target shape.** Each guard's cold block only stores the raw values it must hand back into one
  stack scratch area (8 bytes per cell) and jumps, with its exit number, to a single shared cold
  block that calls `runtime::jit_deopt_exit`. The layout of each exit (registers and slots with
  their representations, and the bytecode pc) lives in `Compiled::exits` (`runtime::Exit`), so the
  runtime boxes the values and flushes dirty slots from that table.
- **Why a table.** The layout is static per guard, so it costs nothing to keep beside the code,
  and boxing is cold work: doing it in Rust from a table instead of in emitted code shrinks every
  compiled body without slowing any path that runs often.
- **Impact.** On the why-ry programs the IR shrank by about 31% and Cranelift time by about 40%.

## Native functions loop, read vectors, and inline small callees

- **Previous shape.** Native functions (`decisions.md`, "Native scalar functions") took scalars
  only and refused every loop, so a typed kernel such as a dot product or a running sum went
  through `jit_call` with a frame and boxed arguments.
- **Target shape.** `crates/jit/src/native.rs` also takes plain integer, double and logical
  vectors (as their object pointers; the caller roots them and the body allocates nothing), and
  compiles `for` over `seq_len`, `seq_along`, `a:b` or a vector, `while`, `repeat`, `break`,
  `next`, `length`, and `[[`/`[` with a scalar index. A typed tier-1 caller passes vector
  arguments straight through (`Argument::Unboxed`/`Argument::Slot` in `Action::NativeCall`).
  Every case R would warn or fail on (an index out of range or `NA`, a length above
  `.Machine$integer.max`, a fractional `a:b` start, a pending interrupt at a loop's back edge)
  returns `FAILED`, and tier 0 reruns the call.
- **Scoping.** A local first assigned inside a loop, an `if` branch or the right operand of
  `&&`/`||` goes out of scope where that region ends, so a later read refuses the function rather
  than guess whether R would find it bound.
- **Inlining.** A native call of a callee whose body has at most 24 calls and operators and no
  `return` walks the callee's body in place (its own locals, loops and lookup environment), up
  to three levels deep and never into a callee already being inlined. Both walks (typing and
  emitting) decide the same way, because the decision reads only the callee's body.
- **Strict mode does not drop guards.** A strict-clean program is a hint, not a contract: values
  from stubs or untyped packages can still differ from their declared types at run time. Typed
  code is faster because its closures compile at their first call and their calls go frameless,
  not because guards disappear.
- **Impact.** `sq <- function(x) x * x + 1` summed over 100,000 doubles 50 times runs in 0.035 s
  on ry against 1.99 s on GNU R; the same loop through `g <- function(x) sq(x) - sq(x - 1)` runs in
  0.056 s against 5.44 s, with both `sq` calls inlined into `g`'s 144-byte native function.

## Closures made at one site share compiled code

- **Previous shape.** Compiled code hung off a weak reference keyed on the enclosure it was
  compiled for, and ran only for that enclosure. A `function(v) ...` passed as an argument was a
  promise of its source, so the AST evaluator made each closure with its own body, which compiled
  its own bytecode and, under `RY_JIT=force`, its own machine code on every call: `vapply` over
  an anonymous function cost 306 µs against 8.2 µs on tier 0.
- **Target shape.** A function literal that is a whole call argument compiles as promise code
  (`compiles_as_promise` in `crates/interp/src/bytecode.rs`), whose `Op::Closure` gives every
  closure from that site one bytecode body. Compiled code is *portable* when each speculation
  starts from the enclosure or a long-lived environment and finds a binding in a long-lived one
  (global, base, a namespace or its imports, the search path; `long_lived` in
  `crates/jit/src/jit.rs`). Portable code holds what it embeds strongly and runs for any
  enclosure. `Compiled::checked` is a weak reference keyed on the enclosure whose lookups the
  epoch and stamps were last proven for; a new enclosure resets them, so `runtime::valid` walks
  every speculation from that enclosure (`Compiled::rooted`) before trusting them. Code that
  speculates on a function local to its enclosure keeps the per-enclosure scheme.
- **Why a weak reference, not the enclosure's address.** A dead enclosure's address can be reused
  by a new environment whose lookups nobody checked.
- **Impact.** The `vapply` case runs at tier 0's speed under `RY_JIT=force` (about 15 µs per
  call of ten elements), and base R's `pmin`, whose body makes such a closure, is no slower with
  the JIT on than off.

## Bodies that would mostly hand back to tier 0 compile only for loops

- **Previous shape.** A hot body compiled whenever its plan had at least one native action. In
  package code (`[.data.frame`, `[.zoo`, S4 `is`, ggplot2 internals) most instructions are calls
  tier 1 cannot specialise, so the compiled body was a chain of `jit_step` calls back into tier 0.
  Each such step costs more than tier 0 running the instruction itself, and Cranelift spent about
  0.6 s compiling these bodies on a 12-file finbench slice, which made the suite about 8% slower
  with the JIT on than off.
- **Target shape.** `compile_in` refuses with `MOSTLY_TIER0` (`crates/jit/src/analysis.rs`) when
  the plan has fewer native actions than tier-0 steps, unless the compile was triggered by a hot
  back edge or `RY_JIT=force` is on. The refusal sets `runtime::LOOPS_ONLY` in the body's tier
  state, so later hot entries return at once and only a hot loop can still compile it, because a
  loop's native header pays for its steps on every iteration. A fully typed closure's first-call
  compile goes through the same gate.
- **Impact.** On the finbench slice the gate refuses about four in five compiles and most of the
  compile time, and the JIT-on wall gap against `RY_JIT=off` drops from about 8% to about 4.5%.
  The why-ry programs are unaffected: their bodies are loops or mostly native.

## Tier 0 records type feedback only for warm bodies

- **Previous shape.** With the JIT installed, tier 0 tested the type of every value an
  instruction produced and stored a bit for it, in every body from its first entry. Almost no
  body of package code ever reaches a compile threshold, so this was pure overhead: about 25M
  instructions of a 4.2G ggplot2 render, most of what made the gallery slower with the JIT on.
- **Target shape.** `Decoded::of` (`crates/interp/src/bytecode.rs`) hands out the feedback
  pointer only when `warm` holds: the body has a loop, has compiled code, or is halfway to the
  entry threshold and the tier has not given up on it. A body refused as `MOSTLY_TIER0` gets the
  entry count `runtime::NEVER_HOT`, so tier 0 stops asking the tier about its calls.
- **Two follow-ons keep the gates from trapping a body.** `runtime::invalidate` resets the entry
  count to half the threshold rather than zero, so the activation that just deoptimized records
  the type that broke the speculation and the recompile sees it; from zero, the body could
  compile the same speculation until it ran out of compiles. And a `LOOPS_ONLY` body that did
  compile for its loop still runs its calls in tier 0 (`compiled_for` returns early), because
  entering compiled code from the top would hand most instructions back one by one.
- **A first-call `MOSTLY_TIER0` verdict is not final.** The first call plans without a profile,
  so that refusal leaves the tier state alone and the body tiers up normally later.
- **Why loops always record.** A loop can turn hot inside a body's only activation, and its
  compile needs the types of the instructions that ran before it. Switching recording on at
  half the back-edge threshold left those without feedback, so the loop was refused as cold
  every time and the why-ry array programs ran 5 to 40 times slower.
- **Why half a threshold.** A compile reads the feedback of the activations before it; half the
  threshold leaves 1,500 entries of history, which sees every type a
  specialization needs, while cold bodies pay one comparison per activation.
- **Impact.** The ggplot2 render's JIT overhead fell from +55M to +26M instructions, and the
  gallery is no slower with the JIT on than off (6.99 s against 7.10 s, three interleaved runs).

## A typed closure compiles at its first call only when that call can run long

- **Previous shape.** Every fully typed closure compiled at its first call, because its
  annotations give the types a profile would otherwise collect. On a finbench slice that was
  183 compiles of about a millisecond each, of bodies such as `alpha + beta` whose call costs a
  microsecond and which most tests call a handful of times.
- **Target shape.** A first-call compile is refused as `BRIEF` (`crates/jit/src/analysis.rs`)
  unless the plan has a loop or calls a closure or native function directly
  (`Plan::may_run_long`), the cases where the first activation itself can run long, recursion
  included. The refusal is not counted, so the body tiers up at the entry threshold like any
  other. `compile_in` takes a `Trigger` (first call, hot entry, hot loop) in place of the
  `in_loop` flag, so each profitability gate states which triggers it applies to.
- **Impact.** The why-ry typed programs are unchanged (their bodies loop or recurse); typed
  test-suite code no longer pays for compiles that never run again.
# Decision record: string keys sort by rank, and merge pairs come from sorted codes

- **Previous shape.** `order` and `sort` on character vectors compared the strings themselves
  (a comparison sort over `Vec<String>`-like keys, or text hashed into a map), the radix sort
  gathered `(key, row)` pairs for every bucket, `paste(collapse =)` built each row as a `String`
  and joined them, `make.unique` kept every name as a Rust `String` in a hash set, and the
  `merge` kernel kept an `FxHashMap` of `Vec` per key. Each held a second copy of data the heap
  already had, so peaks were 1.17 to 1.37 times GNU R's.
- **Target shape.** A character key becomes `Column::Ranks`: one `u32` per row, the rank of its
  text among the vector's distinct strings, 0 for `NA` (`string_ranks` in
  `crates/interp/src/sort.rs`). The distinct strings are found by data.table's trick: each
  CHARSXP's truelength temporarily holds a negated slot index, checked against the list of slots
  so a stale value is never trusted, and restored afterwards. Strings are interned, so a CHARSXP
  is its text per encoding. Only the distinct texts are sorted, by collation for `order` and
  `sort` and by bytes for the radix method, and the ranks then go through the integer radix sort.
  The radix sort scatters rows only, and gathers `(key, row)` pairs only for buckets of at most
  65,536 rows, which fit the cache. `make.unique` with an ASCII separator and ASCII names marks
  the names the same way and builds candidates as CHARSXPs; `paste(collapse =)` writes all rows
  into one byte buffer and makes one CHARSXP. `merge`'s matches are found by binary search in
  sorted `(code, row)` pairs, sized by a counting pass and written straight into the result.
- **Impact.** At 1e6 rows every base kernel's peak is within 1.13 times GNU R's (`sort(chr)` 1.04,
  `order(chr)` 1.03 at 0.11 s against 1.9 s, `paste(collapse =)` 0.89, `merge` 0.89 at 0.8 s
  against 2.2 s, `unsplit` 1.13), and `make.unique` on 1e6 names fell from 227 MB and 0.9 s to
  127 MB and 0.45 s (GNU R 92 MB, 1.1 s). The truelength marks survive allocation, because the
  collector never reads truelength and the marked strings stay reachable from the vector being
  read, but every exit, including an error, must restore them.

## A release is `ry` alone, with the runtime embedded and the R front end folded in

- **Previous shape.** A release archive held `ry`, the runtime (`libR.so`, `libR.dylib` or `R.dll`) and `ry-rcmd` side by side, and `rcmd::runtime_home` required `ry-rcmd` beside the runtime.
- **Embedding.** `crates/repl/build.rs` embeds the file `RY_EMBED_RUNTIME` names, zstd-compressed (level 12 in release, 1 otherwise, since a debug runtime is about a gigabyte), plus `libR.dll.a` beside it on Windows, keyed by a hash of the compressed bytes. `just package` builds `libr` first and then `ry-lang` with the variable set. A build-time variable, not a cargo feature or artifact dependency: a plain `cargo build` embeds nothing and stays fast, and a variable needs no unstable `bindeps`. A section written after linking (Bun's approach) would need per-platform object rewriting; `include_bytes!` keeps the signature and the loader untouched on macOS.
- **Lookup order.** `RY_LIBR`, then beside the executable, `deps/`, `../lib/ry/`, and only then the embedded copy (`repl::ry_runtime_library`), so development builds and distribution packages behave as before.
- **Extraction.** Into `ry/runtime/<key>/` of `XDG_CACHE_HOME`, `%LOCALAPPDATA%` (Windows) or `~/.cache`. Each file is written under a per-process name and renamed into place, the library last, so a complete library means a complete directory and concurrent first runs both succeed; on Windows a rename over a loaded library fails, which counts as success when the file is already complete. A failure to write names `XDG_CACHE_HOME` and `RY_LIBR`; a failure to load an extracted library adds the `noexec` hint.
- **Multi-call.** `rcmd::front_end` holds what was `ry-rcmd`'s `main`. `ry` runs it when started as `ry-rcmd` (argv[0]) or, on Windows, as an R home's `R.exe`, `Rscript.exe`, `Rcmd.exe` or `Rterm.exe`. The R home's scripts exec `bin/exec/ry-rcmd`, a symlink to the front end, and the Windows front ends are hard links where possible, since `ry.exe` carries the runtime. `runtime_home` uses the `ry-rcmd` beside the runtime when there is one and the running executable otherwise. The `ry-rcmd` binary stays for development builds and tests. A home records its runtime and front end in `.libr`, and pruning removes only homes with both equal, so two copies of `ry` sharing one extracted runtime never remove each other's home. The runtime cache and the R homes share `rcmd::cache_directory`, and staging names carry `rcmd::staging_suffix` (process id plus clock nanoseconds), since containers sharing a cache volume often share a process id.
- **Tests.** `crates/ry/tests/scenarios/single-binary.scenario` runs `single-ry`, which the harness builds with `RY_EMBED_RUNTIME` and copies into a directory of its own, then rebuilds plain `ry`. `tools/smoke-test.sh` checks that an archive holds `ry` alone, that the runtime was extracted, and (with `RY_SMOKE_PACKAGES`) that `ry install` compiles C code.

## `[[` by name on a named atomic vector is the element, not `T | NULL`

- Previous rule: `c(foo = 1L)[["foo"]]` was `integer | NULL`, treating a named atomic vector like a
  list. R stops with "subscript out of bounds" on an absent name in an atomic vector, so a read that
  returns never yields `NULL`, and the `NULL` member only produced false reports downstream (diffobj
  `symb[["add"]]` into `gsub`). Lists keep `T | NULL`, which R does return.
- Target shape: `extract_result` gives `T` for `T[named]`. The spec (`type-system.mdx`, "`[[` on
  vectors") and the indexing fixtures say so.

## S-era package sources are R sources

- `R CMD INSTALL` collates `R/*.R`, `R/*.r`, `R/*.S`, `R/*.s` and `R/*.q`. ry read only `.R`, so
  every NAMESPACE export defined in an S-era file (Hmisc, VGAM, boot, cluster) was reported as
  undefined: 627 errors on a fresh 99-package sample. `cli::is_package_source` is the one predicate
  the CLI and the server use; a `.s` file outside `R/` (such as `src/fast.s`, assembler) is not R.

## Correlated-path allowances added for the fresh sample

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
  or a directly indexed value keeps it, as `guards__a_length_test_that_admits_zero_keeps_null` pins.
- A guard on `is.x(name <- value)` narrows `name`.
- A bare `NA` local that a closure fills with `<<-` is a placeholder like `NULL` (knitr).
- `seq`'s integer and double overloads come before the Date and POSIXct ones, so a `from` of an
  unresolved numeric type does not pick a date.

## Strict-proven types do not drop JIT guards yet

- **Rule.** No runtime guard drops because of a strict-mode proof until a per-function strict-clean
  verdict reaches the runtime. Once it does, only a base SEXP-type or scalar/vector-kind guard on a
  non-escaping local or parameter, after its force point, may drop, and only when the type derives
  from checked sources alone. Guards on `Any`, stub returns, cross-package values, `@trust` and
  `@if-unknown` bindings, class, attributes, the object bit, length beyond a checked scalar, `NA`,
  and global lookups always stay.
- **Why.** A tier-1 parameter read forces the promise and then runs one fused check (SEXP type,
  no attributes, no object/ALTREP bit, length 1), so dropping only the type compare saves one
  compare per read; every other guard is a lookup guard that the rule keeps. Native functions
  already skip annotation checks, because the specialization's types decide them at compile time.
  Typed and untyped warm times are equal on every why-ry row, so compile latency, not guards, is
  where the time goes.
- **Consequence.** The verdict plumbing is not built. Revisit it when a profile shows guard cost.

## Completion pushes down names whose declared type cannot fit, and never promotes

- **Before.** Completion ranked equally good matches by source and use counts only, so in
  `if (is_|)` a helper declared `-> character` ranked beside one declared `-> logical`.
- **Shape.** `expected_type` in `crates/ide/src/ide.rs` reads what the code around the cursor
  wants: a logical for an `if`/`while` condition and an operand of `!`, `&&` or `||`, and for an
  argument the parameter type its global or stub callee *declares*. It reads the declared
  scheme, not the call's instance, because the instance took its parameter types from the
  argument being typed (`print(i)` made the slot `integer` and pushed `iconv` down), and it
  unions the parameter across every stub overload, because the argument picks the overload only
  once it is written (`lapply`'s first overload takes a named list, which pushed `seq_along`
  down). A candidate is compared by coarse value shape (`type_mismatch`), with R's
  logical-to-integer-to-double widening allowed, and a known mismatch sorts after the fits within
  the same match score and source group. Inferred function types are ignored on both sides,
  since they say how a body ends, not what callers may pass.
- **Rejected:** promoting candidates that fit. Project helpers with an inferred `logical` result
  then outranked `is.na` and friends, and two characters on call heads lost 2 points on purrr and
  1.3 on R6.
- **Impact.** Neutral on httr2, purrr and R6 and +0.3 points on finbench at two characters,
  with completion still about 2 ms median on the packages and 8 ms on finbench; the gain is in
  annotated code, which the `completion_expected_type` fixtures pin.

# Decision record: a cold check holds one parse tree per worker

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

# Decision record: the one-shot check scans kind columns and overlaps its serial phases

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

## The standard library is the `std` namespace, with an optional prelude

- **Previous shape:** the library's signatures lived in `types/ry.Rtypes` beside the DataFrame API, in
  the always-active namespace `ry`, so every name was always in scope and `ry::List.map` was the only
  qualified spelling. Its per-function reference was a hand-written copy of the stub docs in
  `reference/standard-library.mdx`, which drifted from the declarations.
- **Prior art:** Roc auto-imports its builtin modules with no opt-out; Rust has `std::prelude` and
  `#![no_implicit_prelude]`; Gleam has no prelude and imports explicitly. R already reads `pkg::name`
  as namespace access, so the Rust shape fits without new syntax.
- **Chosen shape:** the library is its own namespace, `std` (`types/std.Rtypes`, `types/std.exports`;
  `ry` keeps the DataFrame API and Arrays). `std::Name` always resolves. `[check] prelude`, default
  `true`, additionally puts the bare names in scope; `false` makes a bare name unbound (with a fix to
  `std::Name`) unless the project defines it. Types (`Result`, `Dict`, `Set`, `Array`) stay in scope,
  because they are type syntax, not namespace members. The checker gates bare lookups through
  `stubs::outside_prelude`, which reads the setting only for library names, so flipping it rechecks
  only items that use the library (`test_incremental.rs`). The runtime binds both namespaces and
  attaches `package:std` only with the prelude on; the host passes the setting as the negative flag
  `RUN_FLAG_NO_PRELUDE`, so `R CMD` and older hosts keep the prelude.
- **Docs:** `crates/semantics/tests/test_std_docs.rs` renders `docs/.../reference/std/` from
  `types/std.Rtypes` and fails when the committed pages are stale (`just docs-std` rewrites them), so the
  declarations are the single source of truth for the checker, hover and the docs.
- **Impact:** one source of truth for the API docs; no new syntax; incremental cost bounded to items
  that use the library. Gap: compiled code fast-paths only bare library calls, so `std::List.map`
  is not fast-pathed or JIT-looped (`backlog.md`).

## Optimized tests build with the bench profile, not fat LTO

- **Previous shape.** Optimized tests ran as `cargo test --release`, which builds with the release profile and so with fat LTO (`lto = true`, one codegen unit). Its link of libr's `test_linpack` peaked at 11.1 GiB of RSS and took about 30 minutes; on a 16 GB machine with anything else running it was OOM-killed.
- **Why not change release.** Shipped binaries and every benchmark number in the docs come from `cargo build --release`, and fat LTO is part of what those numbers measure. Cargo always builds `cargo test --release` with the release profile, so tests cannot get a different setting under that command.
- **Measured** (`cargo test ... -p libr --test test_linpack --no-run -j4` from a clean profile directory, peak RSS of the largest child process): fat LTO 11.08 GiB in 1,797 s, the bench profile 3.60 GiB in 685 s.
- **Target shape.** `[profile.bench]` inherits release with `lto = false` (thin local LTO) and 16 codegen units. `cargo test-release` (`.cargo/config.toml` alias for `cargo test --profile bench`), `just perf`, `just simulate` and `cargo bench` use it. Plain `cargo test --release` still works on a machine with the memory.
- **Dev debug info.** `[profile.dev] debug = "line-tables-only"`: libr's `test_linpack` went from 0.28 to 0.11 GB and the debug `libR.so` from 1.03 to 0.85 GB. An interp test binary barely shrinks (0.88 to 0.78 GB), because its `.text` (185 MB) and relocations dominate, not debug info.

# Decision record: start-up and imports run GNU R's code, with its hot loops native

- **Problem.** A warm `ry load` of a large S4 package spends most of its time before the package's
  own code runs: process start (base, `methods` and the default packages, about 130 to 170 ms) and
  the imports' `loadNamespace` (about 230 to 300 ms for lubridate, data.table and xts). Both run
  GNU R's own R code from the installed R home, so neither the loader nor the S4 cache touches them.
- **Options weighed.**
  - *A preloaded heap image* of the state after start-up (and after the imports). The heap sits in
    one fixed reservation, but the interpreter's own state (symbol and string tables, `Globals`,
    builtin and native-routine tables, connections, external pointers into loaded DLLs) lives in
    Rust collections and C libraries outside it, so an image needs a serializer for every one of
    them, and an import with compiled code cannot be imaged at all. Kept as the end shape for base
    alone, under "bundle base R into the `ry` binary" in `backlog.md`.
  - *A fork server* (a resident process that has started and loaded the imports, forking a child
    per `ry load`). It would cut start plus imports to a few milliseconds, but it is not exact: the
    child inherits the server's environment variables, user profile, `tempdir()` and library
    paths unless each is replayed, packages that start thread pools at load (data.table's OpenMP)
    switch to one thread after a fork, and a package updated on disk leaves the server stale.
    Rejected while exactness is the bar.
  - *Native hot loops behind GNU R's code* (chosen). The runtime replaces one base R function at a
    time with a copy whose per-item loops run natively, guarded so that it applies only to the
    exact GNU R code it was copied from.
- **`registerS3methods`** (`crates/interp/src/register_s3_methods.R`, `do_register_s3_methods` and
  `do_local_s4_generics` in `envir.rs`, `install_register_s3_methods` in `session.rs`). Its loops
  called `tryCatch`, `methods::is` and `get0` per method, and cost 41 ms of the default packages
  and 34 ms of the imports above (37 ms each on GNU R). The native loop
  performs the same steps in the same order (generic lookup, `genfun@default` for an S4 generic,
  table creation, the overwrite check with `identical()`, and the `delayedAssign` promise with the
  same environment `assignWrapped` makes) and calls back into R for each step that can signal or
  dispatch: `asNamespace` of an unloaded package, the not-found error, `methods::is` on an object,
  `@default`, creating a table, reading an unforced binding, `environmentName`. Now 8 ms in each.
  `methods::is(f, "genericFunction")` is skipped for a value with no class attribute and no S4
  bit, where it is always `FALSE`.
- **Why a hash guard.** Base R's R code is read from the installed GNU R so that ry behaves like
  whichever release it finds (licensing record above). The copy follows R 4.3.3, so the runtime
  replaces the installed function only when an FNV-1a hash of its formals and body (symbols and
  strings by their text, attributes ignored) equals the 4.3.3 one; any other release keeps its own
  function at its own speed. `RY_PRINT_REGISTER_HASH` prints the installed function's hash for
  updating the constant with the copy.
- **Pinned by** `namespaces_register_s3_methods_as_gnu_r_does` in `crates/libr/tests/test_install.rs`,
  which installs two packages under ry and under GNU R and compares one script's output (every
  branch: known generics, another package's generic, an overwrite and its message, local plain and
  S4 generics, delayed registration), and asserts the replacement is in place under R 4.3.3.
- **Next by cost.** `methods::cacheMetaData` of the imports (about 150 ms, lubridate most of it) is
  dominated by `.checkGroupSigLength`, which `.updateMethodsInTable` runs for every member of a
  group generic and which rescans the whole group tree each time (1.2 ms per call on ry, 0.8 ms on
  GNU R). The same guarded replacement would apply to it.

## Typed closures run as native functions over records, tags and match

- **Previous shape.** A typed closure ran as any other: tier 0 matched arguments, built promises
  and an environment, and tier 1 compiled the body once a profile was hot, speculating on boxed
  `SEXP` values. Natives (`crates/jit/src/native.rs`) existed only for scalar and vector bodies
  reached from compiled code, and any record, tuple, tag or `match` refused them. Typed programs
  ran no faster than untyped ones (orders 0.43 s typed vs 0.50 s untyped).
- **Target shape.**
  - *Call-time entry.* `Tier.call` (`jit_call` in `runtime.rs`, called from `apply_closure`) runs
    a typed closure (`annotations::is_typed`) as a native at its first call. Arguments must be
    positional or name-exact, and each value is read without forcing: a constant, a forced
    promise, a promise of a constant, or a promise of a symbol bound to a plain value. Its `Ty`
    comes from the value (scalar, attribute-free vector, plain record as `Rec(layout)`), the
    native is cached per closure and `Ty` list, and a trampoline cached per machine signature
    passes words in and boxes the result. Debug, step, trace and moves tracking skip the path.
    This supersedes the `BRIEF` refusal above for typed closures reached this way.
  - *Records as pointers.* `Ty::Rec(layout)` holds a record pointer; distinct layouts join to
    `Ty::Obj`. Field reads speculate from per-layout slot-kind profiles that the heap records at
    allocation (`heap::records::slot_kind`) and native construction records at compile time; the
    read checks the unboxed bit and the integer/logical prefix, an empty profile makes the read
    fail, and a mixed profile refuses the native.
  - *Construction and match.* A record is built from a stack array of (representation, bits)
    pairs by `jit_native_record`. Zero-slot tags are shared immutable singletons. `match` with
    exact-layout patterns, `_` and binds resolves statically on `Rec(L)` and branches on the
    layout id for `Obj`; a non-exhaustive match fails to tier 0. Guards, `|`, `..`, literals and
    nested patterns refuse.
  - *Rooting.* `jit_native_mark`/`jit_native_release` bracket entry, return, failure and loop
    back edges, except where a loop carries a record into a pre-loop local.
- **Why not more.** Annotation checks stay storage-only, and a native fails (tier 0 reruns the
  call) rather than deoptimizing mid-body, which keeps every native side-effect-free until it
  returns. Uniqueness in-place update, scalar replacement of non-escaping records, strings and
  the standard library inside natives, vector builtins, and recompiling when a profile widens
  are not built (`backlog.md`).
- **Impact.** orders 0.50 s (GNU R) / 0.44 s (untyped ry) / 0.034 s (typed), expr_tree 0.33 / 0.34
  / 0.008 s, checksums equal. `fuzz_typed_record_natives_agree_with_tier_zero` in
  `crates/jit/tests/test_tiers.rs` pins agreement across JIT modes.

## Typed ry has its own numbers, `Bool`, `Str` and `List`

- **Before:** typed ry borrowed R's `integer`, `double`, `logical` and `character`, which carry `NA` and recycle, and `Array<T>` was a strict R vector. Typed programs could not be written without R data types.
- **Shape:** two number types, `I64` and `F64`, plus `Bool`, `Str` and `List<T>` (which replaces `Array<T>`), all `NA`-free. The design is `development/design/native-types.md`; the contract is the reference section "Numbers, `Bool` and `Str`".
- **Number set cut to `I64` and `F64` (orchestrator decision).** The first version took all thirteen of Roc's number types (`I8`..`U128`, `F32`, `Dec`). Each width multiplied the checker's constraint cases, the interpreter's arithmetic and the JIT's register types, and no typed program used anything but `I64` and `F64`. Another width returns only when the benchmark hill climb shows a program that needs it. Any other literal suffix is a syntax error (`syntax` lexer). The internal literal constraints (`Constraint::Int`, `Constraint::Frac`) default to `I64` and `F64` and have no annotation spelling; `number` is the only written one. An R `integer` or `raw` value becomes `I64` at the boundary.
- **Representation:** `F64`, `Bool` and `Str` reuse R's `double`, `logical` and `character` storage, so they cross into R for free. `I64` has its own vector type, `SexpType::I64` (code 28, `I64SXP`), with `REALSXP`'s layout: the 16-byte header, length and capacity, then 8 bytes per item at `VECTOR_DATA_OFFSET`. A scalar `I64` is a length-1 vector and a `List<I64>` is the same object at length n with the heap's Array flag (`numbers::alloc`, `numbers::list`). It carries no attributes and is never ALTREP.
- **Why a dense `I64` vector (decision).** The previous shape was a boxed record per number (`Fields::Number`, one raw slot) and an R list of boxes for `List<I64>`, which gave two representations for one type, one GC-traced object per item, and an item-by-item arithmetic path (`arith::item_by_item`, `Items::RyValues`). A dense vector gives one representation for scalars and lists, a zero-copy handoff to the typed runtime (`crates/typed`), no per-item tracing (it is a leaf), vectorized same-length list arithmetic (`numbers::arithmetic`, no recycling, overflow an error) and storage compatible with bit64's `integer64`. A new type code beats a gp bit on `REALSXP`: R code and C code that test `TYPEOF` refuse it instead of reading the bits as doubles, and `is_atomic` is false for it, so every R builtin raises the `Num.to_r()` boundary error (`records::boundary_message`). Crossing to R stays explicit through `Num.to_r` and `List.to_r`.
- **Literals:** a suffix names the type (`5i64`), and a `-` written directly before a literal is part of it. An unsuffixed literal keeps R's type unless an expected type, the other operand, or a later use decides it. A literal branch adapts to the sibling branch's ry type (`adapt_literal_branch`), and a match arm or `if` arm in a function's tail adapts to the declared result.
- **The checker decides a literal's run-time type (decision).** Before, the runtime converted literals by its own syntactic rules (the operator in `numbers::operands`, the annotated binding in `lower::with_annotated_literals`), so a literal the checker typed `I64` anywhere else stayed an R `double`: a `match` arm adapted to a declared `I64` result returned a `double`, which broke soundness, and so did an unannotated `[1, 2]`. Now each `ItemCheck` records the unsuffixed literals it typed `I64` (`i64_literals`), `semantics::file_i64_literals` maps them to file ranges, and the preflight elaborates the program for ry's runtime by spelling each as `5i64` (`preflight::with_i64_suffixes`), the mirror of erasure for GNU R. `ry run` hands the elaborated text to the runtime through `ry_set_script_source`, and the console's filter releases it as the command. A file with no ry syntax and no strict mode skips analysis (`preflight::has_ry_syntax`, which also counts `#:` comments and `Module.function` calls). Columns after an elaborated literal shift right in run-time error messages, an accepted cost. A `List<I64>` given to `DataFrame()` becomes an R `integer` column when every item fits and `double` otherwise, as `Num.to_r` converts (`frame::column_contents`), because an elaborated `[3L, 5L]` column is an `I64` list. `with_annotated_literals` remains because `ry load` and `source()` run unelaborated text; it is redundant on elaborated text, and goes once those paths elaborate too. `ry run` rewrites the file's own bytes, not a lossy UTF-8 copy: the check reads each invalid byte as `?` so offsets stay byte offsets, and `ry_set_script_source` escapes invalid bytes as reading the file does, so a Latin-1 script keeps its bytes. A whole literal spelled with a fraction or exponent (`2.0`, `1e3`) converts digit by digit, because an `f64` rounds past 2^53.
- **String and logical literal variables (decision).** `greeting <- "hi"` and `flag <- TRUE` bind a literal variable (`Constraint::StrLiteral`, `BoolLiteral`) the way a number literal binds `WholeLiteral`/`FracLiteral`: an annotated binding or a typed parameter in the same item decides `Str`/`Bool`, and every other read defaults it to R's type, so plain R types exactly as before. Meeting a `List` item variable binds the ry type directly in the var-var arm of `unify` (`infer.rs`), because no constraint spells "only `Str`". Call arguments and annotated-binding values that are plain reads join `literal_reads` so the call or annotation, not the read, decides them.
- **`/` decides `F64`** for undecided items and is a `type-mismatch` on `I64`, as Roc gives `/` a `Frac`.
- **Lengths, counts and positions are `I64`** (`standard_library::whole_number`), as in Roc.
- **List operators work item by item** and never recycle: on a `List<I64>` they run over the buffers (`numbers::arithmetic`), and on nested lists item by item (`arith::item_by_item`).
- **`try f(x)`** is the `Ok` value or an early return of the `Err`; `try (expr)` keeps R's `try()`.
- **No row variables.** An open tag union cannot pass its unlisted tags through with their types; this stays until a real program needs it (user direction to drop them from the design).
- **Impact:** typed programs are written in ry types only (`tools/bench/why/typed/`). The cost is speed: the JIT has no register type for `I64`, so typed `I64` code runs in the interpreter (`backlog.md`).

## Typed values are native, immutable, acyclic and freed by reference counts

- **Previous shape.** Typed natives (`crates/jit/src/native.rs`) build records as R heap objects
  traced by the mark-sweep collector and rooted through handle marks, and the first draft of
  `development/design/typed-runtime.md` kept typed lists and strings as R vectors and `CHARSXP`s
  with stack maps for roots.
- **Chosen shape.** Typed ry's own values (numbers, `Str`, `List`, `Dict`, `Set`, records, tuples,
  tags, closures) are native values outside the R heap. They are immutable and acyclic (a closure
  never captures itself; recursion goes through a lifted definition), so compiler-inserted
  reference counts in the Perceus style free them deterministically, with borrow inference as in
  Lean 4, field moves, drop specialization and reuse of a uniquely held block. No tracing collector
  sees them, so compiled frames need no stack maps. R values stay under the collector and meet
  typed code only at the boundary: an entry stub checks a guess and runs a second compiled copy
  that reads the R value in place, or the program converts explicitly (`List.from_r`).
  References go one way: a native block may preserve an R value, but every native value crossing
  to R is converted (a typed closure becomes an R closure over converted captures), so no cycle
  runs through both heaps. R values in compiled locals sit on the handle stack.
- **Why.** Exact counts make in-place update a count-of-one test, freeing deterministic, and a leak
  checkable at the end of every test run. Native layouts (inline small strings, unboxed lists of
  flattened records, a dense-entry `Dict`) are the Rust-shaped data the 1.5 to 2 times Rust bar
  needs, and R vector headers, intern tables and collector tracing would cost on every operation.
- **Layouts.** Data-oriented throughout: a heap record or tag variant is a slot in a per-type pool
  stored as a struct of arrays (one column per field and one count column), a reference is a
  32-bit index (28 index bits, a closed union's 4-bit variant index above them; open or wider
  unions use 64-bit references), with column bases reloaded after any call that may grow them, lists store record items as columns and `Bool`
  as a bitset, short non-escaping lists live on the stack, and a `Dict` over densely numbered keys
  is a flat array plus a presence bitset, while a hashed `Dict` packs seven hash bits into the upper
  bits of each index entry.
- **Impact.** A `List<F64>` reaches C only through a copy, and an ALTREP wrapper is the deferred
  fix if the boundary copy shows up. The design and its test matrix are in
  `development/design/typed-runtime.md`.

## Typed code compiles ahead of time, and `ry build --exe` writes an executable

- **Chosen shape.** `ry run` and `ry load` compile each typed function of a strict file before its
  first call (when the lazy loader forces its binding, with workers compiling its callees ahead),
  and `ry install` compiles all of a package's at install time into the
  installed package, so typed code never starts in an interpreter. There is no code cache
  (AGENTS.md, "Performance principles"): an installed package's code file is build output, one
  position-independent file per namespace, mapped read-only and shared between processes, with a
  per-process table for addresses and a stamp of the ry build and callee interface hashes, and code
  for a script or a package under development is compiled in memory as the session uses it. `ry build --exe` appends that file and the
  untyped bytecode to ry's runtime as a standalone executable. Plain `ry build` keeps writing a
  source tarball, as `devtools::build()` does, because existing workflows rely on it.
- **Tests.** Every typed program runs interpreted, compiled, and compiled with annotations erased,
  with identical output required, plus `RY_FORCE_GUESS_FAIL`, `RY_CHECK_FACTS` and an empty-pool
  leak check (from Bun's compiler, `prior-art.md`). `ry check --explain` reports slow paths worst
  first by loop depth.

## The methods package's core runs natively behind hash-guarded prologues

- **Previous shape.** Every S4 operation ran GNU R's methods R code on ry's interpreter, with only
  the C routines (`methods.rs`) native. A cold first `new()` on the generated S4 package
  (2,892 definitions) spent about 7.7 s running definitions, and `new()` itself cost about 180 µs
  per call against GNU R's 130 µs.
- **Target shape.** The methods package's core is Rust (`crates/interp/src/methods_core.rs`), and
  the package's R code and metadata objects stay as they are: classRepresentation objects, the
  `.__C__` and `.__T__` tables, generic objects, `MethodDefinition`, `.SigLength` and the method
  tables. Those objects remain the single source of truth, and the core reads and writes them
  directly, so there is no native mirror to drift and saved RDS objects, `methods:::` callers,
  Reference Classes and the S4 cache keep working.
- **Mechanism.** `R_initMethodDispatch` (methods' `.onLoad`) calls `install_fast_paths`, which, for
  each function named in `methods_core.R`, compares a stable hash of the formals and body with
  `FAST_PATHS` and on a match rewrites the body to `{ prologue; original body }`. The prologue calls
  an `.Internal` that either does the whole job and returns, or returns without observable effect.
  A hash mismatch (another GNU R version) leaves the function as it is, so the core can never
  apply to code it was not written against. Patching the closure in place, instead of replacing the
  binding, reaches every copy that holds the same closure. The default `initialize` method has
  several separate copies, including one per class that inherited dispatch cached in `.AllMTable`,
  so every entry of the generic's tables is hashed and the matching ones are patched. A preloaded
  base image carries the patched closures.
- **Exactness rules.** A fast path reads promises only when forcing them runs no code
  (`quiet_value`), bails before any effect wherever R code could observe the work (validity
  functions, `as()` caching coerce methods, user `initialize` methods, `.S3Class` slots, a data
  part not set through the standard replacement method already cached in `coerce<-`'s table, list
  entries in the class table), and evaluates the calls that may signal in the R
  function's frame, so condition calls and `sys.calls()` match GNU R. Known difference: when a fast
  path completes, the R function's frame-local temporaries are not bound; only code that inspects
  that frame from outside (a debugger, `sys.frames()` from a validity method the fast path never
  calls) could see it.
- **Versions.** `FAST_PATHS` lists the hashes each function may have, one per `methods` version the
  fast path was checked against; the bundled GNU R 4.6.2 is the primary target. A miss on a
  methods package whose version is the bundled one is an error at start-up, because it means the
  base changed under the core; a miss on any other version keeps the R code. The R expressions the
  native definitions fill in (coerce and replace methods) are R text in `METHODS_TEMPLATES` in
  `methods_core.rs`, parsed once at install time.
- **Definitions.** `setGeneric`, `setMethod` and `setClass` are native for the shapes a package
  usually writes: local supers only, no `setClassUnion` member classes passed as a
  classRepresentation, a `validity` that is `NULL` or a function of `object`. Anything else bails
  to the R code before any effect. The native results are byte-identical to the R path (classes,
  generics, method tables and `serialize()` output, compared on the generated S4 packages, the
  Matrix sources and the methods package's own tests). Adding a direct subclass rebuilds the
  superclass's whole subclass list as `completeSubclasses()` does, regenerating every indirect
  extension (whose `by` and `to` carry the package attribute differently from the first time) and
  repeating a class reached through two direct subclasses; appending would differ. One benign
  exception to bailing before any effect: `setClass` evaluates `tryNew()` for slot prototypes
  before its last checks, which fills the same `initialize` dispatch cache the R code fills.
- **Switches.** `RY_METHODS_NATIVE=0` turns the definition fast paths off (the differential uses
  it), `RY_TRACE_METHODS_BAILS=1` prints why each call bailed, and `RY_PRINT_METHODS_HASHES=1`
  prints the hashes of the installed functions, for adding a version.
- **The S4 cache is interim.** The end state has no S4 cache: the cold native path itself reaches
  a first `new()` under 100 ms, and then the cache is deleted (AGENTS.md, "Caching is a sign of a
  slow design"). Until then it stays, because restoring is about 10 times faster than running the
  definitions natively (61 ms against about 850 ms), and incremental reload of it is not worth
  building for a cache that is going away. It is saved uncompressed: the save fell from 344 to
  about 190 ms and the cache grew from 1.5 to 9.4 MB, with the restore unchanged. Saving at process
  exit instead of after the first run was rejected, because `.MTable` gathers inherited entries
  during a session and the saved state would depend on what ran.
- **Order.** Object creation and class queries came first because they are the run-time hot path
  (`new()` 13.7 µs, `validObject` 3 µs, `is()` 2.3 µs against GNU R's 130, 73 and 11.8 µs). Then
  the definition functions; `setClassUnion`, `cacheMetaData` and inherited dispatch remain
  (`backlog.md`, "Open: native methods core").
- **Impact.** Cold first `new()` on the bench package fell from 8.6 s to 6.3 s with the native
  queries, and native definitions bring the generated `target/s4plain` package (2,892 definitions)
  from about 2.2 s of S4 work to about 0.9 s. `crates/oracle/cases/s4__native_core_queries.R` and
  `s4__native_definitions.R` pin the covered functions, including their error messages and calls,
  against GNU R.

## The typed core lowers R syntax at the first call and runs on 16-byte strings and view lists

- **Previous shape.** Strict typed closures ran on the scalar natives in `crates/jit/src/native.rs`, which take R values in and out of every operation, so programs over strings, lists and local functions (`report`, `closures`, `bond_book`) gained nothing.
- **Chosen shape.** `crates/typed` lowers a strict typed closure to a typed SSA IR at its first call for the argument types it sees, inserts Perceus reference counts (`ownership.rs`), and compiles it with Cranelift (`backend.rs`). `jit::native::call` is the one seam: it hands the argument values to `typed::call`, which returns `None` to fall back. A call that fails at run time (an `NA` where an integer cannot hold one, an overflow, an index out of range, an interrupt) returns status 1 and the interpreter reruns it, so the typed core never reports an error itself.
- **Lowering from R syntax at run time, not from salsa.** The runtime cannot link the analysis, and the closure's body is the source of truth the interpreter also runs. The guards on resolved functions (`Speculation`, rechecked on every call) make the lowering sound against rebinding. The serialized per-function IR in `typed-runtime.md` remains the design for install-time code files.
- **`Str` is 16 bytes, not 24.** Two words fit the two-register return of Cranelift's tail convention and halve list storage; strings up to 15 bytes are inline, flagged by bit 63 of the second word.
- **One list header with a data pointer.** A `List` header points at its items, so an R vector argument is passed as a view (flag `VIEW`) over R's own data with no copy, and the same compiled code serves owned and borrowed lists. Writing to a view copies first.
- **Base closures behind lazy-load promises.** Base binds its closures to lazy-load promises, so a function is base's own when its binding cell is base's, and `resolve_function` forces such a promise (it only reads the package database) instead of refusing.
- **Counted loops check their ends once.** `for` over `seq_len`, `seq_along`, `a:b` or a vector tests `counter == end` at the latch and steps with `Op::Step`, which cannot overflow because the counter never passes the end.
- **Impact.** On the why-bench, typed and strict: `closures` 0.348 s to 0.006 s, `report` 0.640 to 0.009, `bond_book` 0.739 to 0.049, `loop_sum` 0.021 to 0.010, `array_sum` 0.025 to 0.004, `array_map` 0.048 to 0.017.

## Docs information architecture: Introduction, Tutorial, Reference, Development

- **Previous shape.** The sidebar had Introduction, four numbered step sections of about three pages
  each (`guides/`, `type-checking/`), Reference and Contributing. Installation and the editor setup
  were long pages inside the first step, and the step pages repeated reference material in prose.
  The owner judged the result walls of text with no clear concept.
- **Target shape.** Introduction (`getting-started.mdx`, `installation.md`, `ry.mdx`,
  `benchmarks.mdx`), a Tutorial of four pages under `tutorial/` (one per step, each a run of
  snippets with real output), Reference (`reference/`, the contracts and the long material that
  used to sit in the steps: DataFrames, data.table migration, arrays and known limits in
  `type-system.mdx`), and Development (`development/`, renamed from `contributing/`, with design
  drafts in `development/design/`). Editor setup is a table on the installation page.
- **Redirects.** Every old URL redirects through the `redirects` map in `docs/astro.config.ts`,
  to the tutorial section that absorbed it where one exists.
- **Sidebar scroll.** starlight-theme-black scrolls `.sidebar-content` inside a pane with
  `overflow: hidden`, while Starlight's `SidebarPersister` saves and restores the scroll of
  `#starlight__sidebar`, which never scrolls. `docs/src/components/Sidebar.astro` wraps the theme's
  sidebar and saves `.sidebar-content`'s `scrollTop` to sessionStorage on click and `pagehide`, and
  restores it in an inline script right after the sidebar markup, so it applies before first paint.
- **Impact.** The introduction and tutorial pages shrink from about 3,000 lines to about 1,300, a reader finds one page
  per step, and the sidebar keeps its place across navigation.

## The start-up image is build output of the base tree, and every start re-reads the environment

- **Previous source of truth.** Without an image, every start re-derived the same session from
  base R's files (baseloader, lazy-loaded base, the six default packages, methods rebuilding its
  S4 tables): an empty `ry run` took about 125 ms (median, Linux, bench profile). A first design
  wrote the image at run time on the first eligible start, into `~/.cache/ry/image`, keyed by the
  build, the session flags, base R's files, the library paths and the steering variables, with
  every other environment read recorded through an `image::variable` wrapper and compared on load.
- **What was structurally weak.** That image was a cache of derived state, and it had a cache's
  failures: a key that missed an input (a shared `.Random.seed`, a stale lazy-load database of a
  package outside R home, relative `R_LIBS`, start-up warnings shown only on the writing run), and
  a wrapper every interpreter environment read had to remember to use.
- **Target shape.** The image is build output. `ry-rcmd build-base` writes it as its last step
  into the base tree (`library/base/R/ry-start.image`) in an emptied environment, a release embeds
  it with the tree, and nothing writes it at run time. The header carries `RY_RUNTIME_ID`; an
  image of another build is skipped silently (`just start-image` refreshes it in a development
  checkout). After the load, each start clears the options and re-runs the environment-reading
  steps: the C-level options, `.Machine`, `.Platform`, base R's profile, and the `.onLoad` hooks
  of utils, grDevices and stats (`interp::session::start_from_image`). The image holds only what
  those steps leave alone, the namespaces and their attachment, which depend on nothing in the
  environment but `image::LOADING_VARIABLES`; a start that sets one runs the slow way. Strings
  under the writer's R home are relocated to the tree the image file lives in
  (`heap::image::Relocation`). The writer fails, failing the build, when start-up warned or left
  `.Random.seed`. User packages get the same treatment as install-time output of `ry install`
  (`backlog.md`).
- **No "existing objects" check.** Rust state that holds heap objects across start-up must be a
  fixed object, a heap root, or saved by `image::save_state`; a check that refused every object
  live before start-up flagged only `package:ry`'s frame, which the loader overwrites safely.
- **Impact.** Empty `ry run`: about 120 ms to 60 ms median. Re-running the environment steps
  costs about 12 ms against loading a run-time image (47 ms). `methods::getClass("numeric")` with
  `R_DEFAULT_PACKAGES=NULL` starts the slow way (63 ms; the run-time image keyed that variable and
  took 56 ms). The four cache bugs cannot occur: no image depends on
  the run-time environment. Options, the search path and namespace paths match a slow start
  (`start-up-image.scenario`). Development builds start slowly after a runtime edit until the
  image is rewritten. Analysis is unaffected: the image is runtime-only.
