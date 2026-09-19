# Test-user reports: resolved findings

This file archives the simulated-user findings that are closed. An open one stays in `backlog.md`,
and a finding moves here when it is fixed, so the backlog shows only the work that is left while the
reports themselves survive. Each entry keeps three things: what was reported, what the measurement
actually showed, because the premise was wrong more than once, and what shipped.

## The typing-enthusiast round

### The checker gave a wrong answer rather than a skipped one, in two confirmed cases

`limitations.md` sells the whole trust story on one sentence, which is that a gap means checks are
skipped rather than that wrong answers are produced. Both of these broke it, and both were silent
under `strict = true`.

One lesson is worth keeping. Each produced a type belonging to neither of the program's possible
states, being a stale field type and a merged signature, and in both cases that single wrong type
generated a false positive and a false negative at once. That is why either could be mistaken for a
mere precision gap when read from one direction only.

**A field write through a nominal value was discarded, and the stale belief was used both ways.**
`replacement_written_type` handled `Record` and an empty `Tuple` and let everything else fall through
to `_ => prior`, so a nominal kept its declared field types after a contradicting write. The write is
now checked against the representation rather than applied to it. A nominal's representation is
fixed, so a value the declared field type refuses is reported at the value, a field the
representation does not carry is reported by name, and the binding keeps its nominal type either way.
That is what makes the `@type` invariant hold past construction. Both spellings, `$` and
`[["literal"]]`, go through one path. An opaque or non-record representation stays quiet, because R
only warns and coerces for `x$f <- v` on an atomic, so there is nothing to refuse. A structural
record still retypes, which is the contrast the report drew. The reference is updated and seven
fixtures pin it.

**A branch join involving a function slot invented a type belonging to neither path.**
`join_writes_reporting` let a branch's scheme replace whatever preceded it, and a monotype replace a
scheme, so a conditionally reassigned function kept one arm. `pick` exported
`fn(shout: logical) -> fn(x: T) -> character`, taking the parameter from the identity branch and the
return from the `paste0` branch, which is a signature neither function has. Both directions followed.
A character-expecting call on `pick(FALSE)(1L)` was accepted although R gives `integer`, and numeric
use was rejected with ``found `character` ``, which was a claim about the value that was simply
untrue.

The join now unions the two entries' monotypes, and it unions rather than unifying. `join_types`
tried unification first, and two instantiated schemes always unify through their fresh variables,
because `fn(x: T) -> T` unifies with `fn(x: U) -> character` by binding `T := character`. That is
exactly how the fabricated signature arose. `pick` is now
`<T, U> fn(shout: logical) -> fn(x: T) -> T | fn(x: U) -> character`, calling it returns
`integer | character`, and a single reaching write still keeps let-polymorphism. The reference
already specified this join correctly, and the implementation had never matched it.

The original report's framing was wrong, and correcting it is the useful part. This was not correct
code being rejected. R fails on the other path, because `pick(TRUE)(1L) + 1L` is a non-numeric
argument to a binary operator, so refusing the reproduction is right. What was broken was the
fabricated type and the inaccurate message. The reproduction still reports, now as
``found `integer | character` ``.

### False positives on the first thing a newcomer writes

**Guard narrowing did not fire at file top level.** `recognize_guard` required the tested read to be
in `naming.resolutions`, and a top-level variable read by a later statement is not, because scopes
are per item so it is a non-local. The guard bailed at the slot lookup, which is why the
byte-identical guard inside a function body was clean: there, everything is one item.

A non-local read now gets a checker-minted slot, with ids allocated above every id naming issued so
they cannot collide, seeded with the type the read observed. The ordinary environment carries the
refinement from there, so there is no second place to keep flow state. Null, negated and family
guards all narrow at top level now.

One limit is inherent and is documented rather than hidden. A refinement does not outlive the
statement it was made in, because checking is per top-level statement. So `if (is.null(x)) stop(...)`
followed by a separate top-level statement does not narrow, while the same idiom inside a function
body or one braced block does. Both are fixtured. Making it cross statements would mean threading
flow state between items, which `item_check`'s per-item memoization is built to avoid. That is the
reason rather than an oversight.

