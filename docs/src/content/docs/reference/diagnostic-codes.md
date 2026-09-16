---
title: Diagnostic codes
description: Every code ry can emit, what triggers it, and how to silence it
---

Every finding ry reports carries a stable code. This page lists them all.

## How to read a finding

```text
unresolved

  ! I could not resolve `validte_url` in this package, its imports, or builtins. Did you mean `validate_url`?
   --[R/a.R:2:22]
 1 | validate_url <- function(x) TRUE
 2 | check <- function(x) validte_url(x)
   |                      ^^^^^^^^^^^
```

| Part | Meaning |
| --- | --- |
| `unresolved` | The diagnostic code, heading the finding. Stable across message rewordings: it is what you name in a suppression comment, in `ry.toml`, and what `--output json` reports as `code` |
| `!` | Severity — `!` for a warning, `x` for an error. `--min-severity error` reports only errors and exits on them alone |
| `R/a.R:2:22` | File, line, and character column of where the finding starts |
| `^^^^` | The exact source range the finding is about — the name, the token, or the expression, not the whole statement |

Some findings add a related location, drawn nested under the finding from its own file — `duplicate` uses it to point at the other definition. See [the CLI reference](/reference/cli) for the JSON shape.

## Suppressing a finding

A `# ry: allow(...)` comment silences findings whose range starts on **its own line** (as a trailing comment) or on **the line directly below it**. There is no block or file scope.

| Comment | Silences |
| --- | --- |
| `# ry: allow(unused)` | The one named code |
| `# ry: allow(unused, naming-style)` | Every listed code |
| `# ry: allow(all)` | Every code |

```r
# ry: allow(unused)
scratch <- 1L

total = 1L  # ry: allow(assignment-operator)

flag <- T  ## ry: allow(all)
```

The marker is found by a line scan, not a parse: the first `#` on the line, all leading `#` stripped, then the literal `ry:` and `allow(` up to the first `)`. So `## ry: allow(x)` and `#ry:allow(x)` both work — but a `#` inside a string literal earlier on the line moves where the scan starts.

**Not suppressible**, because these are not reported against an R source file: `stub` (reported on `.Rtypes`), `config`, and the `unresolved` / `unused-import` findings reported on `NAMESPACE`.

### Silencing a whole class

| Directive | Effect | Scope |
| --- | --- | --- |
| `# typing: off` | Turns type checking off for this file, overriding `[check] typing` | File; must be a top-level comment |
| `# typing: on` | Turns type checking on for this file | File |
| `# typing: strict` or `#: @strict` | Turns on type checking and strict mode for this file | File |

`# typing: off` does **not** silence `annotation` findings — a malformed `#:` comment is reported whether or not the file is type-checked. An unrecognized value such as `# typing: onn` is itself reported as an `annotation` error.

Several R idioms are recognized directly, with no comment needed:

| Construct | Effect |
| --- | --- |
| `globalVariables(c("a", "b"))` at top level | Those names never report `unresolved`, project-wide |
| A file that calls `R6Class` | `self`, `private` and `super` resolve in it |
| A name created by `<<-` anywhere in the file | Resolves |
| A name starting with `.` or `_` | Never reports `unused` |
| `library(pkg)` for a package with no shipped stub | `unresolved` is suppressed project-wide, except for near misses of names your own project binds |

Formatting is suppressed by a different mechanism — `# fmt: skip`, `# fmt: off` / `# fmt: on`, `# fmt: skip-file`. See [formatting rules](/reference/formatting-rules).

## The codes

Most codes are on by default. These are the opt-ins, all configured in [`ry.toml`](/reference/configuration):

| Code | Turn on with |
| --- | --- |
| `type-mismatch` | `[check] typing = true`, or `# typing: on` in the file |
| `strict` | `[check] strict = true`, or `# typing: strict` in the file |
| `naming-style` | `[lint] naming-style = "snake_case"` or `"camelCase"` |
| `maybe-undefined` | `[check] maybe-undefined = true` |
| `unused-parameter` | `[lint] unused-parameter = "warn"` or `"error"` |
| `unused-import` | `[lint] unused-import = "warn"` or `"error"` |
| `shadows-builtin` | `[lint] shadows-builtin = "warn"` or `"error"` |
| `shadows-namespace` | `[lint] shadows-namespace = "warn"` or `"error"` |

To go the other way: `[check] unused = false`, and any `[lint]` code set to `"off"`.

### Syntax

