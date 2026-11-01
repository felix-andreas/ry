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

# Decision record: package tests through the oracle

`oracle packages` (`crates/oracle/src/packages.rs`, `contributing/corpus-and-oracle.md`, "Package
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
- **GNU R's quirks are kept on purpose**: `%*%` uses BLAS unless an operand may hold a NaN or an infinity, then a plain loop (which also stands in when BLAS is missing, since reference BLAS sums in the same order); `La_solve`'s condition check norms the already factored copy when `a` was not double; `La_dgecon` estimates with `n` as the leading dimension, which ry refuses for `m < n` instead of reading past the matrix.

# Decision record: the default packages' utils, tools, grDevices and graphics routines

The routines live in `crates/interp/src/{utils,tools,graphics}.rs`, wired by package in
`builtin_libraries::implementation`. Each behavior was derived from GNU R's documentation and its
observed output (the oracle cases `io__*`, `colours__*`, `graphics__*` and
`utils__sizes_hashtabs_and_tools`), never from its C source.

- **Devices record pages of shapes and render them when they close.** Every file device (pdf,
  postscript, xfig, pictex, png, jpeg, bmp, tiff, svg, cairo_pdf) opens a `graphics::Device`
  holding its size, point size, the base graphics parameters and the pages drawn so far. A page is
  a list of `graphics_render::Shape`s (paths, circles, text) in inches from the bottom-left corner,
  each with the clip rectangle in force, so the drawing routines know nothing about file formats
  and each format is one writer over the same pages. `plot.new` starts a page when the layout's
  last figure is done; `dev.off`, `quit()` and the end of a session write the file: PDF and SVG
  as vectors, PNG, BMP and TIFF rasterized with tiny-skia, text outlined with ab_glyph from the
  system's Helvetica-compatible fonts (Liberation Sans, then others). Rendering at close rather
  than streaming keeps devices free of file handles and makes a multi-page bitmap device one file
  per page, as GNU R's `%d` names expect. jpeg, postscript, xfig, pictex and cairo_ps have no
  writer yet and write nothing beyond what they did before. File behavior otherwise follows GNU
  R: vector devices create their file at open (so a bad path fails there), a pdf without pages
  is still a valid document, and bitmap devices with no page write nothing.
- **The drawing geometry is GNU R's, checked against its pdf output.** A line of `lwd` 1 is
  1/96 inch, a margin line is `mex * csi`, text sits on a margin line by its baseline offset by
  `ylbias`, symbols are sized from half the character height, font sizes round to whole points,
  and text widths come from Helvetica's AFM metrics, the same table `strwidth` uses. Rendering a
  plot through both engines' pdf devices and rasterizing them is how to check a change: plot,
  hist, barplot and `par(mfrow)` layouts differ in a handful of pixels. Contour labels use a
  clean-room "flattest window" placement, so they can sit in a different spot than GNU R's.
- **Display lists and snapshots stay empty.** `dev.control` and
  `dev.displaylist` toggle a per-device flag, but no call is ever recorded, so `recordPlot` returns
  GNU R's shape (`list(NULL, <raw state, pkgName "graphics">)`, `engineVersion` 16) with a `NULL`
  display list and empty state, which plot capture such as evaluate's reads as "no plot was made".
  `replayPlot` of such a snapshot is a no-op and any other snapshot is an error. The snapshot does
  not capture `Pars`: restoring parameters on replay would need the snapshot to own a copy of them,
  and that belongs with recording the display list, which would also let devices replay a
  plot onto a resized or copied device.
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
  them) at the nearest whole point size, with the cap height for `strheight`. That matches the pdf
  device's numbers exactly; bitmap devices in GNU R measure with cairo fonts and can differ.
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

## Syntax trees are evicted, and only small syntax facts are memoized

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
- **Every hot body compiles, loop or not.** Loop-only compilation was a stopgap while entering
  compiled code meant forcing every argument through a promise helper, which made
  `function(a, b) a + b` slower compiled than in tier 0. Direct calls removed that cost, so the
  loop-only rule went with it.
