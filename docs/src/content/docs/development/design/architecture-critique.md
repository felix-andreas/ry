---
title: Architecture critique
description: "A candid review of ry's architecture and code quality: ranked findings with evidence, what a greenfield start would build differently, where ry is below the state of the art, measured build times, and what is worth changing now"
---

This draft is a critique of ry's architecture and code quality, written by a reviewer outside the
tracks that build it. It answers three questions: what a fresh start would build differently, where
ry is below the state of the art, and what the code quality is. It complements
[The big levers](/development/design/big-levers/), which covers strategy, and
[Prior art](/development/design/prior-art/), which records what was adopted and rejected. It does
not reopen those unless the code contradicts them.

The evidence is the source tree, one cold build with `cargo build --timings`, and counts from
scripts over `src/`. No runtime benchmark was run for this review, because the review machine had no
GNU R, so every statement about run time cites a number already recorded in the repository or is
marked unverified. Costs are relative: S is one focused change, M is a few, L is a redesign of one
component, XL is a redesign that crosses crates.

## The short version

The analysis side is close to the bar it sets. The syntax tree and the HIR are flat columns, the
incremental firewalls are pinned by execution counts, and the server is steppable. Its remaining
problems are local: the checker's working state and the type representation.

The runtime side is where the architecture is weakest, and for one reason. R's semantics are
written down many times by hand (the AST evaluator, tier 0, the quickened handlers, the direct
builtin entries, the fused kernels, the tier-1 lowering, the native scalar functions, the typed
core), and speed has been bought by adding another copy for each hot shape. Each copy is fast and
fuzzed against the others, but the count is why a change is slow to make, why the interpreter crate
is 186,000 lines, and why the largest remaining lever (removing calls) has no home: tier 1 has no
R-level intermediate representation in which inlining and speculation could be expressed once.

Three smaller structural facts compound it. The data-oriented shapes that `decisions.md` settles
for the frame stack and the code header are not what the code has. The interpreter crate sits above
Polars, a TLS stack and a rasterizer, so an edit to the evaluator costs two minutes. And no workflow
gates a release on a test.

## Ranked findings

The rank weighs impact on the north star (ten times GNU R on untyped code) and on the cost of every
later change. "Now" means the change pays for itself in the current phase. "Greenfield" means it is
the right shape, but only a restart or a much later rewrite justifies it.

