# ry

Read this before you change the language server and the CLI. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Server inputs are `Durability::MEDIUM`, except open documents and `ProjectFiles` (`LOW`), and every setter names its durability.** One `LOW` input that most memos read turns each keystroke into a project-wide verification (`.agents/memory/decisions/ide-server.md`, "Decision record: editor responsiveness at 300,000 lines").
- **On the server's worker every tracked read goes through `Worker::cancellable`.** A notification queued behind the running job flips the cancellation token, and a query fetch outside the catch unwinds through `run_job` and stops the worker with no panic message.
- **Salsa clears a handle's cancellation flag whenever a top-level query returns**, so background work that must yield polls `CancelHandle::has_queued_jobs` between units (`Worker::in_parallel`).
- **miette's header column counts bytes, so `read_span` re-counts characters** (`crates/ry/src/cli.rs`); a multi-line range clamps to the first line, and sources are borrowed (`NamedText`).
- **The CLI and the language server must compute `check_cache::key` identically, and whatever a name-keyed cross-file map reads must stay inside `check_cache::surface`** (`crates/ry/src/check_cache.rs`). Otherwise `.ry_cache/check.json` hands back stale findings. The seeded simulation in `cli.rs` catches a read that escapes the surface.
- **A simulator failure is a server bug until shown otherwise, but check the client model first**: a reopened document continues its version, and the first-wave-is-a-subset check holds only when no config or disk change landed since the sync.
