---
title: Design drafts
description: "Working documents for features that are not settled: proposals, open questions, and designs in progress"
---

These are working documents, not contracts. Every other page in this
documentation describes what ry does today, and is kept accurate. The pages
below describe what ry might do, what is still undecided, and why. They are
deliberately absent from the sidebar, so this page is the way in.

Read a draft for the reasoning, not for current behavior. Where a draft and
the rest of the documentation disagree, the rest of the documentation is
correct. The authoritative typing contract is the
[type system reference](/reference/type-system/), and
[limitations](/type-checking/limitations/) lists what the checker cannot do
yet.

A draft leaves this folder in one of two directions. Its design is built and
graduates into the reference pages, or it is declined and the reasoning stays
here as the record of why.

## Drafts

### [Open type-system questions](/contributing/design/open-questions/)

This page collects the type-system questions with no settled answer. Each one
lists the options on the table and the current stopgap. The questions are
tagged unions, S3 dispatch, data frame and matrix modeling, the variadic `...`
body, and the import model. Traits are here too, as a closed entry. They were
declined rather than deferred, and the reasoning is recorded.

### [Data masking](/contributing/design/data-masking/)

This page is about checking non-standard evaluation, where a bare name inside
`dt[...]` or inside a dplyr verb is a column reference that no lexical scope
can see. Part of this has shipped, and the reference describes that part. The
page is the sketchpad for the step that has not shipped. That step needs
column vocabularies, and it depends on the data frame row-type design.

### [Inline type syntax](/contributing/design/inline-type-syntax/)

This page proposes writing types inline instead of in `#:` comments, as a
compiled dialect with its own file extension. Its own recommendation is not to
build it for inline typing alone, because the ergonomic case does not survive
scrutiny. The page is kept because checked record constructors and tagged
unions would need the same machinery, so the costing is reusable if those are
ever wanted.

### [REPL design](/contributing/design/repl/)

This page records how `ry repl` loads R at runtime instead of linking against
it at build time. That choice is what keeps the rest of the workspace free of
a dependency on R. The console has shipped, so most of the page is a design
record. The open part is wiring the analysis stack into it.
