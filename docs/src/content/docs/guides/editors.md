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
- hover with the inferred type of any expression, parameter or loop variable
- completion, including the fields of a list such as `person$` and the argument names a call
  still accepts (`second = `), with your own names ranked ahead of keywords and the library
- go-to-definition, find references, rename, and a call hierarchy that lists the callers and
  callees of a project function, across files
- signature help with each parameter's type and default, and inlay hints for assignments, loop
  variables, and parameter names in calls with two or more positional arguments
  (`grow(principal = 100, years = 2)`)
- quick fixes for misspelled names, arguments, fields and columns, and for the lints that have one
  obvious rewrite (`T` to `TRUE`, `=` to `<-`, a trailing comma), and an "Apply all safe fixes"
  action your editor can run on save; [Fixes](/reference/diagnostic-codes#fixes) lists them all
- an outline that nests functions under RStudio-style sections (`# Accounts ----`) and helper
  functions and sections under the function that defines them, plus workspace symbol search
- folding for blocks, comment runs, sections and `# region` markers, and selection ranges that grow
  from a name to its argument, call, statement and block
- semantic highlighting of parameters, called functions and package namespaces, which an R grammar
  cannot tell apart, with functions from packages and base R marked as library functions
- formatting with the same rules as `ry fmt`, for the whole document, a selection, or as you type
  (a line break lays out the line you finished, and `}` the block it closes, when your editor
  formats on type)

Hover, completion and inlay hints use inferred types even when type errors are switched off, so you
get them on any R project.

Rename edits the binding you picked and nothing else: a local `total`, a global `total`, and the
word "total" in a string are three different things.

ry's own syntax, from [records and tags](/type-checking/records-and-tags#in-the-editor) to
[DataFrame](/type-checking/dataframes) columns, gets the same features as plain R.

## Settings

The server reads the same `ry.toml` as the command line, so the editor and CI report the same
findings. Every editor setting is listed under
[editor settings](/reference/configuration#editor-settings).
