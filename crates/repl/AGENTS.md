# repl

Read this before you change the R console behind `ry repl` and `ry run`. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **The console's crossterm is patched (`patches/crossterm`) to drain the terminal before returning an event**, or the rest of a paste waits on reedline's two-second cursor query. The `use-dev-tty` reader is no substitute.
- **R gets console input one line per chunk** (`take_chunk`), because R discards the rest of its buffer on an error. A read below top level is answered from the console, never from `pending` (`crates/ry/tests/test_repl_e2e.rs`).
- **The console preflight checks each command as its own file against earlier commands**, and buffers plain R unanalyzed until the session holds ry syntax or strict mode (`.agents/memory/decisions/typed-stdlib.md`, "Inline annotations at run time").
- **The console reads typed-ahead bytes only up to the first line break** (`take_typed_line`), leaving the rest for fd-0 readers. Ctrl-C needs an R `interrupt` handler plus a chained SIGINT handler, since GNU R swaps its own in during `Sys.sleep` and `system()`.
- **The REPL never feeds R input through the console to set state**: start-up work goes through `RApi::eval` before `run_Rmainloop()` (`crates/repl/src/libr.rs`; `development/design/repl.md`).
- **A REPL pty test expects text the echoed command cannot contain** (`Error: boom`, not `boom`), or it matches the echo before the command runs.