| # | Finding | Target shape | Cost | When |
|-:|---|---|:-:|---|
| 1 | R's semantics exist in about eight hand-kept copies, and agreement is checked by fuzzing, not by construction | One definition per operation, from which the fast path, the slow path and the tier-1 lowering are derived | XL | Now, starting with element access and arithmetic |
| 2 | Tier 1 translates one bytecode instruction to one `Action` and hands the result to Cranelift. No IR carries frame states, so only effect-free callees inline | An R-level SSA IR with frame states between bytecode and Cranelift, where inlining, speculation and environment elision are passes | XL | Now. It is the precondition of the call-removal lever |
| 3 | Version-pinned fast paths: bodies of base R 4.6.2, rlang 1.3.0 and withr 3.0.3 are matched by hash and replaced. They vanish silently on any other version | Report every pin that fails to match, test the pins against the benchmark dependency set, and retire each one as finding 2 makes it unnecessary | S, then ongoing | Now |
| 4 | `interp` is one crate of 186,273 lines, of which about 49,000 are the engine. It depends on Polars, reqwest and rustls, tiny-skia, and three regex engines. An edit costs 2m06s | An engine crate with no heavy dependency, and separate crates for the numeric ports, graphics, network services and the table binding | L | Now. Agent velocity is bounded by this loop |
| 5 | The settled data-oriented shapes are not built. The frame stack is an array of 20-field records, and code metadata lives in an R list read by position on every call | Build the packed code header and the hot/cold frame columns that `decisions.md` already specifies | L | Now. Both are on the call path |
| 6 | `heap()` and `interp()` return `&'static mut` from a global, with about 1,500 call sites, under a crate-wide `allow(unsafe_op_in_unsafe_fn)` and 5,795 `unsafe fn` | Pass the context as a pointer or a register-pinned argument. Shrink the unsafe surface to the object accessors | L | Greenfield for the full shape. Now for the aliasing in new code |
| 7 | The checker binary depends on the interpreter. Workspace feature unification turns on `loader`'s `runtime` feature for every build | Build `ry-lang` without the runtime feature, as `decisions.md` says it is built | S | Now |
| 8 | The checker's working state is 22 hash tables keyed by dense ids, and `Ty` is a pointer to a leaked enum that holds `Vec`s | Columns and bitsets indexed by `ExprId` and `BindingId`. A `u32` type id into a struct-of-arrays store, with a per-item store for types that contain variables | M, XL | Columns now. Type store greenfield |
| 9 | One analysis fact has two lookup paths chosen by `Db::one_shot()`, plus six hand-rolled caches that exist to avoid salsa's per-read cost | One per-file item tree query that returns dense arrays, with salsa only at file-interface and item-check granularity | L to XL | Now. It removes a class of CLI-against-server divergence |
| 10 | The heap runs three liveness mechanisms together: generational tracing with card marking, two old generations, and deferred reference counts with five bookkeeping sets | Tracing decides liveness. The reference count answers only "may this be modified in place" | L | Greenfield, unless a GC bug forces it |
| 11 | No workflow runs a test on push, the release workflow runs none, and 17 libFuzzer targets run nowhere. A segfault under GC torture was handled by marking it intermittent | A release job that fails on the fixture suites and a bounded fuzz pass. Treat a GC-torture crash as a blocker | S | When the broken-main phase ends. The segfault now |
| 12 | `decisions.md` is 9,832 lines and mixes built shapes with planned ones. `MEMORY.md` is about 52,000 tokens with duplicated bullets | Mark each decision built or planned, mark superseded ones, and cut `MEMORY.md` to what one read can hold | S | Now. Every agent session pays for it |

### Status

A finding listed here is closed as far as its row says, and the text below it in this page
describes the tree as the review found it.

| # | Status |
|-:|---|
| 2 | Frame states built, the R-level IR open. A point where tier 1 can leave compiled code is a frame state in `crates/ssa`, every exit shares one block, and the runtime rebuilds the interpreter frames from the cells the exit saved, the frames of inlined callees included. On top of that, tier 1 runs a call of a known closure without a frame, an environment or promises when the call passes one positional argument per formal, behind a guard on the callee's code, with arguments and results unboxed. `orders` in the applications suite runs 1.82 times as fast, and no other program moves, because tier 1 compiles 0 to 4 bodies per program. The planner in `analysis.rs` still picks one `Action` per instruction and decides the inlining, and `ssa` has no R-level value nodes or passes (`decisions.md`, "Tier 1 leaves compiled code through frame states"; `backlog.md`, "Open: JIT on against JIT off") |
| 4 | Built. `interp` is the engine (126,000 lines) between `numeric` below it and the leaf crates `stats`, `graphics`, `frames` and `services` above it, and it links no Polars, reqwest, rustls or tiny-skia. An edit to `eval()` costs 82 s on `iterate` (95 s before) and 11 s on `dev`. Cargo recompiles every crate above an edited one, so the engine and its leaves still rebuild after an evaluator edit. The cut that shortens that loop puts the evaluator core above base's builtins, and `backlog.md` holds it under "Open: the evaluator's edit loop" |
| 7 | Built. The loader's backend for ry's runtime is a module of `libr`, `loader` has no `runtime` feature, and `cargo tree -p ry-lang -i interp` finds nothing |
| 8 | Columns built. The checker's per-expression, per-pattern and per-binding facts are 24 columns and bitsets in the check arena (`crates/semantics/src/dense.rs`), and no hash table in `check.rs` is keyed by a dense id. The type store is open (`backlog.md`, "Open: the analysis core after the single lookup path") |
| 9 | Built, with per-item memos kept where each one is a firewall. No `Db::one_shot` exists and every host runs the same queries: trees live in the host's `TreeStore`, an item resolves the names it spells through one `item_links` query that also holds its interface edges, and one walk per file finds the cyclic groups. Of the four `SalsaGlobals` caches, three are removed and the fourth is a column. On the finance bench a cold check takes 0.76 to 0.80 s and 252 MB (0.72 to 0.77 s and 244 MB with two paths). In the server a body edit settles in 3 to 4 ms and a new definition in 6 to 7 ms on both builds, and a file's diagnostics settle 8 ms later after it is opened (24 ms against 16 ms). The item tree is not one per-file query of dense arrays: HIR, naming, links and check stay per item, because each memo is the firewall that keeps an edit inside one item. The caches in `infer.rs` and `types.rs` go with the type store |
| 12 | Built. Every entry in `decisions.md` opens with a status line (`built`, `planned` or `superseded`), with the exceptions to a built entry named in that line, and superseded entries a reader no longer needs are cut. `MEMORY.md` is 44 KB, about 11,000 tokens, and its open work lives in `backlog.md` |

