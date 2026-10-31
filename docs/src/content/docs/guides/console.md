---
title: Console
description: An R console with completion from the type checker
---

`ry repl` runs the R installed on your machine, unchanged, behind a line editor whose Tab completion
comes from the type checker. Completing `account$` lists the record's fields with their types,
worked out from the code you typed rather than from the live session.

```sh
ry repl            # interactive console
ry run script.R    # run a script through the same R, then exit
```

These are the only commands that need R. Everything else in ry reads the source and never runs it.
The [command reference](/reference/cli#server-repl-and-run) lists their options.
