---
title: Authoring stubs
description: The .Rtypes declaration format, overload sets, export manifests, and the scope rules for what belongs in the corpus
---

:::note[Status]
The stub format for the standard library ships. The corpus holds about 950 typed declarations in
eleven `.Rtypes` files in the repository's top-level `types/` directory, and generated `.exports`
[manifests](#export-manifests) cover every namespace R ships plus fifteen CRAN namespaces without
typed declarations. What is not built yet is listed [at the end of this page](#what-is-not-built-yet).
The typing contract itself is the [typing reference](/reference/type-system); this page describes
how the standard library feeds into it.
:::

## Why stubs exist

The checker has only a small kernel built in: the operators, plus `c` and `list`. Without stubs,
everything else in the standard library would be `Unknown`. `T`, `F`, and `pi` would have no type,
`length`, `nchar`, `seq_len`, `paste`, and the rest would have no signature, and every call through
them would lose all type information.

Stubs fill that gap. A stub is a declaration-only description of base R or another package, loaded
as an immutable input, so the checker knows the standard library the way rust-analyzer knows `core`
and `std`.

## Stub format

A stub file is a declaration-only file with the extension `.Rtypes` ("R type information"). Every
line that is not blank or a comment is one declaration:

```
name : <type-expr>
```

The type expression uses exactly the `#:` annotation grammar, read by the same parser, so there is
no second notation to build or to keep in sync. A stub file has nowhere to write a function body,
which makes "declaration-only" a structural fact rather than a convention, just as it is for
TypeScript's `.d.ts` and Python's `.pyi` files. Blank lines and `#` comments are ignored, whether a
comment fills a whole line or trails a declaration, and the loader turns each type expression
directly into a `TypeScheme`.

A declared `name` can be an R identifier, an infix operator such as `%in%` (or a project's own
`%||%`), or an S3 operator method such as `+.Date`, `-.POSIXct`, `Arith.difftime`, `Compare.Date`,
or `Ops.gg`. The operator spellings are how a stub gives a class arithmetic or comparison, and
[operator methods on a class](/reference/type-system#operator-methods-on-a-class) describes the
dispatch order. The suffix is the nominal type's own name, so a project stub line
`+.ggplot : fn(e1: ggplot, e2: Any) -> ggplot` is all a `+`-based DSL needs.

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

### Opaque types

Besides values, a stub can declare a type. A line `@type NAME` declares an *opaque nominal type*: a
named type with no inspectable representation, for standard-library values the type grammar cannot
describe structurally, such as `data.frame`, `factor`, `connection`, or `Date`.

An opaque nominal is compatible only with itself, and a value of it can only come from a function
declared to return it. So `Sys.Date()` is a `Date`, and passing it where a `factor` is expected is a
type error. The functions that *consume* such values usually keep `Any` parameters, because R
coerces liberally; the nominals exist to tighten return types. Using `$`, `[`, or `[[` on an opaque
nominal gives `Unknown` rather than an error, so `df$amount` and `df[rows, ]` stay usable, and strict
mode flags each such access. The [typing reference](/reference/type-system#nominal-types) has the
full rules.

A type name may contain an interior dot, as `data.frame` does, and a project's own `@type` or
`@alias` of the same name shadows the stub's type. The structural form
`@type NAME {REPRESENTATION}` cannot be written in `.Rtypes`: the opaque nominal is the only stub
form, and a `@type` line whose name is not a plain identifier is reported as an error on its own
line.

### Why a dedicated format

Stubs used to be ordinary R files with placeholder bodies, such as `length <- function(x) 0L`, from
which the loader harvested the `#:` annotation and ignored the rest. That let a stub carry a
meaningless, unreachable body, and it meant a full parse and lowering just to reach the annotation.
A dedicated declaration file fixes both: a body cannot be written, so it cannot creep back in, and
the loader parses nothing but declarations.

The extension `.Rtypes` echoes R's own `.Rd` convention without colliding with `.Rd`, `.Rmd`,
`.Rda`, or `.RData`. A JSON or TOML format was rejected because it would need a second type
notation; reusing the `#:` grammar keeps one source of truth for type syntax, inline and in stubs.

Other ecosystems split along the same line. Statically typed hosts that publish types for foreign
code use separate declaration files (TypeScript's `.d.ts`, Python's `.pyi`, Sorbet's `.rbi`), while
dynamically typed hosts that retrofit types onto their own source put them inline, as Elixir and
Erlang do with `@spec`. R does both. Inline `#:` comments are the primary form for a project's own
code, and `.Rtypes` files cover packages that cannot be annotated at the source, such as base R or a
CRAN package. Since both use the same grammar, they are one notation in two carriers.

### Data-masking functions

A declaration may put the `@masked` attribute in front of its function type:

```
summarise : @masked fn(.data: Any, ...: Any) -> Any
```

This marks a function that evaluates the arguments it collects in `...` inside a data mask, the
dplyr style of non-standard evaluation. A bare name in one of those positions is treated as a
column reference and raises no unresolved-name warning, while an argument that matches a declared
formal resolves normally. `@masked` requires a variadic function type. The typing reference
specifies the details under
[data frames and non-standard evaluation](/reference/type-system#data-frames-and-non-standard-evaluation).

## Overloads and generics

Declaring the same name more than once in one file builds an ordered *overload set*, with each
further declaration appending a candidate:

```
sum : fn(...: integer[] | logical[], [na.rm]: logical) -> integer
sum : fn(...: double[], [na.rm]: logical) -> double
sum : fn(...: Any, [na.rm]: logical) -> Any
```

A call commits to the first candidate that accepts its arguments, so `sum(1L, 2L)` is `integer` and
`sum(1.5, 2.5)` is `double`. The typing reference specifies the selection rules under
[overload sets](/reference/type-system#overload-sets).

Order each set from most specific to most general, and end it with the most general candidate,
conventionally an `Any` fallback. The fallback keeps a mixture the other candidates cannot express,
such as `sum(TRUE, 1L)`, from erroring, and it is also what the name resolves to when it is used
without being called, so a value never carries a narrower contract than the calls the name accepts.
A project override that redeclares a name replaces the whole set, so an override that wants
overloads must declare all of them itself.

Notice where the rest parameter sits in those three lines. A parameter declared before `...` can be
filled by position, so `[na.rm]` has to come after it, or `sum(1L, 2L)` would bind `1L` to `na.rm`
and fail.

The editor shows the whole set. Signature help on a call lists every candidate, with the committed
one active and filled in with that call's types, and the others showing their declared type
parameters. An incomplete call matches no candidate yet but still lists the set, which is exactly
when the list helps most. Hover on the name shows the committed candidate's signature (or, when the
name is not being called, the last declaration's) plus a `(+N overloads)` note, and
go-to-definition jumps to the first declaration.

Four rules shape the current corpus:

- **A genuinely parametric function gets a real generic.** A higher-order helper whose result
  depends on the *type* of its argument, such as `lapply`, `Reduce`, `print`, `invisible`,
  `setNames`, or `suppressWarnings`, is written with `<T> fn(...)` binders and keeps a precise
  polymorphic scheme. Element-preserving vector functions (`rev`, `sort`, `unique`, `head`, `tail`,
  `sample`, `rep`, and the set operations) use `<T> fn(x: T[]) -> T[]`, where the element parameter
  is limited to atomic types. That signature is usually the first candidate of an overload set whose
  list form threads `list[T]` through and whose `Any` fallback covers the rest.
- **A function that hands back the shape it was given declares each shape, narrowest first.** R's
  selection and reordering operations go through `[`, which preserves both the atomic type and the
  names, so the declarations say so. `Filter` takes `T[]`, then `list[named: T]`, then `list[T]`,
  and each candidate returns its own shape; a single `list[T]` return for all three would make
  `Filter(f, c(1, 2)) + 1` a type error on code R runs. `lapply`, `rev`, `unique`, `head`, and
  `tail` carry the same `list[named: T]` candidate ahead of their plain list one, so a named list in
  is a named list out. A fixed-shape input still coerces to a name-keyed list on the way in, though,
  so a field read off the result is `T | NULL`: the operation might drop a name, and the exact field
  types are not carried through.
- **A type-preserving reduction gets an overload set.** Reductions whose result follows R's coercion
  order rather than one input's type (`sum`, `min`, `max`, `range`, `pmin`, `pmax`, `cumsum`,
  `cummax`, `cummin`, and `abs` with its integer and double split) declare one candidate per atomic
  family, plus the `Any` fallback.
- **A value-dependent result falls back to `Any`.** When a function's return type depends on an
  argument's *value* or on how many arguments it gets, as with `seq`, `is`, and `grep(value =)`, it
  returns `Any` rather than a falsely precise type. A call then yields `Any` and never a spurious type
  or arity error, and the name still resolves.

## Override precedence

A project overrides or extends the shipped stubs with its own `.Rtypes` files under
`<project>/stubs/`. The loader folds the project's files over the shipped corpus in sorted path
order, so a project declaration replaces the shipped declaration of the same name. That is how a
project corrects a return type or adds a name the corpus lacks. A missing directory, an unreadable
file, or a malformed line is skipped, because overrides are optional and one bad line must never
block analysis.

A project stub file's name declares its namespace. `stubs/dplyr.Rtypes` declares the namespace
`dplyr`, so its declarations type bare reads and also validate qualified ones: `dplyr::mutate` is a
known-namespace read with the stub's type, while `dplyr::filter` warns "not exported" until the file
declares `filter`. This is how a project types a third-party package today, by hand, since
generating a stub from an installed library is not built. Exports are tracked per declaration, not
per winning declaration, so overriding a shipped name's type does not remove the name from its
shipped namespace, and `stats::sd` stays valid under an `sd` override. Hover shows a name's origin as
the namespace of its winning declaration.

Skipped never means silent, though. `ry check` reports every dropped override declaration as an
error on its own stub line: a line that fails to parse, a declaration naming a type that does not
resolve, and a `@type` line whose name is not an identifier. An unreadable override file is an IO
failure. While a `.Rtypes` file is open, the editor shows the same problems as diagnostics.

## Export manifests

Base R alone exports 1,408 names, and the typed corpus deliberately covers only a curated,
high-value subset. On its own, that would make every real export outside the subset warn as
unresolved, because the checker could not tell `recover`, a real `utils` export, from a typo.

Export manifests close the gap. Every shipped namespace's `.Rtypes` file is paired with a
`types/<namespace>.exports` file listing, one per line, every name the namespace really exports.
`scripts/export-manifests.R` generates them from a live R session and records the R version in the
header, so refreshing them means rerunning the script against a newer R. The script refuses to
overwrite a manifest recorded against a newer R than the running one, because regenerating on an
older R would silently drop the names the newer version added, and every use of one would become a
false `unresolved` finding.

A manifest does not need an `.Rtypes` file beside it. Fifteen namespaces have a manifest and no
typed declarations at all, so every name from them is `Unknown`: `tibble`, `tidyr`, `readr`, `purrr`,
`stringr`, `forcats`, `lubridate`, `magrittr`, `rlang`, `glue`, `scales`, `knitr`, `jsonlite`, `R6`,
and `tidyverse`. What they contribute is a *known export set*, and that matters more than it sounds.
Attaching a package whose exports the checker cannot list turns off unresolved-name detection for
the whole project, as the [typing reference](/reference/type-system#naming-and-scoping) specifies.
Before these manifests existed, a single `library(stringr)` meant that a clean run said nothing
about typos anywhere. Adding typed declarations for any of these packages later is purely additive:
the manifest stays the export set, and the `.Rtypes` file supplies the types.

A manifest name is a known global that makes the weakest possible claim:

- A bare or `pkg::`-qualified read of a manifest name always resolves, with neither an unresolved nor
  a not-exported warning. It has the type of its `.Rtypes` declaration if there is one, and
  `Unknown` otherwise. Precision is the typed corpus's job; the manifest's job is to stay quiet
  about names that really exist.
- Completion offers manifest-only names without decoration, since there is no type to show. A
  non-syntactic name is skipped when completing a bare name and backtick-quoted after `pkg::`, since
  inserting it raw would change the syntax. Typo suggestions draw on manifest names too.
- The unit suite pins the pairing in both directions: every value declaration in a shipped
  `.Rtypes` file must appear in its namespace's manifest, and a stubbed non-export is a hard
  failure. That catches a declaration added to the wrong file, and names R has moved between
  namespaces, such as `traceback` and `standardGeneric`, which live in `base` rather than `utils` or
  `methods`. A conditional namespace may additionally override a base name, as data.table's
  class-preserving `merge` does.

The manifests mirror the three ways R exposes a namespace:

- **Attached by default.** `base`, `stats`, `utils`, `graphics`, `grDevices`, `methods`, and
  `datasets` are visible everywhere without qualification, so their names always resolve.
  `datasets` is special: its objects are lazy data rather than namespace exports, so its manifest is
  built from the search-path listing. The famous data frames are typed in `datasets.Rtypes`, which
  makes `iris` a `data.frame` rather than `Unknown`.
- **Shipped with R but not attached.** `tools`, `parallel`, `compiler`, `grid`, `splines`, `stats4`,
  and `tcltk` validate their `pkg::` reads in every project, so `tools::file_ext(path)` needs no
  `library(tools)`, exactly as in R. A bare read resolves only once the project attaches or declares
  the package.
- **Conditional.** `data.table`, `dplyr`, `ggplot2`, `testthat`, and every manifest-only CRAN
  namespace switch on their manifest and their stubs together, as described under
  [conditional namespaces](#conditional-namespaces). While a package is inactive, its names warn
  whether they are qualified or not. A meta-package activates the packages it attaches:
  `library(tidyverse)` exports almost nothing itself, but it attaches `dplyr`, `ggplot2`, `tibble`,
  `tidyr`, `readr`, `purrr`, `stringr`, `forcats`, and `lubridate`, so those nine switch on with it.
  That is also how such a project gets dplyr's and ggplot2's typed declarations instead of a
  manifest's `Unknown`. The membership is easy to check, because the generator script prints what a
  live `library(tidyverse)` attaches.

The manifests vendor R's export lists, so analysis never needs an R installation. They are data,
not types, and a manifest name costs nothing at check time until some code actually reads it.

## The base environment

Everything that can be written as a type scheme has one source of truth, the stubs, plus a small
hardcoded kernel:

- **Value bindings**: `T` and `F` become a monomorphic `Scalar(Logical)`, and `pi` becomes a
  `Scalar(Double)`.
- **Base functions**: each becomes a type scheme, seeded into the template environment next to the
  built-in kernel.

### What stays hardcoded, and why

The operators, `c`, and `list` are algorithms, not types. The operators need the numeric promotion
lattice and the comparison families, `c` needs variadic atomic promotion that drops `NULL`, and
`list` needs to synthesize records and tuples. The `#:` grammar can express none of that, so these
stay built in. The one-source-of-truth goal holds for everything a type can describe, and the kernel
is a deliberate exception, not an oversight.

### What the stub grammar cannot say yet

Two extensions that a faithful corpus needed have landed:

| Extension | Example | Form |
|-----------|---------|------|
| Variadics | `paste`, `sum`, `cat` | a trailing `...: TYPE` rest parameter, as in `fn(...: Any) -> character` |
| Dotted parameter names | `na.rm`, `length.out` | an interior `.` is allowed in parameter and field names |

The gaps below remain, and each one limits how precise the affected declarations can be:

| Gap | Example | Extension needed |
|-----|---------|------------------|
| A trailing dot in a parameter name | `stop(call. =)`, `warning(immediate. =)` | parameter names currently allow only an interior dot |
| Absorbing a named argument into the rest parameter | `data.frame(x = 1)`, `Sys.setenv(VAR = "v")`, `par(mfrow = ...)` | the checker never routes a named argument into `...`, so a sink for arbitrary named arguments has to stay an `Any` value |
| An empty type for a function that never returns | `stop`, `q` | without one, a `NULL` return would poison the join in `x <- if (ok) v else stop(...)`, so these stay `Any` |
| A return that mirrors the argument's shape | `rev(opts)$timeout` on a fixed-shape `opts` | a declaration cannot say "the same record back", so selection and reordering return a name-keyed `list[named: T]`, and a field read off it is `T \| NULL` rather than the field's own type |
| A nullable result under a member-wise operator | `names`, `dim`, `nrow` | a `T \| NULL` return would false-positive on `1:nrow(df)` and `for (nm in names(x))` until flow narrowing or NULL-tolerant joins exist, so these return `Any` |

Three rows have already left this table. Type-preserving reductions declare
[overload sets](#overloads-and-generics). Element-preserving functions declare generic `T[]`
signatures, where the `T[]` suffix carries the atomic-element bound specified under
[type parameters](/reference/type-system#type-parameters-and-generic-application). And function
compatibility no longer demands a matching parameter count: a function can serve a callback
interface whenever it accepts every call shape the interface promises, so a spare optional formal is
fine and a callback-style stub can declare its real signature. `lapply(words, nchar)` works with all
of `nchar`'s formals declared. What remains a scalar claim is a function whose result's atomic type
differs from its input's while its shape follows the input, such as `nchar` or `toupper`.

## Loading and namespacing

Stubs are immutable, set-once inputs, the analogue of rust-analyzer's `Durability::HIGH`: they are
loaded, parsed, and interned once, and no ordinary edit invalidates them. There is one deliberate
exception. Assembling the library also reads the package-metadata input, which switches the
[conditional namespaces](#conditional-namespaces) on and off, so when a project starts or stops
declaring or attaching such a package, the library is rebuilt. That is rare, and worth the full
refresh.

The `StubLibrary` is a handful of flat, string-keyed tables, built once and folded over every
source:

- `schemes` maps a name to its ordered list of schemes, one per declaration (see
  [overload sets](#overloads-and-generics)).
- `nominals` is the set of `@type` names.
- `masked` maps each `@masked` name to the formals declared before its `...`.
- `exports_by_namespace` maps a namespace to the names it declares.
- `declarations` maps a name to the source index and range of its winning declaration. The declaring
  namespace is derived from the source index rather than stored, so the two can never disagree.
- `known_exports` is the union of every manifest's names.

Typing is not partitioned by namespace. The per-namespace export table answers `pkg::name`
validation next to the flat scheme map, which is why an override that wins a name's type never
un-exports it from the namespace that declared it.

- Every shipped `.Rtypes` file is folded into the one flat map in file order. A later source that
  redeclares a name replaces its whole entry, the same rule that governs project overrides, while
  repeated declarations within one source build the name's overload set. The seven default-attached
  namespaces therefore join the base scope together, and a conditional namespace joins the fold only
  while it is active.
- The flat map seeds the per-document inference template, so every stub name resolves as a bare
  global no matter which shipped file declared it. A name's *last* scheme becomes its plain
  environment binding, because the corpus orders candidates from specific to general. A name with
  several schemes also registers its overload set for call-site selection.
- `pkg::name` resolves against the same flat map, so a qualified read has exactly the type of the
  bare name. The per-namespace export table powers the warnings for an unknown namespace and for a
  name a namespace does not export, which the typing reference specifies under
  [namespace access](/reference/type-system#namespace-access).

### Conditional namespaces

Some shipped namespaces describe packages R does not attach by default: `data.table`, `dplyr`,
`ggplot2`, and `testthat` come with types, and the [manifest-only](#export-manifests) CRAN set comes
without. Folding them in unconditionally would let `fread`, `mutate`, or `str_to_upper` resolve in a
project that never uses them, stealing a typo warning. So a conditional namespace joins the library
only when the project uses the package, and any one of these signals is enough:

- A `DESCRIPTION` dependency field (`Depends`, `Imports`, `Suggests`, or `Enhances`) names it, or a
  `NAMESPACE` `import` or `importFrom` names it as a source.
- A project file attaches or loads it with `library()`, `require()`, `requireNamespace()`, or
  `loadNamespace()`, with a literal name or string as the package argument, anywhere in the file.
  This is the only signal a script workspace has, since only packages carry metadata files.
- The project ships its own `stubs/<pkg>.Rtypes` override for the namespace. Writing one is the
  clearest possible statement that the project uses the package, and the override then folds over
  the shipped declarations as usual.
- A meta-package that attaches it is active: `library(tidyverse)` activates the nine packages it
  attaches, because attaching is how R makes their names reachable.

`testthat` is in the set for a different reason than the data packages. A package's `tests/`
directory is often a third of its source, and every line of it calls these names. Without typed
stubs, the only way to quiet them was the blanket tolerance that an unknowable export set earns, and
that tolerance also silenced typos in the package's own functions. With the stubs, `expect_eqaul` is
reported, along with a suggestion. An expectation's `object` parameter is `Any`, because an
expectation compares whatever the code under test produced, and each `expect_*` returns its
`object`, which is the value testthat passes through invisibly.

While a namespace is inactive, it simply is not there: its names warn as unresolved, a `pkg::` read
behaves like a read from any package without stubs, and its `@type` nominals are unknown type names.
The hosts keep the attach facts current for each synced file. The language server rescans only the
changed document and fills in the rest of the workspace during idle priming, so switching a
namespace on never costs a whole-project sweep on a keystroke.

### NAMESPACE import validation

`ry check` and the language server both validate a package's `NAMESPACE` file against the loaded
stubs. An `importFrom(pkg, name)` whose namespace the corpus knows (shipped or project) but which
does not export `name` is an error at the import:

```text
`medain` is not exported by `stats`, so this package will not load.
```

R refuses to load a package with such an import, so the code cannot run at all; it is the same rule
as an `export()` of a name the package never defines. A namespace without stubs is not checked,
because stubs are the only export source the checker has. Resolution itself is unchanged, and a
stubbed name resolves bare with or without an import.

The check needs no interner, because it reads a string-keyed export table built once from the loaded
corpus. That is what lets an open `NAMESPACE` buffer be validated live without touching the
engine's shared interner, the same isolation the `.Rtypes` buffer path keeps.

Three parts of namespacing are not built:

- A separate name-to-scheme table per namespace, instead of one flat fold. Today two shipped
  packages cannot declare the same name with different types.
- `library(pkg)` attaching a namespace on demand.
- Gating bare third-party names on `NAMESPACE` imports. That would be R-correct, and whether to do it
  is a deliberate strictness decision; today's flat fold is intentionally permissive.

### Incremental hygiene

In the [query engine](/contributing/architecture#the-semantics-database), the stub library is a
set-once input. Its revision never advances, so it can never invalidate a query that reads it. That
is the entire isolation property, and it holds automatically: a stub never triggers recomputation
because it never changes, not because some separate check prevents it.

A package binding that shadows a stub name is an ordinary structural edit. It changes which
definition wins that name in the package symbol index, and that change flows through the per-symbol
interface to exactly the files that reference the name, the same path as any definition appearing
or disappearing. A stub value and an unrelated user type that share a name, such as the value `T` and
`#: @type T`, live in the value and type namespaces respectively, and never interact.

## Scope: model only what is statically reasonable

The corpus models:

- fixed-arity signatures;
- scalar constants;
- common vectorized atomic operations with a stable result shape;
- prenex rank-1 generics;
- nominal type and class declarations.

It deliberately does not model the following. For these, declare the part of the signature that is
statically stable and stop, or use `Any`:

- non-standard evaluation and data masking, such as `subset`, `with`, and formulas;
- tricks that forward `...`;
- the dispatch dynamics of S3, S4, R5, and R6, such as `UseMethod` and `NextMethod`;
- `do.call`, `match.arg`, `match.call`, `sys.call`, and `Recall`;
- reflective environment access, such as `environment`, `assign`, `get`, and `eval`;
- partial argument matching;
- replacement functions, such as `` `names<-` ``.

The rule of thumb: if a function's return type is not a static function of its argument types, but
depends on a runtime value or class, leave the declaration out or give it `Any`.

### Corpus compromise vocabulary

The corpus optimizes for two things, in this order: no false errors on idiomatic calls, and then the
most precise return type that is still sound. The compromises that keeps forcing are named once in
the header of `base.Rtypes` and referred to by name in individual entries:

- **Any-param**: a parameter is `Any` even though R documents a type for it, because R itself coerces
  arguments and `nchar(42)` is legal, so a declared type would reject calls R accepts. The checker's
  own coercions reduce how often this is needed: an `integer` widens to `double` at a parameter, a
  whole-number double literal counts as an `integer`, and a scalar coerces into a vector position.
  The `T[]` generics restore the rest of the precision.
- **scalar-claim**: an elementwise function declares its scalar result (`character` rather than
  `character[]`). A scalar claim coerces into every vector position and so can never cause a false
  positive downstream, whereas a vector claim would break `if (grepl(...))`.
- **type-preserving**: the result's type follows the input's, as for `sum`, `sort`, and `rev`.
  Element-preserving vector functions declare `<T> fn(x: T[]) -> T[]`, and reductions whose result
  follows R's coercion order declare one [overload candidate](#overloads-and-generics) per atomic
  family. Every set ends with an `Any` fallback, which is also what a call selects while an
  argument's type is still undetermined; because a fallback taking `Any` binds nothing, it never
  narrows the caller.
- **NULL-hybrid**: the result is `T` or `NULL` depending on the runtime value, as for `names`, `dim`,
  and `nrow`. These return `Any`, for the reason the gaps table above gives.
- **named-formals**: declare the named formals whose types are worth checking, such as `paste`'s
  `sep` and `collapse` or `read.csv`'s `header` and `sep`. An undeclared named argument to a
  variadic function is not an error: the rest parameter absorbs it and checks it against the rest
  parameter's element type, just as R would accept it. So an open space of named arguments works
  with `...: Any`, as for `read.csv`'s pass-through to `read.table` and `lm`'s fitting controls.
  Declaring a formal buys a typed check; leaving it out buys pass-through.

The corpus also borrows a two-tier dynamic marker from typeshed. A real `Any`, for a return that
genuinely cannot be typed, is kept distinct from a greppable marker for a declaration that is merely
incomplete, mirroring typeshed's split between `Any` and `Incomplete`. That makes a partial stub
first-class and improvable, and lets tooling find what still needs work.

## Worked example

Here is a fragment of the base stubs:

```
T : logical
F : logical
pi : double
length : fn(x: Any) -> integer
nchar : fn(x: Any, [type]: character, [allowNA]: logical, [keepNA]: logical) -> integer
seq_len : fn(length.out: Any) -> integer[]
```

Each line produces one scheme, which the checker renders back in the same notation:

| Binding | Scheme |
|---------|--------|
| `T`, `F` | `logical` |
| `pi` | `double` |
| `length` | `fn(x: Any) -> integer` |
| `nchar` | `fn(x: Any, [type]: character, [allowNA]: logical, [keepNA]: logical) -> integer` |
| `seq_len` | `fn(length.out: Any) -> integer[]` |

`nchar`'s subject and `seq_len`'s count are Any-params (see the
[compromise vocabulary](#corpus-compromise-vocabulary)), so `nchar(42)` and `seq_len(10)` check
clean while the return types stay precise.

This R type-checks against those stubs plus the built-in kernel:

```r
n    <- length(c(1L, 2L, 3L))  #: integer  (stub scheme)
half <- pi / 2                 #: double   (operator kernel on a double scalar)
flag <- T                      #: logical  (stub value binding)
```

The stubs and the kernel work together here: `length`'s scheme types `n`, the operator kernel
promotes `pi / 2`, and the `T` value binding types `flag`.

`paste`'s variadic arguments and its `sep` and `collapse` named formals can be declared directly,
just like `seq_len`'s dotted `length.out`:

```
paste : fn(...: Any, [sep]: character, [collapse]: character | NULL, [recycle0]: logical) -> character
```

Where the rest parameter sits among the named ones is part of the signature, because it decides
which parameters a positional argument can still fill.

One gap remains, and it degrades a return to the scalar claim: a function like `nchar`, whose
result has a different atomic type from its input but should follow the input vector's shape,
cannot say so. Element preservation in the style of `rev` is covered by the generic `T[]` suffix.

## Per-edit cost

Because the stub library is a [set-once input](#incremental-hygiene), merely having it loaded adds
nothing to the cost of an edit: an edit never invalidates it, and rechecking a body still touches
only the edited document and whatever refers to it. A document pays inference time only for the
base names it actually uses, which is the feature doing its job rather than bookkeeping overhead,
and a document that uses no base name pays nothing at all.

## What is not built yet

Two extensions are designed but not built, and their absence shows:

- **Third-party packages beyond the shipped set.** The corpus covers the namespaces R itself ships,
  plus conditional stubs for `data.table`, `dplyr`, `ggplot2`, and `testthat`. Any other CRAN
  package's names are `Unknown`, with an unknown-namespace warning. A project can close the gap for
  itself today by writing its own `stubs/<pkg>.Rtypes`. The intended long-term answer is generating
  stubs by introspecting the installed library, because which packages and versions are installed
  is a property of the project, not of ry.
- **Keying the corpus to an R version.** Base R evolves: functions appear and signatures change, and
  the shipped corpus describes one snapshot rather than whichever version a given project runs.