**A duplicate type name in one script file went unreported, and the report's premise was half
wrong.** `duplicate_type_diagnostics` returned early for any non-package file, and the report read
that as the check not running for script projects, implying a script's declarations should conflict
across files like a package's. They should not. Measured, a script's type declarations reach only
their own file, so an `@alias Shared` in `one.R` is not visible from `two.R`, which reports that it
does not know the type. Two scripts may therefore each declare `Thing`, and reporting that as a
conflict would have been a new false positive.

The real gap was narrower. A name declared twice inside one script file, where the namespace
genuinely is shared, went unreported, the later declaration silently won, and the mismatch it
produced was unfalsifiable: a `Thing` declared `double` above and `character` below yields
``expected `character`, found `double` `` with nothing in view to explain it. Both declarations are
reported now, each pointing at the other, with wording that names the namespace it is judged against.

Packages already handled the within-file case, because the project map counts one file twice, and
that path is untouched. Three script fixtures pin it, plus a CLI test for the cross-file
no-conflict half, which the fixture suites cannot express because a case is one file. The reference
said duplicates are errors regardless of file while also saying that a non-package file does not join
the project namespace. It now states which namespace a duplicate is judged against.

**`= NULL` is no longer exempt from the default-value check.** This was a documented decision rather
than an oversight, saying that a `NULL` default is R's no-value sentinel and is always allowed, so it
was weighed rather than flipped. The measurement settled it, because the exemption hides a real
crash. Declared `[title]: character` with `title = NULL`, the body's `if (title == "draft")` passed
the checker and failed at run time with R's argument-is-of-length-zero error. Declared
`character | NULL`, the same body is caught statically, and adding `if (is.null(title))` clears it,
so the remedy was already fully supported and only the exemption stood in the way.

The default is checked like any other now, with its own diagnostic rather than a bare mismatch,
because this is the usual R spelling and the fix is specific. The message says that the parameter
defaults to `NULL`, which its declared type does not admit, that a caller who omits it leaves `NULL`
in the body, and that the fix is to declare it as a union with `NULL` and narrow with `is.null()`.
`Any` still admits it, an unannotated parameter is unaffected, and a non-`NULL` default is unchanged.

Three examples in the reference itself relied on the exemption and now read `character | NULL`. One
of them guarded with `if (!is.null(label))` while declaring `label` as plain `character`. A
specification demonstrating the lie in its own sample is the clearest evidence the rule was wrong.

### Annotations that validated themselves and then enforced nothing

**An annotation in call-argument position is reported instead of silently dropped.** As filed, the
danger was that a typo inside such an annotation is reported, so the user got positive feedback that
a block doing nothing was live. Verified per position: a braceless function body, a braceless `if`
branch and a parenthesised expression all attach, and only `ARGUMENT_LIST` drops, so the check keys
on that alone. It reports rather than attaches, because giving a lambda parameter an annotatable
position is a separate and still-open expressiveness question.

**An annotation declared the parameter list, not just the parameter types.** As filed, one bad
`@param missing_one` produced 5 errors, four of them telling correct `f(1L)` calls they were missing
an argument. Reproducing it showed the cascade was one symptom of a general defect. The exported
signature was the annotation as written, so every way the two sides can disagree about arity was
resolved in the annotation's favour and charged to the call sites.

| disagreement | before | R |
| --- | --- | --- |
| `@param` names a formal that does not exist | 1 real error plus 1 per call site | not applicable |
| only some formals annotated | 1 error per call site, nothing at the definition | calls are fine |
| more declared types than formals | nothing at the definition, 1 error per call site | calls are fine |
| `[x]` declared optional, formal has no default | `f()` accepted | `argument "x" is missing` |
| `...` declared, formals fixed | `f(1, 2, 3)` accepted | `unused arguments` |

The partial-annotation row mattered most, because annotating a single `@param` of several turned
every correct call into a finding, and that is the ordinary way to start annotating.

