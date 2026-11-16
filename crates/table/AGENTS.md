# table

Read this before you change the table engine. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Polars computes `NaN` where R says `NA`** (`cor` over a constant group), so each IR lowering that can produce `NaN` decides which R means (`crates/table/tests/test_table.rs`).