- **Function speculation is validated by the binding epoch alone.** Compiling marks each
  speculated symbol and binding cell `WATCHED`, and the heap bumps one global epoch when a watched
  cell is assigned or a watched symbol is defined in any function frame (which could shadow the
  speculated binding). A call site then checks one integer instead of re-walking the environment
  chain. The cost is spurious invalidation when an unrelated frame defines a watched name, which is
  rare, and recompiles are capped per body. When the epoch has moved, each speculation is
  re-resolved from the environment its lookup started in, which for a function looked up inside an
  inlined or directly called closure is that closure's environment, not the compiled body's.
- **A call to a speculated closure is direct and passes forced promises.** Arguments that are
  effect-free are evaluated eagerly in the caller, wrapped as already forced promises, and bound
  positionally into a frame sized for the formals, skipping the matcher. Early evaluation is only
  invisible when the callee cannot assign anything an argument reads before forcing it
  (`g <- function(x) { assign("y", 99, envir = parent.frame()); x }`), so the callee must be
  frame-safe (`analysis.rs`, `frame_safe`): it calls only itself and `FRAME_SAFE_BUILTINS`, each
  lookup recorded as a speculation from the callee's environment, and the call defers to tier 0
  while a calling handler is established. S3 dispatch from those builtins on a classed argument is
  the one accepted hole. A promise object is still
  allocated, because `substitute()` and `missing()` in the callee need one; eliding it entirely
  needs compiled-to-compiled calls that know the callee never reflects.
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
- **Interrupts deoptimize.** A compiled backward jump reads the interrupt flag and deoptimizes to
  tier 0's backward `Jump`, which raises the interrupt, so there is one place that handles it.

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
- **One source of truth per side:** the types live once in `types/ry.Rtypes` (namespace `ry`,
  always active), and the runtime functions live once in `crates/interp/src/standard_library.rs`,
  registered with `BuiltinDef::accepting_native`. A test asserts the two name sets are equal and
  that every declared parameter name is accepted at run time.
- **`Dict` and `Set` are native kinds** (`Fields::Dict`, `Fields::Set` in `crates/heap/src/records.rs`)
  with value semantics, insertion order and an open-addressing index, because a named list
  matches partially and allows duplicates, and an environment has reference semantics.
  `identical()` ignores order, as Roc's equality does. Updates copy, which makes a growing loop
  quadratic until the compiler can reuse a value it owns (backlog).
- **Errors are tags, not conditions:** a function that can fail returns `:Ok(x) | :Err(:Tag)`, and a
  wrong argument type raises an R error that names the parameter, so a misuse is loud and a domain
  failure is a value.
- **Impact:** no change for plain R (no shipped name collides, and all names are new symbols). The
  checker, hover, completion and goto-definition read the stub file like any other, so incremental
  analysis gains nothing new to invalidate. The user contract is `reference/standard-library.mdx`;
  the draft with open questions is `contributing/design/standard-library.md`.

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

## parallel's natives without fork

- `crates/interp/src/parallel.rs` implements parallel's `.Call` routines that do not fork: the L'Ecuyer stream jumps (`nextStream`, `nextSubStream`) and the multicore queries, which report a parent process with no children. That is enough for `RNGkind("L'Ecuyer-CMRG")`, `detectCores()`, `doParallel` registration and sequential `foreach`. The fork routines (`mc_fork` and the rest) still error, so `mclapply` with more than one core fails; `backlog.md` tracks it.

## Corpus package tests: compatibility details that are easy to get wrong

