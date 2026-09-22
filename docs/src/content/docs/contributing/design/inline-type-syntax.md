---
title: Inline type syntax
description: "Proposal, not implemented: inline type annotations as a compiled R dialect"
---

This is a proposal. Nothing on this page is implemented.

The recommendation, up front, is **not to build this for inline typing alone**. The ergonomic case is
weaker than it looks (section 3 shows why), and the cost is close to total: a typed file is not R
until it is compiled, so every other R tool is blind to it, and every contributor needs this tool
installed. Build it if, and only if, the capabilities in section 7 are wanted. Those need a syntax,
and inline typing is the cheapest way to prove the machinery they would sit on. Sections 3 and 7 are
the ones that decide the question; the rest is design detail for whoever says yes.

## 1. What it is

A source language in which a type is written right where it applies, compiled to plain R. Today's
notation looks like this:

```r
#: <T: numeric> fn(values: T, scale: T) -> T
scale_by <- function(values, scale) values * scale
```

The proposal would allow this instead:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T {
  values * scale
}
```

## 2. Four inline positions, and `#:` stays

Types go in four places, all reusing the existing notation exactly:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T { ... }
count: integer <- 0L
lapply(xs, function(v: integer) v + 1L)
```

The four positions are the generic binder, the parameter type, the return type, and the binding
type. The binder needs a position of its own because today's notation writes it as a prefix on the
whole type expression, `<T> TYPE`, and inline there is no whole type expression to prefix. Every
language with generics solves this the same way.

The `#:` comment stays available in a typed file, deliberately. Several annotation forms are not type
expressions at all, and so have no inline position: `@type` and `@alias` declare types, `@new` mints
a nominal value, and `@trust`, `@if-unknown`, and `@strict` are directives. (The typing reference is
explicit that `@new` is an annotation form, not a type expression.) Banning `#:` in typed files, as
an earlier draft leaned toward, would make the dialect strictly *less* expressive than plain R, with
no record types, no nominals, and no generic aliases.

Keeping both carriers also makes conversion additive. A `.R` file becomes a typed file by being
renamed; nothing has to be rewritten, and annotations can move inline one at a time, or never. The
division fits in one line: inline syntax carries types on code, and `#:` carries declarations and
directives.

## 3. The ergonomic case, corrected

An earlier draft listed four costs of the comment carrier. Three of them do not survive checking, and
that correction matters more than the original claim, because it is what moves the decision to
section 7.

**Parameter names are not duplicated.** Names are optional in the compact form, so this checks with
no name written twice:

```r
#: fn(character, double) -> double
fee_for <- function(region, amount) { ... }
```

Writing the names is a style choice for readability, not a tax the carrier imposes.

**A broken attachment is loud, not silent.** A plain `#` comment in between does detach a `#:`
block, but the result is an error whose message contains the fix: "A `#:` typing comment must be
followed immediately by an expression. Put it directly above the definition, below any roxygen2
block." The natural roxygen order works without any diagnostic, so this is one self-explaining error,
not a trap.

**Annotating a local works.** A `#: integer` line above a local assignment is checked.
`count: integer <- 0L` is shorter, but that is a matter of taste.

**One real gap remains.** A `#:` comment attaches to a statement, and a lambda inside a call's
argument list is not a statement, so its parameters cannot be annotated:

```r
out <- lapply(xs, function(v) v + 1L)     # `v` has no annotatable position
```

An annotation written in that position used to be silently ignored. It is now reported as an error
that tells you to give the value its own binding, which is the fix the comment carrier could make on
its own. But a diagnostic only makes the gap visible; it does not give you a way to type the lambda
in place. Only inline syntax does that.

So the honest ergonomic case is one position that comments cannot reach, plus shorter locals, and
that does not justify a new source language.

## 4. Compiling to R