The fix is one rule, now in the reference: an annotation declares the types of a definition's
parameters, never the parameter list. R matches arguments against the `function(...)` header, so
`check_declared_function` builds the exported signature from the formals, taking their names, order,
optionality and `...` position from the code, and fills the types from the declaration, name-aware.
Every disagreement is reported once at the definition and never again at a call. Two of the five rows
were false negatives, so this also closes call shapes that R rejects and the checker accepted. Both
were verified against R 4.3.3 rather than assumed.

Two things came with it, and both are required for the signature to be honest.

- **An undeclared formal keeps its inferred type.** It is a fresh variable like any unannotated
  parameter, so the export edge in `close_scheme` either generalizes it or erases it to `Unknown`.
  Partial annotation now adds checking instead of removing it, so `#: @param x {integer}` on
  `function(x, y) x + y` infers `y: integer` and catches `f(1L, "no")`.
- **An elided return is inferred from the body.** The reference already promised this and the
  implementation had never done it, and a fixture literally named
  `elided_definition_return_still_infers` was blessed at `-> Unknown`. A written `-> Unknown` is
  treated identically, because the reference says `Unknown` records that the checker could not tell
  and is not an explicit escape hatch. `Any` is the way to say do not check this.

One thing was folded in. `reconcile_declared_optionality` was a second, partial version of this
reconciliation reachable only from the item root, so a nested definition never got it. It is gone,
and its diagnostic blames the function definition like its siblings instead of the whole assignment.
A formal tested with `missing(x)` counts as optional against the annotation too, which was a false
positive on R's optional-without-default idiom.

Seven fixtures pin it. Findings are byte-identical across data.table, dplyr, ggplot2 and shiny, at
7,116 findings. Those packages carry no `#:` annotations, so that is a no-collateral-damage check
rather than coverage.

### Rendering dropped information the message depended on

**A rejected function names the position that failed instead of printing both signatures.** As filed,
`lapply(words, function(s) s + 1L)` over a character list reported
``expected `fn(character) -> T`, found `fn(s: U) -> U` ``, which describes a call that should fit and
never mentions `character`, `+` or numeric. The underlying claim was confirmed directly, because
`function(x) x + 1L` and `function(x) x` both render `fn(x: T) -> T` in a diagnostic, so an
acceptable and an unacceptable function are indistinguishable.

The premise was right and the diagnosis pointed at the renderer. A constraint belongs to the variable
rather than to the type. It can only appear in a binder prefix, and a diagnostic renders a monotype,
so there is no place in `fn(s: U) -> U` for "U must be numeric" to go. Printing both signatures is
therefore the wrong shape for this failure whatever the renderer does.

What ships instead is `InferenceTable::explain_function_mismatch`, which re-walks the pairing and
names the one position that failed, and a finding that says what that position needs rather than
showing a type it cannot show. For a parameter it says that this function is passed `character` but
its parameter `s` is used as a numeric value, or that its parameter accepts `character` when there is
a type to show. For the return it says that this function must return `logical` but its body produces
a numeric value.

Both signatures are still printed for a shape disagreement, meaning arity, optionality or a rest
parameter, which is the case they genuinely explain, and a fixture pins that fallback. The pairing
rule is one function, `pair_parameters`, shared by the compatibility verdict and the explanation, so
the two cannot drift.

The filed alternative, pushing the expected parameter type into the lambda body, was not taken, and
the reason is architectural. `CallArgument` inters each argument exactly once before any signature
matching, so an overload probe can re-match without re-running expression inference. Bidirectional
checking of a lambda argument would re-infer the body per candidate. It is worth revisiting as its
own slice if callback diagnostics need to point inside the lambda, and the position-naming message
covers the reported cases without it.

The same root cause is confirmed gone for the `Filter` report and for the
``expected `list[T] | T[]`, found `character[]` `` one. Four fixtures pin it. Across data.table,
dplyr, ggplot2 and shiny the finding counts are unchanged and exactly two messages differ, both real
corpus findings that now read clearly.