Of the analysis core's smaller findings, the keystroke scan is closed: the server walks a file's
`source()` calls only when the edited file's `source()` literals change. The rest are in
`backlog.md` under "Open: the analysis core after the single lookup path".

## The runtime

### One operation, many implementations

Take `x[[i]]`. The AST evaluator implements it in `eval.rs`. Tier 0 implements it as a generic
handler and again as quickened handlers per observed type. A direct builtin entry implements it for
calls that skip argument matching. Tier 1 lowers it as an `Action`, and the native scalar functions
in `crates/jit/src/native.rs` implement the cases the lowering calls out to. The typed core has its
own. `interp::testing::check_element_access_agreement` exists because these can disagree.

Prior art already names the fix: the Deegen principle, one semantic description per bytecode from
which the tiers are derived, is in the adopted column. The code does the opposite, and every new
fast path widens the gap. The practical version for ry does not need a generator. Write each
operation once as an `#[inline]` Rust function over a small value abstraction, with the guard and
the slow path as explicit parts. Tier 0 calls it, and tier 1 either calls the same function or
lowers the same guard list. The AST evaluator then has no reason to exist beside tier 0 except for
`eval()` of one-off expressions, and a compile-then-run path covers those.

The version-pinned fast paths are the extreme of the same habit. `package_fast_paths.rs:43` matches
`rlang` 1.3.0 `trace_back` and `withr` 3.0.3 functions by version and body hash, and
`base_fast_paths.rs` does the same for 22 base functions of GNU R 4.6.2, with about 17 native
entries. `methods_core.rs` is a 5,991-line native reimplementation of the methods package. A user on
rlang 1.3.1 gets correct results and a different performance profile, and nothing says so. These
pins are a legitimate bridge, but they are measuring debt: each one marks a function tier 1 cannot
yet make fast, and benchmark numbers that depend on them describe one lockfile.

### Tier 1 has no place to remove calls

[The big levers](/development/design/big-levers/) puts the untyped ceiling at about three times
GNU R without call removal. The architecture explains why the lever has not moved. Tier 1 is a
template compiler: `crates/jit/src/analysis.rs:803` picks one of about 42 `Action` variants per
bytecode instruction, and `crates/ssa` emits Cranelift IR for it. The `ssa` crate mirrors
Cranelift's frontend call for call (`iconst`, `brif`, `declare_var`, `seal_all_blocks`) and runs no
pass of its own, so it is a layer without a function. Its one test and the stale comment in
`analysis.rs` ("Cranelift's frontend turns it into SSA") suggest it is unfinished.

