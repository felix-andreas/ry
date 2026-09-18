---
title: Inline type syntax
description: "Proposal, not implemented: inline type annotations as a compiled R dialect"
---

This is a proposal. Nothing here is implemented.

The recommendation, up front, is not to build this for inline typing alone.
The ergonomic case is weaker than it looks, which section 3 shows, and the
cost is close to total. A typed file is not R until it is compiled, so every
other R tool is blind to it and every contributor needs this tool installed.
Build it if and only if the capabilities in section 7 are wanted. Those need a
syntax, and inline typing is the cheapest way to prove the machinery they
would sit on. Sections 3 and 7 are the two that decide it. The rest is design
detail for whoever says yes.

## 1. What it is

This is a source language where a type is written at the position it
describes, compiled to plain R. Here is the notation today:

```r
#: <T: numeric> fn(values: T, scale: T) -> T
scale_by <- function(values, scale) values * scale
```

Here is the proposal:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T {
  values * scale
}
```

## 2. Four inline positions, and `#:` stays

A type goes in four places, all reusing the existing notation verbatim:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T { ... }
count: integer <- 0L
lapply(xs, function(v: integer) v + 1L)
```

The four positions are the generic binder, the parameter type, the return
type, and the binding type. The binder needs its own position because the
existing notation puts it as a prefix on the whole type expression, as
`<T> TYPE`. Inline there is no whole type expression to prefix. Every language
with generics solves that the same way.

A `#:` comment stays available in a typed file, and that is deliberate.
Several annotation forms are not type expressions at all and have no inline
position. `@type` and `@alias` declare types, `@new` mints a nominal value,
and `@trust`, `@if-unknown`, and `@strict` are directives. The typing
reference is explicit that `@new` is an annotation form, not a type
expression. Banning `#:` in a typed file, which an earlier draft leaned
toward, would make the dialect strictly less expressive than plain R. There
would be no record types, no nominals, and no generic aliases.

Keeping both carriers also makes conversion additive. A `.R` file becomes a
typed file by renaming it. Nothing must be rewritten, and an annotation moves
inline one at a time, or never. The division states in one line: the inline
syntax carries types on code, and `#:` carries declarations and directives.

## 3. The ergonomic case, corrected

An earlier draft of this page listed four costs of the comment carrier. Three
of them do not survive checking. The correction matters more than the original
claim, because it is what moves the decision to section 7.

**A parameter name is not duplicated.** That claim was wrong. A name in the
compact form is optional. This checks, with no name written twice:

```r
#: fn(character, double) -> double
fee_for <- function(region, amount) { ... }
```

Writing the names is a style choice for readability, not a tax the carrier
imposes.

**Broken attachment is loud, not silent.** An intervening plain `#` comment
does detach a `#:` block. The result is an error whose message contains the
fix, and it reads: "A `#:` typing comment must be followed immediately by an
expression. Put it directly above the definition, below any roxygen2 block."
The natural roxygen order works with no diagnostic. This is one
self-explaining error, not a trap.

**Annotating a local works.** A `#: integer` line above a local assignment is
checked. `count: integer <- 0L` is shorter, which is a preference.

**One real gap remains.** A `#:` comment attaches to a statement, and a lambda
inside a call argument is not a statement, so its parameter cannot be
annotated:

```r
out <- lapply(xs, function(v) v + 1L)     # `v` has no annotatable position
```

Worse, an annotation written in that position is silently ignored. A
deliberately contradictory type produces no diagnostic. Two separate defects
hide there: the silence, and the gap. The silence should be fixed in the
comment carrier whatever happens to this proposal. An annotation in a position
that cannot attach should be reported. That is a small change, it is valuable
on its own, and it belongs in the backlog either way. A diagnostic only makes
the failure visible, though. It does not give you a way to type the lambda.
Only inline syntax does.

The honest ergonomic case is therefore one position that comments cannot
reach, plus shorter locals. That does not justify a source language.

