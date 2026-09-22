---
title: Design drafts
description: "Working documents for features that are not settled: proposals, open questions, and designs in progress"
---

These are working documents, not contracts. Every other page of this documentation describes what
ry does today and is kept accurate; the pages below describe what ry *might* do, what is still
undecided, and why. They are deliberately left out of the sidebar, so this page is the way in.

Read a draft for its reasoning, not as a description of current behavior. Where a draft and the rest
of the documentation disagree, the rest of the documentation is right. The typing contract is the
[type system reference](/reference/type-system/), and [limitations](/type-checking/limitations/)
lists what the checker cannot do yet.

A draft leaves this folder in one of two ways: either its design is built and graduates into the
reference pages, or it is declined and stays here as the record of why.

## Drafts

### [Open type-system questions](/contributing/design/open-questions/)

The type-system questions that have no settled answer yet: tagged unions, S3 dispatch, modeling
data frames and matrices, the body of a variadic `...` function, and the import model. Each entry
lays out the options and the stopgap in place today. Traits are here too, as a closed entry, since
they were declined rather than deferred and the page records why.

### [Data masking](/contributing/design/data-masking/)

How to check non-standard evaluation, where a bare name inside `dt[...]` or a dplyr verb is a
column reference that no lexical scope can see. Part of this has shipped, and the reference
describes that part. The page is the sketchpad for the next step, which needs column vocabularies
and depends on how data frame row types are designed.

### [Inline type syntax](/contributing/design/inline-type-syntax/)

A proposal to write types inline instead of in `#:` comments, as a compiled dialect with its own
file extension. The page's own conclusion is not to build it for inline types alone, because the
ergonomic case does not survive scrutiny. It is kept because checked record constructors and tagged
unions would need the same machinery, so the costing is worth having if those are ever wanted.

### [REPL design](/contributing/design/repl/)

How `ry repl` loads R at runtime instead of linking against it at build time, which is what keeps
the rest of the workspace free of any dependency on R. The console has shipped, so most of the page
is now a design record; the open part is wiring the analysis into it.