Every runtime that made a dynamic language fast has a middle tier that knows the language: V8's
Maglev and TurboFan graphs, JavaScriptCore's DFG, LuaJIT's trace IR, and for R itself Ř's PIR. What
they share is frame states: each point that can leave optimized code records how to rebuild the
interpreter's frame. With that, inlining a closure, eliding its environment, unboxing across a call
and speculating on a callee's identity are ordinary passes. Without it, each needs a special case,
which is what the effect-free-callee inliner and the fast paths are. `decisions.md` plans frame-state
maps. The critique is that the plan should be an IR, not maps bolted to the template compiler, and
that `ssa` is the natural crate to become it.

Compile cost supports the same move. At 5 to 7 microseconds per bytecode word, `[<-.data.frame`
takes 125 ms to compile (figures from `decisions.md`). That is Cranelift doing register allocation
on unoptimized templates. A middle IR that removes guards and boxes before lowering shrinks
Cranelift's input, which is the only way to keep it as the backend at this cost.

### The settled shapes are not the built shapes

| Fact | What `decisions.md` settles | What the code has |
|---|---|---|
| Frame stack | A 40-byte hot record with cold columns beside it | `FrameStack { slots: Vec<Frame> }`, where `Frame` has about 20 fields including `on_exit`, `handlers`, `restarts` and `srcref` (`interp.rs:152-213`) |
| Code metadata | A packed code header read with one load | An R list with positional slots `SOURCE=0` to `SLOTS=10` (`bytecode.rs:1099-1113`). The register count is read through `pool_ints(code.cdr(), REGISTERS)` on each call (`bytecode.rs:6420`, `6458`, `6511`) |
| Symbol table | Dense indices | `symbols: FxHashMap<Sexp, Sexp>` and `large: FxHashSet<Sexp>` in `Heap` (`heap.rs:105`) |
| Interpreter state | Small hot context | `Interp` with about 72 fields |

Keeping bytecode metadata in R objects makes the garbage collector trace it for free, and that is
the only thing it buys. It costs a pointer chase and a type check per field on the call path, and it
makes code identity a pointer where a `u32` code id would index feedback, loop tables and compiled
entries in flat arrays. Greenfield, a code object is a row in a Rust arena and the `BCODESXP` is a
handle to it.

### The heap

The object header is good: 16 bytes, with the kind, GC bits, flags and a 16-bit reference count
packed (`object.rs:295-305`). The collector around it is three designs at once. Sticky-mark
generational tracing with card marking decides what dies young. Two old generations decide what is
scanned. Deferred reference counts are reconciled through `zero_count`, `escaped`, `escaped_lists`,
`returned`, `untracked` and `expect_dead`. Each has a measured reason in `decisions.md`, and
together they are the part of the system most likely to hold a bug that only GC torture finds. The
recorded response to one such segfault was an `intermittent = true` mark.

R needs a reference count for exactly one question, whether a vector may be modified in place.
A count that answers only that question can be imprecise in the safe direction and needs no
reconciliation. This is a judgment, not a measurement: the deferred counts also free memory early,
and removing them needs the allocation benchmark to say what that is worth.

### Global mutable state

`interp.rs:699` and `:729` define `heap() -> &'static mut Heap` and `interp() -> &'static mut
Interp` over a global `UnsafeCell`, with `#[allow(clippy::mut_from_ref)]`. There are about 1,440
calls in `interp`, 54 in `jit`, 33 in `typed` and 20 in `libr`. Two live `&mut` to the same object
are undefined behavior in Rust whether or not the program is single threaded, and LLVM is entitled
to use `noalias` on them. Nothing is known to miscompile today (unverified either way), but the
exposure grows with every inlining improvement in the compiler.

The surrounding numbers say the same thing about discipline: 3,794 `unsafe fn` in `interp`, 1,175
in `libr`, and a crate-wide `#![allow(unsafe_op_in_unsafe_fn)]` in `heap`, `interp`, `jit` and
`typed`, which turns a whole function body into an unsafe block. A C-API runtime needs raw
pointers, but it does not need them in the statistics ports. The target is a small unsafe core (the
object accessors, the stack, the boundary) under safe wrappers, with the context passed as a raw
pointer that is never turned into a long-lived `&mut`.

