---
title: Design drafts
description: "Working documents for features that are not settled: proposals, open questions, and designs in progress"
---

These are working documents, not contracts. They describe what ry *might* do, what is still
undecided, and why, so read them for their reasoning, not as a description of current behavior.
Where a draft disagrees with the rest of the documentation, the rest of the documentation is right:
the typing contract is the [type system reference](/reference/type-system/), and
[limitations](/reference/type-system#limitations) lists what the checker cannot do yet.

The drafts are left out of the sidebar, so this page is the way in. A draft leaves this folder when
its design is built and graduates into the reference pages, or stays here as the record of why it
was declined.

## Drafts

### [Typed ry's native value types](/development/design/native-types/)

ry's own value types (`I64`, `F64`, `Bool`, `Str`, `List<T>`) and the record, tag and pattern
additions, with the explicit R boundary and the runtime contract.

### [Typed ry's hill climb](/development/design/language-climb/)

How typed ry grows: a fitness function over speed against Rust, friendliness cases and soundness
fuzzing, run by `just climb`, and the log of each step.

### [Open type-system questions](/development/design/open-questions/)

Type-system questions with no settled answer: tagged unions, S3 dispatch, modeling data frames and
matrices, the body of a variadic `...` function, the import model, and non-standard evaluation.
Each entry lays out the options and the stopgap in place today. Traits are a closed entry, kept to
record why they were declined.

### [Data masking](/development/design/data-masking/)

How to check non-standard evaluation, where a bare name inside `dt[...]` or a dplyr verb is a column
reference that no lexical scope can see. The shipped part is specified in the typing reference;
the next step needs column vocabularies and depends on how data frame row types are designed.

### [Inline type syntax](/development/design/inline-type-syntax/)

The proposal to write types inline instead of in `#:` comments. Inline annotations are now built
and on by default (see [inline annotations](/tutorial/runtime#inline-annotations)), so the page's
recommendation against building them is superseded; it remains the record of the design.

### [REPL design](/development/design/repl/)

How the console loads R at runtime instead of linking against it at build time, which keeps the rest
of the workspace free of any dependency on R. The console has shipped, so most of the page records
the design; the open part is feeding live-session facts and pre-evaluation diagnostics into it.

### [The R runtime](/development/design/r-runtime/)

How ry becomes an R implementation of its own: an ABI-compatible C API (with measured usage across
the benchmark's 232-package dependency closure), the object model and GC that API forces, a bytecode
interpreter with a Cranelift JIT fed by ry's static types, lazy package loading from the analysis
index, a table type on Polars, and the order to build it in. The C API, the object model, the
interpreter and the table engine are built; the JIT is not.

### [Prior art from new language implementations](/development/design/prior-art/)

What Roc, Bend, goose and Vx do best in memory management, compilation, typing and C interop, and
which of their ideas the R runtime adopts or rejects given that R's C API must keep working. It is
written as an Open Knowledge Format v0.2 concept document, with sources in its frontmatter.

### [Packages and R versions in the runtime](/development/design/packaging/)

How packages install into ry's runtime (prebuilt GNU R 4.6 binaries load unchanged through the
ABI-compatible `libR`), how ry chooses between GNU R and its own runtime and between R versions while
the runtime is incomplete, and what the lazy loader, the analysis and the corpus read from an
installed library. A package manager of ry's own is left open. Only package installation and the
explicit runtime choice are implemented.

### [Records, tuples and tags](/development/design/records-and-tags/)

Roc-style records, tuples and tagged unions as ry's replacement for S4, R6 and Reference Classes.
They are implemented, and the typing rules have moved to the reference; the page keeps the
rationale, the runtime representation as ry's own heap objects, and the open questions.

### [The standard library](/development/design/standard-library/)

A Roc-style standard library of `Result`, `List`, `Str`, `Num`, `Bool`, `Dict` and `Set` functions, named
`Module.function` as plain R identifiers. It covers which of Roc's functions ry takes and why, how
the names avoid every base R and recommended-package name, how the functions are typed as generic
stubs, and why they are native builtins that are always available on ry's runtime.

### [The typed runtime](/development/design/typed-runtime/)

How typed ry runs close to Rust's speed: a typed core IR compiled ahead of time from the checker's
types, Rust-like layouts for numbers, strings, lists, records and tags, direct calls with no
environment, native values freed by compiler-inserted reference counts with in-place reuse, and
conversion only at the R boundary. It builds to the runtime contract of the native-types design.
