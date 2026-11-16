# format

Read this before you change the formatter. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Formatter placement is token-level** (`last_token_before`, walking back from the offset; a subtree scan is quadratic on deep files), because trailing comments attach inside expression nodes. It refuses only on R-grammar errors. `ry fmt` on 309,000 lines takes about 0.5 s on four cores; what remains is cursor traversal (a reference-count bump per `SyntaxNode`) and allocation. Sweep a corpus with `format_directory` (`RY_FORMAT_DIR`, `test_fuzz.rs`).