| Code | Severity | On by default | Triggered by |
| --- | --- | --- | --- |
| `syntax-error` | error | yes | Anything the R parser rejects. The message varies with the failure ("unclosed `(`; expected `)` to close the parameter list"); the code does not |
| `syntax-error` | error | yes | An assignment target R refuses — a computed value or a number where a name belongs (`1 + a <- 2`). R parses these and fails at run time, so the mistake is real but the parse looks clean; the commonest cause is a line ending in an operator that pulls the next line in as its right-hand side, which the message names when that is what happened. A `!`-headed target is deliberately exempt: `!` binds tighter than `<-`, so `expr(!!name <- value)` — building an assignment rather than performing one — has that same shape |

The code says **whose grammar was broken**, not which stage noticed: your R is `syntax-error`, your `#:` comment is `annotation`. So a type expression that does not parse, and a form the annotation grammar refuses deliberately such as a nested `<T>` binder, both report as `annotation` alongside the malformed-block findings below.

**One mistake, one finding.** Recovery reports the first thing it cannot use and then stays quiet about the consequences: a `#:` region reports once (plus, at most, one unclosed opener, which is a structural fact about a construct rather than a per-token consequence), and an unterminated argument or parameter list ends at the next statement instead of adopting it. A function missing its body is not reported when its parameter list never closed — that is the same mistake said twice.

**A mistake stays on its own line.** A string or backtick-quoted name may span lines in R, so one that never closes can only be discovered at the end of the file — but a token reaching that far would take every statement below it out of analysis, which is what a stray quote does while you are still typing it. An unterminated one ends at its line break instead: the quote is reported where it opens, and the rest of the file keeps its diagnostics, its definitions, and its completions.

A statement that fails to parse as R suppresses every name-resolution and typing finding overlapping it — the checker draws no conclusions from source it could not read. A broken annotation suppresses nothing outside its own block; inside it, the block carries no typing payload, so the refusal is the only finding.

### Name resolution

| Code | Severity | On by default | Triggered by |
| --- | --- | --- | --- |
| `unresolved` | warning | yes | A bare name read that resolves nowhere: not a local, not a top-level definition in this package, not a `NAMESPACE` import, not a builtin or stub export. Adds `Did you mean` for a near miss |
| `unresolved` | warning | yes | `pkg::name` where the stub corpus knows `pkg` but `pkg` does not export `name`. `pkg:::name` is exempt — it legitimately reaches unexported names |
| `unresolved` | warning | yes | `pkg::name` where `pkg` is neither a stub namespace nor a declared `DESCRIPTION` dependency: "unknown package namespace `notapackage`" |
| `unresolved` | **error** | yes | In `NAMESPACE`: `importFrom(pkg, name)` where `pkg` has stubs and does not export `name`. An error rather than a warning because R refuses to load the package |
| `unresolved` | **error** | yes (`check` only) | In `NAMESPACE`: `export(name)` naming something the package defines nowhere at top level — `R CMD check`'s "undefined exports". Not reported by the language server |
| `maybe-undefined` | as configured | yes | A read some path reaches with no prior write — the name is introduced only in conditionally executed code, and R raises `object 'x' not found` on the other path. The read still resolves, so this is not `unresolved`. Off by default: the flow analysis treats two conditions that always agree at run time as independent branches, so a guard pattern like `if (ok) v <- …` followed by `if (ok) use(v)` reports even though it is safe |
| `unused` | warning | yes | A write inside a function body that no read ever reaches, including a store overwritten before every read. A write in a frame some inner scope super-assigns with `<<-` is exempt: the write is what makes `<<-` find that slot, so deleting it would send the assignment to the global environment instead |
| `unused` | warning | yes | In a script, a top-level binding nothing later reads. Package files are exempt — any file may use them. S3 method names are exempt: dispatch is not a read |
| `duplicate` | warning | yes | A top-level name defined more than once across a package's files. Both sites report, each with a `note` pointing at the other. Scripts are exempt — rebinding in a sequential script is ordinary |

### Typing

`annotation` covers malformed `#:` comments and is always on. `type-mismatch` and `strict` are opt-in; see [type-system reference](/reference/type-system) for the semantics behind them.

