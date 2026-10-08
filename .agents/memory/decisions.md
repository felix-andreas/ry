# Decision record: standard-library stub format

## DECIDED — dedicated declaration-only stub files

Stdlib type information ships as **dedicated declaration-only stub files**, not ordinary R files (the current `crates/analysis/src/stdlib_base.R` placeholder-body approach — `length <- function(x) 0L` harvested by ignoring the body — is retired). The accepted shape is the Expert's design: a flat list of `name : <type-expr>` declarations that **reuse the existing type-expression parser** (so the only new grammar is a thin declaration-line layer, not a second type system); a function body is unrepresentable, so "declaration-only" is enforced structurally the way `.d.ts`/`.pyi` do. Override precedence (project stub wins over shipped stdlib, selected by detected R version) reuses the existing `stdlib.rs` `load_with_overrides` fold; the assembled stub library stays a **set-once input kept out of the incremental dependency graph** (engine invariant — do not route stub files through `did_change`).

**File extension:** a dedicated one, name to be chosen by the CTO + Expert design pass — **not** `.Rstub` (that specific name was vetoed). Candidates: `.Rtypes` / `.Ri` / `.Rtypes` / `.Rdecl`; avoid collisions with `.Rd` (R documentation), `.Rmd`, `.Rda`/`.RData`.

**Overloads:** v1 uses `Any` for ad-hoc overloads only. Genuinely parametric higher-order functions (`lapply`, `Map`, `Reduce`, `identity`, …) must get **real generic types** via the existing `<T> fn(...)` polymorphism — they are NOT widened to `Any`. Only functions whose return type varies by argument type (`abs`, `rep`, `seq`) fall back to `Any`/`Incomplete` for now. The permanent solution — **traits in the type system vs. overload sets** — is deferred to a later decision. The declaration grammar must permit repeated declarations of one name now, so adopting either later needs no corpus rewrite.

**Syntax highlighting:** LSP semantic tokens first (one server implementation colours both inline `#:` annotations and the stub files, in every LSP client, reusing spans the server already computes); a tree-sitter/TextMate grammar for offline/static highlighting is a later nice-to-have that reuses the declaration grammar.

**Open questions routed to the CTO design pass:** (1) does `type_syntax` already expose a parse-a-bare-type entry point, or must one be extracted (effort driver); (2) per-namespace vs per-item stub file granularity, and whether editing a project stub live triggers a coarse (non-incremental) re-seed vs a restart; (3) `pkg::name` needs a real `NamespaceGet` HIR node (currently `Unsupported`) for re-exports — confirm deferral to the CRAN tier. The full Expert proposal (ecosystem precedent from `.pyi`/`.d.ts`, worked format examples, the precedence stack) should be folded into `docs/.../stdlib-stubs.md` when the format is built.

# Decision record: beta semantics & quality program

**Status: DECIDED (user-ratified).** Direction chosen after a full adversarial audit of the type checker core, LSP surface, formatter, config, stub corpus, and engine, with findings verified empirically. The prioritized execution list is `backlog.md`; the user chose **semantics-first sequencing** and delegated the flow-model and strict-mode calls ("do what is necessary to achieve best quality"; "we want a sound type checker; it is okay to not support every R construct"). The contract page (`reference/type-system.md`) is updated contract-first per Phase-1 item as it is implemented.

## R variable model — mutable slots with union joins

The fresh-binding-per-`<-` (let-shadowing) model is retired: it is not R. R assignment mutates the current function-scope environment, so a branch or loop assignment must be visible after the construct. Decided model:

- A function scope holds **mutable variable slots**; each `<-`/`=`/`->` writes the slot, `<<-`/`->>` walks the lexical chain to the nearest enclosing slot.
- A read sees the **union of reaching definitions** at that point (a conditional write joins with the prior type; a loop-body write joins across iterations).
- Unused detection falls out: a **write** that no read reaches is unused (report assignments, not bindings).
- Flow-sensitive *narrowing* (`if (is.null(x))`) is a later layer on the same model, not part of this decision.

Rationale: the old model was verified unsound (`x <- 1L; if (f) x <- "two"; x + 1L` typechecked clean and crashes at runtime) and produced unused-lint false positives on the two most idiomatic R patterns (conditional update, loop accumulator). No lint-local fix existed.

## Multi-member unions — join/annotation-only, never in unification variables

General unions `A | B | C` (normalized: flat, deduped, order-insensitive; `T | NULL` becomes the special case) are adopted for joins and annotations. **The HM-speed guardrail:** a union imposes no union *constraints* on inference variables (plain substitution-binding of a variable **to** a union value is permitted, like any other type) — unification stays syntactic (a union unifies only with a structurally equal union); all member-wise directional logic lives in `check_compatibility`. This keeps inference decidable and fast and matches the existing unification-is-the-invariant-floor split. Tags/discriminated-union `match` (post-beta) builds on these unions.

## A type error inside a test assertion stays reported

`expect_error(f("bad type"))` is how an author tests that a function refuses bad input, and the
call inside it really is type-incorrect, so the finding is *true*. The question was whether an
expectation that asserts a condition should suppress type findings in its payload.

**It should not.** Blanket-suppressing inside `expect_error` and its siblings would also silence a
genuine mistake in the test — a misspelled callee, the wrong argument passed by accident — and test
code deserves the same checking as everything else. It would also be a special case for one
function family with an open-ended tail (`expect_warning`, `expect_condition`, `expect_snapshot(error =
TRUE)`, `tryCatch`, `try`), which is the kind of accretion the design bar refuses.

The answer is the general mechanism that already exists: `# roughly: allow(type-mismatch)` on the
assertion. It is explicit, it is local, and it says what it means. Documented on the diagnostics
page under "testing that something is rejected".

Worth revisiting only for the stronger form TypeScript's `@ts-expect-error` has: a suppression that
*itself* reports when the expected finding does not appear, turning "ignore this" into "assert a
finding here". That is a feature, not a special case, and it would apply to every code.

## The type system is Hindley-Milner, and stays fast to check (user directive)

**Only admit features that are fast to check — which means HM.** The bar for any proposed addition to the type system is: does it keep inference to unification over the existing constraint mechanism, decidable and linear-ish, with no search and no global solving? If not, it does not go in, however useful it looks. Soundness and speed are not traded for expressiveness.

What this rules out, so nobody re-derives it: **type classes / traits** (declined, not deferred — see the tripwire note in `contributing/design/open-questions.md`), **general subtyping** (subtype inference is a different, slower algorithm than unification; a declared *coercion* at a named boundary is HM and is the shape any variance work must take), and any construct requiring backtracking search over a program-wide constraint set.

**The sanctioned exception is declaration files.** A `.Rtypes` stub may do things a user's own annotated code may not — today that means ad-hoc overloading. The exception is bounded on purpose: the cost of a non-principal feature is proportional to how much code it applies to, and a stub surface is a fixed, curated corpus the project itself maintains, not user code. It is also where the need is real: R's base library was never designed with types, so no principal scheme describes `min` or `abs`, and a gradual checker that cannot describe the standard library is not usable at all. A user's `#:` annotation stays pure HM, which is what keeps the *user-facing* promise ("your code, checked, fast") honest.

## Overload sets — bounded ordered probes

