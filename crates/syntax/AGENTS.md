# syntax

Read this before you change the lexer and parser. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **A parse error inside a `#:` comment is not an R syntax error** (`SyntaxError::in_annotation`, code `annotation`). The runtime's parser and the console's completeness test skip it, or a malformed comment stops a script GNU R would run.
- **A function has one signature, resolved in `syntax::annotation` for checker and runtime alike**, and both spellings lower into one `Annotation`, so no checker path knows the spelling. A path that lowers a function from less than its file keeps the statement and the `#:` lines above it (`.agents/memory/decisions/syntax-semantics.md`, "A function has one signature, resolved in `syntax::annotation`").
- **Recovery stops at the statement boundary when the next line assigns** (`starts_new_statement`). `=` does not count, because inside a list it is a named argument.
- **One mistake, one finding.** A lexer `ERROR_TOKEN` suppresses its statement's boundary report, and the `#:` grammar reports once per region (`error_unclosed`). Blame the incomplete construct, not where the search stopped.
- **An unterminated quote ends at its first line break**, so typing a quote does not take the rest of the file out of analysis.
- **`#:` block-form rules count lines over all of the block's tokens**, since a stitched line's marker may sit in a nested node. A `<T>` binder list prefixes the next type; a block containing an `ERROR` node carries no typing payload.
- **A ry rule on a construct that parsed in full reports through `Parser::error_rule`**, so the runtime keeps ry's message and a console reports instead of waiting for input. `error_at` there blames whatever token follows.
- **Inline type grammar defers its trivia** (`Parser::deferred_trivia`): a new `ann_*` path emits through `bump`, `start` or `checkpoint`, never the builder directly.
- **An annotated binding is recognized only at a statement start** (`Parser::statement`), because R reads `name:TYPE <- v` as `` `:<-` ``. An inline union continues onto a line starting with `|`.
- **The parser's acceptance gate is zero unadjudicated divergences against tree-sitter, with R as the referee**, judged on the erased text (`erase_inline_annotations`). A new error path adds its golden case (`crates/syntax/tests/errors`).
- **Syntax-only highlighting lives in `syntax::highlight`, shared by server and console.** An argument name is `Parameter` only with a direct `EQ` child, because `f(x)` passes `x` as a value.
- **`testing::in_parallel` deals items out statically and catches each item's panic.** Fixture renders reuse one analysis database per worker thread, so handing items to whichever worker is free would give each database a different history on every run; keep the static partition, and keep `RY_TEST_WORKERS` in failure messages so a failure replays with the same partition.
- **A test's nested `cargo build` goes through `testing::cargo_build`,** which drops the variables cargo sets for the test (`CARGO_PKG_*`, `OUT_DIR`, ...). Build scripts declare `rerun-if-env-changed` on some of them, so an inherited value rebuilds most of the dependency tree, here and again in the next plain `cargo build`.