## 4. Compiling to R

**The output is annotated R.** Each inline type is emitted as the `#:` comment
that means the same thing, so the generated file independently type-checks
under today's contract. That gives a complete correctness test nearly for
free, because checking the source and checking its output must produce the
same findings, and it keeps the generated file self-describing. Section 3's
complaints do not apply to generated output. Duplication and attachment are
the compiler's problem there, not a human's.

**The generated file under `R/` is the runtime truth for everyone else.** R
runs it, `source()` reads it, another package sees it, and CRAN ships it.
Nothing outside this tool ever reads a typed source file. That is the whole of
the interop story, and it should be stated rather than implied.

**Analysis reads the source, and a stale twin is reported rather than
ignored.** A typed source shadows its generated twin, so each definition is
analyzed once. Shadowing alone would make a stale committed twin invisible,
which is the configuration where a reviewer reads one program and the author
reads another. The twin's header hash is therefore checked during analysis
too, and a mismatch is a project-level diagnostic in the editor, not only a
`ry build --check` failure.

**A mixed package needs no special rule.** A typed source participates in the
package namespace exactly as its twin would. Collation order is defined over
the logical file set, which is the source where one exists and the twin
otherwise. That set is total, so "later file wins" keeps its meaning.

**No position map has to be persisted.** Diagnostics come from analyzing the
source, so the positions are already the author's. A map is needed only to
trace a runtime error in generated code back to its origin, which is a
debugging convenience. The committed, formatter-normalized output plus a
header pointer is the fallback every compiled-to-host language relies on.

**The editor never writes.** Generation happens in `ry build`, and in a single
`ry build --watch` that the user starts. Two editors open on one project would
otherwise be two processes writing one path. Determinism plus the header hash
means a redundant build writes nothing at all.

## 5. Naming

A source file is `.ry`, and source files live in a `Ry/` directory beside
`R/`. A type declaration file is `base.ry.stub`. Two consequences belong to
this design rather than to the naming itself.

- The dialect names itself rather than borrowing R's extension family, so
  nothing about the filename implies R can execute the file. That is good,
  because R cannot.
- `Ry/` must be listed in `.Rbuildignore`. `R CMD check` notes a non-standard
  top-level directory, and the generated R under `R/` is what should ship. A
  typed source could physically sit in `R/`, because R's file collector
  ignores an unknown extension. A sibling directory keeps tarball hygiene and
  roxygen simple, and it leaves no doubt about which file ships.

A compound extension such as `.ry.stub` is a trap for a matcher that splits on
the last dot. Any file-kind matcher this adds must match the full suffix.

## 6. The parser work

An earlier draft claimed this is not a large parser change. That was wrong,
and the reason is specific. The type grammar is not self-delimiting. It
currently lives in a region the R grammar skips as trivia, and every
type-parsing function takes a precomputed region end derived from newlines.
Inline there is no region end, so termination becomes a grammar decision at
two genuinely ambiguous points.

- Where a return type ends and the body begins. `-> double { ... }` is easy.
  `-> fn(integer) -> double { ... }` is a higher-order return type containing
  its own arrow, and the `{` could belong to either.
- Whether `<` and `>` are binder brackets or comparison operators. R's own
  grammar cannot disambiguate this, because `a < b > c` is a parse error in R.

Two things are not problems, and this is verified against R's parser. A
parameter type and a `->` in the return position are pure extensions.
`function(region: character)` and `function(a, b) -> double {` are both hard
parse errors in R today, so nothing is being reinterpreted.

One position does collide. `count: integer <- 0L` already parses in R, as the
replacement form `` `:<-`(count, integer, 0L) ``. No working program contains
it, because `` `:<-` `` is not defined in base R, so the form errors at run
time unless someone defines that function deliberately. The dialect must still
state that it claims the form, and it must use the presence of `<-` to tell a
binding declaration from a sequence expression, because a statement-level
`a: b` is valid R.