### The C API layer

`libr` exports 747 `extern "C"` functions and crosses the boundary through a C trampoline with
`setjmp` and `longjmp` (`native.c`, `boundary.rs`). Putting the C API first was the right call and
it shows: packages install against it. Two things would differ greenfield. The exported surface
would be generated from one table (name, signature, implementation, tested or not), so the header,
the export list and the coverage report could not drift. And `libr` would not link `loader`,
`semantics` and salsa, which it does today because the loader's runtime half lives beside its
analysis half.

## The analysis core

The analysis core is the strongest part of the repository, and its findings are about the last
factor of two.

**Checker state.** `Checker` (`check.rs:1573-1688`) holds 22 hash containers keyed by `ExprId`,
`BindingId` or `PatId`, nine of them sets. The hottest, `Environment.entries:
FxHashMap<BindingId, EnvEntry>` (`check.rs:1429`), is probed on every variable read. Call matching
allocates `vec![false; n]` per call (`check.rs:6677`, `14765`, `9618`), and `semantics` has 23
`Vec<bool>` sites. All of these keys are dense, so the performance rules in `AGENTS.md` apply
directly: columns, bitsets, and stack-inline scratch. `RecordedTypes` (`check.rs:638`) already has
the right shape.

**Type representation.** `Ty` is `&'static TypeEntry` (`types.rs:92-95`). `TyKind` embeds a `Vec`
in tuples, records, unions and named types, and `FunctionType` has six. Every `Ty::new` builds
those vectors before it probes the intern table, so a hit still allocates and frees
(`types.rs:1216-1228`). Types that contain inference variables go into the same immortal table
(`types.rs:1254`), so the server keeps the scratch types of every item it has ever checked. Roc's
`Subs` and Zig's `InternPool` are the reference: a `u32` id, children as spans in a side array, and
a per-item store that is dropped with the item.

**Two paths for one fact.** `Db::one_shot()` and `editor_facts()` (`semantics.rs:55-65`) select
between project-wide maps and per-name projections for the same lookups. `SalsaGlobals` adds four
`RefCell<FxHashMap>` caches (`semantics.rs:2799-2804`), `InferenceTable` adds two more
(`infer.rs:72`, `79`), and `types.rs:1208` has a hand-rolled recent-entry cache in front of 64
mutex shards. Each exists because a salsa read costs more than the fact, with about 14 tracked
queries per item. rust-analyzer's answer is fewer, coarser queries: one item tree per file, and
salsa only where an edit must be firewalled. That removes the dual path and the caches together,
and the caches are exactly what the performance rules call a sign of a slow design.

**Smaller findings**, each S or M and worth doing now:

| Finding | Evidence |
|---|---|
| A keystroke does work proportional to the number of package files | `did_change` calls `sync_sourced_files` (`server.rs:1589`), which canonicalizes the root, clones every file path and reads `file_source_calls` per file (`server.rs:1612-1644`). The comment at `server.rs:468` promises work proportional to open documents |
| Tag completion and two reference searches walk every syntax tree, untracked | `ide.rs:5401`, `4434`, `4692`. The existing `spelled_names` query (`ide.rs:5999`) is the model |
| "Is this file strict" has three definitions that disagree on the default | `semantics.rs:1157`, `semantics.rs:1169`, `load_index.rs:727` |
| Strings where `Symbol` exists | `StubLibrary` is nine maps keyed by `String` (`stubs.rs:28-49`). `visible_arithmetic_classes` re-merges two cloned sets per file (`semantics.rs:2887`) |
| Sync paths that log where `AGENTS.md` says panic | `server.rs:1569`, `1774`, and `unwrap_or_default` on an unreadable `DESCRIPTION` at `server.rs:689` |
| A discarded unification result | `let _ = self.table.unify(...)` at `check.rs:14983` |
| File text held three times | `semantics.rs:147`, `tree.rs:41`, `semantics.rs:215` |
| The server mirrors a tracked query by hand | `attached_by_file` in `Worker` (`server.rs:422-498`) |