| Code | Severity | On by default | Triggered by |
| --- | --- | --- | --- |
| `annotation` | error | yes | A type name that is not a builtin, a declared `@type`/`@alias`, or a stub class. Adds `Did you mean` |
| `annotation` | error | yes | A malformed block: `@forall` after `@param`, `@param` after `@return`, more than one `@return`, a duplicate type-parameter name, `@new` with no nominal, or an unknown constraint (only `numeric` and `atomic` exist) |
| `annotation` | error | yes | A type expression the annotation grammar cannot read, or a form it refuses on purpose — a `<T>` binder anywhere but the outermost level of the block |
| `annotation` | error | yes | A dangling `#:` — no expression on the next line, a blank line in between, or no type expression at all |
| `annotation` | error | yes | A `#:` inside a call's argument list, where an argument is not a statement and nothing can be annotated |
| `annotation` | error | yes | Type-argument arity: arguments applied to a non-generic, the wrong number of them, or a bare reference to a generic that needs them |
| `annotation` | error | yes | A `@type`/`@alias` block that is not at file top level; `@new` naming an `@alias` rather than a `@type`; the same type name declared twice in one namespace — across package files, which share a project-wide one, or twice inside one script, whose declarations reach only their own file (`@type` and `@alias` share the namespace either way) |
| `annotation` | error | yes | An unrecognized `# typing:` directive value |
| `type-mismatch` | error | no | An argument, or a returned value, whose type does not match the declared one |
| `type-mismatch` | error | no | Calling something that is not a function, or a callee that may be `NULL` |
| `type-mismatch` | error | no | Too many positional arguments, or a required argument missing |
| `type-mismatch` | error | no | An argument name the function has no parameter for, or supplied twice. Names the parameter list and suggests the nearest match |
| `type-mismatch` | error | no | No overload of an overloaded function matches the call |
| `type-mismatch` | error | no | `$` or `[[` naming a field or position that does not exist (with a suggestion), or `$` on an atomic vector |
| `type-mismatch` | error | no | `[[` on something that is not a list, an index that is not valid for the vector, or an index shape that is not modeled — `x[]`, a named index, a multi-index (matrix and data-frame subsetting) |
| `type-mismatch` | error | no | An operator applied to an operand, or a pair of operands, it is not defined for |
| `type-mismatch` | error | no | `for` over something that is not a sequence |
| `type-mismatch` | error | no | A constraint violated: `numeric`, `atomic`, or a scalar numeric |
| `type-mismatch` | error | no | An alias cycle, or an infinite type (the occurs check) |
| `type-mismatch` | error | no | Annotation rules: `@if-unknown` where the type is already known, an annotated parameter that also has a default, annotation parameters that do not match the function's formals, or a `[]`/`[named]` element type that is not atomic |
| `strict` | error | no | An expression whose type is undetermined — "this expression has an undetermined type (`Unknown`)" |
| `strict` | error | no | A reference with no known type — "could not determine the type of `x`" |
| `strict` | error | no | A binding whose type does not stabilize across loop iterations |
| `strict` | error | no | A binding defined recursively |

Enabling `strict` also raises every `unresolved` finding in the file from warning to error. The count does not change; the severity does, which matters if `--min-severity error` gates your build.

### Lint

| Code | Severity | On by default | Triggered by |
| --- | --- | --- | --- |
| `assignment-operator` | warning | yes | `=` used as assignment. The range is the `=` token alone |
| `boolean-shorthand` | warning | yes | An identifier that is exactly `T` or `F`. Identifiers inside `#:` annotations are exempt, so a type variable named `T` is fine |
| `trailing-comma` | **error** | yes | A comma after a call's last argument. In R this supplies a missing argument rather than being ignored — hence error, unlike the other default-on lints |
| `naming-style` | warning | no | An assignment target or a function parameter that does not match the configured casing. `SCREAMING_SNAKE_CASE` conforms under either style. Always a warning: the `"warn"`/`"error"` levels do not apply to this lint, which is configured by style value instead |
| `unused-parameter` | as configured | no | A formal no read resolves to. `...` is exempt, and S3 generics and their methods are exempt entirely — their formals are dictated by the generic |
| `unused-import` | as configured | no | An `importFrom(pkg, name)` in `NAMESPACE` whose `name` appears in no token of any checked source. Whole-namespace `import(pkg)` is never checked. Reported by `check` only, not by the language server |
| `shadows-builtin` | as configured | no | A top-level binding whose name `base` exports. Requires stubs to be installed |
| `shadows-namespace` | as configured | no | A top-level binding whose name a non-`base` stub namespace declares and that resolves bare (`stats::filter`, `utils::head`) |

`missing-comma` is retired and never emitted — the parser rejects `f(1 2)` as a syntax error, exactly as R does. The config key still parses so old configs keep loading, and is ignored.

### Tooling

| Code | Severity | On by default | Triggered by |
| --- | --- | --- | --- |
| `stub` | error | yes | A declaration in a project's `stubs/*.Rtypes` file that would otherwise be dropped in silence: a line that is not a `name : TYPE` declaration, an invalid name, a missing or invalid type, an unknown type name, or `@masked` on a non-variadic function type. The range covers the whole line |
| `config` | error | yes | A malformed `ry.toml` — a TOML parse failure, or the wrong type of value on a known key. Reported on the config file by the language server only; `check` reports config failures as a usage error with exit code 2. An *unknown key* is not this finding: it is a `check` warning on stderr and never blocks loading |
