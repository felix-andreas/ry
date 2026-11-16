# jit

Read this before you change the optimizing tier. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **Every JIT helper that can allocate releases its handles** (`scoped`, `crates/jit/src/runtime.rs`). A compiled loop has no backward jump where tier 0 would release them, so a miss keeps every boxed scalar alive until the call returns.
- **Compiled code writes variables back before anything that can read the environment and forgets them after anything that can write it**, warnings included. Run the `ry-jit` oracle engine (`RY_JIT=force`) and `test_tiers.rs` after changing either tier.
- **A compiled body's return leaves its assigned locals out of the frame unless something reads the frame afterwards** (`jit_frame_read_later`, `crates/jit/src/runtime.rs`): an environment, a pending environment or registered exit code. Anything new that reads a slot frame after its body returned must be added to that test (`.agents/memory/decisions/jit-ssa.md`, "A compiled return writes locals back only when something can still read the frame"; oracle case `jit__locals_read_after_return`).
- **A JIT activation's pending unwind is a collector root through `Interp::held_unwinds`**, because a deopt exit allocates before tier 0 raises it (`run`, `crates/jit/src/jit.rs`; oracle case `conditions__exiting_handler_from_compiled_code`).
- **A closure call that tier 1 inlines has no interpreter frame while it runs compiled** (`.agents/memory/decisions/jit-ssa.md`, "Tier 1 leaves compiled code through frame states"). A callee instruction that could run R code or look at the call must therefore be an exit that rebuilds the frames, never a helper call: R code run under an inlined call sees a stack without the callee. An inlined call holds no bindings either, so a formal passed on by name is read from the outermost frame (`jit_load_passed`, `crates/jit/src/runtime.rs`).
- **The bare interpreter of `crates/jit/tests/test_tiers.rs` has no base R closures.** `tryCatch`, `withCallingHandlers`, `assign` and `sys.calls` are "could not find function" there, so a tier test that uses one compares two identical errors. A shape that needs base R goes into an oracle case (`crates/oracle/cases/jit__*.R`), with each callee run on its own first, because `RY_JIT=force` compiles a caller at its second entry and inlines only a callee that already has tier-0 code and feedback.
