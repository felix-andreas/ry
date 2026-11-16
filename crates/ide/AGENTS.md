# ide

Read this before you change editor features. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Cursor position model: the smallest end-inclusive expression wins and a `NameRef` wins a tie.** `semantics::item_name_range` is the one answer to which token declares an item, and `:=` is never a definition.
- **A quick fix is a field on the diagnostic (`fix`), never re-derived in `ide`.** Editor gaps are in `.agents/memory/backlog.md`, "Open: editor parity".
- **Completion ranking by expected type only demotes, and only on declared types** read across every overload (`.agents/memory/decisions/ide-server.md`, "Completion pushes down names whose declared type cannot fit, and never promotes").
- **`list` and `c` have stubs only for signature help and their value type**; the checker types their calls first. data.table's `.()` reads as `list()` when `.` is unbound inside a `[` argument (`is_query_list`).
- **The IDE fixtures print the token a range covers**, not a byte offset, so a stub addition never forces a re-bless. If one fails after a corpus change, the range is genuinely wrong.
