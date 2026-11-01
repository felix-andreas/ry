---
title: Editors
description: What the language server gives you in your editor, and how to set it up
---

In your editor, ry shows every finding of `ry check` as you type, and it tells you the type of
anything you hover, so you see a mistake the moment you make it instead of when the code runs. In
VS Code, install the
[ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) and open an R
project: there is nothing else to set up. [Installation](/installation) covers Zed, and any other
editor that speaks LSP can run `ry server` as its language server for R.

You get:

- diagnostics as you type, the same ones `ry check` reports
- hover with the inferred type of any expression
- completion, including the fields of a list or record such as `person$`
- go-to-definition, find references, and rename
- signature help and inlay hints
- formatting with the same rules as `ry fmt`

Hover, completion and inlay hints use inferred types even when type errors are switched off, so you
get them on any R project.

Rename edits the binding you picked and nothing else: a local `total`, a global `total`, and the
word "total" in a string are three different things.

## ry's own syntax

Records, tags, `match`, type definitions and `DataFrame` get the same features as plain R:

- **Tags and `match` patterns.** Hover on a tag, in a value or in a pattern, shows its payload type,
  and go-to-definition, references and rename follow a tag across the project. A tag spelled by the
  standard library, such as `:Ok`, is not renamed. Inside a pattern, completion offers the tags the
  matched value can still hold and the payload fields not yet named.
- **Records and tuples.** Hover on a field name, in a literal or in a pattern, shows the field's
  type, and `$` completes only what the value really has.
- **Type definitions and annotations.** Go-to-definition on a type name or a type parameter such as
  `T` jumps to its definition, and a type parameter renames within its definition or function.
  Hover on a standard-library alias such as `Result` shows its definition.
- **DataFrames.** Inside `col("...")` or any column string of a verb, completion offers the columns
  of the DataFrame at that step, and hover shows the column's type. Inside data.table's
  `sales[i, j, by]`, it also offers the query symbols `.N`, `.SD`, `.I`, `.GRP`, `.NGRP` and `.BY`.
- **Highlighting.** Semantic tokens color type names, type parameters, tags, the `match` keyword and
  `=>`, in inline annotations, `#:` comments and `.Rtypes` stub files alike.

A type error in a record, tuple or tag points at the one field or item that is wrong. An inlay hint
is left out where the code already spells the type, as with a payload-free tag or an `@Money(...)`
constructor.

## Settings

The server reads the same `ry.toml` as the command line, so the editor and CI report the same
findings. Every editor setting is listed under
[editor settings](/reference/configuration#editor-settings).
