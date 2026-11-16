# loader

Read this before you change the package-development loader. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **A load plan is backend-neutral.** `runtime.R` is shared; `gnur.R` with `r.rs` and `ry.R` with `crates/libr/src/loader_backend.rs` are the backends' hooks. Never put R-specific logic in `loader::diff`.
- **On ry's runtime a lazy binding is a promise over a per-file source handle** (`FileSource`), and its first force reparses only the definition, so srcrefs are real (`.agents/memory/decisions/libr-rcmd.md`, "Lazy definitions keep source references").
- **Every load of a strict package is checked, on both runtimes** (`Plan::strict`). A refused plan is never committed.
- **Every reload rule keeps `test_reload_fuzz.rs` green** (a reload against a fresh load in a model). Hand-reasoned rules missed swapped writers and double wrapping.
- **A body edit must stop at `file_binds`**: equal binds backdate `package_bindings`, and `test_incremental.rs` counts the executions (`.agents/memory/decisions/libr-rcmd.md`, "A reload re-derives one file and rewrites only the bindings it changed").
- **A cold load analyzes the package on a thread while base R starts** (`loader_backend::prepare` in `libr`), and strict mode reaches the runtime as an atomic, so starting R never waits. S4 definitions are held back until first use (`.agents/memory/decisions/libr-rcmd.md`, "A load holds back S4 definitions and caches its index across processes").
- **R traps on the GNU R backend:** `environment<-` strips bytecode; `keep.source.pkgs` must not shadow `keep.source`; `.Internal(...)` evaluates its arguments, so quote a language argument; `ls()` sorts, so hot paths use `names(env)`; the attached environment keeps names the namespace dropped unless each sync removes them.
- **A path that reaches R must not carry Windows' `\\?\` prefix**, which `fs::canonicalize` adds (`loader::find_package_root` strips it). Embedded R sources keep LF endings (`.gitattributes`), because GNU R's parser rejects `\r`.