**A function member of a union renders with its grouping parentheses.** As filed,
`(fn(A) -> B) | NULL` printed identically to `fn(A) -> (B | NULL)`, which are two different types, so
copying a type out of a finding and back into an annotation silently changed it. The reference
already specified the fixed behavior, saying that an optional callback is written
`(fn() -> integer) | NULL` and that this is also the form such a union renders as. The renderer
simply joined members with ` | ` and never parenthesized.

The rule is narrow and it is the only ambiguity in the grammar. `->` extends over a whole union, so a
function inside a union needs its parentheses back, and nowhere else does. Everything else is
delimited by a bracket, a comma or a closing paren.

It reached more than the filed case. The nullable-callback finding was describing a function that
returns a nullable character while talking about a value that may itself be `NULL`, and it now reads
`(fn(x: Any) -> character) | NULL`. A branch join of two function signatures printed
`fn(shout: logical) -> fn(x: T) -> T | fn(x: U) -> character`, which under the rule that `->` extends
over a union is not the type meant at all. One fixture pins both spellings, written as the rendered
forms pasted back, so it fails if either stops round-tripping.

**A refused annotation reports once, and the report is the true one.** As filed, both halves
reproduced, at 5 findings for the rank-2 case rather than 7. The first was a false claim that only
one compact annotation fits in a `#:` block, against a block holding exactly one, and the braceless
`@param count integer` claimed a form clash that did not exist. Both prescribed a blank line that
would not have helped. There were three separate causes, and each is closed.

- **The form classifier counted parser-recovery debris as block items.** Recovery re-parents the
  pieces of a type it could not read to the block, so one line yielded three top-level types. The
  rules are about lines, because every one of them says to separate with a blank line, so the check
  compares whole `#:` lines now, and a line's form is its first item's. A line carrying a second item
  did not parse as the form it committed to, which is a parse failure rather than a clash.
- **The refusal recovered by consuming the binder and stopping**, which left the position with no
  type at all, so the enclosing parameter list then failed on the type that followed. That is one
  real finding under three consequences. It reads the type as if the binder were absent now.
- **A refused block still carried its typing payload**, so the names the binder would have bound were
  reported as unknown types on top of the refusal. The parser marks a region it refused with an
  `ERROR` node, and a block containing one carries no payload and reports nothing of its own. The
  contract already said this for a shape violation, and a parse failure is the same situation.

Every rank-2 position now gives exactly one finding, covering a parameter, a return, a list element
and a directive payload. That last one was silently accepted before, because `ann_braced_type`
allowed binders, so `@param f {<T> fn(T) -> T}` parsed, bound nothing and said nothing. A directive
payload is not the outermost level, since `@forall` and `@type Name<T>` are where the expanded and
named forms declare parameters, so it is refused like any other nested binder.

The code moved too, and the docs page that documented the old behavior is updated. **A diagnostic
code says whose grammar was broken, not which stage noticed.** `syntax-error` means R the parser
could not read, and `annotation` means a `#:` comment that is wrong. Keying it on the stage put this
deliberate limit under `syntax-error` while its siblings, an unknown constraint and a malformed
block, reported as `annotation` only because lowering happened to catch them. `SyntaxError` already
carried the `in_annotation` flag for exactly this distinction.

Two traps are worth keeping, and breaking the suite found both. An `ANNOTATION_MARKER` is not always
a direct child of the block, because a stitched line's marker lands inside whatever node was open
across the break, so line boundaries must be counted over all tokens. And a `<T>` binder list is a
prefix of the type after it rather than an item, so counting it as one refuses every stub declaration
that has a binder, which silently emptied the whole stdlib corpus.

One thing was noticed while fixturing and not fixed. The same cascade shape is visible in
`annotations-types.R.test` for `fn([x] integer)`, at 4 findings, and `fn([1]: integer)`, at 9. Those
are ordinary parser recovery rather than a deliberate refusal, so they need the recovery to
resynchronize at the parameter boundary. It is worth a look, and it is a different mechanism.

## The syntax-error round

One user broke the parser on purpose across 17 cases, using the shapes people actually produce
mid-edit, and judged direction and readability as separate verdicts. This was a user directive. Every
number below comes from running the binary, and R 4.3.3 was the referee wherever it mattered whether
something is an error at all.

