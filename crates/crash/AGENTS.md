# crash

Read this before you change the report a fatal signal prints. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **`crates/crash` owns the SIGSEGV/SIGBUS/SIGABRT handler, installed first in `ry`'s `main`.** `image.rs`'s touch recorder and Rust's stack-overflow handler are the only other SIGSEGV users, and a new one must chain as they do.