- **Hashed frames list in GNU R's order.** `ls()`, `as.list(env)` and `mget` over a hashed environment follow GNU R's bucket order, and packages' snapshot tests record that order. `heap::env::Frame::listed_cells` replays GNU R's table instead of storing one: buckets from the PJW hash, an insert prepends to its bucket, and when the count passes 0.85 of the size the table grows to `size * 1.2 + 1` and rehashes in bucket order, resetting the count to the occupied buckets. The global environment is hashed from its creation, as in GNU R. Replaying keeps the frame's own storage the single source of truth, at the cost of rebuilding the order on each listing, which only listing functions pay.
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
- **Shape.** `syntax::matching_nodes` filters on green nodes and builds red nodes only for matches (and, lazily, their shared ancestors); `syntax::for_each_token` reads token text with no red nodes. `testing::check_parse_invariants` pins both to the red walks. `SalsaGlobals` holds per-check `RefCell` maps for the two lookups, and `map_child_types` returns the input type when no child changed.
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
- **What declines, and why.** Keyed subjects and extra attributes (key semantics), auto-indexing filters (data.table adds an `index` attribute to the source as a side effect), joins without `on` (key joins), `x[]` (returns `x` itself), and reductions whose result depends on summation order: data.table's GForce (`by`, no `:=`, j in `.N`/`f(col)` form) adds doubles sequentially in plain double, while outside GForce base R uses long double, so only GForce-form `sum` and `mean` without `na.rm` are accepted. To match GForce, the engine sums doubles in row order per group (`table::group` dense path single-threaded for double sums, plan path via `cum_sum().last()`).
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

## `object-systems = false` stops class definitions in user code at run time

- **Previous shape.** Only the lint read the option, so `ry run` happily defined S4, Reference and R6 classes in a project that had turned them off.
- **Target shape.** `ry run` and `ry repl` on ry's runtime pass the option to the interpreter as a run flag (`RUN_FLAG_OBJECT_SYSTEMS`). With it off, `run_closure` calls `refuse_object_system`, which raises the lint's message (one text, `syntax::native_values::object_systems_disabled_message`, and one table of defining functions, `object_system_function`, shared by both) when a call named `setClass`, `setClassUnion`, `setGeneric`, `setMethod`, `setValidity`, `setRefClass` or `R6Class` (bare or after `::`) runs a closure owned by `methods` or `R6` and its caller's top environment is the global environment.
- **Scope.** User code is whatever has the global environment as its top level: scripts, the console, and functions defined there. Packages are exempt, installed or loaded with `ry load`, because they must keep loading (`stats4` defines S4 classes when it loads) and a package's classes are the package author's call; the lint still flags a project package's source. Matching by call name keeps the hot path to one flag test when the option is on and one symbol-name lookup per closure call when it is off; a renamed alias (`f <- setClass`) slips through, which is acceptable for a guardrail whose job is to stop deliberate use.