**The output is annotated R.** Each inline type is emitted as the `#:` comment that means the same
thing, so the generated file type-checks on its own under today's contract. That gives a complete
correctness test almost for free, because checking the source and checking its output must produce
the same findings, and it keeps the generated file self-describing. None of section 3's complaints
apply to generated output, since duplication and attachment are the compiler's problem there, not a
person's.

**The generated file under `R/` is the runtime truth for everyone else.** R runs it, `source()` reads
it, other packages see it, and CRAN ships it. Nothing outside this tool ever reads a typed source
file. That is the whole interop story, and it should be stated, not implied.

**Analysis reads the source, and a stale twin is reported, not ignored.** A typed source shadows its
generated twin, so each definition is analyzed once. Shadowing alone would make a stale committed
twin invisible, which is exactly the situation where a reviewer reads one program and the author
another. So the twin's header hash is checked during analysis too, and a mismatch is a project-level
diagnostic in the editor, not only a `ry build --check` failure.

**A mixed package needs no special rule.** A typed source takes part in the package namespace exactly
as its twin would. Collation order is defined over the logical set of files, meaning the source where
one exists and the twin otherwise. That set is complete, so "the later file wins" keeps its meaning.

**No position map needs to be persisted.** Diagnostics come from analyzing the source, so their
positions are already the author's. A map would only be needed to trace a runtime error in generated
code back to where it came from, which is a debugging convenience. The committed,
formatter-normalized output plus a pointer in the header is the fallback every compile-to-host
language relies on.

**The editor never writes.** Generation happens in `ry build`, or in a single `ry build --watch` the
user starts; otherwise two editors open on one project would be two processes writing to one path.
Because the output is deterministic and carries a header hash, a redundant build writes nothing.

## 5. Naming

A source file is `.ry`, and source files live in a `Ry/` directory next to `R/`. A type declaration
file is `base.ry.stub`. Two consequences belong to this design rather than to the naming itself:

- The dialect names itself instead of borrowing R's family of extensions, so nothing about the file
  name suggests R can execute the file. That is good, because R cannot.
- `Ry/` must be listed in `.Rbuildignore`, because `R CMD check` notes a non-standard top-level
  directory, and the generated R under `R/` is what should ship. A typed source could physically sit
  in `R/`, since R's file collector ignores unknown extensions, but a sibling directory keeps the
  tarball and roxygen simple and leaves no doubt about which file ships.

A compound extension such as `.ry.stub` is a trap for any matcher that splits on the last dot, so any
file-kind matcher this adds must match the full suffix.

## 6. The parser work

An earlier draft claimed this is not a large parser change. That was wrong, and for a specific
reason: the type grammar is not self-delimiting. Today it lives in a region the R grammar skips as
trivia, and every type-parsing function is given a precomputed end for that region, derived from
newlines. Inline, there is no region end, so where a type stops becomes a grammar decision, at two
points that are genuinely ambiguous:

- **Where a return type ends and the body begins.** `-> double { ... }` is easy, but
  `-> fn(integer) -> double { ... }` is a higher-order return type with its own arrow, and the `{`
  could belong to either.
- **Whether `<` and `>` are binder brackets or comparison operators.** R's own grammar cannot tell,
  since `a < b > c` is a parse error in R.

Two things are *not* problems, which has been verified against R's parser. Parameter types and a `->`
in the return position are pure extensions: `function(region: character)` and
`function(a, b) -> double {` are both hard parse errors in R today, so nothing is being reinterpreted.

One position does collide. `count: integer <- 0L` already parses in R, as the replacement form
`` `:<-`(count, integer, 0L) ``. No working program contains it, because `` `:<-` `` is not defined
in base R, so it fails at run time unless someone defines that function on purpose. Still, the
dialect must say that it claims the form, and it must use the presence of `<-` to tell a binding
declaration from a sequence expression, because a statement-level `a: b` is valid R.

That bounds the superset claim precisely: every *runnable* R program is a valid typed program. That
is weaker than "every parseable R file", and it is the honest version.