**`check.rs`** is one `impl Checker` of about 364 methods over about 60 mutable fields in 19,999
lines. File size is not the problem. The problem is that every method can read and write every
field, and that ambient depth counters (`overload_probe_depth`, `operator_method_depth`,
`query_depth`) act as mode flags. The file also selects behavior by function name in 91 match arms,
including `"setnames"`, `"setorder"`, `"fifelse"` and `"as.data.table"`. Whether the directive that
ry adds no special cases for data.table covers the type checker is a question for the user. If it
does, those arms belong in stub attributes.

**The loader** keeps its session in process statics (`crates/libr/src/loader_backend.rs:177`, `261`, `279`, `281`) that
panic on a second set, so it cannot be simulated twice in one process, which the robustness contract
requires. Its index lives in `semantics`, which is what drags salsa into `libr`.

## Build and release

One cold debug build of `ry-lang`, `libr` and `rcmd` on four cores:

| Measure | Value |
|---|--:|
| Wall time | 4m03s |
| CPU time across 561 units | 881 s |
| Polars crates | 286 s (`polars-ops` 105 s, `polars-core` 66 s) |
| Cranelift and regalloc | 138 s (`cranelift-codegen` 104 s at `opt-level = 3`) |
| Workspace crates | 178 s (`interp` 43 s, `semantics` 36 s, `heap` 18 s, `table` 15 s) |
| Debug `libR.so` | 880 MB |
| Debug `ry` | 180 MB |
| Packages in `Cargo.lock` | 555 |

`decisions.md` records the loop that matters more: 2m06s per `interp` edit on the `iterate` profile
and 5m51s for release. Roc-class compile time is not reachable through profiles. It needs the
dependency graph to change:

- `interp` depends on `table`, and so on Polars, for 19 call sites in four files. Invert it: the
  table crate registers its builtins with the engine, and the engine does not know Polars exists.
- `interp` unconditionally compiles reqwest and rustls, tiny-skia, `pcre2-sys` and three regex
  engines. The engine needs none of them. The numeric and statistics ports (about 47,000 lines),
  graphics (18,000) and services (20,000) change rarely and should not recompile with the evaluator.
- `cargo tree -p ry-lang -i interp` prints `interp <- loader <- ry-lang`. The workspace setting
  `feature-unification = "workspace"` enables `loader`'s `runtime` feature everywhere, so the
  checker binary has 504 dependencies where `decisions.md` says it does not depend on the runtime.
- `libr` has 23 separate `[[test]]` binaries, each linked against the full runtime, and no fast
  linker is configured.
- `editors/zed` is a workspace member and adds about 60 wasm crates to the lockfile.

Three pipeline facts are below any reasonable bar and cheap to fix once main is meant to be green.
Every workflow is `workflow_dispatch` only, and `release.yml` runs no test. No job tests macOS or
Windows. The release is not reproducible: actions are unpinned, packages come from live CRAN, and
BOLT comes from apt. The runtime also depends on a nightly feature, `explicit_tail_calls`
(`interp.rs:26`), for tier-0 dispatch. That is a defensible bet, but it is the single point where
a compiler change can stop the build. The `rust-toolchain` comment cites a different feature and
`rust-version = "1.91"` is false for a nightly-only workspace.

## What a greenfield start would build differently