# Decision record: one Rmath, one codec stack, and the clean-room numerical routines

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
- **`nlm`, `nlminb` and `polyroot` are written from the published algorithms**, not GNU R's code:
  Dennis and Schnabel's UNCMIN for `nlm`, a trust-region quasi-Newton method behind PORT's
  `nlminb` interface (the same interface and stopping messages, not PORT's exact iterates), and
  Jenkins and Traub's three-stage algorithm for `polyroot`, which also fixes the order of the roots
  GNU R returns.
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
  - `file_syntax_sites` collects every per-file site in one `syntax::walk_green` pass, and
    `lints::syntax_lints` walks the green tree, building a red node only where `may_lint` says a
    lint can fire.
  - The duplicate-type map is read once per file. `item_frame_binders` resolves an item's frame
    binders in one query, cached in `SalsaGlobals`.
  - `Name` and `Ty` are interned with `revisions = usize::MAX`. They are never garbage-collected.
    That costs nothing in a one-shot check. In the server the tables grow with the distinct types
    and names ever seen, which repeat heavily because types are structural.
  - The CLI creates every input at `Durability::HIGH` except `PackageMetadata.attached`, the one
    input it sets after analysis starts.
- **Impact.** About 26% less CPU, 30% less wall time and 70 MiB less peak memory on the
  benchmark code bases, with byte-identical findings. Invalidation is unchanged, because every new
  query is per file or per item; the one server-visible difference is that the `Name` and `Ty`
  tables never shrink.


## The checker presumes R code is correct where R's semantics leave the question open

- **Previous shape.** Inference treated every use as a demand. A formal read on any path was required at every call, a field a list literal lacks read as `NULL` (so `opts$force` on `list(spec = ...)` was a finding), `list()` was a closed empty tuple whose reads were errors, a vector partner in arithmetic pinned its operand to a scalar, a callback called only with forwarded `...` got the arity of its first use, and a `pkg::name` read of a shipped package was "unknown" until something attached it. Run over the GNU R corpus with typing on, this gave 3,112 errors on code that CRAN checks, essentially all false.
- **Target shape.** Each rule now asks what R does at run time and refuses only what R itself would fail on, or what is a near-certain typo:
  - A formal is required only when every path forces it (`collect_branch_uses` returns the forced set, `join_forced` intersects branches), and a same-name argument to a project function (`inner(finally = finally)`) leaves the decision to the callee.
  - A name a record-like list lacks reads `Unknown` with a strict origin, because R lists are open and code adds fields later; a near miss of a field is still an error. An empty `list()` reads `Unknown`, or contributes `NULL` inside a union, which is what the field-by-field accumulator idiom needs.
  - Logical widens under the numeric constraint (`widen_numeric_binding`), and a vector partner leaves the flexible operand open with a `double[]` result.
  - A callback called with nothing but forwarded `...` is `fn(...: Any) -> T`.
  - `pkg::name` on a shipped namespace is accepted without attaching it, since R's `::` loads without attaching.
- **Why `Unknown` rather than `NULL` for an absent field.** `NULL` is precise only when the literal is the whole value; in real packages the literal is a default that callers and later code extend, and a `NULL` type then spreads into `if` and arithmetic as false errors. `Unknown` keeps the read quiet and makes strict mode name it.
- **Impact.** The corpus count fell to 624 (`backlog.md`, "what `ry check` still reports on the GNU R corpus"). Correctness: fewer true positives on literal-only lists and qualified typos of inactive namespaces, both recorded as gaps. Incremental analysis is unchanged: every rule reads the item's own body or per-name projections.

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
  source, `docs/src/components/Venn.astro`, used by the landing page and `why-ry.mdx`.
- **The migration is sold as moving your code into the future, not as switching to ry.** The
  destination is the future of the code base (typed, checked on every edit, safe for agents to
  change, fast), and ry is the tool that takes it there, usable today on the code it already has.
  The pitch avoids "migrate to ry" and "switch to ry", and step 4 is called "Go fully typed" rather
  than "Move to ry's types". The blue-to-purple Venn story stays and reads as the code moving
  forward.
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
  people skim a landing page and the snippets carry the argument; detail lives in `why-ry.mdx`.
- **Motion shows the story; it never decorates.** The migration section sets the Venn beside the
  step tabs, and each step moves a "your code" marker into its region and dims the others (the
  `data-step` attribute on `Venn.astro`; without it the diagram stays static, as on `why-ry.mdx`).
  Runtime and migration snippets reveal code first and output after, once on first view and again
  on each tab switch, and one neutral pill slides between tabs (no colored outline or glow). Every
  snippet function uses a `{}` body. All motion is off under `prefers-reduced-motion`.
- **A tab appears only once its feature has a real snippet.** The runtime section gets a benchmark
  tab when the JIT benchmark table lands. The DataFrame tab shows the Polars-style API, never
  data.table's `x[i, j, by]` (kept for compatibility only), with one line for the
  `tools/bench/table` result. No placeholder tabs.
- **The migration has four steps, each with a real snippet and real output:** check the code you
  have, add types as `#:` comments, switch to inline types, then move data to ry's records, tags
  and tables under strict mode. Each step is optional and reversible, which is what makes adopting
  ry safe for an existing code base. `why-ry.mdx` tells it in full.
- **GNU R stays a first-class target.** `#:` comments keep a file plain R, so a team that keeps
  GNU R loses only the runtime features. The pitch says so in one sentence and does not dwell.
- **The benchmark is honest about typing.** Its three columns are GNU R, ry on the same untyped
  code, and ry on typed strict code. Typed code is not faster yet (`backlog.md`), and the pitch
  says so instead of implying that types buy speed. When typed runtime features do get faster, the
  third column shows it without a rewrite of the copy.
- **The docs sidebar follows the four steps** (`docs/astro.config.ts`): "1. Check the code you
  have" and "2. Type your code" hold classic R, "3. Switch to inline types" and "4. Go fully
  typed" hold what ry adds (strict mode sits in step 4). The landing page, `why-ry.mdx` and the
  sidebar use the same step names, so a pitch step leads straight to its docs. Pages were regrouped,
  not moved, so every URL and cross-link keeps working.
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
  or "declared union" concept. The user guide teaches it as a table in
  `type-checking/records-and-tags.mdx`; the contract is the reference section "Nominal records,
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

## data.table's helpers run data.table's own C, and the C API is what makes them fast

- **Previous shape.** The plan was to route R calls to `fread`, `fwrite`, `setkey`, `rbindlist`, `melt`, `dcast`, `shift`, `fifelse`, `tstrsplit` and friends to the engine's kernels in `table::ops` and `table::io`, which beat data.table when measured from Rust. Under ry's runtime those calls ran data.table's own C through libr, at up to 2.5 times GNU R's time, because every C API call (`TRUELENGTH`, `STRING_ELT`, `LENGTH`, `INTEGER`, the type predicates, `PROTECT`) entered the full error boundary.
- **What measurement showed.** With the boundary skipped on those calls, data.table's C under ry runs at GNU R's speed or faster: `fread` of the 1e7-row groupby file takes 2.3 s against GNU R's 2.8 to 3.1 s, and `transpose` (the core of `tstrsplit`) 0.062 s against 0.055 to 0.068 s at 1e6 rows. Routing `fread` to the engine's reader made it slower, 3.2 to 3.7 s: the reader itself takes 1.4 s, but turning its columns into R vectors (one CHARSXP per string) costs about 1.9 s. The numeric helpers are already a `memmove` or a single loop in data.table's C, so the engine has nothing to win there.
- **Target shape.** No routing hook for the helpers. They stay data.table's own code, so their output is data.table's by construction. Their speed comes from the C API (`crates/libr/src/vectors.rs` and `memory.rs`): an accessor whose argument is a plain, non-ALTREP object of the right type reads it directly, and enters the boundary only to raise an error. Every package with compiled code gains from this, not only data.table. The engine is fastest where data stays inside it: frames (`frame_read_csv` reads the same file in 1.4 s) and the `[` bridge, whose results are small.
- **The `[` bridge imports only what a query reads.** It plans against a schema read from the R vectors' types, `query::referenced_columns` names the columns the plan reads, and only those convert. A character column converts each distinct CHARSXP once, because R interns its strings, and then gathers the views. Converting every column used to make `x[, sum(v1), by = id4]` on the groupby table 20 times slower than data.table.
- **Impact.** One source of truth for each helper's semantics, data.table's own code. The checker still types `fread`, `rbindlist`, `melt` and `dcast` as returning frames, which is not true at run time (`backlog.md`, "The checker and the runtime disagree about data.table's readers").

## An escape lasts until the outermost native call returns

- **Previous shape.** A vector whose writable element pointer native code took (`STRING_PTR`, `DATAPTR`, `VECTOR_PTR` on a character vector or list) joined the escaped set for good, and every minor collection rescanned all of its elements. data.table calls `STRING_PTR` on its input columns, so after one `unique(x)` every later minor collection reread the 3e7 string elements of the groupby table, each read a cache miss on a CHARSXP header. `shift(x$v1)` went from 0.024 s to 0.14 s once any such call had run.
- **Target shape.** `Heap::retire_escaped` runs when the outermost native call returns (`crates/libr/src/boundary.rs`). Native code may store through the pointer only while that call runs, so each old escaped vector moves to the remembered set: the next minor collection rescans it once, then it is an ordinary object. GNU R's collector never sees those stores at all, so this is strictly safer than GNU R. While a call still runs, an old escaped character vector is not rescanned: a minor collection marks every young CHARSXP instead, which is sound because a character vector holds only CHARSXPs and they point at nothing. A young CHARSXP that is garbage then lives until the next major collection.
- **Minor collection budget.** The young budget is the larger of `GcPolicy::young_limit` (8 MB) and half the old generation, as GNU R's trigger grows with its heap, so a session holding a large table does not collect on every large vector it allocates.
- **Impact.** The single-loop data.table helpers run at GNU R's speed in a session that has called any data.table function, and `rbindlist` of two 1e7-row tables drops from 1.75 s to 1.26 s; `setkey`, `dcast` and `unique` now match or beat GNU R. `crates/heap/tests/test_gc.rs` pins that stores through an escaped pointer survive minor collections, before and after the escape is retired.
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
  item memos, about 35 ms before any request could answer. They now read `file_definitions`,
  `file_interface_reads` and `file_conditional_writers` (`crates/semantics/src/semantics.rs`), so
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
  derives every item's naming and the per-file folds (`file_interface_reads`,
  `file_conditional_writers`) across all cores, as `ry check` does. Until it ends, the first wave
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
- **Allocator API status (checked against the Rust docs, the tracking issue and releases.rs).**
  `std::alloc::Allocator` was nightly-only (`allocator_api`, rust-lang/rust#32838) until
  rust-lang/rust#156882 stabilized a subset for Rust 1.100.0: the `Allocator` trait, `Global`,
  `System`, and the allocator parameter of `Box<T, A>` and `Vec<T, A>` with their `new_in` and
  `with_capacity_in` families. 1.100.0 is scheduled for stable on 12 November 2026. The
  `Store` API, `StaticAllocator`, and `Box::into_pin` with a custom allocator stay unstable, and
  std's `HashMap`, `String`, `Rc` and `Arc` take no allocator parameter. CI builds with
  `dtolnay/rust-toolchain@stable`, the dev container has rustc 1.97.0 (which rejects
  `Vec::new_in`), and the nix flake uses the latest nightly. The workspace has no
  `rust-toolchain` file and no `rust-version`.
- **The route that works on every toolchain.** `allocator-api2` is the stable polyfill of the
  trait and of `Vec`/`Box`, and `hashbrown` (already a `heap` dependency) accepts it for its maps.
  `bumpalo` with its `allocator-api2` feature gives a bump arena those collections can allocate
  in, so `hashbrown::HashMap::new_in(&bump)` and `allocator_api2::vec::Vec::new_in(&bump)` build and
  run on stable rustc 1.97.0 (bumpalo 3.20, hashbrown 0.16, allocator-api2 0.2). Move to std's `Vec<T, A>` and
  `Box<T, A>` once every toolchain the project builds with is 1.100 or newer; maps stay on
  `hashbrown`. Whether `bumpalo` implements the stabilized std trait without its nightly
  `allocator_api` feature is unverified.
- **The global allocator stays mimalloc.** The `ry` binary sets it as `#[global_allocator]`
  (default feature `mimalloc`, lazy arena commit in `crates/ry/src/main.rs`), and `libR.so` has its
  own (`crates/libr/src/libr.rs`), pinned to `features = ["local_dynamic_tls", "v2"]`: the default
  TLS model fails `dlopen`, and v3 crashes in `mi_thread_init` on the second thread that allocates
  after the first one exits. Arenas sit on top of the global allocator, not in place of it.
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
- **Impact.** Fewer allocations and frees per phase, smaller memos, and scans that read contiguous
  memory. Incremental analysis is unaffected as long as memoized results stay owned and
  value-compared, which this rule requires. Each audit item is measured before and after, and a
  layout change that does not pay is reverted.