That last point bounds the superset claim precisely. Every runnable R program
is a valid typed program. That is weaker than "every parseable R file", and it
is the honest version.

## 7. What a syntax would make possible

This is where the value is, if there is any. There are two categories,
separated by whether the compiler has to invent a runtime representation.

**The representation is already decided.** An inline type, as in section 2,
compiles to a comment over unchanged code. A record or tuple constructor
compiles to `list(...)`, because the type system already has both shapes
structurally. What a constructor buys is a checked construction site. A
misspelled or missing field is caught where the mistake is, rather than at
some later read.

**The representation is still open for a tagged union.** A tagged union
declares that a value is one of several kinds, with dispatch that reports the
case you forgot. This is the feature with real pull, and it is the one that
most needs deliberate design, because the runtime representation is an open
choice. A field the compiler owns, such as `list(.tag = ...)`, is closed and
controllable. R's class attribute is open, it is mutable, and it carries
inheritance that this type system does not model. Neither is obviously right,
and neither should be assumed. Nothing here should be designed until inline
typing has shipped and the dialect has users.

Two claims an earlier draft made here were false and are withdrawn.
Non-bypassable construction is not something only syntax can offer. `@new` is
already the only nominal introduction, and its construction is already checked
against the representation. A compiler-written constructor still emits
`list(...)`, so any R code that fabricates the list bypasses it equally.

Anything that changes evaluation is a different proposition. Block scope is
one example, since R's braces are not a scope. The output then stops
resembling the input, and every debugging story gets harder. That is where a
dialect stops being a frontend and becomes a language.

## 8. The cost, and what would actually test it

A typed source file is not R. Until it is compiled, roxygen2 will not document
it, `devtools::load_all()` will not load it, RStudio will not highlight it,
CRAN will not accept it, and every contributor who touches it needs this tool.
The risk is not that a build step is unfamiliar. `R CMD build` is one, and
roxygen2 is code generation that most packages already run. The risk is that
the source of truth stops being R, for collaborators and for CRAN.

A standalone script is the cheap place to start, and it does not test that
risk. `ry run script.ry` would type-check, compile in memory, and execute
through the R runtime the REPL already embeds. It needs no generated file, no
packaging, and no collaborator toolchain. That makes it a good way to find out
whether inline typing is pleasant, and no evidence at all about whether a
package author will accept a generated `R/` tree, because a script removes
every variable that constitutes the objection. Testing the packaging question
requires putting a generated tree in front of package authors.

## 9. Open questions

- Where a generic binder goes, if not `function<T>(...)`, given that R's `<`
  is a comparison operator. Section 6 explains the ambiguity.
- What the source directory is called.
- Whether an inline type may annotate `...`, which the comment notation types
  as a rest parameter.
- Whether the script path in section 8 ships with no `ry build` at all in the
  first version.
- Whether hover, go-to-definition, and rename work on an inline type position.
  They are expected to fall out of the existing type-position support, and
  that is unverified.
- How the editor behaves on a half-written inline type, where error recovery
  matters more than it does in a comment.

## 10. Prior art

**`we-data-ch/typr`** is a typed language for R, written in Rust, transpiling
to R, with the extension `.ty`. It is a sibling language rather than a
superset. `fn` replaces `function`, a statement ends in a semicolon, and a
boolean is lowercase, so an existing R file cannot adopt it by adding
anything. That is the opposite of the position here, where renaming a file is
the whole of adoption.

Its type reasoning runs on Prolog, which is a required install alongside R.
Whether that could answer at editor latency is not something this survey can
judge. Its documentation site could not be read, and its own README calls the
project an early prototype. The shape of the bet is the opposite of the
fast-to-check rule in `decisions.md`.

**The compiles-to-host lineage**, which is TypeScript, Sorbet's `.rbi`, and
Python's stub files, converged on the same split this project already has.
Declarations go in a separate file for foreign code, and annotations go inline
for your own. `.Rtypes` implements the first half, and this proposal is the
second.