**Nine of the 17 cases are exactly right**, meaning one finding, on the token that broke it, worded
so an R user learns their own rule rather than the parser's state. An unclosed `{`, a stray `}`, an
unterminated string, a missing comma between arguments, `if ()` with no condition, and both
truncated-annotation cases are all single precise findings. A broken statement correctly silences the
semantic findings around it, so a function missing its `}` mid-file produced one error and no noise
from the three definitions after it. The model message is the `else` one, which says that `else` must
stay on the same line as the `if` branch it belongs to, or the `if` must be inside braces. It explains
R's rule and gives both fixes. That is the bar the rest should meet.

### An unclosed opener put a false finding on every following line

Recovery did not stop at the statement boundary. After reporting the unclosed `(`, the parser was
still inside the argument list, so each following line read as another argument.

```r
totals <- sum(c(1, 2, 3)      # the one real mistake
a <- mean(c(1))               # "missing `,` between these arguments"
b <- mean(c(2))               # "missing `,` between these arguments"
c <- mean(c(3))               # "missing `,` between these arguments"
```

That is 5 findings, 4 of them on code the user must not change, and it scaled with the file. The
adopted lines lost their own definitions too, so a later read of `a` came back `unresolved`. It is
one finding now, on the unclosed `(`, and the same for `[`.

**The rule is not to break at a line break, and two existing fixtures are why.** A list running onto
the next line is ordinary R, and a fragment there really is a forgotten separator. Both
`sum(alpha\n beta)` and `function(x\n y)` were pinned reporting a missing comma, correctly, and a
blanket line rule regressed both to a worse pair of findings.

What separates the cases is whether the next line assigns. A line binding a name with `<-` or `<<-`,
or opening with `if`, `for`, `while` or `repeat`, is the next statement, and adopting it is what does
the damage. A comma before it still makes it an argument, so `run(1,\n  x <- 2)` stays silent.

**`=` was in that list at first, and that was wrong, because it cost the commonest missing-comma
case.** Inside an argument or parameter list, `name = value` is a named argument, which is exactly
what a multi-line call with a forgotten comma looks like. Counting it as a statement start gave this
snippet

```r
config <- list(
  title = "Revenue"
  subtitle = "by quarter",
  width = 800
)
```

the unclosed-opener report instead of the one that names the missing comma, then broke the rest of the
call into top-level statements. That drew two `assignment-operator` findings and an `unused` out of
source that had not parsed, contradicting the documented rule that a failed statement suppresses
findings overlapping it. Six findings for one comma, and the one the docs promised was gone. R itself
keeps consuming until the opener closes, so the named-argument reading is also the faithful one. A
fixture on that exact snippet pins it now. The three original cases all use `<-` and were never
affected, which is why nothing caught it.

Two more things were needed beyond the predicate. The newline is consumed while parsing the element
before the loop head, so the check has to scan backwards rather than watch trivia go by, and a
peek-based version looked right and never fired. And once the list breaks, the statement loop still
saw a stray token where it wanted a newline and reported again, then recovered over the next line and
swallowed it anyway. An unterminated list marks the statement now, and the boundary check neither
re-reports nor recovers.

### A parse error pointed off the code, at zero width

```r
scale_by <- function(x,        # 3-line file
value <- 42
print(value)
```

The third finding was `expected a function body` at `4:1-4:1`. **The claim first filed here was
imprecise, and correcting it is the point.** With a trailing newline that line does exist, because an
editor counts the empty position after the last `\n`, so it is not unplaceable. It is zero characters
wide, underlining nothing, on the blank line past the code, and pointing away from the construct that
is actually incomplete.

Two things changed. The report blames the `function` keyword, which is the construct that lacks a
body, eight characters wide and on the code. And it is not reported at all when the parameter list
never closed, because that is the same mistake said twice and the unclosed opener is the version
worth reading. That case went from 3 findings to 2, and the two that remain are exactly R's own
reasoning, since R also reads `value` as a parameter and then chokes on `<-`.

### The annotation grammar reported every token it could not use

