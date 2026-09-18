---
title: Authoring stubs
description: The .Rtypes declaration format, overload sets, export manifests, and the scope rules for what belongs in the corpus
---

:::note[Status]
The standard-library stub format ships. The corpus holds about 950 typed
declarations across eleven declaration-only `.Rtypes` files in the
repository's top-level `types/` directory. Generated `.exports`
[manifests](#export-manifests) cover every namespace R ships, plus fifteen
CRAN namespaces that have no typed declarations. The
[end of this page](#what-is-not-built-yet) lists what is not built. The
authoritative typing contract is the
[typing reference](/reference/type-system), and this page describes how the
standard library feeds that contract.
:::

## Why stubs exist

Only a small kernel is built into the checker. That kernel is the operators,
plus `c` and `list`. Without stubs, everything else in the standard library
would resolve to `Unknown`. `T`, `F`, and `pi` would be untyped, and `length`,
`nchar`, `seq_len`, `paste`, and the rest would have no signature, so every
call through them would lose all type information.

The stub format closes that gap. A stub is a declaration-only description of
base or of another R package, loaded as an immutable input, so the checker
knows the standard library the same way rust-analyzer knows `core` and `std`.

## Stub format

A stub file is a dedicated declaration-only file with the extension `.Rtypes`,
for "R type information". Each non-blank, non-comment line is a declaration:

```
name : <type-expr>
```

The type expression reuses the `#:` annotation type grammar verbatim, because
the same parser reads both. There is no second type notation to build or to
keep from drifting. A stub file has no place to write a function body, so
"declaration-only" is enforced structurally. TypeScript `.d.ts` and Python
`.pyi` files are declaration-only the same way, by construction rather than by
convention. Blank lines and `#` comments are ignored, whether the comment is a
whole line or trails a declaration. The loader harvests each type expression
directly into a `TypeScheme`.

A declared `name` is an R identifier, an infix operator such as `%in%` or a
project's own `%||%`, or an S3 operator method such as `+.Date`, `-.POSIXct`,
`Arith.difftime`, `Compare.Date`, or `Ops.gg`. The operator spellings are how
a stub gives a class arithmetic or comparison. See
[operator methods on a class](/reference/type-system#operator-methods-on-a-class)
for the dispatch order. The suffix is the nominal's own name, so
`+.ggplot : fn(e1: ggplot, e2: Any) -> ggplot` in a project stub is all a
`+`-based DSL needs.

```
# base.Rtypes, a fragment

# plain value bindings
T : logical
F : logical
pi : double

# a fixed-arity function signature
length : fn(x: Any) -> integer

# a parametric higher-order function, a real generic scheme rather than Any
lapply : <T, U> fn(x: list[named: T], f: fn(T) -> U, ...: Any) -> list[named: U]
```

### Why a dedicated declaration format

The earlier approach shipped a stub as an ordinary R file with a placeholder
body, such as `length <- function(x) 0L`. The loader harvested the `#:`
annotation and ignored the body. That let a stub carry a meaningless,
unreachable function body, and it required a full parse and lowering just to
reach the annotation. A dedicated declaration file removes both problems. A
body is unrepresentable, so a stub cannot drift into carrying one, and the
loader parses only declarations.

The extension `.Rtypes` evokes R's own `.Rd` documentation convention without
colliding with `.Rd`, `.Rmd`, `.Rda`, or `.RData`. A JSON or TOML stub grammar
was rejected because it would need a second type notation. Reusing the `#:`
type grammar keeps one source of truth for type syntax across inline
annotations and stub files.

Besides a value declaration, a line of the form `@type NAME` declares an
opaque nominal type. That is a named type with no inspectable representation,
for a standard-library value the type grammar cannot describe structurally,
such as `data.frame`, `factor`, `connection`, or `Date`. An opaque nominal is
compatible only with itself. A value of it comes only from a function declared
to return it, so `Sys.Date()` is a `Date`, and passing it where `factor` is
expected is a type error. A type name may contain an interior dot, as
`data.frame` does, and a project's own `@type` or `@alias` of the same name
shadows the stub type. A consumer of such a value keeps `Any` parameters,
because R coerces liberally. The nominals tighten returns. Using `$`, `[`, or
`[[` on an opaque nominal yields `Unknown` instead of erroring, so `df$amount`
and `df[rows, ]` stay usable, and strict mode surfaces each access. The
[typing reference](/reference/type-system#nominal-types) specifies this. A
structural `@type NAME {REPRESENTATION}` declaration is not expressible in
`.Rtypes`. The opaque nominal is the stub form, and a `@type` name that is not
a plain identifier is a stub error on its own line.

### Cross-ecosystem note

A statically typed host that publishes types for foreign code uses a separate
declaration file. TypeScript has `.d.ts`, Python has `.pyi`, and Sorbet has
`.rbi`. A dynamically typed host that retrofits types onto its own source puts
them inline, as Elixir and Erlang do with `@spec`. R does both. Inline `#:` is
primary for a project's own source, and a separate `.Rtypes` file covers a
foreign package, such as base or a CRAN package, that cannot be annotated at
the source. `.Rtypes` reuses the same type grammar as the inline form, so the
two are one notation in two carriers.

### Data-masking functions

A declaration may carry the `@masked` attribute before its function type:

```
summarise : @masked fn(.data: Any, ...: Any) -> Any
```

The attribute marks the function as evaluating its rest-absorbed arguments in
a data mask, which is dplyr-style non-standard evaluation. A bare name in one
of those argument positions is treated as a column reference, so it raises no
unresolved-name warning. An argument that matches a declared formal resolves
normally. `@masked` requires a variadic function type. See
[data frames and non-standard evaluation](/reference/type-system#data-frames-and-non-standard-evaluation)
in the typing reference for the full semantics.

## Overloads and generics

Repeating a name declares an ordered overload set. Each further declaration of
the same name within one source appends a candidate:

```
sum : fn(...: integer[] | logical[], [na.rm]: logical) -> integer
sum : fn(...: double[], [na.rm]: logical) -> double
sum : fn(...: Any, [na.rm]: logical) -> Any
```

A call commits the first candidate that accepts the arguments, so `sum(1L, 2L)`
is `integer` and `sum(1.5, 2.5)` is `double`. The typing reference specifies
the call-site selection rules under
[overload sets](/reference/type-system#overload-sets).

Order each set most-specific first, and end it with the most general
candidate. That last candidate is conventionally an `Any` fallback. It keeps a
mixture the other candidates cannot express, such as `sum(TRUE, 1L)`, from
erroring. It is also what a non-call use of the name resolves to, so a value
never carries a narrower contract than the calls the same name accepts. A
project override that redeclares a name replaces its whole set. An override
that wants overloads declares all of them itself.

Note where the rest parameter sits in those three lines. A parameter declared
before `...` can be filled positionally, so `[na.rm]` must come after the rest
parameter, or `sum(1L, 2L)` would bind `1L` to `na.rm` and fail.

The editor shows the whole set. Signature help on a call lists every declared
candidate. The committed one is active and rendered with that call site's
types filled in, and the rest show their declared type parameters. An
incomplete call matches no candidate yet and still lists the set, which is when
the list helps most. Hover on the name shows the signature of the committed
candidate, or of the last declaration when the name is not being called, plus a
`(+N overloads)` note. Go-to-definition points at the first declaration.

Three rules govern the current corpus.

- **A genuinely parametric function gets a real generic.** A higher-order
  helper whose result is a function of the argument type is written with
  `<T> fn(...)` binders and keeps a precise polymorphic scheme. `lapply`,
  `Reduce`, `print`, `invisible`, `setNames`, and `suppressWarnings` are such
  helpers. An element-preserving vector function uses `<T> fn(x: T[]) -> T[]`,
  where the element parameter is constrained to atomic types. `rev`, `sort`,
  `unique`, `head`, `tail`, `sample`, `rep`, and the set operations are such
  functions. That signature is usually the first candidate of an overload set
  whose list form threads `list[T]` and whose `Any` fallback covers the rest.
  - **A function that hands back the shape it was given declares each shape,
    narrowest first.** R's selection and reordering operations go through `[`,
    which preserves both the atomic type and the names, so the declaration
    says so. `Filter` takes `T[]`, then `list[named: T]`, then `list[T]`, and
    each candidate returns its own shape. One `list[T]` return for all three
    would make `Filter(f, c(1, 2)) + 1` a type error on code R runs. `lapply`,
    `rev`, `unique`, `head`, and `tail` carry the same `list[named: T]`
    candidate ahead of their plain list one, so a named list in is a named
    list out. A fixed-shape input still coerces to a name-keyed list on the
    way in, so a field read off the result is `T | NULL`. The operation may
    drop a name, and the exact field types are not carried through.
- **A type-preserving reduction gets an overload set.** A reduction whose
  result's atomic type follows R's coercion order rather than one input's
  declares one candidate per atomic family, plus the `Any` fallback. `sum`,
  `min`, `max`, `range`, `pmin`, `pmax`, `cumsum`, `cummax`, `cummin`, and
  `abs` with its integer and double split are such reductions.
- **A value-dependent result falls back to `Any`.** A function whose return
  type varies by argument value or by arity is given `Any` rather than a
  falsely precise signature, so a call yields `Any` and never a spurious type
  or arity error. `seq`, `is`, and `grep(value =)` are such functions. The
  name still resolves.

## Override precedence

A project overrides or extends the shipped stubs by putting `.Rtypes` files
under `<project>/stubs/`. The loader folds the project's sources over the
shipped corpus in sorted path order, so a declaration a project supplies
replaces the shipped declaration of the same name. A project can therefore
correct a return type, or add a name the shipped corpus omits. A missing
directory, an unreadable file, or a malformed line is skipped, because
overrides are optional and one bad line must never block analysis.

A project stub file's name declares its namespace. `stubs/dplyr.Rtypes`
declares the namespace `dplyr`, so its declarations type bare reads and also
validate qualified reads. `dplyr::mutate` is a known-namespace read with the
stub's type, while `dplyr::filter` warns "not exported" until the file
declares `filter`. This is how a project types a third-party package today.
The file is hand-written, because generating one from an installed library is
not built. Exports are declaration-level, not winner-level, so a project file
that overrides a shipped name's type does not remove the name from its shipped
namespace, and `stats::sd` stays valid under an `sd` override. Hover shows a
name's origin as the namespace of its winning declaration.

Skipped never means silent. `ry check` reports every dropped override
declaration as an error on its own stub line. That covers a line that fails to
parse, a declaration naming an unresolvable type, and a `@type` line whose
name is not an identifier. An unreadable override file is an IO failure. The
editor shows the same problems as diagnostics while a `.Rtypes` file is open.

## Export manifests

Base alone exports 1,408 names, and the typed corpus deliberately curates a
high-value subset. Without more, every real export outside that subset would
warn as unresolved, because the checker could not tell `recover`, a real
`utils` export, from a typo.

The export manifests close that gap. Each shipped namespace's `.Rtypes` file
is paired with a `types/<namespace>.exports` file that lists every name the
namespace really exports, one per line. `scripts/export-manifests.R` generates
them from a live R session, and the header records the R version. Rerun the
script against a newer R to refresh them. It refuses to overwrite a manifest
recorded against a newer R than the session's, because regenerating on an
older R would silently drop the names the newer version added and turn every
use of one into a false `unresolved` finding.

A manifest does not need an `.Rtypes` file beside it. Fifteen manifest-only
namespaces carry no typed declaration at all, so every name from them is
`Unknown`. They are `tibble`, `tidyr`, `readr`, `purrr`, `stringr`, `forcats`,
`lubridate`, `magrittr`, `rlang`, `glue`, `scales`, `knitr`, `jsonlite`, `R6`,
and `tidyverse`. What they supply is a knowable export set. That matters
because attaching a package whose exports the checker cannot enumerate turns
off unresolved-name detection for the whole project, which the
[typing reference](/reference/type-system#naming-and-scoping) specifies. Before
these manifests existed, a single `library(stringr)` meant a clean run said
nothing about a typo anywhere. Adding declarations to any of them later is
additive. The manifest stays the export set, and the `.Rtypes` file supplies
types.

A manifest name is a known global with the weakest possible claim.

- A bare or `pkg::`-qualified read of a manifest name always resolves, with no
  unresolved warning and no not-exported warning. It types as the `.Rtypes`
  declaration when one exists, and as `Unknown` otherwise. Precision is the
  typed corpus's job, and silence about a real name is the manifest's.
- Completion offers a manifest-only name, undecorated, because there is no
  scheme to show. A non-syntactic name is skipped when bare and is
  backtick-quoted after `pkg::`, since inserting it raw would change the
  syntax. Typo suggestions draw on manifest names too.
- The unit suite pins the pairing both ways. Every value declaration in a
  shipped `.Rtypes` file must appear in its namespace's manifest, and a
  stubbed non-export is a hard failure. That catches a declaration added to
  the wrong file, and a name R moves between namespaces, such as `traceback`
  and `standardGeneric`, which live in `base` rather than in `utils` or
  `methods`. A conditional namespace may additionally override a base name,
  such as data.table's class-preserving `merge`.

The manifests mirror how R exposes each namespace, in three tiers.

- **Default-attached.** `base`, `stats`, `utils`, `graphics`, `grDevices`,
  `methods`, and `datasets` are visible bare everywhere, so their names
  resolve unconditionally. `datasets` is driven by the manifest through the
  search-path listing, because its objects are lazy data rather than namespace
  exports. The famous data frames are typed in `datasets.Rtypes`, so `iris` is
  a `data.frame` rather than `Unknown`.
- **Shipped by R but unattached.** `tools`, `parallel`, `compiler`, `grid`,
  `splines`, `stats4`, and `tcltk` validate their `pkg::` reads in every
  project, so `tools::file_ext(path)` needs no `library(tools)`, exactly as in
  R. A bare read resolves only once the project attaches or declares the
  package.
- **Conditional.** `data.table`, `dplyr`, `ggplot2`, and `testthat`, plus
  every manifest-only CRAN namespace, activate their manifest and their stubs
  together, as [conditional namespaces](#conditional-namespaces) describes.
  While a package is inactive its names warn, bare and qualified alike. A
  meta-package activates the members it attaches. `library(tidyverse)`
  re-exports almost nothing itself and instead attaches `dplyr`, `ggplot2`,
  `tibble`, `tidyr`, `readr`, `purrr`, `stringr`, `forcats`, and `lubridate`,
  so those nine activate with it. That is also how such a project gets dplyr's
  and ggplot2's typed declarations rather than a manifest's `Unknown`. The
  membership is checkable, because the generator script prints what a live
  `library(tidyverse)` attaches.

The manifests vendor R's export lists, so analysis never needs an R
installation. They are data, not types. Adding a manifest name costs nothing
at check time until code actually reads it.

## The base environment

There is one source of truth for everything scheme-shaped, plus a small
hardcoded kernel.

- **Value bindings.** `T` and `F` become a monomorphic `Scalar(Logical)`, and
  `pi` becomes a `Scalar(Double)`.
- **Base functions.** Each becomes a type scheme, seeded into the template
  environment alongside the built-in kernel.

### What stays hardcoded, and why

The operators and `c` and `list` are an algorithm, not a type.

- The operators need the numeric promotion lattice and the comparison
  families.
- `c` needs variadic, `NULL`-dropping atomic promotion.
- `list` needs record and tuple synthesis.

The `#:` grammar cannot express any of these, so they stay built in. The one
source of truth goal is met for everything a type can describe. The kernel is
an explicit carve-out, not an oversight.

### Expressiveness gaps the base functions expose

Two extensions that a faithful corpus needs have landed.

| Extension | Example | Form |
|-----------|---------|------|
| Variadics | `paste`, `sum`, `cat` | a trailing `...: TYPE` rest parameter, as in `fn(...: Any) -> character` |
| Dotted parameter names | `na.rm`, `length.out` | an interior `.` is allowed in a parameter name and a field name |

The gaps below remain. Each one caps how precise the affected declarations can
be.

| Gap | Example | Extension needed |
|-----|---------|------------------|
| A trailing dot in a parameter name | `stop(call. =)`, `warning(immediate. =)` | a parameter name currently allows an interior dot only |
| Absorbing a named argument into the rest parameter | `data.frame(x = 1)`, `Sys.setenv(VAR = "v")`, `par(mfrow = ...)` | the checker never routes a named argument into `...`, so a sink that takes arbitrary named arguments must stay an `Any` value |
| An empty type for a function that never returns | `stop`, `q` | without one, a `NULL` return claim would poison the join in `x <- if (ok) v else stop(...)`, so these stay `Any` |
| A return that mirrors the argument's shape | `rev(opts)$timeout` on a fixed-shape `opts` | a declaration cannot say "the same record back", so a selection or reordering returns a name-keyed `list[named: T]` and a field read off it is `T | NULL` rather than the field's own type |
| A nullable result under a member-wise operator | `names`, `dim`, `nrow` | a `T \| NULL` return false-positives on `1:nrow(df)` and on `for (nm in names(x))` until flow narrowing or NULL-tolerant joins exist, so these return `Any` |

Three former rows of this table have closed. The type-preserving reductions
declare [overload sets](#overloads-and-generics). The element-preserving
functions declare generic `T[]` signatures, and the `T[]` suffix carries the
atomic-element bound that the typing reference specifies under
[type parameters](/reference/type-system#type-parameters-and-generic-application).
Function compatibility also stopped demanding a matching parameter count. A
function serves a callback interface when it accepts every call shape the
interface promises, so a spare optional formal is fine, and a callback-idiom
stub can declare its real signature. `lapply(words, nchar)` works with
`nchar`'s display formals declared. A function whose result atomic type
changes with the input, such as `nchar` or `toupper` returning the input's
shape, remains a scalar claim.

## Loading and namespacing

Stubs are immutable, set-once inputs, which is the analogue of
rust-analyzer's `Durability::HIGH`. They are loaded, parsed, and interned
once, and an ordinary user edit never invalidates them. There is one
deliberate exception. The assembly also reads the package-metadata input,
which activates the [conditional namespaces](#conditional-namespaces). A
metadata flip, meaning that the project declares or attaches such a package,
rebuilds the library. That is rare and worth the full refresh it causes.

The `StubLibrary` is a set of flat, string-keyed tables, built once and folded
over every source.

- `schemes` maps a name to its ordered list of schemes, one per declaration.
  See [overload sets](#overloads-and-generics).
- `nominals` is the set of `@type` names.
- `masked` maps a `@masked` name to the formals declared before its `...`.
- `exports_by_namespace` maps a namespace to the names it declares.
- `declarations` maps a name to the source index and the range of its winning
  declaration. The declaring namespace is derived from the source index rather
  than stored, so the two can never disagree.
- `known_exports` is the union of every manifest's names.

Typing is not partitioned by namespace. The per-namespace export table answers
`pkg::name` validation alongside the flat scheme map, so an override that wins
a name's type never un-exports it from its declaring namespace.

- Every shipped `.Rtypes` file is harvested into the one flat map, folded in
  file order. A later source that redeclares a name replaces its whole entry,
  which is the same rule that governs a project override. Repeated
  declarations within one source build the name's overload set instead. The
  seven default-attached namespaces are therefore attached to the base scope
  together, and a conditional namespace joins the fold only when it is active.
- The flat map is seeded into the per-document inference template, so every
  stub name resolves as a bare global, whichever shipped file declared it. A
  name's last scheme becomes its plain environment binding, because the corpus
  orders candidates specific-first and the last one is the most general. A
  multi-scheme name additionally registers its overload set for call-site
  selection.
- `pkg::name` resolves against the same flat map. The qualified read has the
  stub's type, exactly like the bare name, and the per-namespace export table
  powers the validation warnings for an unknown namespace and for a name a
  namespace does not export. The typing reference specifies them under
  [namespace access](/reference/type-system#namespace-access).

### Conditional namespaces

Some shipped namespaces describe a package R does not attach by default.
`data.table`, `dplyr`, `ggplot2`, and `testthat` come with types, and the
[manifest-only](#export-manifests) CRAN set comes without. Folding them in
unconditionally would let `fread`, `mutate`, or `str_to_upper` resolve in a
project that never uses them, and that would steal a typo warning. A
conditional namespace therefore joins the assembly only when the project uses
the package. Any one of these signals is enough.

- A `DESCRIPTION` dependency field names it. The fields are `Depends`,
  `Imports`, `Suggests`, and `Enhances`. A `NAMESPACE` `import` or
  `importFrom` naming it as a source counts too.
- A project file attaches or loads it. The call is `library()`, `require()`,
  `requireNamespace()`, or `loadNamespace()`, with a literal name or string as
  the package argument, anywhere in the file. This is the signal a script
  workspace has, because only a package carries metadata files.
- The project ships its own `stubs/<pkg>.Rtypes` override for the namespace.
  Writing one is the clearest declaration that the project uses the package,
  and the override then folds over the shipped declarations as usual.
- A meta-package that attaches it is active. `library(tidyverse)` activates
  the nine packages it attaches, because attaching is how R makes their names
  reachable.

`testthat` earns its place for a different reason than the data packages. A
package's `tests/` directory is a third of its source, and every line of it
calls these names. Without the corpus, the only way to quiet them was the
blanket tolerance that an unknowable export set earns, and that tolerance also
silenced a typo in the package's own functions. With the stubs, `expect_eqaul`
is reported with a suggestion. An expectation's `object` parameter is `Any`,
because an expectation compares whatever the code under test produced, and
each `expect_*` returns its `object`, which is the value testthat passes
through invisibly.

While a namespace is inactive it is simply absent. Its names warn as
unresolved, a `pkg::` read behaves like any read from a package without stubs,
and its `@type` nominals are unknown type names. The hosts keep the attach
facts current per synced file. The language server rescans only the changed
document and fills the rest of the workspace in during idle priming, so
activation never costs a whole-project sweep on a keystroke.

### NAMESPACE import validation

Both `ry check` and the language server validate a package's `NAMESPACE` file
against the loaded stubs. An `importFrom(pkg, name)` whose namespace the stub
corpus knows, shipped or project, but which does not export `name`, is an
error on the import site. The message reads `` `medain` is not exported by
`stats`, so this package will not load. `` R refuses to load a package with
such an import, so the code cannot run at all. This is the same rule as an
`export()` of a name the package never defines. A namespace without stubs is
not checked, because the stubs are the only export source the checker has.
Resolution semantics are unchanged, and a stubbed name resolves bare with or
without an import. The check uses no interner, because it reads a
string-keyed export table built once from the loaded corpus. An open
`NAMESPACE` editor buffer is therefore validated live without touching the
engine's shared interner, which is the same isolation the `.Rtypes`
stub-buffer path keeps.

Three parts of namespacing are not built.

- A per-namespace map, from a namespace to its own name-to-scheme table, that
  keeps the shipped packages separate rather than folded flat. Today two
  shipped packages cannot declare the same name with different types.
- `library(pkg)` attaching a namespace on demand.
- Gating bare third-party resolution on NAMESPACE imports. That is
  R-correct, and it is a deliberate strictness decision. Today's flat fold is
  intentionally permissive.

### Incremental hygiene

In the [query engine](/contributing/architecture#the-semantics-database) the
stub library is a set-once input. It is established once and its revision
never advances, so it can never invalidate a query that reads it. That is the
entire isolation property. A stub never triggers recomputation because it
never changes, and that holds automatically rather than through a separate
check.

A package binding that shadows a stub name is an ordinary structural edit. It
changes the package symbol index's winner for that name, which flows through
the per-symbol interface to exactly the files that reference the name. That is
the same path as any other definition appearing or disappearing. A stub value
and an unrelated user type that share a name, such as `#: @type T` and the
value `T`, live in the type namespace and the value namespace respectively,
and they never interact.

## Scope discipline: model only what is statically reasonable

The corpus models these.

- A fixed-arity signature.
- A scalar constant.
- A common vectorized atomic operation with a stable result shape.
- A prenex rank-1 generic.
- A nominal type or class declaration.

The corpus deliberately does not model these. Declare the static-stable
signature and stop, or use `Any`.

- Non-standard evaluation and data masking, such as `subset`, `with`, and
  formulas.
- Tricks that forward `...`.
- The dispatch dynamism of S3, S4, R5, and R6, such as `UseMethod` and
  `NextMethod`.
- `do.call`, `match.arg`, `match.call`, `sys.call`, and `Recall`.
- Reflective environment access, such as `environment`, `assign`, `get`, and
  `eval`.
- Partial argument matching.
- A replacement function, such as `` `names<-` ``.

The rule is this. If a function's return type is not a static function of its
argument types, and depends instead on a runtime value or class, omit the
declaration or give it `Any`.

### Corpus compromise vocabulary

The corpus optimizes for two things, in order. First, zero false errors on an
idiomatic call. Second, the most precise sound return. The recurring
compromises are named once in the `base.Rtypes` header and referenced per
entry.

- **Any-param.** A parameter is `Any` although R documents a type, because R
  itself coerces argument types and `nchar(42)` is legal, so a declared type
  would reject a call R accepts. The checker's own coercions shrink the need
  for this. An `integer` widens to `double` at a parameter position, a
  whole-number double literal counts as an `integer`, and a scalar coerces
  into a vector position. The `T[]` generic design restores the rest of the
  parameter precision.
- **scalar-claim.** An elementwise function declares its scalar result form,
  so `character` rather than `character[]`. A scalar claim coerces into every
  vector position and can never false-positive downstream, while a vector
  claim would break `if (grepl(...))`.
- **type-preserving.** The result's type follows the input's, as in `sum`,
  `sort`, and `rev`. An element-preserving vector function declares
  `<T> fn(x: T[]) -> T[]`. A reduction whose result follows R's coercion order
  declares one [overload candidate](#overloads-and-generics) per atomic
  family. Each set ends with an `Any` fallback, which is also what a call
  selects when an argument's type is still undetermined. A fallback taking
  `Any` binds nothing, so it never narrows the caller.
- **NULL-hybrid.** The result is `T` or `NULL` depending on the runtime value,
  as in `names`, `dim`, and `nrow`. These return `Any`. The gaps table
  explains why.
- **named-formals.** Declare the named formals whose types are worth checking,
  such as `paste`'s `sep` and `collapse`, or `read.csv`'s `header` and `sep`.
  An undeclared named argument to a variadic function is not an error. The
  rest parameter absorbs it and checks it against the rest parameter's element
  type, which matches R. An open named-argument space therefore works with
  `...: Any`, as `read.csv`'s pass-through to `read.table` and `lm`'s fitting
  controls do. Declaring a formal buys a typed check, and omitting it buys
  pass-through.

The corpus also keeps a two-tier dynamic marker, borrowed from typeshed. A
real `Any` for a genuinely untypeable return stays distinct from a greppable
marker for a declaration that is incomplete. The distinction makes a partial
stub first-class and improvable, and it lets tooling find what still needs
work. This mirrors typeshed's split between `Incomplete` and `Any`.

## Worked example

Here is a base stub fragment:

```
T : logical
F : logical
pi : double
length : fn(x: Any) -> integer
nchar : fn(x: Any, [type]: character, [allowNA]: logical, [keepNA]: logical) -> integer
seq_len : fn(length.out: Any) -> integer[]
```

Each line produces one scheme, and the checker renders it back in the same
notation the declaration used:

| Binding | Scheme |
|---------|--------|
| `T`, `F` | `logical` |
| `pi` | `double` |
| `length` | `fn(x: Any) -> integer` |
| `nchar` | `fn(x: Any, [type]: character, [allowNA]: logical, [keepNA]: logical) -> integer` |
| `seq_len` | `fn(length.out: Any) -> integer[]` |

`nchar`'s subject and `seq_len`'s count are Any-param, which the
[compromise vocabulary](#corpus-compromise-vocabulary) explains, so
`nchar(42)` and `seq_len(10)` check clean while the returns stay precise.

Here is R that type-checks against these stubs plus the hardcoded kernel:

```r
n    <- length(c(1L, 2L, 3L))  #: integer  (stub scheme)
half <- pi / 2                 #: double   (operator kernel on a double scalar)
flag <- T                      #: logical  (stub value binding)
```

The stub schemes and the hardcoded kernel interoperate here. `length`'s scheme
types `n`, the operator kernel promotes `pi / 2`, and the `T` value binding
types `flag`.

`paste`'s variadics, its `sep` and `collapse` named formals, and
`length.out`'s dotted parameter name are all expressible directly:

```
paste : fn(...: Any, [sep]: character, [collapse]: character | NULL, [recycle0]: logical) -> character
```

The rest parameter's position among the named ones is part of the signature,
because it decides which parameters a positional argument can still fill.

One gap remains, and it degrades a return to the scalar claim. A function
whose result atomic type differs from the input's, as `nchar`'s does, should
track the input vector's shape and cannot say so. The `rev`-style element
preservation is covered by the generic `T[]` suffix.

## Per-edit cost

Because the stub library is a [set-once input](#incremental-hygiene), its mere
presence adds no per-edit recheck cost. An edit never invalidates it, and a
body recheck still touches only the edited document and its referrers. A
document pays inference time only for the base names it actually references,
which is the feature working rather than bookkeeping overhead. A document that
references no base name pays nothing for the corpus being loaded.

## What is not built yet

Two extensions are designed but unbuilt, and their absence is visible.

- **Third-party packages beyond the shipped set.** The corpus covers the
  namespaces R itself ships, plus conditional stubs for `data.table`, `dplyr`,
  `ggplot2`, and `testthat`. Any other CRAN package's names resolve to
  `Unknown`, with an unknown-namespace warning. A project can close the gap
  for itself today by writing its own `stubs/<pkg>.Rtypes`. Generating one by
  introspecting an installed library is the intended long-term answer, because
  which packages and versions are installed is a property of the project
  rather than of ry.
- **Keying the corpus to an R version.** Base evolves across releases.
  Functions appear and signatures change, and the shipped corpus describes one
  snapshot rather than the version a given project runs.
