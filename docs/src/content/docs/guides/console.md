---
title: Console
description: An R console with completion from the type checker
---

`ry repl` runs the R installed on your machine, behind a line editor whose Tab completion
comes from the type checker. Completing `account$` lists the record's fields with their types,
worked out from the code you typed rather than from the live session.

```sh
ry repl            # interactive console
ry run script.R    # run a script through the same R, then exit
```

You can type [inline annotations](/type-checking/inline-annotations) at the console. R runs each
command with them erased, and ry prints a warning when an inline annotation and a `#:` comment
disagree. In [strict mode](/type-checking/strict-mode), a command with an error does not run.

Started inside an R package, `ry repl` also loads that package and reloads what you change, as
[Loading packages](/guides/loading-packages) describes. These commands and `ry load` are the only
ones that need R. Everything else in ry reads the source and never runs it.

`ry repl` and `ry run` also accept `--runtime ry`, which runs on ry's own, still incomplete,
interpreter instead of GNU R; the console then loads no package. The [command
reference](/reference/cli#server-repl-run-and-load) lists all options.