Measured before the fix, `fn([x] integer)` gave 4 findings and `fn([1]: integer)` gave 9, for one
typo each, with five of the nine on the very same column. The type grammar cannot resynchronize the
way the statement loop can, because a `#:` region is one expression with no boundary inside it to
restart at, so it reported and carried on.

It reports once per region now, on the first thing it cannot use. Both cases report exactly once, and
two separate `#:` blocks still each report their own.

One refinement was needed and is worth keeping. An unclosed opener is discovered at the end of the
construct it names, so first-wins alone dropped the outermost truth in favour of an inner
consequence. `@type Point {list{x: double` reported only that a `,` or `}` was expected in the list
type and never that the `@type` brace was open. One structural report of that kind gets through
regardless now, so that case reports both, outer first.

### Cryptic where the tool already knew better

``unexpected character `“` `` for a typographic quote pasted out of a document had perfect placement
and the parser's vocabulary for wording. Measuring it found a whole family reported the same way:
curly quotes of both kinds, the en dash, the em dash and the real minus sign, full-width parentheses
and punctuation, and the invisible ones.

The invisible ones were the worst. A non-breaking space printed as ``unexpected character ` ` ``,
which is a message pointing at what looks like ordinary whitespace and calling it unexpected.

Each names what the character is and what to write instead now, saying for example that `–` is an en
dash rather than R syntax and to use `-` instead. An invisible one is described rather than quoted,
because printing a zero-width space between backticks shows the reader an empty pair and a caret over
nothing. A character with no known ASCII counterpart keeps the generic wording.

### A dangling binary operator swallowed the next statement

```r
total <- 1 +
label <- "next"
print(label)
```

There is no syntax error, and that is right, because R parses it as
`total <- ((1 + label) <- "next")` and fails at run time. But the user got two
``I could not resolve `label` `` warnings on the lines that look correct, and nothing anywhere about
the dangling `+`.

It was filed as wanting a lint, and **the cause turned out to be sharper than that.** R refuses the
shape outright, saying that the target of an assignment expands to a non-language object, because the
swallowed line became the target of an assignment and `1 + label` is not something you can assign to.
`resolve_assignment_target` had a fallback arm that tried `replacement_base` and then fell back to
`self.resolve(target)`, so an unassignable target was silently read as an ordinary expression and its
names reported.

It is reported where the mistake is now, with the cause named when a trailing operator really is what
pulled the next line in. The message says that this is a value rather than a name, so nothing can be
assigned to it, and that the operator at the end of this line pulled the next line in as its
right-hand side. The target's own reads are suppressed, so the two misleading warnings are gone,
while a genuine later read of the name still reports.

That clause is decided structurally rather than by whether the target spans a line. Naming carries
the span between the two operands, which is where the operator lives, and the hint fires only when
the line break falls after it. A break before the operator is an ordinary continuation, and a
multi-line left operand says nothing about where the operator sits. A plain contains-a-newline test
would have described both wrongly.

It is deliberately narrow, because R accepts more than a bare name. Only a computed value and a
non-string literal are reported. `"x" <- 1` binds `x` in R and stays quiet. `` `y` <- 1 ``,
`names(v) <- ...`, `attr(v, "k") <- ...` and `v[[1]] <- ...` all stay quiet. A parenthesised target is
left alone, because R's own handling of `(x) <- 1` is odd enough that refusing it would be guessing.
All of these were checked against R 4.3.3.

Two carve-outs the corpus forced are worth remembering.

- **A `!`-headed target is exempt.** `!` binds tighter than `<-`, so `expr(!!name <- value)`, which
  builds an assignment rather than performing one, parses as an assignment to `!!name`. This was a
  real false positive in dplyr, whose authors ship `utils::globalVariables("!<-")` to placate
  `R CMD check`. The refusal cannot tell it from a bare `!x <- 1` typo, and the metaprogramming form
  is common while the typo is not. Another unary operator is still reported.
- **The operand-gap span needs a bounds check.** Error recovery can leave the operands out of order,
  and `TextRange::new(lhs.end, rhs.start)` then panics. The `ide` fuzz suite caught it and no fixture
  did, which is the argument for the day-one fuzz rule in one line.

