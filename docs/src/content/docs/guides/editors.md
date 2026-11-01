---
title: Editors
description: What the language server does in your editor
---

`ry server` is a language server, so any editor that speaks LSP can use it. The
[VS Code extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) starts it
for you, and [installation](/installation) covers Zed and other editors.

It gives you:

- diagnostics as you type, the same ones `ry check` reports
- hover with the inferred type of any expression
- completion, including the fields of a record such as `person$`
- go-to-definition, find references, and rename
- signature help and inlay hints
- formatting with the same rules as `ry fmt`

Rename edits the binding you picked and nothing else: a local `total`, a global `total`, and the
word "total" in a string are three different things.

ry's own syntax gets the same features as plain R:

- **Tags and `match` patterns.** Hover on a tag, in a value or in a pattern, shows its payload type,
  and go-to-definition, references and rename follow a tag across the project. A tag spelled by the
  standard library, such as `:Ok`, is not renamed. Inside a pattern, completion offers the tags the
  matched value can still hold and the payload fields not yet named.
- **Records and tuples.** Hover on a field name, in a literal or in a pattern, shows the field's
  type, and `$` completes only what the value really has. Inside a record literal, a tag's payload
  or an `@Name(...)` constructor, completion offers the fields the expected type still lacks, so
  `m: Record{amount: double, currency: character} <- (amount = 1, cu)` completes `currency`.
- **Type definitions and annotations.** Go-to-definition on a type name or a type parameter such as
  `T` jumps to its definition, and a type parameter renames within its definition or function.
  Hover on a standard-library alias such as `Result` shows its definition.
- **DataFrames.** Inside `col("...")` or any column string of a verb, completion offers the columns
  of the DataFrame at that step, and hover shows the column's type. Inside data.table's
  `sales[i, j, by]`, it also offers the query symbols `.N`, `.SD`, `.I`, `.GRP`, `.NGRP` and `.BY`.
- **Highlighting.** Semantic tokens color type names, type parameters, tags, the `match` keyword and
  `=>`, in inline annotations, `#:` comments and `.Rtypes` stub files alike.

A type error in a record, tuple or tag points at the one field or item that is wrong, and a pattern
that can never match points at the misspelled field or tag. An inlay hint is left out where the code
already spells the type, as with a payload-free tag or an `@Money(...)` constructor.

Hover, completion, and inlay hints use inferred types even when type errors are switched off, so you
get them on any R project. The server reads the same `ry.toml` as the command line, and every editor
setting is listed under [editor settings](/reference/configuration#editor-settings).