| Area | Today | Greenfield |
|---|---|---|
| Crate boundaries | Engine, base library, graphics, network and table binding in one crate. Loader split across `loader`, `semantics` and `libr` | `engine` (values, bytecode, tier 0) with no heavy dependency. `base` for builtins. Graphics, network and table as leaf crates that register into the engine. A loader that owns its index and takes a session value |
| Value representation | Tier-0 registers are `Vec<Sexp>`, always boxed | Registers typed by the bytecode compiler where the type is known, with a boxed column and a raw 64-bit column, so scalar loops never allocate before tier 1 exists. This is not NaN-boxing, which prior art rightly rejects for an API that hands out `SEXP` pointers |
| Code objects | Metadata in an R list | Rows in a Rust arena indexed by a `u32` code id, with feedback, loops and compiled entries as parallel columns |
| Semantics | One hand-written copy per tier and fast path | One definition per operation with explicit guards, consumed by every tier |
| Tiers | AST evaluator, tier 0, quickening, template JIT | Tier 0 with quickening, then one optimizing tier over an R-level IR with frame states. No AST evaluator and no template tier, which the removed baseline tier already showed does not pay |
| Heap | Tracing, two old generations and deferred counts | Non-moving generational tracing, with a saturating count used only for copy-on-write |
| Context | `&'static mut` globals | A context pointer in a pinned register, passed explicitly |
| C API | 747 hand-written exports | The same functions, declared in one table that generates the header, the export list and the coverage report |
| Language objects | `LANGSXP` trees kept for every closure body | Bytecode plus source spans, with the pairlist tree built only when code asks for it (`body()`, `substitute()`, `match.call()`). Unverified: how much of this ry already defers |
| Type store | Global interned pointers | `u32` ids, a global store for closed types and a per-item store for inference |
| Queries | About 14 tracked queries per item and two lookup paths | One item tree per file and coarse queries |
| Checker | One 364-method `impl` | A core inference walk, with frame queries, guards and call matching as components that take an explicit context |
| Release | Manual dispatch, no tests | A gate that runs the suites and a pinned, reproducible build |

Three things a restart should keep exactly as they are: the flat syntax tree, the HIR, and the
order of work that put the C API before the JIT.

## Where ry stands against its references

| Reference | What it does | Where ry is |
|---|---|---|
| rust-analyzer | Coarse queries, item trees, a read pool over snapshots, LRU on parse trees | Finer firewalls and a flatter tree than rowan. Behind on query granularity, on parallel reads (one worker serializes requests) and on bounded memory |
| Roc, Zig | Types as `u32` into struct-of-arrays stores. Arena per phase | The HIR matches. Types and checker state do not |
| V8, JavaScriptCore | Interpreter and optimizing tiers generated from or checked against one semantics. Frame states and on-stack replacement both ways | Hand-kept copies. Guards exist, an optimizing IR does not |
| LuaJIT | A tiny value word, a tiny IR, and compile times in microseconds per trace | Boxed tier-0 registers, and 125 ms to compile one large function |
| Bun and JSC startup | Snapshots and lazy parsing keep startup in milliseconds | Not measured in this review |
| Ř, FastR | Environment elision, promise inlining and speculation on call targets, in an R-specific IR | None of the three as general passes |
| Bend, vx-lang, goose | Data-oriented cores, dense ids, arenas | Met in `syntax` and `hir`. Not met in `interp`, where the call path reads hash tables and R lists |
| Roc build times | Seconds for an incremental build | Two minutes per evaluator edit |

## Code quality

The surface hygiene is very good, and better than most hand-written code bases of this size.

| Check (non-test `src`) | Result |
|---|---|
| `unwrap()` | 0 outside `libr` (9) |
| `todo!`, `unimplemented!`, `mod.rs`, `allow(dead_code)` | 0 |
| TODO and FIXME comments | 7 in the workspace |
| `let _ =` on a fallible operation | 1 (`check.rs:14983`). The rest are query warm-ups |
| Comments that cite history or milestones | None found |
| Em-dashes in source | 2 |
| `expect` | `rcmd` 78, `ry` 29, `interp` 28, `libr` 13, `semantics` 12 |
| `panic!` | `interp` 81, `heap` 19, `typed` 13 |
| `static mut` | 12 |
| `.ok()` that hides a read error | `cli.rs:590`, `1129`, `1332`, `server.rs:851`. A missing `DESCRIPTION` and an unreadable one look the same |

The guideline violations that matter are structural, and three of them hit the design review
triggers in `AGENTS.md`. Multiple sources of truth: strictness, the dual lookup path, the
hand-mirrored `attached_by_file`, and R semantics themselves. Persisted derived state: the six
caches. A design more complicated than the semantics require: the heap's bookkeeping sets.