Functions whose result type depends on the argument type get **ordered overload sets** (stub surface first; repeating a name within one `.Rtypes` source appends a candidate; a later source replaces a name's whole set). Call sites try schemes in declaration order using the existing probe-then-rollback machinery; first compatible match wins. Principal-type purity is knowingly relaxed *at overload sites only* (declaration order is semantic — the TS/mypy model).

**Scope is the point, and it is enforced, not conventional:** only a plain or namespace-qualified name whose declarations come from a `.Rtypes` source can be overloaded (`GlobalEnv::overloads` reads the stub library and returns `None` the moment a script or package binding shadows the name). A project's own R code cannot declare an overload set, and should not gain the ability — see the HM record above. Project override stubs *can*, because a `.Rtypes` file is a declaration file for foreign code either way.

**Overloads are the escape hatch, not the mechanism, and the corpus is the pressure gauge.** Of the 35 sets the corpus declares, about a dozen are atomic-family promotion (`abs`, `min`, `sum`, the `cum*` family — a numeric constraint in disguise), about a dozen are shape dispatch (`head`, `rev`, `Filter`, `lapply` — a stand-in for shape-mirroring returns, which also explains why `abs` needs one candidate per shape *and* per family), nine are S3 operator method tables (`+.Date`, `Arith.difftime` — dispatch, which needs a multi-entry table under any design and would survive untouched), and two are genuinely two-form functions. So two thirds are workarounds for two absent features, both of which are HM-compatible and both in `backlog.md`. Watch that ratio: a rising count of promotion/shape sets is the signal to build those two features, never to design traits.

Three call-site rules keep selection sound (implemented in `try_overloaded_call`):

- **Arguments are inferred once, before any probe.** Expression inference writes environment/recorded-type state the probe snapshot does not reverse, so probes run only instantiation + argument *matching* (the signature-matching half of function-call inference is split out as `match_arguments` for exactly this reason).
- **A fit is a fact or a guess.** The caller's open inference variables are recorded before probing (`collect_unbound_vars`); a candidate that fits while leaving every one of them untouched — same representative, same entry, so binding, redirecting and constraint-tightening all count as touching — was chosen by the concrete arguments, and it beats a candidate that fits only by narrowing them (which would over-commit a `function(x) sum(x)` wrapper). Facts beat guesses, and **among fits of the same kind declaration order decides — the first wins**, which keeps one reading of order everywhere (first-match at call sites, most-specific-first in the corpus, and the last declaration for a value use of the name). A lone fit is never a guess — it is the only signature that accepts the call. Probing rolls back, so the winner is re-probed to commit; matching is a pure function of the table, which is back in its pre-probe state, so the fit repeats.
  - **A last-fitting-wins tiebreak was tried and removed:** its purpose was to keep `function(x) sum(x)` from committing to a narrow candidate, and the fact rule already does that — a general fallback taking `Any` accepts *without binding*, so it is a fact and outranks every guess above it. Last-wins additionally forced any set whose candidates differ only in a *sequence* shape (`lapply`, named list in → named list out; a lambda callback makes every candidate a guess) to be declared most-general-first, which contradicts both the corpus convention and the value-use rule, which resolves a non-call use of the name to the LAST declaration and would then hand out the narrower contract.
- **Strict-then-courtesy rounds.** The whole-number-literal-as-integer courtesy is disabled in the first selection round (`sum(1, 2)` must pick the double candidate — R computes a double) and re-enabled in a second round only when nothing matched strictly (a name whose only fitting candidate wants `integer` still accepts `foo(1)`). Exact matches outrank conversions.

**All-fail wording: name the set only when the candidates disagree about what is wrong.** `NoMatchingOverload` ("no overload of `f` matches — I tried all N declared signatures", plus the first candidate's failure as a hint) is right when the call could have meant any of several shapes and each rejects it elsewhere or differently — `pick("word")` against `fn(integer)` and `fn(double)` is refused at the same argument by both, and neither reason is *the* answer. Two failure shapes have a single answer, and the wrapper buries it along with the argument's own range (it blames the whole call): every candidate failing for the *identical* reason, and one candidate getting strictly further into the call than any other — measured as the index of the first argument each blames, with a whole-call verdict (arity, unknown name) counting as no progress. The deeper candidate is the signature the caller meant: a two-parameter callback handed to `lapply` is refused at the callback by the candidate that accepted the sequence and at the sequence by the candidate that wanted a named list, so the callback is the finding.

## R's object systems — the boundary is dispatch, not Hindley-Milner

A recurring claim is that S3/S4/R6 support is fundamentally at odds with a Hindley-Milner core, and
that supporting it therefore costs soundness or speed. **That is the wrong diagnosis, and it is
recorded here so it is not re-derived.** The declarative core of all three systems maps onto
machinery the checker already has and already relies on: nominal types with a checked representation
(`@type` + `@new`), record field projection on a nominal, and declaration-ordered overload sets.
The existence proof is that **S3 dispatch already runs inside the inference core**: an operator on a
nominal is dispatched through `+.Class` / `Arith.Class` / `Ops.Class`, statically, soundly, today.
S4 inverts the claim hardest — `setClass` is a record declaration with slot types written literally
in source, `new(...)` a named-argument constructor call, `x@slot` a field projection,
`setMethod(signature = ...)` an overload candidate — so it is *more* statically declared than the
`#:` annotations the checker already consumes. Hand-writing the equivalent (`@type` + a wrapper
constructor carrying `@new`) yields full checking of slot types and constructor arity with no strain
on inference at all; the gap is a lowering pass, not type theory.

**Three things genuinely do keep dispatch out, and they are the real reasons:**

- **Dispatch needs a class known at the call site.** Inside an unannotated `function(x) speak(x)` the
  argument is an open inference variable, so there is nothing to dispatch on and the only sound
  answer is `Unknown`. R code is most dynamic exactly where dispatch matters most, so generic
  dispatch structurally underdelivers where it would be used. (Same shape as overload selection with
  a flexible argument: never guess, fall back.)
- **Inheritance is subtyping, and there is none.** `TyKind::Named` matches by exact name (type
  arguments aside); no hierarchy exists anywhere in the compatibility relation. S4's `contains=` and
  R6's `inherit=` require one, and nominal subtyping is a new axis in `compatible` and in the
  union/join rules — the one place where a soundness regression is a real risk rather than a worry.
- **A generic's method set is global mutable state, which fights the interface firewall.** Any file
  may add `print.foo`. If "the methods of generic G" is an input to every call of G, one new method
  invalidates every call site in the workspace. Incremental support needs a separately memoized
  method-set query per generic, so that adding a method invalidates only calls to that generic and
  editing a method body invalidates nothing. Getting this wrong turns a single edit into a full
  revalidate at 300k LoC.

**Therefore one decision splits into three, decided separately:**

1. **False positives are defects, not deferred features.** `setGeneric("f", ...)` not defining `f`
   (so every call to a project's own S4 generic reports `unresolved`) and the absent R6 stub
   (`R6::R6Class` reporting an unknown namespace) are bugs. They cost nothing in soundness or speed
   and are fixed independently of any object-system ambition.
2. **Declarations may become nominals; this is the cheap win.** Recognizing `setClass` and
   `R6Class(public = list(...))` as class declarations that produce a nominal with typed slots or
   fields catches the mistakes users actually make (slot and field typos, constructor arity) with no
   subtyping, no new global state, and per-file firewalling like any other item.
3. **Dispatch and inheritance stay unmodelled** on the three grounds above, and an unmodelled
   construct stays `Unknown` — a coverage limit reported by strict mode, never a guess. Revisit only
   with a memoized per-generic method-set query and a decided nominal-subtyping design.

## `T[]` — atomic-element constraint, not traits

The core vector generalizes to carry an element *type* (so it can hold a variable), with a new **atomic-element constraint** kind on inference variables — the same mechanism as the existing numeric constraint (`<T: numeric>`), rendered `<T: atomic>`. This resolves the former open question (typing-design §1) in favor of option (a): the constraint mechanism is already built, proven, and fast; a trait system is not justified by this need alone.

Implementation notes: valid vector elements are `Scalar(_)`, a constrained `Variable(_)`, and `Any`/`Unknown` (statically untracked element). `Constraint` became a proper lattice merged via `join` (not `Ord`): numeric ∧ atomic-element = `ScalarNumeric` (a scalar `integer`/`double`, rendered `<T: scalar numeric>`), which defaults to `double` at binding boundaries like plain numeric. Annotation lowering records the atomic-element bound directly on the element variable's entry rather than through `constrain_type`, because the element may be a rigid `<T>` binder — the annotation itself makes the promise there, while a function body must not add bounds the annotation never declared.

## Coercion policy at parameter positions

Three verified false-positive factories are fixed at the compatibility level: whole-number `double` literals are accepted at `integer` parameters (generalizing the rule `:` already had); `integer` widens to `double` at parameter positions; vectorized stdlib stubs declare `T[]` parameters so scalars coerce up (instead of scalar parameters falsely rejecting vectors). Rationale: with the old policy, the *more precise* a stub was, the more false positives it produced (`seq_len(10)`, `toupper(c("a","b"))`, `round(x, 2)` all errored) — which pressure-cooked the corpus toward `Any`.

## Signature matching is name-aware

Annotation-vs-definition parameter matching by flat position (spec'd and implemented) was an accepts-then-crashes soundness hole: R call sites match by **name**, so a positional zip routes values to wrongly-typed formals. Both the contract and the implementation switch to name-aware matching where names exist.

## Strict mode

Strict = **Unknown origins are errors AND unresolved-name references are errors** (they stop being plain naming warnings under strict); a recursion-induced `Unknown` return is an origin. Explicit `Any` remains the sanctioned escape hatch — "allowed unknown" is expressed by annotating, never silently. **Per-file toggle:** a top-of-file `#: @strict` / `#: @strict off` directive overrides the config default in both pipelines (the gates are already per-file; derive the directive from the parse so incrementality holds). Sound-by-refusal is acceptable policy: an unsupported construct may be refused loudly; it must never be silently mistyped.

## A reported column counts characters; caret art counts terminal cells

The CLI reported byte columns, in the rendered header and in `--output json` alike, and padded the caret by the same byte count. On any line carrying non-ASCII text before the finding — which R source does as soon as a string holds a name, a unit, or an em dash — the number disagreed with every editor and the caret sat visibly to the right of the code it accused, sometimes past the end of the line. Byte columns served nobody: a consumer would have to re-read the file as bytes to use one, and no editor or reviewer counts that way.

Two units, because the two jobs are different. **A column is characters** (`LineIndex::line_column_chars` for the JSON `column`/`endColumn` fields and the server's human-readable `file:line:column` strings; the CLI's snippet source re-counts miette's byte column the same way for the report header) — the number a person can act on. **Underline art is terminal cells**, because a CJK character or an emoji occupies two of them and a character count would under-pad; the graphical reporter measures them, so the two units never have to be reconciled by hand again. The JSON field documentation changed with it (a stated contract, so the change is deliberate and recorded here). The LSP protocol path is untouched: it converts to the negotiated encoding — UTF-16 by default — at the protocol edge, which is what LSP specifies and what byte columns were never meant to serve.

## The CLI reports through miette, not a hand-rolled renderer

**Previously** every user-facing message the CLI printed was assembled by hand. `render_human_diagnostic` computed a gutter width, sliced the finding's first line out of a `LineIndex`, padded a caret row by terminal cells, truncated multi-line ranges with a dim note of its own wording, and printed related locations as one-line `= note: … --> path:line:col` trailers. Beside it, a second path styled `error: ` / `warning: ` prefixes with `console` and printed an underlying I/O failure with a bare `eprintln!` on the next line, and `ConfigParseError` built its own `in <path> for <key> at line L, column C` sentence. Three renderers, three notions of where a message points, and every improvement (a related location deserving its own snippet, a config error deserving its own line of source) meant more hand-rolled layout.

**Target shape:** one report type in the CLI, drawn by `miette`'s `GraphicalReportHandler`. A report is a severity, an optional diagnostic code, a message, an optional cause, an optional snippet (named source + labelled range), and nested related reports — enough for a finding, a companion note, and a bare failure alike, so the same reporter draws all of them. Errors that know their own source implement `miette::Diagnostic` themselves (`ConfigError` carries the config text and the toml span), and the CLI hands them to the same entry point. The look follows the destination: colour and unicode on an attended terminal, monochrome unicode when colour is refused, plain ASCII into a pipe or a file.

**Impact.** *Correctness:* the underline is the library's, so multi-byte and wide glyphs are handled in one audited place; related locations are drawn from their own file instead of being reduced to a coordinate; a configuration failure is shown rather than described. *Simplicity:* the caret/gutter/width arithmetic is gone, and `RelatedNote` resolution moved out of the parallel worker into the sequential render step, so the workers now return plain diagnostics. *Performance:* the reporter is built once (`LazyLock`) instead of re-derived per finding, and the snippet source is borrowed — a custom `SourceCode` impl rather than `NamedSource`, which owns its text and would copy the whole file into every finding reported against it. *Incremental analysis:* untouched; this is presentation, and the JSON Lines contract is unchanged.

**Two things miette does not do for us.** It counts the header column in **bytes**, so the borrowed-source `read_span` re-counts it in characters — otherwise the human header and the JSON record would disagree about the same finding. And it draws every line a range covers, so a range spanning more than three lines is clamped to its first line with the reach stated in the label; a finding on a long item must not print the item.

**The theme is ours, not the stock one.** A snippet is a window on the source, not a box: the gutter runs unbroken down every row (`vbar_break = vbar`, in place of the stock dotted `·` row), the header opens with a plain rule rather than a corner (`ltop = hbar`), the range is underlined with carets (`underline = '^'`), and the closing rule under each snippet is dropped — one line saved per finding, and a run of findings prints dozens. The closer is the one part that is not themeable: miette hardcodes it from `lbot`+`hbar`, both of which the cause-chain arrows and multi-line span brackets also draw, so it is filtered out of the rendered string instead, matching only a line that is *nothing but* the rule (a nested report indents its own, in the parent's colour).

**The README's hero image is generated, and its renderer had to learn the palette.** `scripts/render-diagnostic-svg.rs` runs `ry check` under a pty and turns the ANSI it emits into an SVG, because GitHub strips escapes from code blocks but honours a `<style>` block inside an embedded SVG. Its SGR parser matched whole escape bodies, which handled the previous renderer's single-parameter codes and silently dropped every compound one the graphical reporter emits (`36;1;4` for the underlined locus, `35;1` for the caret) — the failure is a *monochrome image that still looks plausible*, so the render is worth an eye after any change to the palette, not just a re-run. Parameters are applied one at a time now, with cyan/magenta/green/yellow and underline added. The snippet-closing-rule width is read off miette 7.6, which hardcodes it; `check_draws_the_snippet_as_a_window` fails if an upgrade moves it, verified by changing the constant.

## Annotation formatter — parse, then pretty-print

The per-line re-indent walk (no bracket matching, lines never split) is replaced: join the `#:` block, parse it with the real `type_syntax` parser, pretty-print with **per-bracket hug bits** (an opener followed by content on its own line stays hugged; its closer mirrors it), honoring `indent_width`; on parse failure the block is left verbatim (which also ends prose corruption, and removes the drifted duplicate tokenizer). Both the fully expanded and the hugged style are stable fixed points; mixed shapes normalize.

## Config subsystem

Workspace root comes from `InitializeParams` (never process CWD); discovery = nearest `roughly.toml` ancestor of the target, identical in LSP and CLI; unknown keys are **errors with toml spans**, surfaced as diagnostics on `roughly.toml` plus a window message — never a startup panic; config reload triggers a diagnostics refresh; one config struct chain end-to-end (the four parallel representations collapse).

## Guard narrowing

Flow-sensitive narrowing is implemented as **branch-edge entry refinement on the slot model**, not
a separate flow analysis: a recognized guard condition (`is.null(x)`, the `is.*` family, negation)
computes refined types for the guarded slot's two edges, and each refinement is an ordinary
undo-logged environment write inside the branch region — branch writes replace it, the region
rollback reverts it, and the branch join sees final values with no new machinery. Early-exit
persistence falls out of divergence-aware joins: a branch that never falls through (`return`,
`stop`, `break`, `next`, blocks ending in them) contributes neither value nor state, so the
surviving edge's refinement (and only it) applies after the `if`.

Deliberate limits, for soundness and zero false positives:
- **Union member filtering only.** Family guards do not invent a shape for `Any`/`Unknown` — a
  refined `character | character[]` would false-positive against scalar-claim stub signatures.
  The one non-union refinement is `is.null`'s true edge on `Any`/`Unknown` → `NULL`.
- **Statically undecidable members stay on both edges** (inference variables, flexible-element
  vectors, opaque nominals — `is.list(data.frame)` is true at runtime).
- **Local slots only** (parameters, function/script locals). Package globals keep winner
  semantics; guarded *expressions* (`is.null(x$field)`) are not tracked.
- **No `&&`/`||` decomposition** and no in-condition narrowing (the right conjunct of
  `!is.null(x) && x > 0` does not yet see the refinement) — recorded follow-ups.
- The guarded key is resolved exactly as a read resolves (local slot under a naming context; the
  flat global entry in context-less fixture states), so fixtures and production share one path.

## Expanded annotation syntax: `@param name {TYPE}`

**DECIDED (user-proposed, ratified).** The expanded parameter directive is `@param name {TYPE}` / `@param [name] {TYPE}` — name first, braced type second — replacing the JSDoc-ordered `@param name {TYPE}`. Rationale: (1) the wrapping payload (the type) becomes the trailing element, so multi-line types continue cleanly under the directive instead of leaving the name dangling after the closing braces; (2) one shape across all directives (`@type Name {TYPE}`, `@alias Name<T> {TYPE}`, `@param name {TYPE}`); (3) the grammar becomes unambiguous — the name is a single identifier token right after the directive, which retires the swallow-the-tail-as-name ambiguity and leaves room for an optional trailing description as a first-class extension. The old order is rejected with a targeted error naming the new form, not a generic parse failure. `@return {TYPE}` / `@forall` are unchanged.

# Decision record: error-tolerant lowering (syntax errors do not erase a file)

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate).

Previous shape: any error node in a tree short-circuited `lower_with_diagnostics` to an EMPTY module, so one half-typed keystroke dropped the file's whole export set — the package symbol index re-folded on every keystroke inside a broken window, dependents flooded with unresolved-name errors, and diagnostics/hover/completion for the rest of the file went dark. The single source of truth for "what exists mid-edit" was the parse tree's error bit, applied at file granularity.

Chosen shape (statement granularity, rust-analyzer-style):
- Well-formed statements always lower; a broken file keeps every export whose statement parsed.
- A broken statement contributes NOTHING (no names, no reads, no cascading diagnostics) — "a broken region reports its syntax error and nothing else" (typing-reference §Syntax errors is the contract).
- Two salvage shapes inside broken regions: (a) sequence-level — an ERROR node's well-formed assignment children lower normally, fragments are filtered by kind (`ExpressionKind::Assign` keeps, everything else drops), and a well-formed non-assignment fragment sharing a line with a following ERROR sibling is dropped as the split half of the broken statement; (b) expression-level — a broken assignment with an intact name side keeps its definition with the value degraded to `ExpressionKind::Missing`.
- `Missing` is a distinct HIR kind from `Unsupported`: both type `Unknown`, but `Missing` records no strict origin (the syntax error already covers the region) while `Unsupported` stays strict-relevant (a complete construct the checker cannot model).
- A checked annotation on a `Missing`-valued definition binds its DECLARED type unchecked (a hole proves nothing; demanding proof is a guaranteed false mismatch), so annotated definitions keep their contract for callers mid-edit — zero downstream invalidation.

Impact: correctness — no false unresolved/type errors from half-typed code (pinned by `test_malformed_lower` controls + the `error_tolerant_lowering` project fixtures); simplicity — one policy sentence governs all cases; performance — the malformed-flip engine witness went from "2 index refolds + referrer recheck" to "0 refolds, edited-file-only recheck" (exports byte-equal across the flip, early cutoff does the rest); incremental analysis — the highest-churn edit state (typing inside a construct) now has the smallest blast radius.

# Decision record: is.null shaping of unconstrained inference variables

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate).

Previous shape: at an `if (is.null(x)) fallback else x` join with `x` and `fallback` both unbound inference variables, the join unified the two variables (the type model deliberately never invents a union *for* a variable — a long-standing HM-speed decision). `or_else(NULL, "text")` then bound the single variable to `NULL` from the first argument and rejected the second: a false positive on the coalesce/or-default idiom, previously recorded as a structural design tension with annotation as the workaround.

Chosen shape: **the guard itself carries the missing information, so consume it.** In `condition_refinement`, when the recognized predicate is `is.null` and the guarded local slot resolves to a *completely unconstrained* inference variable `A` (entry `Unbound`, constraint `Unconstrained`), bind `A := T | NULL` for a fresh `T` — a union shape the model already supports from annotations (`@param x {T | NULL}` produces exactly it) — and let the existing member filtering narrow the edges. No new type form, no deferred unions, no join special-casing: the coalesce body then joins `fallback` with the narrowed `T`, generalizing to `<T> fn(value: T | NULL, fallback: T) -> T`, byte-identical to the verified-clean annotated form. Negated guards work through the existing edge swap; constrained variables (a numeric bound contradicts a NULL member) and rigid/declared parameters (the annotation is the contract) are never reshaped.

Deliberate consequence, pinned as a fixture: testing a parameter for `NULL` and then using it *unguarded* afterwards is now a genuine finding (`if (is.null(x)) 0L else 1L; x + 1L` errors) — the test declared `NULL` a possible inhabitant, and the annotated form of the same code already behaved this way, so the model is now consistent rather than lenient-when-unannotated.

Impact: correctness — kills the last recorded idiom false positive from the sweep, with zero regressions across every fixture suite and the realworld corpus (no expectation changed anywhere else); simplicity — ~20 lines in one function, reusing the union machinery end to end; performance — one extra variable + union per shaped guard, negligible; incremental analysis — unaffected (a per-file inference detail).

# Decision record: data-masked NSE resolution (data.table / with-family)

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate; user-reported pain: data.table code drowned in could-not-resolve warnings).

Previous shape: every bare name in `DT[region == "west", .(total = sum(amount)), by = product]` failed lexical resolution and produced a could-not-resolve warning (plus type errors from the base index-arity rules), because data.table evaluates i/j/by in the data's own frame — the same false-positive class R CMD check and lintr hit on NSE code. `:=` lowered to `Unsupported`, hiding its operands from the IDE entirely.

Chosen shape — structural mask recognition in the naming walk, diagnostic suppression at the emission edge:
- Two recognizers set a mask depth during resolution: a `[` bracket whose arguments carry an unambiguous data.table signature (`by =`/`keyby =` argument names; `:=` or `.()` calls; `.SD`/`.N`/`.I`/`.BY`/`.GRP`/`.EACHI` symbols — none of which occur in base indexing, so `m[i, j]` is untouched), and the base masking family `with`/`within`/`subset`/`transform` (callee not locally shadowed), which masks arguments after the data.
- A read that fails lexical resolution inside a mask is recorded in `NamesLocal.masked_reads` **in addition to** `non_locals` — masked names still resolve stubs and package globals normally (a `sum` in `j` keeps its scheme; the first design that kept masked reads out of `non_locals` lost stub typing and was revised). Both could-not-resolve emitters (the analysis package pass and the engine's per-file query) skip masked ids; the typecheck fallthrough for unresolved non-locals was already silent `Unknown` with no strict origin.
- The recognized bracket itself lands in `NamesLocal.masked_subsets` and types as silent `Unknown` before the base index-arity rules (`[.data.table` returns shapes those rules must not judge).
- `:=` now lowers as an ordinary binary call so its operands stay visible to naming, hover, and the mask.

Deliberately NOT silenced: anything that resolves (locals, stubs, globals) keeps full checking inside masks; base indexing keeps lexical resolution and its warnings. Strict mode stays silent on masked column reads — NSE is a recognized dynamic construct with intended semantics, not an unmodeled hole. Future extension recorded in the backlog: honor `utils::globalVariables()` and generalize the mask marker into `.Rtypes` stub syntax so dplyr verbs can declare masked parameters.

Impact: correctness — kills the dominant NSE false-positive class with zero regressions (all suites; base-R control fixtures pin the non-masked behavior); simplicity — one mask-depth integer and two sets on the existing naming result, suppression at the existing emission edges; performance — a shallow per-bracket marker scan, negligible; incremental analysis — unaffected (per-file naming facts).

# Decision record: monomorphic recursion for function-valued assignments (letrec typing)

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). The naming half (a closure RHS sees its own target binding) landed separately; this is the typing half.

Previous shape: the recursive read inside `fact <- function(k) ... fact(k - 1L) ...` resolved (post-naming-fix) to a slot with no environment entry yet, typing as silent `Unknown` — the recursion contributed nothing to the function's scheme and bad recursive signatures went unnoticed.

Chosen shape — the classic `let rec` rule, scoped to function-valued assignments: before inferring the RHS, the target slot is pre-bound to a fresh inference variable (created inside the binding's generalization level); the body's recursive reads unify against it; the variable then unifies with the inferred function type. Recursion is **monomorphic** (all recursive uses share one instantiation; no polymorphic recursion — undecidable in general). Applied on the local-assignment path in both context and context-less inference (fixture drivers pre-bind the global slot). One subtlety: the top-level package-winner path writes the final generalized scheme to the global entry while `exported_value_schemes` reads the per-site *local* entry first — the pre-bound placeholder must be overwritten there too, or cross-file consumers see a dangling variable as `Unknown` (caught by the project-suite fixtures; the winner path now writes both entries).

Deliberate scope limits, pinned by fixtures: `<<-`-defined recursion keeps the silent-`Unknown` behavior (rare; the enclosing-slot join semantics make a placeholder ambiguous); mutual recursion between two local closures stays a loud unresolved reference on the earlier-defined one (letrec visibility is per binding, not per block — full block-letrec is a recorded possible extension); top-level mutual recursion already resolves through the package interface fixed point, whose oscillation guard pins genuinely cyclic schemes to `Unknown`. Strict attribution for top-level recursion whose converged scheme retains `Unknown` (an origin on the binding) remains the open part (b) in the backlog — it needs identical attribution in both pipelines to keep the differential byte-exact.

Impact: correctness — recursive helpers get real schemes and violations of recursively-inferred signatures are caught (`countdown("not an integer")` errors; previously silent); the polymorphic-identity and cross-file suites pin zero regressions; simplicity — one pre-bind + one unify on the existing assignment path; performance — one extra variable per function-valued assignment, negligible.

# Decision record: bare-name resolution stays ungated by NAMESPACE imports

**Status:** decided (agent-owned decision under the delegated ownership mandate); the full import model stays post-beta.

The fork: should a bare name resolving only via the stub corpus (`median` → the stats stub) require the package to import it (`importFrom(stats, median)` / `library(stats)`), or resolve unconditionally?

Decision: **unconditional resolution against the shipped corpus is correct, not a shortcut.** The shipped namespaces — base, stats, utils, methods, graphics, grDevices — are exactly the packages R attaches in every default session, so a bare `median` genuinely resolves at run time in the environments R code actually runs in. Project `.Rtypes` stubs are explicitly user-authored: writing one declares "my project uses these names" — gating them on NAMESPACE entries would only make the user say the same thing twice. What NAMESPACE gating would really buy — per-file visibility for *non-default* packages, masking warnings, `library()` attach ordering — requires the full import model (typing-design §6), which remains post-beta; when that lands, gating falls out of it naturally rather than being bolted on ahead of it. Until then, the NAMESPACE surface stays what it is today: import validation (typo detection against the corpus) and the opt-in `unused-import` lint.

Impact: no behavior change — this closes the fork by ratifying the current shape and pointing the future work at the import model.

# Decision record: top-level mutual recursion as whole-file letrec groups; self-recursion stays tolerant

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate).

Previous shape: any top-level recursion — self or mutual — resolved through the package interface fixed point starting members at `Unknown`, and arithmetic/joins over `Unknown` cannot sharpen, so every recursive function exported an `Unknown`-flavored scheme; an annotated consumer (`#: logical` on `is_even(4L)`) false-errored `expected logical, found Unknown`.

Chosen shape, in two deliberate halves:
- **Mutual groups (≥ 2 members) are letrec groups.** A module pre-pass detects candidates (top-level function-valued assignments, last writer per symbol), overapproximates reference edges by source-range containment, and pre-binds every member on a *mutual* cycle to a fresh variable **one level below module scope** — the load-bearing subtlety: unification level-adjusts the group's shared variables up to the placeholders' level, so if the placeholders sat at module level, finalization's generalize would quantify nothing and the export would carry a *free* variable, which `import_scheme` on a consuming document erases to `Unknown` (found empirically: the producer's table was perfect while consumers saw `Unknown`). Members stay monomorphic through the module walk (siblings constrain each other), then one finalization pass exits the group level, defaults escaping numerics, generalizes each member, and rebinds both its environment keys. `is_even`/`is_odd` now exports `<T: numeric> fn(n: T) -> logical` and consumers check.
- **Pure self-recursion at top level keeps the tolerant fixed point.** A first implementation applied letrec to self-loops too — and the realworld corpus immediately caught the cost: the idiomatic tree fold (`if (is.list(x)) sum(sapply(x, sum_leaves)) else x`) needs the recursive type `T = double | list[T]`, which HM cannot express; monomorphic recursion pinned the parameter to `double` and the nested-list call site false-errored. The old `Unknown` is load-bearing gradual tolerance for exactly this shape, so self-loops are excluded on purpose (the earlier whole-file variant also made every top-level function monomorphic in-file, breaking polymorphic reuse — `mirror(1L); mirror("x")` — which is why the scope is cycles, not all definitions).

Fixtures pin all three behaviors with the rationale in-file: the typed mutual pair (project + context-less), the tolerant self-recursive package function, and the tree fold staying clean. Local (in-function) recursion is typed by the per-assignment letrec from the earlier slice; both pipelines share `check_module_with_naming`, so differential parity held with no pipeline-specific code.

Impact: correctness — mutual recursion (previously always `Unknown` + consumer false positives) now types precisely, with zero regressions across every suite and the corpus; simplicity — one pre-pass, one finalization, a shared member map; performance — a per-module candidate scan bounded by arena size, negligible. Open remainder recorded in the backlog: strict attribution for the deliberately-`Unknown` self-recursive schemes.

# Decision record: architecture — hand-written parser, rowan syntax trees, salsa queries

**Status:** implemented; this is the rationale for the shipping crates.

## Why

The previous implementation's load-bearing limits were structural: tree-sitter caps syntax-error quality (opaque ERROR nodes), costs ~8µs/LoC (derived: ~2.5s of the 3.7s cold pass at 302K LoC is parse) and 60× source-size trees (measured via `analysis-stats`, forcing the rope-only/LRU machinery), and cannot see `#:` annotations (forcing the re-lexing subsystem over a reconstructed buffer); the analysis unit is the whole file (keystroke floor = whole-file re-inference; file-granular interface edges manufacture cluster SCCs); `CoreType` is a deep-cloned enum (allocation churn caps inference near a measured ~6µs/LoC; interned types are projected — external precedent, not yet measured here — to reach ~1); the engine is single-threaded by design. Each was diagnosed and mitigated in place; the mitigations are workarounds around the architecture, not the architecture.

## Target shape

The crate graph enforces the layering (crate boundaries are the compiler-checked analog of "make illegal states unrepresentable"):

- `crates/syntax` — hand lexer (`#:` annotations lexed as structured trivia and parsed as first-class nodes with real spans), hand recursive-descent/Pratt parser, rowan green/red trees (lossless, width-only → position-independent subtrees; resident — the tree-bytes-per-source-byte ratio is *measured* on the real corpus at Phase 1 and feeds the memory gate; "~2× source" is an unsourced estimate until then), typed AST views, optional statement-level incremental reparse (an optimization, not load-bearing — see the corrected incrementality note below and in the appendix), hand-tuned recovery and Elm-quality syntax errors. Annotations are ONE internal concept with pluggable surface *spellings*, but the pluggability seam lives at annotation **recognition**, not the lexer: `#:` is structured trivia, while a valid-R inline form — R ≥ 4.4 ships the experimental `declare()` base primitive (runtime no-op; Posit's quickr already annotates with `declare(type(...))`) — is ordinary call syntax recognized at lowering. Depends on nothing semantic.
- `crates/semantics` — the salsa database and all queries: per-**item** item tree and HIR with span maps — the analysis unit is the *item*, NOT the top-level statement: nested definitions are items too (fields/methods inside class-constructor calls such as `R6Class`/`setRefClass`/S4 blocks, and functions defined inside function bodies), otherwise R's common giant-single-statement OO files degenerate straight back to whole-file granularity; item identity hashes kind + name (+ parent, with an index disambiguator), never bare position or index, so inserting an item does not shift unrelated items' identities (copy rust-analyzer's *current* `AstIdMap` design — its pre-2025 index-based one had exactly the shifting problem). Naming; **interned/hash-consed types** (id equality, no deep clones); per-item inference with whole-file fallback for genuinely coupled files (letrec groups, captured-write re-pass, monotype accumulation — the `scc_definition_plan` eligibility analysis generalizes, and its recorded blockers — letrec-member exports, `top_level_capture_repass`, whole-file substitution coherence for recorded expression types — must be *resolved by the Phase 2 design*, not assumed away); **symbol-granular** interface resolved via salsa fixpoint cycles (kills fake cluster SCCs at the root; mapping the contract-pinned fixed-point semantics — rounds bounded by global count, period-2 oscillation pinning to `Unknown`, last-writer-wins winner order, walk-shadowed routing, letrec groups, self-recursion tolerance — onto `cycle_initial`/`cycle_fn` is a **named Phase 2 design deliverable**: salsa hard-panics at 200 iterations, so legacy's pin-and-continue must live inside the cycle function and reaching salsa's cap is a bug); parallel prime + snapshot reads; plus a plain from-scratch wiring of the same cores as the permanent differential oracle. Depends on syntax + salsa (pinned version; risk terms in the appendix).
- `crates/ide` — features over semantics snapshots; sees only the query API. `crates/format` — formatter on syntax only (compiler-enforced). `crates/ry` — LSP server + CLI.
- Position-independence note (corrected by the adversarial review): width-only green subtrees make an untouched item **structurally equal** across edits elsewhere in the file, so per-item derived values (item tree, HIR) compare equal and salsa's early cutoff prunes downstream work — **value equality, not pointer identity, is the mechanism** (rowan's green `Eq` is structural with a pointer fast path; its node cache dedups only ≤3-child nodes, so a from-scratch reparse shares no large subtrees; rust-analyzer's barriers are likewise `ItemTree`/`AstIdMap` value equality). Statement-splice reparse of open documents can make those compares pointer-fast, but it is an optional optimization: parse stays a pure per-file salsa query and correctness never depends on splicing.

## Testing doctrine for `syntax` (mandatory)

The parser is the foundation of everything; it must be **extremely well tested — more is better, and duplicated coverage is welcome, never pruned for elegance**. Layers, all of them, not a selection: (1) tree-sitter-r's parser corpus imported wholesale **and converted into the fixture-harness format** (user requirement: the suite is at least tree-sitter-r's, expressed as fixtures); (2) a real-world parse corpus — R's base library sources plus top CRAN packages — checked for lossless round-trip and acceptance parity; (3) exhaustive hand-written per-construct suites (every operator, precedence pair, call form, literal form, string/raw-string/escape variant, `#:` annotation form, and every error-recovery scenario) with golden trees and golden error messages; (4) property tests (token cover = input, node ranges nest, reprint == input); (5) fuzzing — random bytes and structure-aware mutations — with never-panic + always-lossless invariants, wired up in Phase 0 and run against **every** parser increment from the very first (a parser that only handles literals gets fuzzed the day it exists), continuously thereafter (a corpus-seeded fuzz run is part of the phase gates, and CI runs a bounded fuzz pass); (6) statement-reparse equivalence (incremental result tree == from-scratch tree for randomized edits); (7) acceptance cross-check against R's own parser where an R installation exists (local-only, like every R-requiring test). Redundancy across these layers is a feature: the same construct covered five ways is the point.

## Operational notes

- **Why statement-level reparse instead of tree-sitter-style GLR incrementality:** a hand parser is *estimated* at ~0.5–2µs/LoC (external precedent; no hand parser exists here yet — the Phase 1 bench converts this into a measurement) against tree-sitter's ~8µs/LoC here (derived from the recorded cold pass: ~2.5s of 3.7s at 302K LoC is parse), so even a full reparse of an 18K-LoC file is ~20ms; statement-level splice bounds keystrokes below that. The deep reason for rowan's width-only green nodes (corrected): an unchanged item's subtree is **structurally equal** after edits elsewhere in the file — position independence is what makes value equality hold across shifted offsets — so per-item derived values compare equal and salsa's early cutoff prunes downstream work. Pointer identity is NOT supplied by a from-scratch reparse (rowan's node cache dedups only ≤3-child nodes) and is not what rust-analyzer relies on either (its barriers are `ItemTree`/`AstIdMap` value equality); splice can upgrade the compares to pointer-fast for open documents, as an optimization only.
- **Why salsa rather than extending the in-house engine:** parallel snapshot reads + write-cancellation and first-class fixpoint cycles (proven in rust-analyzer and Astral's `ty`, whose type inference uses salsa fixpoints) are precisely the two things we would otherwise hand-roll; a concurrent red-green memo core is the one component not worth building in-house. The in-house engine's query *decomposition* (per-symbol firewalls, names-only cutoffs, durable/open fold splits) is the asset that transfers. **Salsa risk terms (recorded so they are managed, not rediscovered):** pin the salsa version and upgrade deliberately — the public API churns hard and often (multiple breaking releases per year through 2026); vendoring/forking is the exit hatch, exactly as argued for rowan. Fixpoint non-convergence is a **hard panic at 200 iterations** — legacy's pin-to-`Unknown` lives inside the cycle function, and reaching salsa's cap is a bug, never a fallback. Interned-value GC is young (landed 2025) and both rust-analyzer (~4× memory on its salsa migration until tuned with per-query `lru`) and ty (multi-GB blowups) hit real memory cliffs — hence the Phase 2/3 memory gates, with per-query `lru` and interned GC as the first levers. Parallel + fixpoint iteration had real hang bugs (fixed upstream in 2025); a parallel-cycle stress test is part of the doctrine and gates Phase 4's multi-core work.
- **Interned types are the deepest remaining inference win:** legacy `CoreType` is a deep-cloned enum; hash-consed id-based types (rustc/`ty` style — equality is id compare, substitution cached) are the difference between the measured ~6µs/LoC and a *projected* ~1µs/LoC ceiling (external precedent, proven or refuted at the Phase 2 perf gate). Design them in from the start; do not port the clone-based representation.
- **Limiting factors, ranked** (what the rewrite is for): (1) file-granular analysis, (2) type-representation churn, (3) parse cost/tree size, (4) single-threadedness — (4) is a ÷cores multiplier while (1)–(3) are asymptotic or large per-op wins; the ultimate ceiling is R's dynamic semantics (a semantics budget, not infrastructure).
- **Inline annotations (Python-style), future path:** annotations are ONE internal concept with pluggable surface spellings, and the seam is annotation *recognition* (lowering), not the lexer. Today `#:` (structured trivia); near-term option: runtime-neutral valid-R forms — R ≥ 4.4 ships `declare()` as an experimental base primitive (runtime no-op; quickr already uses `declare(type(...))`) — recognized as first-class annotations from ordinary call syntax; a true superset dialect (TypeScript road: inline syntax + strip step) stays a product decision, kept open by the pluggable design — never an architectural blocker. `#:` files must always remain valid ordinary R.
- **Corpus mechanics:** tree-sitter-r's parser corpus lives in its GitHub repository (`test/corpus/`, MIT) — fetch from the repo, not the crates.io package (which may omit tests). Real-world corpus = R base library sources + top ~100 CRAN packages, stored under a **gitignored** corpus directory with a **committed manifest + fetch script** in `scripts/` (the fetch needs outbound network; run it wherever that exists). The R-`parse()` acceptance cross-check needs a local R installation — run it locally, skip gracefully elsewhere; because CI has no R, the Phase 1 acceptance-divergence allowlist is adjudicated against R once locally and then committed/pinned.
- **Known-tricky lexer/parser cases to cover exhaustively from day one:** raw strings (`r"(...)"` / `R"[...]"` — legacy formatter has a byte-for-byte rule for a reason), escapes, `%op%` operators, backtick names, multi-line `#:` annotation blocks (consecutive `#:` lines stitch into one annotation region), statement-boundary/newline sensitivity (R's newline-vs-operator continuation rules), `]]` vs `] ]` disambiguation in nested indexing (`x[[y[1]]]`), the top-level `else`-after-newline error (legal inside braces, a parse error at top level — R language definition), `->`/`->>` assignment, `=` as assignment vs named-argument (context-dependent), unary-minus precedence (`-2^2` is `-(2^2)`), literal forms (hex, `L` integer, `i` complex), and `\(x)` lambdas (R ≥ 4.1) — these are where R parsers get subtle.

## Recorded answers to direct user questions

- **Dramatically better error messages are an explicit GOAL of the new parser, not a side effect** — for R syntax generally and for `#:` type annotations specifically. Recursive descent knows what it was parsing at every point, so the bar is: expected-token sets ("expected `)` or `,`"), paired-delimiter pointers ("unclosed `(` opened here" with both spans), statement-anchored recovery (one broken construct never poisons the file), and — because annotations are first-class grammar — real type-syntax errors with exact token spans *inside* `#:` comments ("expected a type after `|`"), replacing the coarse re-lexed lowering diagnostics of the legacy stack. Wording is pinned by the golden error-message suite (Phase 1 gate) and held to the AGENTS.md Elm/Rust diagnostics goal. **User directive (recorded verbatim in effect):** parity with legacy syntax-error output is explicitly NOT the bar — the messages must be strictly *better*; and the new parser's test suite must be at least tree-sitter-r's, ideally expressed in the fixture setup (hence the Phase 0 conversion requirement).
- **Parsing is a per-file salsa query; do not try to make salsa statement-aware at the parse level.** File text is the salsa input, `parse(file)` the query: an edit re-parses only that file. Sub-file salsa invalidation of parsing is a chicken-and-egg (statement boundaries are only known *after* parsing) and buys nothing: the hand parse is the cheapest stage (est. ~1µs/LoC; a full 18K-LoC reparse ≈ 20ms), statement-level reparse is at most an *internal* optimization of the parse step, and the incrementality that matters happens one level down — an untouched item's subtree in the new tree is **structurally equal** to the old one (width-only greens make equality hold across shifted offsets), so per-item downstream queries (item tree, HIR, inference: the expensive stages) produce equal values and salsa's early cutoff prunes them. Same conclusion rust-analyzer reached (its barriers are `ItemTree`/`AstIdMap` value equality over full from-scratch reparses).
- **Why rowan rather than a hand-rolled tree (the parser is hand-rolled either way):** rowan is not a parser — it is the tree data structure the hand-written parser emits; nothing about parsing is delegated. The green/red design beats a classic typed AST (structs with spans) for four load-bearing reasons: (1) losslessness — every byte including trivia lives in the tree and reprints exactly, and `#:` comments *are* type syntax here, so the formatter, byte-exact round-trip, and annotation tooling come from the representation instead of side tables; (2) error resilience — every parse yields a tree with error nodes local to the break, and the typed AST layer is `Option`-returning views, so consumers never carry a parallel broken-code data model; (3) position independence — width-only green nodes make untouched statements structurally identical across edits (pointer-identical too when splice reparse is used), the property per-item salsa cutoffs are built on (a span-carrying AST shifts every span after any edit, killing sub-file incrementality at the root); (4) structural sharing — immutable refcounted subtrees with builder-level dedup of identical small nodes keep the resident tree ~2× source bytes. Use the *crate*, not an in-house clone of the design: the value is subtle already-hardened machinery (thin-DST layout, red cursors with lazy offsets, node caching, splicing) proven for years in rust-analyzer; hand-rolling reproduces that code minus the hardening with zero design freedom gained, and rowan is small and dependency-free enough to vendor/fork if divergence is ever needed (Biome forked it — precedent for both maturity and the exit hatch). Acknowledged cost: rowan traversal is dynamically kinded and slower than direct structs — which is why inference never walks it; the checker runs on per-statement HIR, rowan serves the fidelity layers (IDE, formatter, refactorings).

# Decision record: fuzzing is pipeline-wide, from each stage's first commit (user directive)

**Context.** The testing doctrine in the greenfield-rewrite record made fuzzing mandatory for the `syntax` crate from day one. The user extended this: fuzzing must cover the OTHER pipeline stages too — it is never an afterthought bolted on later, for any layer.

**Decision.** Every pipeline stage gets fuzz + property coverage the day it exists, alongside its fixtures: lowering, naming, inference, diagnostics, the salsa incremental layer, and later the formatter and IDE features (formatter: idempotence + losslessness under fuzz; IDE: never-panic per cursor position). The semantics harness (`crates/semantics/tests/test_fuzz.rs`) is the template: never-panic across the full pipeline (salsa fixpoints must converge), determinism across fresh databases, diagnostic-range geometry, and incremental-equivalence (edit through the setter == fresh build — the red-green invariant), over a generator biased toward semantically live shapes plus a token-soup robustness arm. `FUZZ_ITERS` scales budgets; a bounded pass runs in the default test suite so CI fuzzes on every change, and `fuzz_deep` variants carry the long runs.

**Validation.** The first semantics fuzz runs found two real crashes within seconds — a non-converging salsa cycle (a growing self-referential type riding the iteration cap into a panic; the legacy oracle crashes on the same input) and inference variables leaking through exported schemes into foreign tables — both of the class that only surfaces "in the large", exactly what per-stage fuzzing exists to catch early.

# Decision record: deep-resolve is memoized per binding epoch with cycle-cut-to-Unknown (replacing depth truncation)

**Context.** `InferenceTable::resolve` (the deep resolver in `crates/semantics/src/infer.rs`) walked the interned type structure recursively with only a depth-64 cap as protection. Interned types form a DAG — shared subtrees appear once in memory but were re-resolved once per occurrence, and a self-referential binding (a variable whose binding transitively contains itself, or a self-referential alias) expanded as a tree up to the cap. On the real-file corpus (~507K lines) this was measured at 397 million inner resolve steps — resolve alone cost more wall time than the legacy stack's entire pipeline — and the depth cap also *truncated meaning*: past depth 64 a type silently stayed unexpanded, a position-dependent semantics no cache could be layered onto.

**Decision.** Deep-resolve is a memoized walk over the interned DAG with explicit cycle detection:

- **Cycle cut:** the walk carries a `visiting` stack of variables under expansion; re-encountering one is an infinite type and resolves to `Unknown`, matching the pin-to-Unknown doctrine used everywhere self-reference grows (salsa fixpoint cap, loop widening). Alias expansion keeps a depth guard as a pure resource backstop, not a semantics.
- **Per-node memo, clean-flag discipline:** results cache in `resolve_cache` keyed by the interned type, but only CLEAN subtrees — those with no cycle cut beneath — are stored, because a node containing a variable currently being expanded resolves differently at top level.
- **Epoch invalidation:** every binding mutation and rollback bumps an epoch counter; the cache self-clears on epoch mismatch. No entry can ever serve a stale binding, and the common case (many resolves between mutations, e.g. rendering a whole item's diagnostics) hits warm.

**Impact.** Corpus inner resolve steps 397M → 4.2M (linear in corpus size); resolve wall 30.8s → 0.3s; whole new-stack corpus pass 53.2s → 12.7s, beating the legacy stack's 13.9s. Semantically the change replaces silent depth truncation with the established cycle semantics — infinite types resolve to `Unknown` at the point of self-reference instead of arbitrarily deep expansion — which the fixture, fuzz, and both differential suites confirm is observation-equivalent everywhere covered. `RESOLVE_CALLS` stays as a standing instrument: near-linear step counts are now an invariant the perf harness can watch.

# Decision record: canonical per-group interface fixpoint — cyclic schemes are forcing-order-independent

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Driven by a corpus-scale finding from the multi-core instrument: 64852 findings when per-file phases were pre-forced versus 64835 when file diagnostics were forced directly — both counts stable across runs and thread counts (one worker equals four exactly), so the delta was never a parallelism race but query-order semantics.

Previous shape / structural weakness: cyclic package-interface groups resolved through salsa's dynamic cycle recovery alone (`item_check_recover` / `global_scheme_recover`): whichever member was queried first became the cycle head, the fixpoint iterated from that head, and a group still changing at the round cap pinned *from that head's perspective* — so which items lost their types to `Unknown` depended on which query happened to arrive first. Every individual forcing order was deterministic, but hover-then-check, check-then-hover, and differently-ordered cold passes could disagree with each other. The legacy stack's whole-package rounds were entry-order-independent; the rewrite's per-item cycle heads were not.

Chosen shape (in `crates/semantics/src/semantics.rs`):
- `interface_sccs(files)` — the static interface-reference graph: an edge from each named package definition item to the winner of every global name its body reads (`non_locals` plus validated `namespace_reads`), condensed by one iterative Tarjan pass in canonical order (project file order, item order within a file). Only *cyclic* groups (more than one member, or a self-edge) are recorded.
- `scc_schemes(files, group)` — the canonical fixpoint of one group: every member starts at the tolerant `Unknown` scheme; each round re-checks every member against the *previous* round's table (Jacobi — one propagation hop per round, so within-round order cannot matter either); convergence is scheme-table equality; a group still changing at the round cap (16, shared with the salsa backstop) pins **all** members to `Unknown` — the only entry-order-free pin. Member checks run `check_item_with_annotation` directly against an overlay environment (`SccGlobals`: round table first, ordinary global resolution otherwise; stub overloads suppressed for member names) — never through `item_check` — so no salsa cycle forms.
- `item_check` **adopts** the canonical scheme as a member's exported scheme (single source of truth: export, hover, and every downstream reader see the fixpoint value, not the one-hop-ahead re-derivation the item's own check just computed), and `global_scheme` reads `item_check` only. The salsa cycle recovery stays as a backstop for reference edges the static graph cannot see.

Impact: correctness — forward, reverse, and phase-pre-forced forcing render identical diagnostics (regression test `cyclic_group_answers_are_forcing_order_independent` in `crates/semantics/tests/test_parallel.rs`; the corpus instruments now agree at 64835 findings for both forcing shapes), and the growing-self-reference pin stays `Unknown`; simplicity — the fixpoint is an ordinary tracked query over an explicit graph instead of emergent salsa cycle-head dynamics; performance — the sequential corpus pass *improved* 12.7s → 10.1s (canonical rounds replace salsa's per-head cycle re-iteration), while keystrokes in a 53K-line package pay ~3ms more per edit (~8%; the once-per-revision validation walk of `interface_sccs`, whose dependency surface is every item's naming — narrowing that surface to a per-item read-name projection is the known lever if it ever matters); incremental analysis — the graph derives from naming only, so edits that leave every member's read-set unchanged backdate `interface_sccs` and the group fixpoint re-runs only when a member's check output changes.

# Decision record: no third constraint kind — two-flexible comparisons stay unconstrained

**Status:** decided and ratified (agent-owned decision under the delegated ownership mandate). This resolves the recorded design fork on two-flexible-operand comparisons without tripping the traits tripwire.

Question: `function(a, b) a < b` — should comparing two flexible operands constrain them (to each other, or to a new "comparable" constraint kind covering numeric/`character`/`logical`)?

Decision: **no.** Two flexible comparison operands stay fully unconstrained — the function infers as `<T, U> fn(a: T, b: U) -> logical` and cross-family calls are accepted. A flexible operand is still constrained to numeric when its partner is concretely numeric (existing rule), and two concretely-known families must still match.

Rationale:
- R's runtime comparison coerces across atomic families (`1 < "2"` is legal, character-compares `"1" < "2"`), so any constraint tying flexible operands to a family or to each other rejects legal programs the checker cannot prove wrong.
- A "comparable" constraint would be the third independent constraint kind — the recorded traits tripwire. Comparisons alone do not justify designing traits: the constraint would be nearly vacuous (every atomic family is comparable), buying almost no precision for real machinery cost.
- The same-family error on two *concrete* operands stays: that case is decidable and catches real bugs (`x < "10"`).

Impact: correctness — ratifies existing behavior (fixture `two_flexible_comparison_stays_unconstrained`; both differentials green, so the oracle agrees); simplicity — no new machinery, the traits tripwire stays armed; the typing reference now states the flexible-operand comparison rules explicitly.

# Decision record: union compatibility commits a flexible argument at first use, in program order

**Status:** decided and ratified (agent-owned decision under the delegated ownership mandate). This resolves the recorded design fork on order-dependent compatibility commits.

Question: a flexible argument checked against a union-typed parameter binds to the whole union (`f(v)` with `f : fn(x: integer | character)` pins `v := integer | character`). A later use of `v` against a different union (`g : fn(x: logical | character)`) then errors even though the intersection (`character`) would satisfy both. Should commits be made order-free (constraint collection + intersection solving), or is first-use commitment the spec?

Decision: **first-use commitment is the spec.** A flexible argument checked against an expected union binds to the whole union at that use, exactly as unification would; uses commit in program order; a later conflicting use reports at its own site against the committed type. The fix for a genuine intersection case is an explicit annotation with the intended member type.

Rationale:
- Program-order commitment is how the checker already treats every other type (`x <- 1L` then `x <- "s"`-style first-use-binds is standard HM); making unions special would demand intersection constraints — a new constraint former squarely on the traits frontier, deliberately out of scope.
- The order-dependence is bounded and predictable: it never changes *whether* an inconsistent pair of contracts errors (some site always reports); it only decides *which* site is blamed — the later use, which is also where a reader's attention should go.
- Program order is the order R evaluates, so the blamed site matches the first call that would misbehave at runtime under the committed reading.

Impact: correctness — ratifies existing behavior (fixtures `flexible_argument_commits_to_the_union_at_first_use` / `union_commit_blames_the_later_conflicting_use` pin both orders; differentials green — the oracle agrees); simplicity — no constraint-solving machinery; the typing reference's union-compatibility section now states the commitment rule and its annotation escape hatch.

# Decision record: strict mode attributes recursive bindings the fixed point cannot fully type

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Closes the recorded gap that deliberately-`Unknown` recursive schemes carried no strict origin.

Context and a finding along the way: the canonical per-group interface fixpoint types **converging** recursion precisely — a top-level `fact` exports `fn(n: integer) -> integer` and mutual `is_even`/`is_odd` export `<T: numeric> fn(n: T) -> logical` (fixtures pin this; the older "self-recursion deliberately stays tolerant `Unknown`" contract is superseded and the typing reference updated). What remains `Unknown` is: (a) growing self-reference pinned at the round cap — those already surface under strict through the undetermined-reference origin at the recursive read (the read sees literal `Unknown`); and (b) cycles that converge *with* `Unknown` embedded (`f <- function() f()` settles at `fn() -> Unknown`) — the read sees a function type, no origin fires anywhere, and the export silently carries `Unknown`. Case (b) was the attribution hole.

Chosen shape: after `item_check` adopts the canonical group scheme, if the member's body produced **no errors and no other strict origins** and the adopted scheme still contains `Unknown` (`types::contains_unknown`), a `StrictOriginKind::RecursiveUnknown` origin is recorded on the whole binding, rendered "strict mode: could not determine the full type of `f`; it is defined recursively — add a type annotation". The clean-body gate keeps the propagation doctrine: when anything inside the body already attributes the `Unknown`, the binding is not re-reported. Accepted over-report: a clean-bodied cycle member whose `Unknown` propagates from a *sibling's* origin is still attributed — detecting that would need group-wide origin bookkeeping inside the fixpoint, and the advice ("annotate this binding") genuinely closes the member's export regardless of the sibling.

Impact: correctness — every `Unknown`-carrying export now has at least one strict attribution (fixtures cover self/mutual/annotated/growing/pure-self-call shapes); the differential accepts the two new-only findings as oracle deficits (legacy attributes nothing and panics on the growing shape); simplicity — one new origin kind and a type walk, no fixpoint machinery; incremental analysis — the check runs inside `item_check`, no new queries.

# Decision record: script frame semantics — sequential immediate reads, settled-frame deferred reads

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Driven by the differential fuzz arm, which exposed that script unresolved checking was entirely missing and that cross-item resolution had no defined contract.

Question: a script's top level is one frame executed top-down. What does a cross-item read resolve to — for naming (unresolved warnings), for the unused check, and for typing — when the frame holds several bindings of the name, when the read precedes every binding, and when the read sits inside a closure?

Decision, one rule per read kind:

- **Immediate reads** (executed at their position in the top-down run) resolve sequentially: the nearest EARLIER top-level binding wins, before package globals and stubs. A use before every definition — including inside the very statement that first binds the name (`x <- x + 1L` with no earlier `x`) — is an unresolved name, because it errors at runtime.
- **Deferred reads** (from inside a nested function — the closure runs after the frame settled) resolve against the whole document: the LAST top-level binding wins, the enclosing statement's own binding included, so self-recursion resolves and types through the cycle fixpoint (`a <- function() a()` exports `fn() -> Unknown` via the round cap; a later rebinding is what the recursive call actually sees at run time).
- **Conditional top-level writes** (inside a top-level `if`/`for`/`while`/`repeat`) create the document's variable slot exactly as the package spec already said: later reads resolve to it; the slot exports no scheme yet, so such reads type `Unknown` (backlogged lift).
- **Quiet reads** (data masking, opaque operators like `|>`) are never reported unresolved but count as uses for the unused check and get full navigation — at runtime they fall back to the enclosing binding.
- The unused check follows the same model: deferred reads keep every binding of the name alive; immediate reads mark definers backward through conditional ones (a conditional rebinding does not end an earlier binding's liveness); a loop reading its carried variable keeps both its own write and the earlier binding alive (the first iteration reads the outer one).

The oracle's frame model differs by construction: one settled slot per name (no sequence), the slot minted before the statement's value resolves, forward captures unresolved, pipe reads not counted as uses, and an occurs-check that rejects some valid self-referential rebindings. Where the models disagree, the rewrite follows R's runtime and the typing reference, and the differential accepts the divergence explicitly: fixture-arm and ide-arm case allowlists with reasons, and the fuzz arm's narrow filters (site-scoped oracle deficits, in-statement slot tolerance, and unpaired type findings over the transitive closure of *unstable names* — multiply-bound, self-referential, or forward-captured). Every acceptance is rollup-counted so drift stays visible.

Impact: correctness — scripts get the unresolved class for the first time (spec-mandated, previously silently absent), duplicate `@type`/`@alias` names now error at every site, and six fuzz-found gaps are fixed with fixtures pinning each; simplicity — one `deferred` bit threaded through `GlobalEnv` instead of a second resolver; incremental analysis — resolution facts stay per-item salsa queries (`frame_slot_positions` is one small per-file map).

# Decision record: undeclared type names error once at the reference and compare like `Unknown`

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Closes the reported gap that a misspelled nominal inside a `@type` body (and every other annotation position) was silently lowered to an opaque nominal.

Chosen shape, three pieces with one source of truth each:

- **Recording:** annotation lowering (`annotations::lower_annotation`) records every `TyKind::Named` mint with the referencing token's range (`Annotation::nominal_references`). Primitives and in-scope binders never reach the record because lowering resolves them first — so binder scoping stays single-sourced instead of being re-derived by a diagnostic walk.
- **Reporting:** `unknown_type_diagnostics` checks the recorded references against the project's `@type`/`@alias` declarations (plus the file's own for scripts) and the stub corpus's nominal vocabulary, erroring at the precise token with a nearest-name hint (`Instument` → "Did you mean `Instrument`?"). Forward references stay legal — the vocabulary is position-independent.
- **Tolerance:** an undeclared nominal compares like `Unknown` at the relation level (`unify`, `compatible`, and the operator checks' `structural()` projection all consult one `undeclared_nominal` predicate), so the typo is reported exactly once and never cascades into value-level mismatches, call-site errors in other items, or operator noise.

Along the way the declared-annotation check was found to silently skip `Named`, `Record`, and `Tuple` declarations (a positive-list gate meant for tolerance had become a hole): `#: Point` on a structural value minted the nominal without `@new`, contradicting the nominal-introduction contract and the oracle. The gate is now a negative list (`Unknown`/`Any` only), which both enforces the `@new` discipline at declared sites and checks record/tuple declarations for the first time. The typing reference states both contracts.

Impact: correctness — the reported bug class is closed with fixtures and fuzz templates guarding it; simplicity — one predicate instead of per-site suppression guards; incremental analysis — the diagnostic is a per-file pass over already-lowered annotations.

# Decision record: annotation shape violations refuse the whole block; the depth caps and vector-element rule keep the oracle's gating split

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Driven by the legacy-corpus differential arm, which itemized every annotation validation the oracle enforced and the rewrite silently skipped.

Question: where do annotation-shape validations live (directive ordering, duplicate/unknown type parameters, applied binders, `@new` payload shape, nesting caps, vector-element atomicity, attachment rules), and what happens to a violating block's typing payload?

Chosen shape:

- **One refusal semantics:** a block with any shape violation keeps only its errors — the whole typing payload (declared type, definitions, `@new`, `@strict`, nominal references) is dropped, so one mistake yields one error and no follow-on findings (`Annotation::errors` / `typing_errors`, stripped in `lower_annotation`). Consumers observe payload absence, not a validity flag; inlay hints gate on *surviving payload* (declared/`@new`/trusted), not annotation presence, so a refused binding hints its inferred type again.
- **Attachment is single-sourced:** `top_level_annotations` computes each top-level block's target (attached / blank-line-separated / dangling); both annotation application (`item_annotation_syntax`) and the dangling-annotation diagnostics read it. A blank line or interposed comment now genuinely detaches the annotation — previously the rewrite silently applied across blank lines, unlike the oracle and the reference.
- **Gating-faithful classes:** the two nesting caps mirror the oracle's split on purpose — past 160 levels the annotation shape is refused (always reported), past 128 the type is refused *for checking* (a typing-class finding that disappears under `# typing: off`). Same for the vector-element rule: lowering records every `[]` element with its range (`Annotation::vector_elements`), and a diagnostics pass with the project vocabulary judges it (aliases expand, nominals refuse, undeclared names stay silent — the unknown-type error owns those), reported in the typing class at the use site. The vector finding does NOT strip the payload (the judgment needs vocabulary lowering lacks), so the declared shape still serves hover/navigation — an accepted, documented difference from the oracle's whole-item abort, visible only in exported schemes.
- **Definitions are top-level-only** and nested `@type`/`@alias` blocks error without entering the vocabulary (`file_type_definitions` reads top-level children only).

Impact: correctness — 14 legacy-corpus cases and the whole reported validation family close, with fixtures pinning each shape and message; simplicity — one errors vector and one attachment walk instead of per-consumer validity checks; incremental analysis — everything stays in per-file parse-pure passes except the vocabulary judgment, which joins the existing per-file semantic families.

# Decision record: statement-level annotations attach at any depth and apply where the expression infers

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Closes the largest legacy-corpus gap: annotations below the item root were invisible to the checker, so the constructor idiom (`#: @new Person` on a local assignment or block-final expression inside a function body) silently did nothing.

Chosen shape, keeping one source of truth per fact:

- **Association:** `statement_annotations(parent)` — the existing adjacency walk — runs over any statement sequence (the file root or a braced block), so top-level attachment, expression-level attachment, and the dangling-annotation diagnostics (now covering nested blocks) share one rule. `item_expression_annotations(db, item)` maps each attached block inside an item to the annotated expression's HIR id by exact range; it is a plain function, not a tracked query (`Annotation` carries `TextRange`s with no salsa-value plumbing, and its callers are tracked queries whose dependencies already flow through `item_syntax`/`item_hir`).
- **Application:** the checker owns one `apply_expression_annotation` seam — assignments apply it before the slot write (the binding takes the annotated type), every other expression applies it where it infers, and a non-assignment item ROOT routes its own annotation through the same seam (closing the bare-expression checked-annotation gap). `@new` reuses `check_new_nominal` (representation check, nominal minted); a checked declared type enforces the same directional `compatible` contract as at the root; `@trust` overrides unchecked. Loop-body re-walk error discarding covers the new errors for free.
- **IDE consequences:** goto-type-definition and hover pick the nominal up from the recorded expression types with no feature-side work; inlay hints skip annotated nested bindings (the annotation already names the type), symmetric with the root gate on surviving payload.

Impact: correctness — eight corpus cases close (1515/1523 matching), with fixtures pinning the constructor idiom in both forms, the mismatch error at the value, trust, bare-expression checks, and nested blank-line detachment; simplicity — one association walk and one application seam instead of per-position special cases; incremental analysis — everything stays inside existing per-item queries.

# Decision record: capture liveness is frame-scoped

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). Settles which writes a closure's captured read keeps alive for the unused check — the corpus differential showed the rewrite marking by NAME across all frames, so a shadowed outer binding never warned.

Rule: a read from inside a nested function keeps every write of the name alive **in the frame the read resolves to** (sequential rebindings of one name in one frame are a single runtime variable, and the closure runs after the frame settled), and no other frame's — a same-named binding in an enclosing frame that the resolved binding shadows is not what the closure reads, so it stays reportably dead. This is exactly R's environment semantics and agrees with the oracle.

Mechanics: frames carry a stable identity (`Scope::id`, minted per defining expression like binding ids so loop re-walks reuse it), each assignment write records its slot's owning frame, and the capture sweep filters on frame + name. Writes recorded after the read stay covered by the existing per-slot `captured_slots` marking at the write site. The typing reference documents the rule with both directions as examples.

Impact: correctness — two corpus cases close and a false-negative class (dead shadowed bindings in closure-heavy code) is gone; simplicity — one id per scope instead of a second liveness structure; incremental analysis — naming stays a per-item pure function.

# Decision record: the last three parity lifts — conditional-slot schemes, export-edge constraint generalization, missing-formal flow

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate). These closed the legacy-corpus differential to zero unexplained divergences, and the arm is now a default-suite gate.

**Conditional top-level slots type.** A statement item's conditional write (`for (i in 1:3) total <- i`) already created the document's variable slot for naming; now it types: `ItemCheck::top_level_bindings` carries the settled, export-closed scheme of every name the item's top-level frame binds, `statement_binding_scheme(item, name)` projects it per binding (a value-eq firewall, with `global_scheme`-style cycle recovery — a statement item reading its own conditionally-written name routes back into its own check), and readers consult it after `package_definitions` (joined across multiple writers) and inside the script sequential search. The winner order is unchanged: an unconditional definition still shadows the slot.

**Export-edge closure generalizes constrained residuals.** `erase_residual_vars` erased every unbound variable to `Unknown`, destroying real information: `mixed_apply <- invoke(mirror)` lost its `numeric` bound and cross-item calls stopped checking. `close_scheme` now generalizes an unbound variable that CARRIES a constraint into a fresh scheme binder (synthetic names never display — the renderer canonicalizes rigids) and erases only unconstrained ones. Instantiation stays per reader, which matches R's call-by-call semantics for the immutable closures this shape produces.

**`missing()` supplied-state flows through the environment.** A third entry kind (`EnvEntry::MissingFormal`) rides the existing branch mark/rollback/join discipline instead of a parallel liveness structure: the `missing(name)` guard's true edge marks a no-default formal's slot, a read of a marked slot errors ("would fail at run time"), any write supplies it back to an ordinary entry, and the marker is branch-local at joins (rejoined state means only "possibly missing", which reads as the supplied type — only definite runtime failures report, as the reference specifies).

Impact: correctness — the corpus differential reaches 1,523/1,523 with one adjudicated acceptance and every new behavior pinned by fixtures in both directions; simplicity — each lift reuses an existing mechanism (per-item checks, the erase walk, the environment discipline) instead of adding a parallel structure; incremental analysis — two new tracked projections with value-eq firewalls, no new interface surfaces beyond them.

# Decision record: NAMESPACE/DESCRIPTION metadata feeds resolution

**Status:** decided and implemented (user ask: "importFrom should work"). Previously the NAMESPACE file was parsed only at the CLI/server layer for import-site problems (unknown-import, unused-import); resolution ignored what the package imports and DESCRIPTION was never read, so real packages saw two false-positive classes: bare reads of names imported from namespaces the stub corpus does not describe warned "could not resolve" (`importFrom(data.table, ':=')` code), and `pkg::` calls into any undescribed namespace warned "unknown package namespace" even for declared dependencies.

**Shape.** One new singleton salsa input, `metadata::PackageMetadata` — normalized (sorted, deduped) `(namespace, Option<name>)` import pairs plus the DESCRIPTION dependency name set — installed by hosts next to `StubSources` (CLI per target; the server at startup, refreshed on NAMESPACE buffer sync and NAMESPACE/DESCRIPTION watcher events, diffing the parsed facts so formatting edits do not invalidate). The NAMESPACE parser moved from the host crate into `semantics::metadata` (single source of truth; the host keeps problem rendering). Consumption is two predicates at the diagnostic edges: `imported_bare` joins the unresolved-check skip set, and `declared_dependency` quiets the unknown-namespace warning. Typing is untouched — imported-but-undescribed reads stay `Unknown` with the usual strict origin.

**The tolerance call.** `import(pkg)` of a namespace without stubs makes every otherwise-unresolved bare read in the package quiet: the export set is unknowable, and guessing would trade the zero-false-positive mandate for typo detection. Typo detection resumes when stubs describe `pkg` (then the export set gates exactly), and `importFrom` names are always exact. Bare resolution of stub names stays ungated (the earlier record) — metadata only ever widens the resolved universe, never narrows it.

**Impact.** Correctness: kills both false-positive classes on real packages; fixture suite `typing-imports` pins both directions (imported quiet, unimported still warns). Simplicity: two predicates over one input; no naming/inference changes. Performance/incremental: the predicates run only after every cheaper skip fails (genuinely unresolved names), and the input diffing confines invalidation to real metadata changes.

# Decision record: data.table awareness — conditional stub namespace, result-shape classifier, typed-subject masking

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate; graduates contributing/design/data-masking.md "idea 1", the first rung of the data-masking ladder in contributing/design/open-questions.md §7).

**The gap.** The masked-bracket recognition was purely syntactic and its result was always `Unknown`: chains lost their class after one bracket, `DT[speed > 20]` (no marker) warned "could not resolve speed" on the most idiomatic data.table line there is, and no shipped stub could give a value the `data.table` class in the first place.

**Shape — three pieces, one gate each.**
- **Conditional stub namespace.** `types/data.table.Rtypes` ships (the `@type data.table` nominal + ~45 high-traffic declarations) but joins `stub_library`'s fold only when `metadata::namespace_active` says the project uses the package: a DESCRIPTION dependency, a NAMESPACE import source, or a `library()`/`require()`/`requireNamespace()`/`loadNamespace()` call with a literal package argument in any project file (`metadata::file_attached_namespaces`, a per-file syntax scan; hosts union it into the new `PackageMetadata.attached` field — the CLI/stats once after load, the server incrementally per synced file plus the idle prime, never a per-keystroke sweep). Gating at the ASSEMBLY means every consumer (bare resolution, `pkg::` validation, nominal vocabulary, completion, shadow lints, typo suggestions, masked verbs) inherits the same universe with no per-site checks; while inactive, data.table behaves exactly like any undescribed package, so its names cannot steal typo warnings. The stub-assembly cycle risk (naming → stubs → activation → naming) is broken by keying activation ONLY on inputs (metadata) — never on naming or item queries.
- **Result-shape classifier.** In `infer_index`: a single bracket whose subject resolves to the `data.table` nominal (from the shipped stub or any project `@type`) classifies `[.data.table` by the bracket's own syntax — no/empty `j` (filters, joins), `:=` calls, `.()`/`list()` calls, and any grouped `j` (`by =`/`keyby =`) keep the subject's class; other `j` shapes stay sound-refusal `Unknown` with a strict origin. The class is a real type: it survives chains, checks against annotations, constrains calls. Column knowledge (element types, membership, `:=` evolution) is deliberately NOT modeled — the typing reference documents the result-class table as the contract.
- **Typed-subject masking.** The same classification records every read under the bracket's index arguments (nested closures included — they are created in the data's frame) in `ItemCheck::masked_reads`; the unresolved-warning renderer skips them. This is checker-derived masking on top of naming's syntactic recognition, so `DT[speed > 20]` and `DT[, x]` go quiet exactly when the subject's class is KNOWN — the syntactic path and its legacy-mirroring `Unknown` stay untouched for unknown subjects.

**Impact.** Correctness: kills the dominant data.table false-positive class (bare column reads in unmarked brackets) and gives chains/annotations a real class to check; sound-by-refusal is preserved everywhere column knowledge would be needed. Simplicity: one assembly gate, one classifier function, one diagnostics skip. Incremental: activation reads only inputs; a flip rebuilds the stub library (rare, worth the full refresh); per-keystroke cost is one memoized single-file scan on the edited document.

# Decision record: the native pipe desugars at lowering

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate; renegotiates the "mirror legacy's silence" term for `|>` specifically).

**Why.** `x |> f(y)` is not an operator in R at all — R's own parser rewrites it to `f(x, y)` before evaluation. Modeling it as an opaque binary operator (quiet reads, silent Unknown) threw away exact static knowledge on one of the most common constructs in modern R. Desugaring is not an approximation; it is R's definition.

**Shape.** `hir::lower_pipe` intercepts `PIPE_GREATER` before binary lowering: a call right-hand side lowers as that call with the piped value inserted as the first positional argument, or — when a `_` placeholder sits as the whole value of exactly one named argument of that call (`pipe_shape`, a syntax-level scan) — substituted as that argument's value instead (the `_` token never lowers, so nothing dangles in the arena). Everything R rejects (non-call RHS, positional/repeated/nested `_`, `_` as a tag) keeps the old opaque-operator lowering — sound silence, never a guess. Naming, typing, overloads, arity checks, strict mode, and every IDE feature inherit the real call with zero changes; error blame on a bad piped value lands on the left-hand expression's own range.

**Impact.** Correctness: pipelines type end to end (`x |> length() |> sqrt()` is `double`), argument errors inside pipelines surface with precise blame, and R's placeholder pitfalls (missing first argument) are caught statically. Simplicity: one lowering seam, no checker/naming changes. Incremental: lowering-local; per-item firewalls unaffected.

# Decision record: formal-aware @masked + the conditional dplyr namespace

**Status:** decided and implemented (agent-owned decision under the delegated ownership mandate).

**The contract was ahead of the implementation.** The typing reference always said `@masked` arguments "matching the declared formals resolve normally" — but naming hardcoded first-positional-argument-is-data, which breaks zero-formal masks (`join_by(x == y)`: every argument is a column reference) and named data arguments. The stub loader now records each masked verb's formals declared before `...` (`StubLibrary::masked: name → leading formal names`, extracted from the lowered `FunctionType`), and the naming walk resolves an argument normally when it matches a leading formal by position or by name, masking everything the `...` absorbs — an empty formal list masks every argument. The base family (`with`/`within`, `subset`/`transform`) keeps its one data argument via its real formal names (`data`, `x`).

**dplyr rides the existing rails.** `dplyr.Rtypes` joins `CONDITIONAL_NAMESPACES` (the data.table record's activation semantics apply unchanged): the verb set is `@masked` and class-preserving (`<T> fn(.data: T, ...) -> T` — mutate on a data.frame is a data.frame, on the data.table nominal a data.table), joins preserve the left class, `join_by` is a zero-formal mask, and the tidy-select helpers plus verb vocabulary (`n()`, `row_number()`, `if_else`, ...) are declared so they resolve inside masks. Composed with the native-pipe desugar, a masked verb call in a pipeline is just a call: `df |> filter(cyl > 4) |> mutate(r = mpg / wt)` types class-preservingly with zero unresolved-column warnings. Where dplyr names collide with attached-stub names (`filter`, `lag` in stats), source order makes the dplyr declaration win exactly when dplyr is active — matching R's own attach shadowing.

**Impact.** Correctness: the documented masking contract is now the implemented one, and the dominant dplyr false-positive class (column reads in verbs, in projects without hand-written project stubs) disappears for declaring/attaching projects. Simplicity: no new mechanism — one map where a set was, one namespace entry. Incremental: unchanged (the masked map lives in the same set-once library).

# Decision record: the shipping binary links no Apple frameworks

**Status:** decided and implemented (user-directed criteria: fewer dependencies, no licensing exposure).

**Why it appeared.** Release macOS binaries are cross-linked on Linux by zig (cargo-zigbuild inside the nix build). Zig ships stubs for libSystem/libc/libm only — the pre-REPL binary linked nothing else, so the SDK-less link worked by accident. The REPL added the first Apple-framework edge: reedline → chrono(clock) → iana-time-zone → core-foundation-sys emits `-framework CoreFoundation`, which zig cannot resolve without a macOS SDK.

**Shape.** `[patch.crates-io]` replaces iana-time-zone with `patches/iana-time-zone`, a version-matched stub whose `get_timezone()` always errors. Safe because chrono consults it only as a *fallback* after its primary timezone sources (`TZ`, `/etc/localtime`) and before its final UTC default — and the only local-time user in reedline is its default prompt's clock display, which the REPL does not use (it renders R's own prompt). The `release` justfile recipe preflights the aarch64-apple-darwin graph for known framework-linking crates so a regression fails in seconds with a named culprit instead of deep inside the nix zig link. The stub must stay version/feature-compatible with what chrono requests or cargo silently prefers the real crate — the preflight catches exactly that failure mode. One nix-specific trap: crane's dep-only builds compile dependencies against a *dummified* workspace copy (local `.rs` files emptied so the dependency cache survives source edits), which would empty the patch crate too and break chrono's compile — `flake.nix` restores `patches/` verbatim into the dummy source (crane's `extraDummyScript`), interpolating only that directory so the cache stays source-independent.

**Alternative implemented first, then reverted:** fetching a macOS SDK (from the widely used third-party mirror of Apple's SDKs) and exporting `SDKROOT` for the darwin cross-build — mechanically verified (with the SDK the failing build links a valid arm64 Mach-O), most general, zero behavior delta. Reverted on user direction: it adds a large third-party artifact to the release closure and Apple's license on redistributed SDKs is gray. Vendoring the tarball is legally worse (you become the redistributor).

**Framework-free is sustainable for this product.** libR is dlopen'd at runtime (zero link-time deps), and a terminal REPL's plotting story is file output, terminal image protocols, or a browser — nothing on the roadmap needs `-framework` at link time. **Triggers to revisit:** a dependency that genuinely needs an Apple framework (native windows, clipboard integration), or signing/notarization pressure — at that point build the mac artifact on a real macOS runner (the only fully license-clean way to use Apple's SDK), which is also the natural home for running the REPL e2e suite against a real R in CI.

# Decision record: diagnostic rendering stays handrolled (miette rejected)

**Status:** decided and implemented (user delegated: "miette or similar, only if it doesn't add too much weight — otherwise handroll").

**Why.** The CLI's human renderer already has the rustc shape (severity header, `-->` location, gutter, snippet, colored carets, related notes); adopting miette would mean rebuilding a working system around a new dependency tree (fancy feature: several transitive crates plus backtrace machinery) for visuals we largely have. The actual weaknesses were fixable in-place.

**What changed.** The header now carries the diagnostic code (`warning[unused]:` — exactly what a `# roughly: allow(...)` suppression must spell, so the output teaches it); multi-line spans render their first line with the underline to end-of-line plus a dim "range continues for N more lines" note (previously every spanned line printed with one dangling caret); paths render relative to the working directory. Revisit miette only if requirements grow past this shape (multi-span labels, error chains with source causes).

# Decision record: vendored export manifests — the name-level truth beside the typed stubs

**Status:** decided and implemented (closes the stub-completeness audit).

**Problem.** The typed stub corpus (~530 declarations) was also the *resolution universe*: any real standard-library export outside it warned "could not resolve" (`recover`, `traceback` were user-reported instances of a ~2,500-name false-positive class — base alone exports ~1,400 names). Chasing completeness with hand-written typed declarations does not scale and was never the corpus's job.

**Shape.** Every namespace R ships pairs with a generated `types/<ns>.exports` manifest — its complete export list from a live R session (`scripts/export-manifests.R`; header records the R version; currently R 4.6.1; `datasets` uses the search-path listing since its objects are lazy data, not namespace exports). The `StubSources` input carries `(sources, manifests)`; the loader unions manifest names into `exports_by_namespace` (so `pkg::name` validation and shadow lints see them) plus a flat `known_exports` set consulted by `package_scheme_exists` after schemes and nominals. A manifest name resolves everywhere a typed name does — bare, qualified, completion, typo-suggestion corpus — but types `Unknown`: precision stays the typed corpus's job; the manifest's job is silence about real names. Three tiers mirror R: default-attached namespaces (incl. the new `datasets`, whose famous frames are typed `data.frame` in `datasets.Rtypes`) are bare-visible unconditionally; R-shipped-but-unattached namespaces (`QUALIFIED_ONLY_NAMESPACES`: tools/parallel/compiler/grid/splines/stats4/tcltk) always validate `::` reads but gate bare visibility on attach/declare; conditional CRAN namespaces gate both, with their stubs.

**Audit teeth.** A unit test asserts every `.Rtypes` value declaration is a real export of its own namespace (`@type` nominals exempt — they name classes, not bindings; conditional namespaces may also override base names, e.g. data.table's class-preserving `merge`). Writing it immediately caught two misfiled declarations — `traceback` and `standardGeneric` are `base` exports, not `utils`/`methods` — both moved.

**Aside discovered en route:** agent containers CAN have real R — `apt` + the CRAN repository installs current R in minutes (data.table/dplyr compile from source) — so R-dependent tooling (manifest regeneration, the REPL e2e suite) runs in-container after all; the long-standing "no agent container has R" assumption is dead.

**Impact.** Kills the could-not-resolve false-positive class for the whole shipped standard library at zero check-time cost for unused names; completion and suggestions widen to the full export lists (non-syntactic names excluded from bare completion, backtick-quoted after `pkg::` — inserting them raw would change syntax); the not-exported warning for `pkg::name` becomes accurate instead of curated-subset-based.

# Decision record: the Zed extension versions on its own line

**Status:** decided and implemented.

**Problem.** Three shipped artifacts carry a version, and only two of them derive from the workspace `Cargo.toml`. The CLI is the source of truth; the VS Code extension bundles that binary, so its manifest carries the same number with the prerelease suffix stripped (the marketplace rejects `X.Y.Z-alpha`) — a mechanical derivation. The Zed extension bundles nothing: it locates a binary at run time (LSP settings path, then `PATH`, then the latest GitHub release), so its version describes the extension's own code and nothing about the CLI. That was settled once and still failed to hold — the release recipe stopped bumping it, and the number was then hand-realigned to the CLI's twice anyway, the second time with a test added to mandate the alignment. A version that must be manually resynchronized to a number it has no relationship with is the duplication, not the cure.

**Shape.** `editors/zed/extension.toml` is a plain-semver line of its own (`0.1.0`), restarted because the extension has never been published to Zed's registry and nothing constrains its history; the wasm crate's `Cargo.toml` version tracks that manifest rather than the workspace, and neither inherits `version.workspace`. It is bumped by hand when the extension changes. The release-metadata test asserts the VS Code derivation only, and its module doc states why the Zed manifest is absent — the test is the thing that would otherwise re-couple them. The prerelease suffix is dropped for good: `-alpha`/`-beta` name the CLI's release channel, which an extension that only locates a binary cannot be in.

**Impact.** One number per artifact with one owner each; a Zed release no longer implies a CLI release or vice versa. The recurring "align the stale zed extension version" commit has no reason to exist.

# Decision record: one source of truth for where an item spells its name

**Status:** decided and implemented.

**Problem.** Three consumers located a top-level item's declared name independently: the IDE (`declared_name_range`: naming's top-level binding, else the first `NAME` node — which is `setGeneric` itself for an S4 generic), the duplicate-definition sites (the first `NAME` node whose text equals the name — no answer for a string target), and the script unused check (naming's binding, else the whole statement). Where naming mints no top-level binding — `x <<- 1` at top level, `setGeneric("name", …)` — the unused warning highlighted the entire statement instead of the name. The classification also read `:=` as an assignment spelling, publishing a definition that R and the HIR (a call to `:=`) never make.

**Shape.** `classify_top_level` now delegates to `top_level_definition`, which returns the item's kind, name, and the range of the syntax spelling that name (the assignment target, or the `setGeneric` name inside its quotes); `:=` is no longer a binding spelling. In the IDE a project-defined generic is one global symbol: its `setGeneric`/`setMethod`/`standardGeneric` strings and its calls navigate, reference and rename together. `semantics::item_name_range(db, item)` re-reads it off `item_node` at the rendering edge (item identity stays position-free). All three consumers read it; the heuristics are deleted.

**Impact.** Item identity and the name site cannot disagree about which name a statement binds. Unused warnings, goto, hover and the outline land on the name for every definition shape. No new query or stored state: the lookup is a hash probe plus a descent into one statement.

# Decision record: a check reads per-name firewalls, never project-wide maps

**Status:** decided and implemented.

**Problem.** `item_check` resolved cross-item reads against three whole maps it read eagerly: `package_definitions` (every winner in the project), `interface_sccs` membership, and its own file's item positions. Any new or renamed definition anywhere changed those maps, so every check in the project re-ran on its next demand, even one reading no name at all. The previous engine had per-symbol firewalls; the new stack had lost them, and nothing measured it (the keystroke probe edits one file and demands only that file).

**Shape.** Three small tracked projections, each re-run per read after a structural edit and backdated when its answer is unchanged:
- `package_name(files, name)`: the name's winner and conditional writers;
- `interface_group(files, item)`: the item's cyclic group, if any;
- `frame_binder(item, name, deferred)`: the file-local binder an immediate or script read sees. Package function bodies (the common item) never consult it.

`SalsaGlobals` holds no map any more. `item_check` also grows the stack on demand (`stacker`): demand recursion is as deep as the longest chain of definitions each reading the next.

**Impact.** Correctness unchanged: every fixture suite is byte-identical. A new definition re-checks only its readers; a statement insertion re-checks only the reads it intercepts (`crates/semantics/tests/test_incremental.rs`). Memory: +3.5% resident at 794K lines (1270 → 1314 bytes/line); cold time unchanged. Remaining gap: salsa-level cycles through statement items, which `interface_sccs` does not model, re-iterate on every structural edit (`backlog.md`).
