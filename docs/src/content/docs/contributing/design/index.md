---
title: Design drafts
description: "Working documents for features that are not settled: proposals, open questions, and designs in progress"
---

These are working documents, not contracts. They describe what ry *might* do, what is still
undecided, and why, so read them for their reasoning, not as a description of current behavior.
Where a draft disagrees with the rest of the documentation, the rest of the documentation is right:
the typing contract is the [type system reference](/reference/type-system/), and
[limitations](/type-checking/limitations/) lists what the checker cannot do yet.

The drafts are left out of the sidebar, so this page is the way in. A draft leaves this folder when
its design is built and graduates into the reference pages, or stays here as the record of why it
was declined.

## Drafts

### [Open type-system questions](/contributing/design/open-questions/)

Type-system questions with no settled answer: tagged unions, S3 dispatch, modeling data frames and
matrices, the body of a variadic `...` function, the import model, and non-standard evaluation.
Each entry lays out the options and the stopgap in place today. Traits are a closed entry, kept to
record why they were declined.

### [Data masking](/contributing/design/data-masking/)

How to check non-standard evaluation, where a bare name inside `dt[...]` or a dplyr verb is a column
reference that no lexical scope can see. The shipped part is specified in the typing reference;
the next step needs column vocabularies and depends on how data frame row types are designed.

### [Inline type syntax](/contributing/design/inline-type-syntax/)

A proposal to write types inline instead of in `#:` comments. The project mandate now requires an
inline annotation syntax, enabled by default, so the page's recommendation against building it is
superseded; its design details and costing remain input for the implementation.

### [REPL design](/contributing/design/repl/)

How `ry repl` loads R at runtime instead of linking against it at build time, which keeps the rest
of the workspace free of any dependency on R. The console has shipped, so most of the page records
the design; the open part is feeding live-session facts and pre-evaluation diagnostics into it.