Long functions cluster where the logic is densest. Five exceed 500 lines
(`render_type_error_message` at 598, `loader::diff` at 554, `Checker::infer` at 552,
`naming::resolve` at 540, `compatible_inner` at 515) and 30 exceed 200. A long match is fine. The
ones to split are `diff` and `analyze_project_with` (493 lines), which interleave several phases.

### Tests

The fixture suites are a real asset: about 1,900 typing cases, 1,967 syntax programs, 862 typing
scripts and 353 IDE cases. The gaps are on the runtime side and in the incremental contract.

| Gap | Evidence |
|---|---|
| The runtime has no fixture suite | `interp`, `jit`, `typed`, `heap` and `table` have none. `interp` has one test per 1,808 lines, `jit` one per 520, and `ssa` has one test in total |
| Runtime correctness depends on GNU R being installed | The oracle's 971 case files compare against a live GNU R, so a machine without it checks nothing. Committed expected outputs would let every session run them |
| Asserted incremental contracts with no execution-count pin | No test in `test_incremental.rs` mentions `type_definition`, the arithmetic-class fold, S3 generics, toggling `strict`, `global_names` or cancellation |
| Thin suites against large code | 58 `typing-tables` cases for about 3,000 lines of frame and join logic. 145 lint cases for `lints.rs` at 3,222 lines. `metadata.rs` (1,090 lines) has 6 unit tests |
| Code that rewrites what the runtime runs has no test by name | `preflight.rs` (708 lines) and `scope_oracle.rs` (502 lines). Unverified whether a scenario covers them |
| Performance budgets do not run by default | `test_perf.rs` in `semantics` and `ry` is `#[ignore]` and needs a corpus that is not in git |
| Fuzz targets that nothing runs | 17 libFuzzer targets and no workflow |

### The knowledge base

`decisions.md` has more than 400 entries in 9,832 lines. An entry does not say whether its shape is
built, and three pairs of entries contradict each other with no superseded mark (lines 7611 and
9279, 7106 and 9257, 4752 and 7417). That is how the frame stack and the code header came to be
settled on paper and absent in code without anyone noticing. `MEMORY.md` is about 52,000 tokens,
which no session reads in full, and has duplicated bullets (lines 30 and 32, 31 and 33). The
backlog holds entries the code has already resolved, for example the `BTreeMap` question on
`ItemNaming`, which uses `SortedMap` (`naming.rs:59-78`). The design index says of the R runtime
that the JIT is not built. Since agents steer by these files, a status field of `built`, `planned`
or `superseded` on each decision is the cheapest high-value change in this review.

## What to do first

1. Fix what costs every session: the feature unification (finding 7), the status of each decision
   (12), and the report for fast-path pins that miss (3).
2. Split `interp` so the evaluator recompiles in seconds (4). Everything after this gets cheaper.
3. Build the code header and the frame columns that are already designed (5).
4. Turn `ssa` into the R-level IR with frame states, and move inlining into it (2). Define element
   access and arithmetic once and have both tiers consume the definition (1).
5. In the analysis core, replace the checker's hash tables with columns (8) and fix the per-keystroke
   scan (the table in "The analysis core").

## Not verified

- No run-time measurement was taken. Every speed figure is quoted from `decisions.md`,
  `backlog.md` or [The big levers](/development/design/big-levers/).
- Whether the aliased `&'static mut` accessors miscompile anything today.
- What the deferred reference counts are worth in freed memory, which decides finding 10.
- The cost of the checker findings. They are read from the code, not profiled.
- Whether tier-0 registers hold unboxed values in any quickened path. The register file's type is
  `Vec<Sexp>`, and the handlers were sampled, not read in full.
- Startup time against Bun, and Carbon's design, were not compared.
- Counts come from regular expressions over `src/` with test modules cut off, so treat them as
  close, not exact.
