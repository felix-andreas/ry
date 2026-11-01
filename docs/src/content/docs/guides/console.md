---
title: Console
description: An R console with completion from the type checker
---

`ry run` without a script opens an R console whose Tab completion comes from the type checker.
Completing `account$` lists the record's fields with their types, worked out from the code you
typed rather than from the live session.

```sh
ry run            # the console, on ry's runtime
ry run script.R   # run a script on ry's runtime, then exit
ry R              # the same console on GNU R, the R installed on your machine
ry R script.R     # run a script on GNU R, then exit
```

You can type [inline annotations](/type-checking/inline-annotations) at the console, and ry prints a
warning when an inline annotation and a `#:` comment disagree. In [strict
mode](/type-checking/strict-mode), a command with an error does not run. On ry's runtime the
annotations become checks when a function is called; GNU R runs each command with them erased.

ry's runtime is still incomplete, so code that needs something it lacks stops with an error that
names it. `ry R` is the console for that code. Started inside an R package, it also loads that
package and reloads what you change, as [Loading packages](/guides/loading-packages) describes.

These commands and `ry load` are the only ones that need R installed. Everything else in ry reads
the source and never runs it. The [command reference](/reference/cli#run-and-r) lists all options.
