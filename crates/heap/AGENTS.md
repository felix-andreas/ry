# heap

Read this before you change the runtime's object model and garbage collector. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Root a handle nothing else roots before allocating** (`heap.root`, or the `keep` list of `Heap::allocate`). Only collection stress shows a miss: run `test_fuzz_native.rs` with a large `FUZZ_ITERS`.
- **Any pointer native code gets into a list's or character vector's elements must escape the vector** (`Heap::escape`), read-only ones included: packages permute through `DATAPTR_RO`, and per-card remembering loses a moved young element. Never inline such an accessor in `Rinternals.h`. Check a barrier change with `RY_GC_STRESS=seed:N RY_GC_VERIFY=1` (`.agents/memory/decisions/heap-gc.md`, "Read-only element pointers escape too").
- **Create a binding outside a function frame through `Heap::define`** (remove with `remove_binding`, re-parent with `set_enclosure`): they move the binding epoch that bytecode inline caches trust.
- **Environment listing order is derived, not stored** (`heap::env::Frame::listed_cells`): newest first, a closure's formals last, hashed frames in GNU R's bucket order, which depends on removals too.