## 7. What a syntax would make possible

This is where the value is, if there is any. The possibilities fall into two groups, depending on
whether the compiler has to invent a runtime representation.

**Where the representation is already decided.** An inline type, as in section 2, compiles to a
comment over unchanged code. A record or tuple constructor compiles to `list(...)`, because the type
system already has both shapes structurally. What a constructor buys is a checked construction site:
a misspelled or missing field is caught where the mistake is made, rather than at some later read.

**Where the representation is still open: tagged unions.** A tagged union declares that a value is one
of several kinds, with dispatch that reports the case you forgot. This is the feature with real pull,
and the one that most needs deliberate design, because its runtime representation is an open choice.
A field the compiler owns, such as `list(.tag = ...)`, is closed and controllable. R's class
attribute is open and mutable, and it carries an inheritance this type system does not model.
Neither is obviously right, and neither should be assumed. Nothing here should be designed until
inline typing has shipped and the dialect has users.

Two claims an earlier draft made here were false, and are withdrawn. Construction that cannot be
bypassed is not something only syntax can offer: `@new` is already the only way to introduce a
nominal value, and it already checks the construction against the representation. And a
compiler-written constructor still emits `list(...)`, so any R code that fabricates the list bypasses
it just as easily.

Anything that changes evaluation is a different proposition. Block scoping is one example, since R's
braces do not create a scope. The output would then stop resembling the input, and every debugging
story would get harder. That is the point where a dialect stops being a frontend and becomes a
language of its own.

## 8. The cost, and what would actually test it

A typed source file is not R. Until it is compiled, roxygen2 will not document it,
`devtools::load_all()` will not load it, RStudio will not highlight it, CRAN will not accept it, and
every contributor who touches it needs this tool. The risk is not that a build step is unfamiliar:
`R CMD build` is one, and roxygen2 is code generation that most packages already run. The risk is
that the source of truth stops being R, for collaborators and for CRAN.

A standalone script is the cheap place to start, but it does not test that risk. `ry run script.ry`
would type-check, compile in memory, and run through the R runtime the REPL already embeds, with no
generated file, no packaging, and no toolchain for collaborators. That makes it a good way to find out
whether inline typing is pleasant to use, and no evidence at all about whether package authors would
accept a generated `R/` tree, because a script removes every variable that makes up the objection.
Testing the packaging question means putting a generated tree in front of package authors.

## 9. Open questions

- Where does a generic binder go, if not `function<T>(...)`, given that R's `<` is a comparison
  operator? Section 6 explains the ambiguity.
- What should the source directory be called?
- May an inline type annotate `...`, which the comment notation types as a rest parameter?
- Should the script path from section 8 ship first, with no `ry build` at all?
- Do hover, go-to-definition, and rename work on an inline type position? They are expected to fall
  out of the existing support for type positions, but that is unverified.
- How does the editor behave on a half-written inline type, where error recovery matters more than it
  does inside a comment?

## 10. Prior art

**`we-data-ch/typr`** is a typed language for R, written in Rust, that transpiles to R and uses the
extension `.ty`. It is a sibling language rather than a superset: `fn` replaces `function`, statements
end in semicolons, and booleans are lowercase, so an existing R file cannot adopt it by adding things.
That is the opposite of the position here, where renaming a file is the whole of adoption.

Its type reasoning runs on Prolog, a required install alongside R. Whether that could answer at
editor latency is not something this survey can judge: its documentation site could not be read, and
its own README calls it an early prototype. Either way, the shape of the bet is the opposite of the
fast-to-check rule in `decisions.md`.

**The compile-to-host lineage** (TypeScript, Sorbet's `.rbi`, and Python's stub files) converged on
the same split this project already has: declarations for foreign code go in a separate file, and
annotations for your own code go inline. `.Rtypes` implements the first half, and this proposal would
be the second.