A lint for a line ending in an operator was measured and rejected instead. Across data.table, dplyr,
ggplot2 and shiny, 15,129 lines end in a continuation operator, and every one of them is legal.

### A string assignment target created no resolvable binding

This was found while doing the above. `"x" <- 1L` is legal R and binds `x`, verified against R, which
prints `1`. Typing gave it the binding, as `x: integer`, and naming did not, so a later `print(x)`
reported that it could not resolve `x`. That is a false positive on legal, if unusual, R.

The filed guess was that `replacement_base` should unwrap the string. **It should not, and that is
the part worth keeping.** The replacement path reads the base before writing it, because
`names(v) <- x` reads `v`, and it marks the write unreportable because a replacement is not a dead
store. `"x" <- 1` is neither. It is a plain definition, identical to `x <- 1`. So the string literal
joins the name-target arm by an or-pattern instead, matching
`ExpressionKind::NameRef(name) | ExpressionKind::Literal(LiteralKind::String(name))`, which the
lowering makes exact, because `string_value` has already stripped the quotes and resolved escapes so
the payload is the name R binds. One pattern, and `<<-`, dotted names and the unused check all follow
from it. The dead store reports at the literal's own range, quotes included, which is what was
written, instead of arriving from the item-level export path with the whole statement underlined.

Backticks never needed this, because `` `y` <- 1 `` lowers to a `NameRef` like any other name.

**The corpus caught a consequence, and it was a real bug, older than this change.** Binding string
targets took data.table from 4078 findings to 4079, and the extra one was a false `unused` on
`%fin%`, a helper operator data.table defines inside `merge` and calls twice on the next two lines.
A `%op%` read is recorded by name in `quiet_operator_reads`, which is what the cross-item check for a
package's own operator needs, and it never reached the slot model, so a local operator definition had
no read resolving to it and looked like a dead store. A backtick definition was already exposed to
this, and the string change only made data.table's case visible.

The fix extracts the slot half of a read as `mark_slot_read` and calls it from the operator path too,
which leaves `resolve_read` as the only place that maps an expression id. Three more pre-existing
false positives went with it, each verified as a genuine use: `%+replace%` aliased from `ggplot2::`
inside `theme_custom`, in three copies being the vignette `.R` plus two `.qmd` files, and `%NA_OR%`
in shiny's `render-plot.R`, used four times below its definition. On the corpus, data.table went back
to 4078 exactly, dplyr was unchanged, ggplot2 went from 1318 to 1315 and shiny from 1045 to 1044. An
unused local operator is still reported, because marking the slot did not make the check blanket
quiet, and a fixture pins that.

### The REPL wrapped a table that fits

`ry repl` never set R's `width` option, so R kept its default of 80 columns whatever the terminal was,
and `print()` wrapped a table with room on screen. The width is measured once at startup now and
applied with `R_ParseEvalString` in the window between `setup_Rmainloop()` and `run_Rmainloop()`.
Verified end to end in a pty: a 200-column terminal gives a width of 200, a 100-column one gives 100,
and an `.Rprofile` that sets 137 keeps 137 on a 200-column terminal, because the profiles have been
sourced by then, so only R's untouched default is replaced.

**The first attempt fed `options(width = ...)` through the ReadConsole hook, and it was wrong twice
over.** This is recorded because the console feed looks like the obvious mechanism and there is
already a `.Platform$GUI` fix using it. It costs a main-loop round trip, and a round trip taken
before the editor's first prompt leaves the terminal in the state R found it in, so the next read
desynchronizes. Two pre-existing pty tests began timing out and the suite went from 3.5 s to 94 s.
Queuing the statement earlier, alongside the other pre-prompt input, did not help, because the round
trip itself is the problem rather than its timing.

**Resize is not tracked, and closing that needs a different mechanism.** A stock terminal session
handles `SIGWINCH` by calling `R_SetOptionWidth`, which R does not export. The console feed is the
only other way in, and it is the thing that just proved unsafe between prompts. It is worth
revisiting if someone finds a third route.
