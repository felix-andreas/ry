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

Hover, completion, and inlay hints use inferred types even when type errors are switched off, so you
get them on any R project. The server reads the same `ry.toml` as the command line, and every editor
setting is listed under [editor settings](/reference/configuration#editor-settings).
