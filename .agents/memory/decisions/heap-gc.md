# Decisions: Heap and collector

The object model, allocation, the garbage collector, and start-up and code images.

## The runtime's object model, collector and native boundary

**Status:** built, except one-step promotion and the 16 MB large-vector budget, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes, and the small `Rf_eval` subset, which "The interpreter and base R" supersedes.

This settles step 2 of `development/design/r-runtime.md`. The code is `crates/heap` (object
model and collector) and `crates/libr` (the C API, built as `libR.so`, with its C half in
`src/native.c` and its headers in `include/`).

**Two crates, split by who may see the layout.** `heap` owns the object layout, allocation, the
collector, interning and environment frames, and knows nothing about C. `libr` owns the runtime
state, the native boundary and every exported name. The interpreter and the JIT will depend on
`heap` and link into `libr`, because a package binary built by GNU R records `libR.so` as a needed
library: whatever answers that name must contain the whole runtime. `libr`'s cdylib has the soname
`libR.so`, so the dynamic linker satisfies a GNU-R-built package from ry's already loaded library
instead of searching for GNU R's.

**One header, three payload shapes, one handle type.** Every object starts with a 16-byte header:
type code, collector bits, object and ALTREP flags, GNU R's 16 general-purpose bits at GNU R's
positions (packages test `LEVELS(x) & 8` for UTF-8), a 16-bit saturating reference count, and the
attribute pairlist. Vectors store length, capacity and then their elements, so `INTEGER()` is an
interior pointer at offset 32 and every element type is aligned. Pairlists, calls, closures,
promises, symbols, binding cells and ALTREP instances are triples. Atomic vectors start zeroed (GNU
R leaves them uninitialized), because Rust code reading uninitialized memory is undefined behavior
and calloc'd pages cost nothing.

**Binding cells are heap objects.** A frame maps symbols to cells (`SexpType::Binding`, internal
code 26, which GNU R leaves unused and native code never receives). A cell's address is stable for
as long as anything references it, so compiled code can watch one and still hold it safely after
`rm()`. The base environment and base namespace keep their cells on the symbols, as GNU R keeps
base bindings in `SYMVALUE`, so a base lookup is one load. Small frames scan; above eight bindings a
frame keeps a hash index. Every frame also keeps a 64-bit symbol filter (one bit per bound symbol,
picked by a multiplicative hash of the symbol's address), so a lookup that walks through frames
which do not bind the name costs one AND per frame instead of a scan. Removing a binding leaves its
bit set, which only costs a scan. The filter took one inline binding slot (eight, not nine) to keep
the frame in the 192-byte size class; it halved `frame_cell`'s cost per `sd(x)`.

**A promise keeps its expression and carries its bytecode in the attribute slot.** A call site
that may call a closure records, in the constant after its call (`Op::CallGeneric`), whether an
argument is worth compiling: a call with no `function`, loop, `break`, `next` or `return` in it
(`compiles_as_promise`). The first time the site calls a closure, `compile_promise_arguments`
compiles each such argument into its own `BCODESXP` for the running closure's frame and stores
the argument list in that constant. Compiling at the first closure call rather than with the
body matters twice over: a site whose callee is a builtin never pays for promise code, and
eager compilation doubled the work at every nesting level (the builtin path compiles each
argument inline as well), so `f(g(h(...)))` cost time exponential in its depth. Promise code
itself has no closure to compile against, so a closure call inside it makes ordinary promises.
`compiled_promise` makes an ordinary promise over the source expression and puts the code
object in the promise's attribute slot, which GNU R never uses for a promise; `force_promise`
runs it with `bytecode::run_promise` instead of the AST evaluator. GNU R instead puts the code
object in `PRCODE` and reads the expression back through `R_PromiseExpr`, which would have sent
every reader of promise code (`substitute`, `match.call`, `sys.call`, deparse, the C API's
`PRCODE`) through an accessor. Keeping the expression in place means none of them changes. The C
API hides the slot: `ATTRIB`, `ANY_ATTRIB` and `DUPLICATE_ATTRIB` see no attributes on a
promise, `SET_PRCODE` and `SET_PRENV` drop the code (it was compiled for that expression and
that frame), and serialization writes no attributes for a promise. `run_promise` skips the entry
profile, the loop states and the optimizing tier, since promise code has no loops and no
closure of its own, and restores the srcref current when forcing began, as the AST evaluator's
`{` does.
Measured with the JIT off: a loop passing `i * 2 + 1` and `sqrt(i) - i / 3` to a closure runs
about 1.6x faster, and `sd(x)`, whose closure arguments are mostly bare symbols (a symbol stays
an ordinary promise), went from about 115.4k to 114.8k instructions.

**`switch` compiles to one table lookup and a chain of identity tests.** `SwitchArm dst src table`
maps the selector to an arm number through a constant table built at compile time (the tag
names, the first non-empty arm at or after each name, the arm per position, the default). The
number is one of the table's own integer scalars, so each arm tests it with `BranchUnlessSame`,
a pointer compare against that scalar, and jumps past the others. The arms first tested the
number with `MatchLiteral`, the `match` literal test, which read both scalars by kind and cost
9% of expr_tree's warm instructions (a `switch` on `node$kind` per node); the identity test cut
expr_tree by 11%. `BranchUnlessSame` is also the op a base call uses to check that its name
still finds the primitive, and every tier already runs it inline. A name lookup tries identity
first, because names and selectors are interned, and compares text only for a selector in
another encoding or `NA` (`eval::switch_name_matches`): GNU R matches as `pmatch` does, by
translated text and with `NA` spelled "NA". One opcode with
a jump table would dispatch in constant time, but every op in the JIT's control-flow graph has at
most one jump target, and the arm chain keeps that invariant. A `switch` whose selector is tagged, dots or missing,
whose alternatives contain `...`, or which has two defaults stays an AST call, which keeps those
rare shapes on one implementation (`switch_compiles`). Both paths read the selector through
`eval::switch_selector`, so they share its checks and warnings. Errors match GNU R's AST
`switch`, not its byte compiler: two defaults raise "duplicate 'switch' defaults", and an empty
default that runs raises "argument is missing, with no default". GNU R's byte compiler instead
says "numeric EXPR required" for the first and returns an invisible `NULL` for the second, but
GNU R runs a closure's first call through the AST, so code that depends on either behaviour
already fails there. The ledger fold
template's per-fill cost dropped from about 19.1k to 17.8k instructions.

**The dispatch loop reads only the code and registers into locals.** `step` is one large inlined
`match`, and every field of `Decoded` it destructured into a local was reloaded or spilled per
instruction under register pressure. It now copies out `code` and `registers` and reads the
constants and the cache through `decoded` where an op needs them, and it fetches the opcode with
an unchecked index, since the code words are built in process and never deserialized. The ledger
fold went from about 17.7k to 16.7k instructions per fill. Returning a `Flow` enum from `step`
instead of its current result was tried and cost about 860 instructions per fill, because the
compiler stopped keeping the loop state in registers; codegen here is fragile, so measure every
change to `step` with callgrind.

**A closure call runs one collection check for its frame.** `frame_environment` reserves room
for the environment plus a binding cell and a default promise per formal (`Heap::reserve`), then
`bind_arguments` builds them with the `_uncollected` allocators, and `args_pairlist` does the same
for the argument list, rooting only its head. This is safe because the caller keeps the argument
values alive and the defaults are reachable from the closure; anything the batch reads that is
not, such as `match_args`' result, is computed before the reservation.

**Compiled `x[[i]] <- v` updates in place before it falls back to `complex_assign`.**
`Op::SubAssign` used to hand every element assignment to `complex_assign`, which builds `*tmp*`
promises and a replacement call per assignment. `assign_element_in_place` (bytecode.rs) now writes
the element directly when the result is certain to equal what `[[<-` or `[<-` returns: a local,
unlocked, unshared, classless, non-Array vector, a scalar position inside it, and either a
non-`NULL` value under `[[` on a list or an attribute-free scalar of the vector's own atomic type.
It applies the same sharing checks as `complex_assign` (`moves::modifying`, `maybe_shared`,
`reuse_permitted`) and clears the binding's missing bits as an assignment does. Everything else,
including coercion, growth, `NULL` deletion and `x[[1]] <- x`, still takes the general path
(oracle case `assignment__element_in_place`).

**The Linux release build is profile-guided.** `just package` on x86_64 Linux runs
`tools/layout/optimize.sh`: an instrumented build of every crate linked into libR and `ry`
(dependencies included) runs `tools/layout/train.py`, the release build compiles every crate with
the merged profile, and BOLT then lays out libR from a second training run on a BOLT-instrumented
copy. Measured against the plain release build in one sitting (4-core cloud VM, medians):

| | plain | PGO | PGO + BOLT |
| --- | --- | --- | --- |
| testthat_full test phase, JIT on / off | 0.252 / 0.248 s | 0.228 / 0.219 s | 0.219 / 0.218 s |
| testthat_full process wall, JIT on | 1.68 s | 1.51 s | 1.44 s |
| testthat_full peak RSS, JIT on (the two optimized builds interleaved in a second sitting) | 82.5 MB | 76.3 MB | 76.1 MB |
| testthat_full callgrind instructions | 2.60 G | 2.22 G | 2.21 G |
| period.apply 2e6, JIT on / off | 0.70 / 0.70 s | 0.56 / 0.55 s | 0.56 / 0.57 s |
| period.apply per period: instructions, I1 misses | 185k, 5,450 | 160k, 3,360 | 160k, 3,050 |

The public suites (`tools/bench/public`, JIT on, median of 3, GNU R time over ry time as a
geometric mean, so higher is better) split into the programs the training ran and the held-out
ones the profile never saw (awfy entirely, and the odd positions of rcp and shootout):

| suite | plain | PGO | PGO + BOLT |
| --- | --- | --- | --- |
| awfy, held out (5) | 1.96 | 2.45 | 2.48 |
| rcp, trained (8) / held out (8) | 3.44 / 2.43 | 3.58 / 2.66 | 3.59 / 2.62 |
| shootout, trained (16) / held out (17) | 2.19 / 2.16 | 2.57 / 2.55 | 2.56 / 2.58 |

PGO accounts for nearly all the gain, and the held-out programs gain as much as the trained ones,
so the profile generalizes. BOLT cuts instruction-cache misses by another 9%. It changes the
public suites by between 4% faster and 3% slower, and testthat_full's process wall time by 1 to
13% faster in each of five interleaved batches of five runs, with JIT on and off. Peak RSS
matches PGO alone: BOLT moves the hot functions into a new 6.6 MB text segment and leaves the
original text mostly unread, so libR's resident pages total 15.6 MB against 16.0 MB. The file
grows from 107 MB to 136 MB, because the original text stays in it.

- **Why the earlier attempt lost.** An earlier attempt profiled only `interp`, `heap`, `libr` and `R`
  and trained on the oracle cases alone, and ran 1.4 to 1.9x slower; that slowdown was not
  reproduced, so its cause stays unconfirmed. What this pipeline found instead is a silent
  failure: passing `-Cprofile-generate`/`-Cprofile-use` in `RUSTFLAGS` changes every crate's
  `-C metadata` hash, which is part of every v0-mangled symbol and codegen-unit name, so the two
  builds name each function differently and the profile matched about 1% of the functions that
  ran. By default LLVM warns only on a hash mismatch, never on a function it finds no record for.
- **The flags go through `RUSTC_WRAPPER`** (`tools/layout/rustc-wrapper.sh`, reading
  `RY_LAYOUT_RUSTFLAGS`), applied to every rustc call with `--target`, which skips build scripts
  and proc-macros. Both builds keep the release profile's `strip` off and use their own target
  directory, because cargo does not rebuild when only the wrapper's flags change. `-p libr -p rcmd`
  and `-p ry-lang` build in separate invocations as a plain release does, because feature
  unification differs per invocation.
- **The build fails on a stale profile.** `-pgo-warn-missing-function` reports functions without a
  record; LTO drops a few functions the instrumented binary never calls (2 of 421k), so more than
  1% of the record count fails the build, and any hash mismatch fails it outright. The `repl`
  embed lookups read `EMBEDDED_RUNTIME`/`EMBEDDED_BASE` through `black_box`, because a `const None`
  folded one arm away in the profiled build and changed control flow in the embedded one.
- **BOLT needs LLVM 20 or newer.** BOLT 18 produced a libR on which oracle case
  `datatable__suite_shapes` segfaults in blake3's AVX-512 assembly, called with a garbage argument,
  and a BOLT 18 run that optimizes only a few functions writes a libR the dynamic loader rejects;
  BOLT 20 lays out the same input correctly.
  Instrumenting all of libR's 100 MB of text takes more than 14 GB, so only the functions the PGO
  profile saw run are instrumented (`tools/layout/hot_functions.py`); BOLT names a local symbol
  `name/1`, and LTO makes nearly every Rust function local. `--emit-relocs` leaves static
  relocation sections that keep the symbol table alive through `strip`, so the script removes them
  with `objcopy --remove-relocations` first.
- **Benchmarks of interpreter changes keep the plain build.** The layout build writes to
  `target/x86_64-unknown-linux-gnu/release`; `just bench` and the other bench recipes build
  `target/release` with plain `cargo build --release`, so a change is measured without a profile
  that was collected before it.
- **BOLT lays out libR only, not the `ry` binary.** `ry` runs too little of any workload for its
  layout to matter. These numbers were measured with callgrind on the nightly `just package`
  build (PGO on every crate, BOLT on libR), started with `ry run --runtime ry` and the JIT on:

  | | empty script | `library(testthat)` |
  | --- | --- | --- |
  | instructions in `ry` / in libR | 1.4M (2.1%) / 48.9M (72.6%) | 1.3M (0.5%) / 227.5M (81.0%) |
  | code pages executed in `ry` / in libR | 192 / 516 | 190 / 595 |
  | minor page faults, whole process (median of 15) | 4,005 | 6,714 |
  | wall time (median of 15) | 28.9 ms | 78.3 ms |

  On the full testthat workload, `ry` executes 0.05% of the instructions (plain release build).
  Each code page executed in `ry` costs at most one fault, so `ry`'s text causes under 5% of the
  start-up faults, and fewer than that, because the kernel maps the neighbouring pages of the
  same file on each fault. So BOLT on `ry` would win less than the 10% that would justify a
  second instrumented training run. PGO alone already cuts the pages `ry` executes at start-up
  from 328 in the plain build to 192. The nightly workflow runs this pipeline: its log shows BOLT
  instrumenting libR and then reordering the blocks of 6,313 functions. The release workflow
  installs the same `bolt-20`, but no release has been cut since the pipeline landed.

**S3 dispatch finds methods through one row per method symbol** ("S3 methods are found through
one row per method symbol" below).

**S4 dispatch keeps a polymorphic inline cache per generic.** S4
(`methods::S4Cache`) keys a site by the generic closure: valid while its `.SigArgs`, `.SigLength`
and `.AllMTable` bindings hold the values seen, with up to four entries by the signature
arguments' first classes, each holding the table's binding cell for the label and the method it
held; `setMethod` and `removeMethod` rewrite that binding, so the entry misses. Inherited methods
are cached only once `.InheritForDispatch` stored them under the label. The cache is a GC root and
is cleared when full rather than evicted. `standardGeneric` went from 14,263 to 8,688 callgrind
instructions per dispatch, JIT off (GNU R 4.3.3: 11,681). Oracle case
`s4__method_cache_invalidation` pins the invalidation shapes. NextMethod passes `.Class` from the class whose method it found and always defines `.Group` (`""` without a group, one shared vector), as GNU R does (`dispatch__next_method_class_and_group`).

**NextMethod runs from a method in slots.** Previously `do_nextmethod` reified the method's frame (through `call.env()`) on every call, and the method reified again on return because the next method's promises were counted against its frame, so one `NextMethod` level cost two environments.
- **Chosen shape.** It finds the method frame by position (`Call::frame_index`, `envir::caller_index`) and reads `.Generic`, `.Class` and the other dispatch variables through `eval::pending_name` and `crate::method_vars`, with no environment. The next method's arguments are promises over the frame's pending environment, which reify keeps as the same object, so a method that assigns to a formal is seen and a promise kept past both returns still reads the method's value. A pending `.GenericCallEnv` resolves as UseMethod's caller does (`pending_enclosure`, which shares `slot_named_like_method` with `lazy_caller`).
- **Parent frame.** As in GNU R, a closure next method's `parent.frame()` is the method's own caller, not the method's frame; a caller still in slots is named by position through `slot_owner`. A builtin next method runs in the method's frame (`apply_next_primitive`).
- **Dead promises.** `MadePromises` holds one counted reference to each promise NextMethod makes for the length of the call. A promise only that reference holds, still unforced, is dead, so it comes off `Frame::unforced` and the method returns in slots. Without the reference the test is impossible: once the callee's slots let a dead promise go, `release_promise` clears its environment and it looks like a forced one. A preserved promise, one a global handler holds and the returned value count as held.
- **Measured.** Callgrind instructions per call, release with line tables, JIT off (on gives the same within 0.1%), main / branch / GNU R 4.6.2: one level `NextMethod` 20,549 / 17,160 / 17,736; two levels 32,354 / 25,190 / 26,240; with arguments 23,113 / 19,619 / 20,736; to a builtin `[` 19,527 / 15,508 / 20,141; `Ops` group 22,786 / 21,717 / 21,784; `[.factor` 24,302 / 18,889 / 28,667. Slot frames turned into environments in 2,000 calls: 4,129 to 0 beyond start-up. `dispatch__next_method_from_slots` pins the semantics.

**The collector: non-moving mark-sweep with sticky mark bits.** A surviving object keeps its mark,
so outside a collection "marked" means "old". A minor collection marks only from the roots, the
remembered set and the escaped set, and sweeps only the pages allocated into since the last
collection, so its cost is proportional to young data, not the heap. A major collection clears the
marks first and sweeps every page. The write barrier records an old parent that receives a young
child. A vector whose element pointer native code took, read-only or writable (`STRING_PTR_RO`,
`DATAPTR_RO` on a list), bypasses the barrier while the native call runs, so it joins the escaped
set and every collection rescans it ("Read-only element pointers escape too"). Weak
references follow ephemeron rules, and the `CHARSXP` intern table is weak, with minor collections
checking only the young strings. GNU R ages objects through several generations; one promotion
after one survival is simpler and costs some premature tenuring, which profiling can revisit
without changing the API.

**Small objects live in bitmap pages** (`crates/heap/src/space.rs`). Objects up to 2 KiB come from
one reserved address range (64 GiB, halved on failure down to 1 GiB for a capped address space such
as valgrind's) cut into 64 KiB pages of 17 size classes, the 40-byte class being a triple. Each page
keeps its mark, allocated and finalizable bits in side bitmaps, one bit per slot, stored as
struct-of-arrays per page. Reasons:
- Marking an old object tests a dense bit instead of loading its header, and sweeping is word
  operations (`dead = allocated & !marks`, then `allocated = marks`) that never read a dead object.
  Only a dead environment is visited, through the finalizable bit, to drop its Rust-side frame.
- Allocation claims a run of consecutive free slots from the allocated bitmap and bumps a pointer
  through it, so there is no free list, no young list and no per-object free. A collection gives
  the unhanded rest of each run back first ("The young generation allocates by bumping a
  cursor").
- Empty pages return to a pool any class can take, so a phase heavy in one size does not strand
  memory in its class.
- Debug builds poison dead slots and set their kind byte to `FREESXP`. Release builds do not touch
  them, so a stale handle is caught only in debug.
- Large objects are allocated one by one with the global allocator and keep a header mark bit.
Measured on a closure-call microbenchmark under cachegrind: 4% fewer instructions and 46% fewer L1
data misses than the free-list pages. Full collections on the loaded dsbench heap take about 35 ms
against GNU R's 130 ms.

**libR keeps freed memory until `gc()`.** mimalloc's purge delay is 1e9 ms (`crates/libr/src/libr.rs`),
because returning pages that stay free for 10 ms made the kernel zero them again on reuse, which was
15% of the benchmark tests' profile. The delay is huge rather than -1 (never) because mimalloc only
schedules a purge when the delay is non-negative, and the forced `mi_collect(true)` that `gc()` runs
(`frame::release_worker_memory`) carries out scheduled purges. With -1, resident memory stayed at its
peak for the whole session; with 1e9, an explicit `gc()` returns free pages as GNU R's does, which
took resident memory after the base benchmark's setup from 91 MB to 76 MB and its setup peak to
135 MB against GNU R's 136. The price is a page refault on the first kernel after a `gc()`, which
GNU R pays too (`cumsum` at 1e6 went from about 2 ms to 5 ms in the benchmark, which calls `gc()`
before each kernel).

**Roots.** The `PROTECT` stack, the precious multiset, the handle stack, registered globals, every
interned symbol, the weak-reference list, and any `RootSource` an interpreter or JIT registers. A
function that allocates while it holds a handle that nothing else roots must root it first, and collection stress is what
exposes a miss.

**Root scanning is proportional to new roots on a minor collection.** The four root stacks (globals,
`PROTECT`, handles, handouts) are `RootStack`s that remember how far the last collection scanned. A
minor collection marks only the entries above that watermark, because an entry below it was marked
by an earlier collection and a mark stays until the next major one; popping or removing entries
lowers the watermark. Permanent objects go through `Heap::add_global_root` (primitives do), and
interned symbols are heap roots already, so a `RootSource` traces only state that changes, such as
frames and handlers. Before this, unserialize held 15k handles while lazy-loading base, and every
minor collection rescanned them along with every symbol and builtin, so `RY_GC_STRESS=startup` on
an empty script took over 300 s in a debug build. It now takes about 7 s, with the `heap` crate at
`opt-level = 2` in the dev profile. Under stress, every eighth collection is major while the heap
is small, and past that one per sixteenth of the live objects, which keeps the total linear.

**Peak memory comes from what the heap keeps alive, not from the allocator.** At 1e6 rows the base
kernels' peak RSS was 1.1 to 1.6 times GNU R's (`format` was the 1.6), and three causes accounted for it:
- **Leaves skip the mark stack.** `Heap::mark` sets a leaf's mark bit without pushing it, where a
  leaf is a CHARSXP or atomic vector with no attributes that is not ALTREP. Otherwise a
  million-element character vector pushes every CHARSXP, which grows the mark stack to 8 MB, and
  the stack keeps its capacity. Every non-leaf object's attrib slot is still traced, which the
  interpreter's compiled promises depend on.
- **The young budget for large vectors is 16 MB, not 64 MB.** It counts only large vectors
  allocated since the last collection, so 64 MB let dead temporaries pile up about 50 MB above
  GNU R's peak, whose 64 MB `R_VSIZE` also counts live data. The setup of
  `tools/bench/internal/base/kernels.R` went from 166 MB to 148 MB (GNU R 134 MB) with no slower kernel.
- **Builtins write into the result vector.** `sample.int` and the random deviates built Rust
  vectors and copied them, holding about 10 MB of scratch at the peak.
- **A large vector beyond memory plus swap is refused before it is requested.** mimalloc reserves
  address space without committing it, so `numeric(1e10)` succeeded and would have failed only when
  touched, where GNU R reports "cannot allocate vector of size 74.5 Gb" because the kernel refuses
  the request. On Linux `Heap` reads physical memory plus swap once (`memory_ceiling`) and
  returns `CannotAllocate` for a larger single object, unless `vm.overcommit_memory` is 1, where the
  kernel grants GNU R any request too. macOS and Windows have no bound, since GNU R's oversized
  requests succeed there.
- **Rejected:** mapping large vector bodies with `mmap` and unmapping them once a collection leaves
  them unused did not lower any peak (rowSums rose from 168 to 173 MB), and
  `MIMALLOC_PURGE_DELAY=0` lowered the setup peak by only 4 MB. The same peaks with glibc
  `malloc` in libR confirm that the allocator's retention is not the gap.
- **Not heap:** ry started about 13 MB above GNU R, all in clean, file-backed pages; anonymous
  memory is at parity. Releasing the relocation tables and the preflight's code pages cut the gap
  to about 3 MB ("Start-up memory: release relocation tables and preflight code pages").

**Removing a binding is O(1) amortized** (`crates/heap/src/env.rs`). The slot stays in place with a
sentinel symbol, the hash index drops its key, and the frame compacts once half its slots are
removed. Removing by shifting kept insertion order but rewrote every later index entry, so `rm()`
over a large frame was quadratic.

**The handle scope roots what the API hands out, not every allocation, and only recently.** While a
native call is on the stack, a handle an API function returns to native code goes to the heap's
handouts stack (`Heap::hand_out`), which the native call truncates on return. Rooting results
rather than allocations matters for `Rf_eval` called from native code: the evaluator's own
temporaries stay collectable, and only the value native code receives is kept.
- Previously every handout stayed rooted until the call returned, so a native loop that allocated
  and dropped temporaries kept all of them, which GNU R would have collected. A call now keeps only
  its most recent `HANDOUT_WINDOW` (1,024) to twice that; once it holds twice the window, the older
  half is dropped in one `drain`, so the cost per handout stays constant and memory stays bounded.
  The invariant this relies on: ry allocates on behalf of native code only where GNU R does, so
  code correct under GNU R, which protects what it keeps across an allocation, never needs an
  older handout. Two kinds of result break that and must not enter the window: a view ry builds
  of something GNU R stores in the argument (`FRAME`'s pairlist) is `Pinned` to the call's handle
  stack, and a field read is `Reachable` and takes no slot at all, which also keeps real
  temporaries in the window longer.
- Only the outermost API function hands out (`Runtime::api_depth`). A nested one returns to ry's own
  code, which the outer function's handle scope covers, and pushing it to the window would let it
  displace native code's recent handouts.
- A result reachable through the argument it was read from (`CAR`, `CDR`, `TAG`, the `CADR` family,
  `ENCLOS`, `R_ParentEnv`, the closure and promise fields, the external pointer and ALTREP fields,
  `PRINTNAME`) is `Reachable` and takes no handle.
- A C context saves and restores `api_depth` and the window's base with the `PROTECT` depth, so a
  jump that leaves an API function without returning through it cannot leave later handouts on
  the unbounded handle stack.
- The previous design kept one handle stack for both the Rust side and native code. Handouts got
  their own stack because the window drops entries from the middle of a call's region, which must
  never touch handles ry's own code pushed.

**Accessors are inline in `Rinternals.h` and mirrored in libR.** The header reads the kind byte
(offset 0), the ALTREP flag (offset 2, bit 1), a vector's length (offset 16), its elements
(offset 32) and a cell's three fields (offsets 16, 24, 32) directly, and calls the exported
function for anything else. `heap/src/object.rs` pins those offsets with const assertions. The
exported functions take identical fast paths before `enter`, so binaries built against GNU R's
headers skip the boundary too. A writable `DATAPTR` into a list or character vector stays out of
line, because it makes the vector escape.

**Each native call owns a `PROTECT` region.** `UNPROTECT` below the region's base is an error, in GNU
R's words, rather than silently unprotecting the caller's objects, which GNU R allows. An imbalance
on return is a warning in GNU R's words, and the region is cut back, so a leak never accumulates.

**The boundary rule.** Inside Rust every non-local exit is an `Err(Unwind)`. Native code ry calls
runs in a C trampoline with a jump target (`ry_call_native` and the per-signature ALTREP, finalizer
and init trampolines). An API function runs its body under `enter`, which catches the `Err` and any
panic, drops everything, and only then calls `ry_jump`. The frames the jump skips are `enter`'s and
the exported function's, and they hold nothing with a destructor at that point, which is the
condition (plain old frames) under which skipping a Rust frame with `longjmp` is defined. Callbacks
that native code passes to the API (`R_UnwindProtect`, `R_ToplevelExec`, `R_ExecWithCleanup`,
`R_tryCatch`, `R_mapAttrib`) run from C, so a jump out of them never skips a Rust frame, and a C++
exception thrown by an `R_UnwindProtect` cleanup unwinds only C frames compiled with
`-fexceptions`. A jump restores the `PROTECT` depth and the `R_alloc` watermark of its context, as
GNU R does.

**Exporting C symbols from a Rust cdylib.** rustc's version script exports only Rust-defined
symbols, so `build.rs` writes a second one from the `RY_EXPORT` markers in `native.c`, which lld
merges. The variadic functions (`Rf_error`, `Rprintf` and their family) must be C, because stable
Rust cannot define C variadic functions. GNU ld rejects a second version script, which matters once
a platform without lld is targeted.

**Finalizers run at safe points, not inside a collection.** A collection happens deep inside an
allocation, where native code must not run, so ready finalizers queue and run when a native call
returns, on `R_gc`, and on `R_RunPendingFinalizers`, each as if at top level.

**Evaluation is a deliberately small subset until the interpreter exists.** `Rf_eval` evaluates
constants, symbols, promises, and calls to a handful of primitives (`.Call`, `.External`, `stop`,
`warning`, `dyn.load`, `list`, `identity`, `invisible`, `quote`), which is what native code needs to
drive native code. Calling a closure is an error that says so.

**ry presents the version of the base R it runs, with a revision of at least 89194** (the lowest
that keeps data.table on its modern branches), and its native encoding is UTF-8. See "The presented
R version comes from the base package the runtime runs" below.

**Considered and rejected.** Source compatibility alone saves nothing, because the opaque `SEXP`
already forces every accessor to be a real function. A moving nursery cannot coexist with raw
pointers held by native code. Rooting every allocation made inside a native call (instead of every
handed-out result) would make `Rf_eval` loops from native code retain all interpreter garbage.
Defining the C wrappers for every API function in C (instead of `enter`) would duplicate every
signature in two languages for no safety gain.

## An escape lasts until the outermost native call returns

**Status:** built.

- **Previous shape.** A vector whose writable element pointer native code took (`STRING_PTR`, `DATAPTR`, `VECTOR_PTR` on a character vector or list) joined the escaped set for good, and every minor collection rescanned all of its elements. data.table calls `STRING_PTR` on its input columns, so after one `unique(x)` every later minor collection reread the 3e7 string elements of the groupby table, each read a cache miss on a CHARSXP header. `shift(x$v1)` went from 0.024 s to 0.14 s once any such call had run.
- **Target shape.** `Heap::retire_escaped` runs when the outermost native call returns (`crates/libr/src/boundary.rs`). Native code may store through the pointer only while that call runs, so each old escaped vector that may hold a younger child moves to the remembered set ("Read-only element pointers escape too" says which): the next minor collection rescans it once, then it is an ordinary object. GNU R's collector never sees those stores at all, so this is strictly safer than GNU R. While a call still runs, an old escaped character vector is not rescanned: a minor collection marks every young CHARSXP instead, which is sound because a character vector holds only CHARSXPs and they point at nothing. A young CHARSXP that is garbage then lives until the next major collection.
- **Minor collection budget.** The young budget is the larger of `GcPolicy::young_limit` (8 MB) and half the old small objects, as GNU R's cons-cell trigger grows with its heap ("Collections run on GNU R's schedule" says why old large vectors do not count).
- **Impact.** The single-loop data.table helpers run at GNU R's speed in a session that has called any data.table function, and `rbindlist` of two 1e7-row tables drops from 1.75 s to 1.26 s; `setkey`, `dcast` and `unique` now match or beat GNU R. `crates/heap/tests/test_gc.rs` pins that stores through an escaped pointer survive minor collections, before and after the escape is retired.

## Read-only element pointers escape too

**Status:** built.

- **Previous shape.** Only a writable pointer (`STRING_PTR`, `DATAPTR`, `VECTOR_PTR`) into a list or character vector made it escape. `DATAPTR_RO`, `DATAPTR_OR_NULL`, `STRING_PTR_RO`, `VECTOR_PTR_RO` and an ALTREP class's `Dataptr_or_null` (or read-only `Dataptr`) over handle storage did not, and the inline `DATAPTR_RO` and `STRING_PTR_RO` in `Rinternals.h` returned the data pointer without calling libR at all. The source of truth for "native code can see these elements" was split between libR and the header, and the header half could not tell the heap.
- **What broke.** data.table's `setcolorder` and `setorder` permute columns with `memcpy` through `DATAPTR_RO`. The heap remembers a large list per 128-element card, so moving a young column from a dirty card to a clean one hid it from the next minor collection, which freed it: `dt[, (missing) := 0]; setcolorder(dt, ...)` then failed with "Column N is NULL; malformed data.table". GNU R remembers whole objects, so a permutation inside one vector is harmless there; card marking is what makes it unsafe in ry, so the fix may not rely on the read-only contract.
- **Target shape.** Every pointer into a handle vector's elements reaches libR (`Rinternals.h` keeps an inline `DATAPTR` and `DATAPTR_RO` for atomic vectors only) and escapes the vector through `Heap::escape(vector, writable)`. A writable escape of an old vector remembers it at once. At retire, an old vector is dirtied and remembered only when it may hold a younger child: it is remembered, or a collection ran during the escape (`Heap::escaped_collected`), which can age its children without remembering it because all of an escaped vector's children are marked. Any other old vector held only old children, and a permutation cannot change that.
- **Counts of an escaped list.** Each escaped list that counts references gets an `EscapedList` entry, from which `retire_escaped` settles the counts native stores bypassed. A writable escape snapshots the elements; the retirement counts what the list holds and releases what the snapshot names. A counted element store in between (`SET_VECTOR_ELT`, `copy_elements`) counts the new value at once and adds it to the snapshot, and releases nothing (`Heap::count_escaped_store`): every value the counts ever counted into the list is in the snapshot, so the retirement releases it exactly once. Releasing the old value as usual would release it twice, once at the store and once at the retirement, and a reconcile could then free it while another list held it. Collections mark every snapshot entry until the retirement, because an entry may have left the list. Attribute stores count as usual, because the retirement settles elements only. A read-only escape records an order-independent digest of the elements (a wrapping sum of hashed pointers) instead of a copy: a permutation leaves both the counts and the digest unchanged, and any other store through the read-only pointer changes the digest, after which every element is counted once more (counts err high: a leak and extra copies, never a free). The first counted change under a digest (an element store or `resize_within_capacity`) does that check and takes the snapshot (`Heap::escaped_snapshot`), because a counted store releases what the slot holds, which native code may have put there uncounted; following the digest through the store instead released such a value and freed it while another list held it. A resize of a list with a snapshot releases no dropped slot, since the snapshot releases them at the retirement. `move_elements` copies from an escaped list, and a list that dies with an entry pending (`release_swept_lists`) releases what the entry counted, not what it holds. Rejected: deferring all counts of an escaped list to the retirement, which would show a value stored into it as unshared to copy-on-modify while native code runs; patching the snapshot at the stored index, which is wrong after a raw permutation moves what the index held; and snapshotting read-only escapes too, which copies a whole list on every `DATAPTR_RO` (vctrs and purrr take one per call on large lists). Entry lookup is a linear scan of the pending entries, because the eight header GC bits are all taken, so no per-object flag can mark a list that has one.
- **Rejected: dirty every escaped vector at retire.** The user's proposed shape. It is correct, but data.table takes read-only pointers on every column it reads, so each later minor collection would rescan them all; the conditional rule costs nothing for those.
- **Impact.** Correctness: every element pointer native code takes inside a native call is covered, and `RY_GC_VERIFY=1` checks the invariant before each non-major collection. Not covered: a pointer kept past the outermost native call (across two `.Call`s, or taken by an embedding host outside any call) that later moves elements, and ALTREP storage outside `data1` and `data2`. A package compiled against older headers keeps the inline pointer and must be reinstalled; `dyn.load` refuses it ("A package ry compiled records its headers' binary interface"). Tests: `element_pointers_keep_permuted_young_elements_alive` (`crates/libr/tests/test_native.rs`, all six accessors), `element_pointer_permutations_keep_the_write_barrier` (`crates/heap/tests/test_gc.rs`, seeded, with exact reference counts), `datatable__setcolorder_young_columns` and `datatable__setorder_young_strings` (oracle regression cases), and `native_cases_keep_the_write_barrier_under_seeded_gc_torture` under `RY_GC_STRESS=seed:N`.

## data.table's C runs at or below GNU R's time: barrier, pacing and string kernels

**Status:** built.

- **Previous shape.** Inside `tools/bench/internal/table/ops.R` several helpers trailed GNU R although they ran faster alone. The cost was the collector inside the timed region. Native code fills a fresh character vector one `SET_STRING_ELT` at a time, and each store looked up the parent's mark bit. `Rf_duplicate` of a character vector or list paid one barrier and one refcount check per element. Large vectors counted against the 8 MB nursery, so `fread`, which allocates every column before filling any, ran a minor collection per column, and each rescanned the 1e6-element string columns already allocated. After every major collection the old-generation limit fell straight to twice what survived, so the explicit `gc()` between two questions bought a full collection in the middle of the next one (31 ms of `rbindlist`'s 90 ms at 1e6 rows).
- **Target shape.** All of it lives in `crates/heap/src/heap.rs`:
  - The write barrier reads the parent's header OLD bit, so a store into a young vector costs one load and one compare ("The young generation allocates by bumping a cursor").
  - `Heap::copy_elements` copies a range between vectors with one barrier decision for the whole range, and `Rf_duplicate` uses it.
  - Large vectors have their own young budget, `GcPolicy::young_large_limit`: 16 MB, or half the old generation if that is larger ("Peak memory comes from what the heap keeps alive" says why not GNU R's 64 MB).
  - The major-collection limit shrinks by a fifth at most per collection instead of falling to what survived ("Collections run on GNU R's schedule").
- **Kernels.** `duplicated`/`unique` without incomparables hash a plain key per type, and for an all-ASCII character vector they hash the CHARSXP pointer, because interning makes equal ASCII text one object (`crates/interp/src/sort.rs`). `strsplit` with `fixed = TRUE` builds its `memchr::memmem` finder once per pattern and collects byte ranges instead of `String`s (`crates/interp/src/strings.rs`). `char_str` validates ASCII-flagged strings strictly, which runs a word at a time, but only as a hint: native code can set any gp bit through `SETLEVELS`, so the flag never licenses `from_utf8_unchecked`. The heap's own `flag_bits::VALID_UTF8`, set once when a string is interned and out of C code's reach, does license it ("Package code that dominates a workload runs natively: rlang's trace_back").
- **Impact.** At 1e7 rows every helper in `ops.R` is as fast as on GNU R or faster (table in `development/design/r-runtime.md`, section 7). `tstrsplit` takes 3.71 s against 5.73 s, `fread` 2.36 s against 3.16 s, and `rbindlist` 0.71 s against 0.90 s. The first `dcast` of a session no longer warms up slowly: 0.028 s against 0.047 s at 1e6 rows. These timings were taken with a 64 MB large-vector budget; at 16 MB, `fread` of a 1e7-row, five-column CSV, `tstrsplit` and `rbindlist` measured the same as at 64 MB (fread 1.31 s against GNU R's 1.27 s on one thread). The cost is memory: up to the budget in dead large vectors, and a collection limit that shrinks over eight collections. `RY_GC_VERIFY=1` passes the oracle suite, and `crates/oracle/cases/vectors__duplicated_fast_paths.R` pins the typed `duplicated` keys and fixed `strsplit` against GNU R.

## What keeps a dropped object alive: registers, handles, caches and dying finalizers

**Status:** built.

GNU R finalizes an object at the first full `gc()` after its last reference goes, and R6, lgr's buffered appenders and processx all depend on that timing. Four places in ry's runtime held a reference GNU R does not have.

- **The register file and the handle stack.** Both are roots. Tier-0 bytecode released handles only on a backward jump and never cleared a statement's temporaries, so a straight-line body kept everything it had touched until it returned. A statement of a `{` now ends with `Op::EndStatement dst start end`, which clears the block's result register and the statement's temporaries (registers are allocated in stack order, so they are exactly `start..end`), and `bytecode::execute_code` releases the activation's handles after every instruction. That is safe because between instructions every live value sits in a register. The AST interpreter's `do_begin` roots only the block's own value on the way out for the same reason. Compiled code shares the same register window, so the JIT lowers the op to inline `NULL` stores (`Action::Clear`), kills those registers in its liveness, and leaves handle release to tier 0 and the activation's exit. `evaluation__finalizer_run_points` pins the timing, and the bytecode, closure, promise and S3 cases give the same output under `gctorture(TRUE)`.
- **Bytecode inline caches.** A cache holds the environment a lookup started from and the cell it found, so a body shared between closures (R6's `clone()` rebinds copied methods with `environment<-`) kept the original object's environment alive. The cache list carries the heap flag `WEAK_PAIRS` (`Heap::mark_weak_pairs`): the marker does not trace its elements, and after weak references settle `clear_dead_weak_pairs` sets a pair whose key (the enclosure) or value (the cell) is unmarked to `NULL`, which is a cache miss, releasing the counts the barriered fill took. Minor collections need no special case: an old list that gained a young key is in the remembered set and is traced, and one that did not can only point at old objects. Emptying every cache at each collection was rejected, because each site then pays a full lookup after every collection.
- **Tier-1 code.** A compiled body embeds its enclosure and the cells its lookups speculated on, and the body outlives any one closure (R6 gives every object's methods the same body). The TIER1 external pointer therefore protects a weak reference keyed on the enclosure, whose value lists what the machine code embeds. While the enclosure lives everything stays reachable; once it dies, `compiled_for` frees the code and clears TIER1, which is safe because a running activation keeps its own enclosure reachable.
- **Removed binding cells.** A cell can outlive its frame entry while a cache holds it, so `remove_binding` sets the cell's value to the unbound marker and a stale cell reaches nothing.
- **Dying finalizers.** Marking a dying weak reference's key, value and finalizer can make another weak reference's key reachable after the ephemeron fixpoint ended, and that reference's value and finalizer were then swept while it stayed registered (processx's exit finalizers ran freed closures). `process_weak_refs` now takes every dying reference off the list before marking any of them, then queues and marks them all. What they reach can no longer be a registered reference's key that missed the fixpoint, because every reference still registered has a key the fixpoint already marked, as GNU R's `CheckFinalizers` does.
- **Visibility of a variable read.** Reading a variable can force a promise, whose expression then decides whether the value prints (`withCallingHandlers(f(), ...)` stays invisible when `f()` is). Compiled code sets the visible flag before the load, not after it, and the analysis treats the flag as unknown after a load that may force.
- **Expression vectors.** `eval()` of an expression vector (`eval::eval_all`, which is how testthat runs a test file) releases each element's handles before the next element runs, exactly as `{` does, so a `gc()` in one top-level statement of a test file finalizes what an earlier statement dropped (lgr's `AppenderBuffer` flushes on destruction this way).
- **Impact:** finalizer timing matches GNU R in the oracle cases `evaluation__block_finalizers`, `evaluation__expression_vector_finalizers` and `evaluation__finalizers_in_compiled_code`. The cost is one instruction per block statement in tier 0 and a handle-stack truncation per instruction. No effect on static analysis.
- A compiled activation's pending unwind is traced through `Interp::held_unwinds`. A deoptimization exit boxes registers and flushes slots, which allocates, before tier 0 raises the unwind, and the result of an exiting handler is reachable from nothing else in that window. `run` in `crates/jit/src/jit.rs` pushes the address of the activation's field around the compiled entry, so `park` and the take stay plain moves. The oracle case `conditions__exiting_handler_from_compiled_code` pins it under the `ry-jit-gctorture` engine.
- A JIT helper that stores its result in the register window also releases the handles it made (`scoped` in `crates/jit/src/runtime.rs`). Interpreter constructors root their results, and tier 0 releases those roots at a backward jump, which a compiled loop does not have.

## libR commits mimalloc arenas lazily, and handle vectors are not zeroed before their fill

**Status:** built.

- **Previous shape.** `libR.so` declares its own mimalloc, separate from the `ry` binary's, and only the binary turned off eager arena commit. In `libR` an eagerly committed arena is backed by transparent huge pages, so after earlier work had purged parts of it, the next large vector faulted them back in and the kernel zeroed and compacted whole huge pages. Every `Rf_allocVector` also zeroed the whole vector, which GNU R does not do for numbers, and a character vector or list was zeroed and then filled.
- **Target shape.** `libR` sets `mi_option_arena_eager_commit` to 0 from an `.init_array` constructor (`crates/libr/src/libr.rs`). `Rf_allocVector` allocates numeric vectors uninitialized through `Heap::alloc_vector_uninit`, and `Heap::alloc_vector` zeroes only the header of a vector it fills with handles. Purging stays on (`MIMALLOC_PURGE_DELAY=-1` shaves a little more, but resident memory would only grow).
- **Impact.** In ops.R's sequence at 1e7 rows, `rbindlist`'s slow first calls dropped from 2 to 6 s to about 1.3 s (GNU R: 0.9 to 2.1 s). Run-to-run noise on the other questions is larger than any change. Package loading gains too: it plans on every core, every planning thread's arena paid the zeroing, and the plan of a 326,000-line package went from 680-990 ms to 330-440 ms.

## The young generation allocates by bumping a cursor

**Status:** built.

- **Previous shape.** Each small allocation went through `allocate_with`, a non-inlined call that ran the collection check, claimed a bitmap word on a miss and took the next free bit by trailing-zero count, then wrote the header. The write barrier looked up the parent's mark bit in its page's side bitmap and cached the last unmarked parent in `Heap::young_parent`. `sd` on 20 doubles cost 121.5k instructions per call against GNU R's 70.6k, and a loop of `cons` + `root` + a scalar vector cost 298 instructions per iteration, 191 of them in `allocate_with`.
- **Target shape** (`crates/heap/src/space.rs`, `heap.rs`):
  - Each size class has a cursor `{next, end, page, next_slot}` over a run of consecutive free slots, claimed from the allocated bitmap by `claim_run` (marked as allocated up front, so a slot never handed out reads as unmarked and is swept like a dead one). The fast path is `next != end`, then a bump. `refill` is cold and takes the next run in the current page, then an available page, an empty page, or a new one. A collection first calls `release_cursors`, which clears the unhanded tail of each run so it is free, not dead.
  - `Heap::allocate` is `#[inline(always)]`: `young_bytes + size <= small_trigger`, the bump, word-wise zeroing of the requested prefix and the header store. The collection check and the `keep` slice live on the cold `allocate_small_slow`, so a caller's `keep` array costs nothing on the fast path.
  - `small_trigger` is derived state: `young_limit.max(old_bytes / 2)`, or 0 under gc stress, refreshed in `Heap::new`, after each collection, and in `update_policy`, which replaced `policy_mut` so nothing can change the policy without refreshing it. Recomputing it per allocation was two loads, a shift and a max on the hottest path; it changes only where it is refreshed.
  - `gc_bits::OLD` is set in the header the first time an object is marked and never cleared. The barrier's fast path is `parent & (OLD | REMEMBERED) != OLD`, one load from a header the store is about to touch anyway, so `young_parent` and its side-bitmap lookup are gone. Since a non-moving heap never promotes by copying, "marked once" is exactly "old". `verify_barrier` cross-checks OLD against the mark bit.
  - `Heap::reserve(bytes, keep)` runs one collection check for a batch, and the `_uncollected` allocators (`alloc_triple_uncollected`, `cons_uncollected`, `alloc_environment_uncollected`, `define_formals_uncollected`, `alloc_compiled_promise_uncollected`) never collect, so a closure call that knows its counts after argument matching pays one check and roots nothing while it builds the frame. `TRIPLE_BYTES` and `ENVIRONMENT_BYTES` size the request. The reservation is a budget, not memory: going past it only moves the next collection earlier.
- **Pools per object type.** The data-oriented mandate asks for a pool per object type, and the size classes already are that: every triple-shaped type (pairlist cells, promises, closures, binding cells, language objects) has one 40-byte layout and shares the 40-byte class, environments are the only 192-byte objects, and scalar vectors fill the 32- and 48-byte classes. Splitting the triple class by `SEXPTYPE` would give each type its own cursor and pages without changing any layout, and it would cost locality: a call allocates promises, cells and conses interleaved, and one cursor puts them in consecutive slots.
- **Impact.** The microbenchmark loop costs 116 instructions per iteration (298 before): the inlined `cons` about 54, a scalar `alloc_vector` 46 plus a 12-instruction zeroing. `sd` costs 110.3k instructions per call (121.5k before), measured as the callgrind difference between 150 and 50 calls on the profiling build. `root`, `allocate_with` and `write_barrier_slow` dropped out of the profile; `refill` runs about eight times per call for about 1k instructions. What is left of the gap to GNU R is the interpreter's call path (`frame_cell`, `Frame::insert`, argument matching), which the batch `reserve` lets it shrink further. `crates/heap/tests/test_gc.rs` pins that bump runs refill the holes a minor collection leaves and that a reserved batch survives the collections after it.

## The string intern table is a packed open-addressed array

**Status:** built.

- **Previous shape.** `heap::strings` kept interned `CHARSXP`s in a `hashbrown` table of pointers, and every probe compared the candidate's bytes through its pointer, so a lookup cost one cache miss per candidate into objects scattered over the heap. With 1e7 live strings `formatC` went from 5.9 s to 7.8 s while GNU R stayed level. `values::StringMemo` hid this per call in `substr` and `formatC` only. A `young: Vec<Sexp>` listed every string interned since the last collection, so a minor collection could remove the dead ones, which cost 8 bytes per new string at the peak.
- **Shape** (`crates/heap/src/strings.rs`). One `Vec<u64>` of slots, linear probing over a power-of-two length. A slot packs the object's address divided by 8 (44 bits, so 47-bit user addresses fit) with 20 bits of its hash, so a probe compares the tag inside the slot and touches the object only on a tag match, about once per million wrong candidates. Deletion leaves tombstones. The table grows at 7/8 load counting tombstones and rebuilds to between 5/16 and 5/8 load, which keeps probe sequences short and its memory near GNU R's (whose table resizes at 0.85).
- **Which young strings to check.** The small-object space keeps an `interned` bitmap beside its mark bits (`Space::set_interned`). A minor collection removes `interned & !marks` on the pages it touched, and the sweep clears the bit with the slot, so no list of new strings is kept. Strings too large for the space (over the largest size class) still go on `young_large`, which is short.
- **A new string costs one probe.** `StringTable::find_or_vacancy` returns either the interned string or a `Vacancy`, which is the first tombstone on the probe path or else the empty slot that ended it. `insert_at` fills that slot. The `CHARSXP` is allocated between the probe and the fill, and that allocation may collect, so the vacancy carries the table's `generation`. Every fill and every rebuild (`place`, which growth, `remove_dead` and `canonicalize` go through) and `load_image` bump it, and a stale vacancy, or one whose fill would need growth, falls back to `insert`. A collection that only turns entries into tombstones leaves the generation alone, because a vacancy stays valid: a tombstone or empty slot on the probe path is still a correct place for an absent string. The fuzz test's `GcStress::EveryAllocation` mode collects at exactly that allocation. Measured warm under callgrind against main with the one-byte `strsplit` path in the same change: team_scores 0.982x, report 0.994x, strings 0.996x, sales 1.000x, testthat_full 0.999x, with peak RSS unchanged.
- **A `CHARSXP` is aligned to 8, not 16.** Its header plus up to 7 bytes fits the 40-byte class instead of 48, which most short strings (`paste0("id", i)`, formatted numbers) are.
- **Impact.** `formatC` at 1e7 rows spends about 0.8 s outside the collector with no live strings and with 1e7 of them, so the time no longer depends on heap state. `values::StringMemo` stays in `substr` and `formatC`, whose results repeat, because a small per-call table stays in cache where even one probe into the session-wide slot array misses it; it is not needed for the time to stay level. `test_gc.rs`'s intern fuzz test mixes in large strings so `young_large` is exercised.

## Symbols stay in a hash map keyed by their `CHARSXP`

**Status:** built.

- **Shape.** `Heap::symbols` is an `FxHashMap<Sexp, Sexp>` from a print name's `CHARSXP` to its symbol. A lookup hashes the pointer and compares stored pointer keys, so it never reads the name or the symbol, and hashbrown's 16-wide control-byte groups keep probes cheap at high load.
- **Rejected: the string table's packed slots.** Two versions were measured on testthat_full under callgrind. Packing only the symbol and comparing `PRINTNAME` on a tag match cost a cache miss per hit. Packed name keys beside a parallel symbol array removed that miss, but linear probing at the string table's 7/8 load averaged 6.6 slot reads per lookup: about 80 instructions per `install_char` against 58 for the map, plus about 2M in `insert`. Growing earlier would shorten the chains, but it would double a table of 16-byte slots one step sooner, costing anonymous memory for about 1.6M instructions.
- **Avoid installs instead.** Constant names in native code come from `values::symbols()`, and `values::install_charsxp_text` installs an element of a character vector without copying it into a `String` (`makeLazy`, `list2env`). `crates/heap/tests/test_gc.rs` (`fuzz_interning_agrees_with_a_reference_map`) compares string and symbol interning against `HashMap`s through growth and collections, also under `GcStress::EveryAllocation`.

## `gc()`, `gcinfo()` and `gc.time()` report the collector's real counters

**Status:** built, except the single byte limit behind `gc()`'s trigger columns, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes.

- **Previous shape.** `gc.time()` returned zeros and `gc()`'s table was made up, so collector time could not be attributed from R.
- **Shape.** `heap::GcStats` keeps live objects (GNU R's `Ncells`), live vector data in 8-byte cells (`Vcells`; a major collection counts it afresh, a minor one adds what it marked), their maxima since `gc(reset = TRUE)`, and user, system and elapsed collection time while `GcPolicy::timing` is on, which `gc.time(TRUE)` sets as in GNU R. `Heap::usage` gives `gc()`'s table, with the trigger columns scaled from the heap's single byte limit, since ry triggers on total bytes rather than on two separate counts. `gcinfo(TRUE)` installs `GcPolicy::reporter`, which prints GNU R's one-line report after each collection while collection is still inhibited.

## Collections run on GNU R's schedule, and dead large vectors go back to the kernel

**Status:** built, except `heap_limit` bounding all bytes and the single old generation, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes.

- **Previous shape.** A major collection ran only when the old generation passed twice the survivors of the last one, and a large vector ran a minor collection at half the old generation. A minor collection promotes everything it marks, so temporaries live at a minor collection piled up as old garbage: a CI report's loop (`keep <- v > 0.5; s <- list(id[keep], v[keep])` over `runif(5e7)`) peaked at 2.6 GB against GNU R's 1.4 GB. Large vectors came from libR's mimalloc, whose purge delay (`crates/libr/src/libr.rs`) keeps freed memory resident until `gc()`, so blocks of other sizes stayed resident on top of the live data. `rep()` built a `Vec<usize>` of output positions, 8 bytes per element, which tripled its peak.
- **Target shape.**
  - `GcPolicy::heap_limit` bounds all of the heap's bytes (old, young small, young large). A large allocation past it, or a minor collection that leaves less than a fifth of `min_heap_limit` (64 MB, GNU R's `R_VSIZE`) free, runs a major collection (GNU R's `R_MinFreeFrac` escalation). After a major one, `HeapGrowth::next_limit` applies GNU R's `AdjustHeapSize` fractions, and the limit never sits closer than that fifth to the need, or every following minor collection would escalate. `HeapGrowth::from_environment` reads `R_GC_MEM_GROW`, `R_GC_GROWFRAC`, `R_GC_GROWINCRFRAC` and `R_GC_VGROWINCRFRAC` with GNU R's presets and ranges (user docs: `reference/cli.md`, "Memory").
  - The small-object trigger is half the old *small* bytes (`old_small_bytes` and `old_large_bytes` are counted apart), as GNU R's cons-cell trigger ignores vector memory. Counting old large vectors let a session holding 300 MB of vectors pile up 150 MB of small garbage (vapply over a 1e6 list: 230 MB peak, now 112 MB, GNU R 58 MB).
  - Vectors of at least `large::MAPPED_BYTES` (1 MB) get an anonymous mapping each, unmapped when they die (`crates/heap/src/large.rs`). `MappedPool` keeps freed mappings up to the vector limit ("Two old generations" below) and resizes them with `mremap`; when no kept mapping is long enough it grows the longest and unmaps the rest, so the pool never holds memory idle while pages are added; otherwise up to its limit stays resident while idle. It is a pool of address space (the allocation pools the performance principles call for), not a cache of derived data: nothing in it can go stale, and the redesign that would remove it is fewer large temporaries, not a faster lookup. A sixteenth of the limit cost the subset loop 4.2 s of system time against 2.2 s; `MADV_HUGEPAGE` raised it to 9.9 s (huge-page zeroing and compaction) and was rejected for the pool; the global allocator's blocks ask for huge pages only while a table operation runs ("Table operations map their large blocks on huge pages").
  - libR's global allocator is `heap::large::Mapped<MiMalloc>`: blocks of 1 MB and more are mapped the same way, so a builtin's scratch buffer goes back to the kernel instead of staying in mimalloc where the mapped vectors cannot reuse it (`sample(x)` on 2e7 doubles: 305 MB peak with mapped vectors alone, 229 MB with the allocator, GNU R 229 MB). Growing such a block is an `mremap`, not a copy.
  - `rep`, `rep.int` and `rep_len` stream positions (`RepPlan` in `crates/interp/src/arith.rs`, `subset::select_positions`), and a plain vector's negative subscripts stream through a keep mask (`gather_kept` in `crates/interp/src/subset.rs`: `x[-(1:10)]` on 2e7 doubles, 305 MB peak, now 171 MB, GNU R 229 MB).
- **Impact.** Peaks against GNU R 4.3.3: rep 242/239 MB, the subset loop 1386/1386 MB, data.table's `dt[v > 0.5]` loop 329/330 MB (`runtime_peak_memory_stays_near_gnu_r` in `crates/ry/tests/test_perf.rs`). The subset loop takes about 18 s against 17-19 s before and GNU R's 28.6 s. On the generated 60,000-line suites the collector's share grows from 0.44-0.58 s to 0.68-0.82 s of 20-28 s suites. `R_GC_MEM_GROW=0` costs far more on ry than on GNU R (finbench: 66 majors and 4.8 s in the collector against GNU R's 1.6 s), because GNU R's second old generation frees promoted temporaries without a full mark and ry has one old generation (`backlog.md`, "Open: collector throughput on the benchmark suites").

## Two old generations, separate vector and node limits, and a pool as large as the vector limit

**Status:** built, except `jit_expand_sequence`: the JIT's `for` walks a compact `1:n` without expanding it (`jit_loop_sequence` in `crates/jit/src/runtime.rs`).

- **Previous shape.** One old generation: a minor collection promoted everything it marked, so a temporary live at one minor collection died only in a full one (`R_GC_MEM_GROW=0` ran 66 full collections on finbench, 4.8 s in the collector). `heap_limit` bounded small and large bytes together, and `MappedPool` kept freed mappings up to a quarter of it. Unmapping every dead large vector beyond that made `vector_arith` (8 MB temporaries) and `loop_sum` (the 40 MB `1:n` the `for` loop expands per call) page-fault fresh mappings each time: 0.6x and 3.8x of GNU R against 1.2x and 5.0x before.
- **Target shape** (`crates/heap/src/heap.rs`, `space.rs`, `large.rs`):
  - Objects age one generation per collection, as in GNU R: a minor collection makes a young survivor `OLD`, and a deeper collection makes an old one `TENURED`. An intermediate collection (`Collection::Intermediate`) traces and frees the untenured old objects and treats the tenured ones as roots through the remembered set. A deeper collection empties the remembered set before it marks, and tracing puts back each tenured object that still points at a younger one (`mark_child::<true>`, `younger_child`). A full heap escalates minor, then intermediate, then major (`escalate_if_full`), as GNU R's `num_old_gens_to_collect` does.
  - `heap_limit` counts large vectors only (GNU R's `R_VSize`) and `node_limit` counts small objects (`R_NSize`, at least 32 MiB), each grown by `HeapGrowth::next_limit` after a major collection. The need includes a fifth of the smallest limit, as `AdjustHeapSize` adds `R_MinVFree`, so a live set just under the grow fraction cannot pin the limit where every collection escalates.
  - A major collection that escalation forced before the program allocated a limit's worth of vectors since the last one counts as thrashing and grows the limit as if the survivors filled it: each such collection tenures the in-flight working set, which only the next major one frees. `sales` ran 21 majors in 20 calls without it, 9 with it.
  - `MappedPool::limit` equals the vector limit. Kept mappings never lift the peak, because the pool maps nothing while it holds a mapping, so the limit bounds only idle memory after a peak. It is allocation policy (a free list in the allocator), not cached derived data: nothing in it can go stale.
  - The JIT's `for` over a compact `1:n` expands it in place (`jit_expand_sequence`) instead of deoptimizing on every call.
  - Tracing a character vector skips elements it already marked, through a 64-entry table of addresses keyed by address bits (`trace_children`). A column of a few repeated strings is the common case, and its elements are CHARSXPs with no references, so a skipped repeat loses nothing. On `sales` the 764,705-element `region` column cost each intermediate collection 3.5 ms of mark-bitmap lookups (about 4.6 ns per element); with the table it costs 0.5 ms, and the first intermediate collection drops from 19 to 12 ms. The sweep was never the cost (about 0.2 ms).
- **Measured** against 0.4.1-beta, one sitting (`tools/bench/bench.py`, ratio to GNU R 4.3.3, peak MiB): `vector_arith` 1.2x/1.2x, 132/165; `loop_sum` 4.2x/4.1x, 82/130; `sales` 1.2-1.7x/1.8x, 128/158 (GNU R 149). `sales` gives back about 8 ms per call to 0.4.1-beta, whose limit of twice the survivors ran 2 majors where GNU R's schedule runs 3 majors and 5 intermediates; that is the price of the lower peak. `runtime_speed_stays_ahead_of_gnu_r` in `crates/ry/tests/test_perf.rs` holds each program above a floor.
- **Suites** (generated 60,000-line packages, `oracle bench --ry-only`, `RY_GC_LOG`): finbench 360 MiB peak, 0.72 s collecting of 18.4 s (5 major, 11 intermediate, 124 minor); dsbench 382 MiB, 0.88 s; biobench 362 MiB, 0.95 s. `R_GC_MEM_GROW=0` on finbench: 48 majors and 3.8 s, against 66 and 4.8 s with one old generation. Most of that is still full collections, because the tight limit escalates past the intermediate one; GNU R takes 1.6 s there.

## Large temporaries: freed from their reference counts, tracing stays for cycles

**Status:** built.

- **Previous source of truth.** Only tracing freed memory. A temporary that a minor collection saw live was promoted and died only in a major collection, so a loop's dead large vectors stayed resident until the heap limit forced one. The copy-on-modify count in each header (`Sexp::refcount`) was kept but freed nothing.
- **Shape.** A per-type hybrid in `crates/heap/src/heap.rs` (`note_zero`, `reconcile`, `shallow_roots`, `free_released`):
  - Every counted store releases the old value through `release_reference`, which reports a count that fell from one to zero. Such an object goes into the zero-count table (`Heap::zero_count`, a flat `Vec<Sexp>` with the `ZERO_LISTED` header bit against duplicates) when it can be reclaimed: a non-ALTREP list, or a non-ALTREP large logical, integer, double, complex or raw vector. Strings and small vectors are left to tracing, since their pages are not returned one at a time.
  - The table is reconciled only inside `maybe_collect`, at an allocation, because C code may hold an unprotected `SEXP` until its next allocation, as GNU R allows. A reconcile runs once the candidates add up to 4 MiB (`RECONCILE_BYTES`) or the table reaches 65,536 entries beyond those held. It sets `HELD` on the shallow roots: the protect stack, precious set, handles, handouts and globals, everything the root sources trace (interpreter frames, the bytecode register file, pending `.Call` arguments, condition handlers), one level deep, weak references, the fields of every object whose fields do not count (`untrack_references`, R's `DISABLE_REFCNT`), and the elements of every list that native code has seen (escaped lists, whose `SET_VECTOR_ELT` stores do not count). A candidate still at zero and not held is released: a list gives up its elements, which may cascade, and a large atomic vector's pages are freed at once.
  - Environments, closures, promises and external pointers stay with the tracing collector, which handles the cyclic subset (the shape of CPython's cycle collector and Nim's ORC). Counts only ever overcount, so a missed decrement keeps an object for tracing and never frees one early. A sweep decrements children in two places only, because elsewhere a leftover count is harmless: a swept zero-listed list releases its elements (`release_swept_lists`), so the columns of a dead data frame become candidates again, and a swept frame waiting for release is cleared first (see "Frames released at return").
  - A freed vector's bytes stay counted until the next collection recounts them, so the collection schedule matches tracing alone. Uncounting them put collections off, and the dead temporaries only a collection finds (forced promise values) piled up: `rep` peaked about a quarter higher.
- **Soundness check.** `RY_GC_VERIFY=1` makes a reconcile free nothing; it runs a full collection instead and panics if anything it would have freed is reachable. The heap fuzzers (`fuzz_reconciles_free_only_unreachable_vectors`, `fuzz_temporaries_are_freed_and_live_vectors_kept`) run that cross-check, and the oracle cases pass under `RY_GC_STRESS=1 RY_GC_VERIFY=1` with the same results as the default run.
- **Measured.** Peak RSS in MiB from `wait4`, ry against GNU R 4.3.3, for the scripts in `reference/cli.md` ("Memory"): subset loop 1097 against 1386 (tracing alone reached 1100 only with `R_GC_MEM_GROW=0`), data.table loop 277 against 340, `rep` 239 against 239 (its live data). Suite CPU is unchanged against tracing alone (finbench 18.4 s, dsbench 20.7 s against 21.6 s, biobench 25.3 s, collector time 0.57 to 0.74 s either way).
- **Frames.** A closure's frame is released at return; see "Frames released at return".

## Frames released at return

**Status:** built.

- **Previous source of truth.** Only tracing freed a closure's frame, so every binding in it (argument promises and their values, local temporaries) lived until a collection found the frame dead. On `sales`, the dead frames of `[.data.frame` and friends held the subset columns, and three majors ran per call.
- **Shape** (`Heap::release_frame`, `release_returned`, `clear_frame` in `crates/heap/src/heap.rs`, called from `run_closure` and `apply_internal_method` in `crates/interp`), following GNU R's `R_CleanupEnvir` and `unpromiseArgs`:
  - Enclosures count: `alloc_environment`, `set_enclosure` and weak references go through counted stores, so a frame's count is exactly its references from the heap.
  - At return, a frame whose count equals the cycles it holds on itself (refcount-1 closures and promises bound in it whose environment is the frame, not the result) and that is not the result, not precious and not a symbol frame is cleared at once: refcount-1 promises give up value and environment, bindings are set to nil, and the enclosure is dropped.
  - Otherwise the frame goes on `Heap::returned` with the closures it created (`Interp::frame_closures`, at most `FRAME_CLOSURES` per frame). At the next reconcile, a frame not reached by a non-handle shallow root is cleared once its count equals its own cycles plus its dead closures (count zero, not held) plus closures a sweep already freed. The caller's scope handle roots every frame it made, which is why frames are tested against the roots before the handles are marked.
  - At return only promises count as the frame's own cycles, never closures. A closure the frame made can be held by a root that counts nothing (native state keeping an R callback, such as the loader's deferred-S4 `force` closure in `DeferredDefinitions`), and only a reconcile flags roots, so a frame holding a closure always waits for the reconcile. Counting closures at return cleared the loader's frame under a live callback ("could not find function `.ry_force_s4`").
  - A collection clears an unmarked pending frame before the sweep, so its children's counts do not stay overcounted.
  - Arguments are not released after the call. Calls pass their arguments in an uncounted slice (the frame-args arena), so a reference count of zero does not prove a promise unreachable: an untracked environment cell binds it without counting. A promise bound in a counted frame is released with that frame instead.
- **Why deferred.** A frame that created a closure (an `on.exit` handler, a local helper, a promise) is usually dead at return too, but only a root scan proves the closure dead, and a scan per return is too slow; batching at the reconcile pays one scan for every pending frame.
- **Impact.** `sales` runs one major per call instead of three, 0.044 to 0.046 s against 0.052 s for the previous release and about 0.084 s on GNU R 4.3.3. Correctness rests on counts only overcounting: `RY_GC_VERIFY=1` checks each reconcile against a full collection, and the oracle cases give identical output with frame release on and off.

## Start-up memory: release relocation tables and preflight code pages

**Status:** built.

- **Previous shape.** ry started about 13 MB above GNU R, all in clean file-backed pages: `libR.so`'s `.rela.dyn` (3.7 MB) stays resident after the dynamic loader has applied it, and the `ry` executable's code for the preflight analysis (about 6.7 MB) stays resident while the script runs.
- **Shape.** An `.init_array` constructor in `crates/libr/src/embed.rs` walks `dl_iterate_phdr` and applies `MADV_DONTNEED` to every loaded object's `DT_RELA` table of 64 KiB or more, aligned inward to pages. `ry run` drops the preflight's state and applies `MADV_DONTNEED` to the executable's executable segments (`release_executable_code` in `crates/ry/src/main.rs`). Both are file-backed and clean, so anything touched again faults back in from the file; nothing reads the relocation tables after load except lazy `DT_JMPREL` binding, which is left alone.
- **Rejected:** RELR relocations (`-z pack-relative-relocs`) would shrink `.rela.dyn` at link time, but they need glibc 2.36 and release binaries promise glibc 2.35.
- **Impact.** Start-up peak 63.7 MB to 53.7 MB against GNU R's 50.4 MB. What remains is `libR.so`'s larger text and read-only data.

## The start-up image is build output of the base tree, and every start re-reads the environment

**Status:** built, except the single `ry-start.image` name that nothing writes at run time, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Previous source of truth.** Without an image, every start re-derived the same session from
  base R's files (baseloader, lazy-loaded base, the six default packages, methods rebuilding its
  S4 tables): an empty `ry run` took about 125 ms (median, Linux, bench profile). A first design
  wrote the image at run time on the first eligible start, into `~/.cache/ry/image`, keyed by the
  build, the session flags, base R's files, the library paths and the steering variables, with
  every other environment read recorded through an `image::variable` wrapper and compared on load.
- **What was structurally weak.** That image was a cache of derived state, and it had a cache's
  failures: a key that missed an input (a shared `.Random.seed`, a stale lazy-load database of a
  package outside R home, relative `R_LIBS`, start-up warnings shown only on the writing run), and
  a wrapper every interpreter environment read had to remember to use.
- **Target shape.** The image is build output. `ry-rcmd build-base` writes it as its last step
  into the base tree (`library/base/R/ry-start.image`) in an emptied environment, a release embeds
  it with the tree, and nothing writes it at run time. The header carries `RY_RUNTIME_ID`; an
  image of another build is skipped silently (`just start-image` refreshes it in a development
  checkout). After the load, each start clears the options and re-runs the environment-reading
  steps: the C-level options, `.Machine`, `.Platform`, base R's profile, and the `.onLoad` hooks
  of utils, grDevices and stats (`interp::session::start_from_image`). The image holds only what
  those steps leave alone, the namespaces and their attachment, which depend on nothing in the
  environment but `image::LOADING_VARIABLES`; a start that sets one runs the slow way. Strings
  under the writer's R home are relocated to the tree the image file lives in
  (`heap::image::Relocation`). The writer fails, failing the build, when start-up warned or left
  `.Random.seed`. User packages get no image ("Installed packages load through native fast
  paths, not an image" below).
- **No "existing objects" check.** Rust state that holds heap objects across start-up must be a
  fixed object, a heap root, or saved by `image::save_state`; a check that refused every object
  live before start-up flagged only `package:ry`'s frame, which the loader overwrites safely.
- **Impact.** Empty `ry run`: about 120 ms to 60 ms median, and 48 ms once the re-run was cut
  (below). Re-running the environment steps cost about 12 ms against loading a run-time image
  (47 ms). `methods::getClass("numeric")` with
  `R_DEFAULT_PACKAGES=NULL` starts the slow way (63 ms; the run-time image keyed that variable and
  took 56 ms). The four cache bugs cannot occur: no image depends on
  the run-time environment. Options, the search path and namespace paths match a slow start
  (`start-up-image.scenario`). Development builds start slowly after a runtime edit until the
  image is rewritten. Analysis is unaffected: the image is runtime-only.

## The start-up image carries the host's environment, regex automata are built lazily, and the heap uses huge pages

**Status:** built, except the `MAP_POPULATE` file mapping and the per-object image reader, which "The start-up image is the heap's pages at a fixed address, mapped in place" supersedes.

- **Previous shape.** Every start parsed and evaluated the loader's runtime source to build
  `tools:ry`, and every compiled regex that needed leftmost-longest or minimal matching built two
  `regex-automata` meta regexes eagerly, each with a full (dense) DFA. The 17 patterns start-up
  compiles (8 of them in `.expand_R_libs_env_var`) spent about 29M of 181M instructions there.
- **Target shape.** `ImageHost::environment` builds the host's environment when the image is
  written; `Globals::host_environment` roots it and `image::save_state` carries it, and
  `loader_backend::attach` takes it from the image (`image::host_environment`) or builds it on a slow
  start. `Matcher::Automaton` holds a `LazyAutomata` that builds both automata on first use, so a
  `sub`/`gsub` whose replacement refers to groups (which only runs the backtracking regex) never
  builds them, and builds them with `dfa(false)`: the full DFA was most of the build cost, and the
  lazy DFA searches as fast (same ns per string in a standalone benchmark; ry's `gsub`/`grepl`/`sub`
  micro-benchmark 800 ms against GNU R's 1012 ms). The image reader's primitive reads are
  `#[inline(always)]` and its "object existed before the load" flags are a bitset.
- **Page faults, not instructions, were the larger cost.** A minor fault costs about 2.5 µs in a
  VM, and an empty run took 7,838 of them (21 ms of system time). The heap's reservation starts at
  a 2 MiB boundary, commits 2 MiB at a time and is advised `MADV_HUGEPAGE`, so the kernel
  (`transparent_hugepage=madvise`) backs it with huge pages. The image file is mapped read-only
  with `MAP_POPULATE` (`image::ImageFile`) instead of read into a private buffer. Faults fell to
  about 3,500 and resident memory is unchanged (39 MB). `MADV_POPULATE_WRITE` on small pages gave
  nothing: it pays the same per-page cost up front.
- **Impact.** Empty `ry run` 181M to 123M instructions, 52 ms to 34 ms median on the same machine
  (GNU R `Rscript` 152 ms). The image load (79M instructions) and the remaining faults are the
  floor. Analysis is unaffected.

## The start-up image is the heap's pages at a fixed address, mapped in place

**Status:** built.

- **Previous shape.** The heap image listed every reachable object with its references as dense
  positions, and the loader allocated each object, filled it in a second pass, re-interned every
  string and symbol and rebuilt every frame. On an empty `ry run` the load took 81.6M of 121.5M
  instructions, 57.4M of them exclusive in `Heap::load_image_inhibited`.
- **What was structurally weak.** The image held a second encoding of objects that already had a
  memory layout, so each start paid a per-object decode for a result that was the same in every
  process.
- **Target shape.** The first heap in a process reserves its small-object space at
  `IMAGE_BASE` (`0x2000_0000_0000`, 64 GiB of address space), and the writer moves every large
  object into a section at `IMAGE_LARGE_BASE`, so a handle in the written pages is valid as is in
  the loader. The writer collects, tenures everything (headers carry `OLD | TENURED`) and writes
  the space's pages, its per-page metadata and the large section. The loader maps both sections
  of the file `MAP_PRIVATE | MAP_FIXED` over the heap (`fill_from` in `interp/src/image.rs`;
  Windows reads them). Soundness rests on two rules (`heap/src/image.rs`, module doc):
  - **The same starting state.** `Heap::image_precondition` digests the space's allocation state,
    the table counts and the fixed objects' addresses after the host registers its fixed objects;
    the writer records it before start-up and the loader compares its own before touching the
    heap. Equal digests mean the image's pages replace every object the loader holds in place. A
    mismatch, or a heap not at `IMAGE_BASE` (a second heap in the process), starts the slow way.
  - **No process-local addresses in the pages.** Spilled frames, external pointers to builtin
    routines (as tokens) and the payload of fixed objects that keep native addresses (ALTREP
    class tables) are cleared in the written copy and restored by the loader from the metadata.
  Relocated path strings are new strings that the listed referring fields are pointed at; the
  pages are not patched otherwise. The header (metadata and interpreter state) is checksummed,
  the pages are not, since reading them would fault in every page and undo the sharing. The
  writer replaces the file by rename, because a mapped file truncated in place raises SIGBUS.
- **Mapping against reading.** Reading the pages into the heap's huge-page memory costs about
  4.5 ms for 14 MB on a 4-processor VM (the kernel zeroes each huge page and then copies into
  it) and makes every page private. A private mapping faults pages in lazily with fault-around,
  copies only pages that are written, and keeps the rest shared through the page cache. Marking
  writes a header only to promote an object, and every image object is already tenured, so
  collections leave mapped small-object pages clean; large objects take a mark bit per major
  collection. Measured on an empty run: mapping 29.2 ms median against 33.3 ms for reading and
  4,337 faults against 3,217, and Pss summed over 4 concurrent workers 87 MB against 131 MB.
  Test suites run in parallel workers, so the mapping is the shape.
- **Impact.** Empty `ry run`: 121.5M to 47.9M instructions, the load phase 81.6M to 7.2M
  inclusive (0.9M exclusive in the heap loader; `Frame::from_image` 1.2M is the largest part);
  38.5 ms to 29.2 ms median wall (paired ratio 0.75, 15 rounds, noisy); peak RSS 39.6 MB to
  38.0 MB; 4 concurrent workers 117 MB to 87 MB Pss. testthat_full process wall 675 ms to 673 ms
  (within noise) and 4 workers 245 MB to 237 MB Pss: its remaining start-up cost is package
  loading, whose lazy-load fetches inflate and unserialize each record (the 29M of inflate
  measured on testthat_full is there, not in the start-up image). Analysis is unaffected.

## Code images: compiled code beside the lazy-load database

**Status:** built, except the file name `R/<pkg>.ryc`, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Shape.** `R CMD INSTALL` writes `R/<pkg>.ryc` beside `R/<pkg>.rdb` (`crates/interp/src/code_image.rs`,
  driver `crates/rcmd/src/code-image.R`). A fresh session records every fetch from the package's
  databases (`ryCodeImageRecord`), loads the namespace, forces every binding, optionally trains
  (`--ry-train=tests|examples|<script>`, `ry install --train`), and compiles and writes each recorded
  closure (`ryCodeImageWrite`). A failure warns and leaves the package without an image.
- **Identity without a second key.** A closure is named by its record's database offset plus the
  order the stream reader creates closures (`serialize::unserialize_with_closures`), which depends
  on the record bytes alone. The image stores each record's length and is stamped with the
  database's length and modification time ("Lazy-load records are read in place from the code
  image").
- **No second copy of the source.** A code object is encoded against its closure's body: every
  language object the code points at is written as its preorder number in the body
  (`serialize::walk_source`). Nested function bodies are compiled eagerly at install
  (`bytecode::compile_nested_functions`) so their code is in the image too.
- **Decoded where compiling would happen, not at the fetch.** A fetch notes where each closure's
  code lies (`code_image::State::pending`, keyed by body, the body preserved so its address cannot
  be reused) and `bytecode::run_body` decodes it at the call that would compile the body. Decoding
  every fetched closure cost 2% more instructions on `library()` of twelve CRAN packages, because
  a namespace fetches many closures it never calls (every registered S3 method); decoding at the
  first call still cost more than it saved, because decoding costs about what compiling does
  (about 65k instructions per closure) and many closures run once. Untrained tier-0 code is
  therefore not a speedup; the image's value is the trained profile and the reserved native
  section.
- **Stamp.** The runtime identity (`RY_RUNTIME_ID`, passed in `StartOptions::build`) and a layout
  magic; any other build reads the image as absent, quietly.
- **Pending code is keyed by body and checked against formals.** The compiler's output depends on
  formals (`local_symbols`, `effects::summarize`, `tail_updates`, the missing-subscript path), and
  `formals<-`/`as.function` share the body, so each pending entry also preserves the fetched
  closure's formals and `take_pending` hands code only to a closure with those formals. Keying by
  closure would miss `environment<-` copies, which are safe to share.
- **A damaged image only costs its speedup.** An unreadable or unparsable file, a payload hash
  mismatch, a record whose closures do not match, or code the decoder rejects warns once
  (`code_image::give_up`), marks the database as having no image, and drops its pending code, so
  the package loads and compiles as it runs. An earlier shape raised an error that made the
  package unloadable and was retried at every fetch.
- **Decoded code is verified, not trusted.** Tier 0 indexes operands, registers, caches and jump
  targets unchecked, and FNV-1a only detects accidents, so `bytecode::verify` checks every decoded
  object linearly (opcodes, per-role operand bounds, instruction-start bitset for targets, no
  fall-through at the end, side-table lengths, switch-table shape). Record ops carry process-local
  layout ids and are refused by the encoder; Field/Item shape caches are written empty; constant
  gp bits are masked to the S4 bit. `fuzz_code_image_load` runs mutated-but-rehashed payloads;
  without the verifier it crashes within 20 seeds. Residual trust: register dataflow (guarded at
  run time where cheap) and the JIT, which assumes compiler-shaped code.
- **Lifetime and writing.** `lazyLoadDBflush` (called by `unloadNamespace`) drops the image and its
  pending entries. Images are written to `.ryc.partial`, fsynced, renamed; never rewritten in place,
  because a mapped file that shrinks raises SIGBUS in the process mapping it.
- **Training.** A trained image keeps each code object's PROFILE counters, inline-cache hints and
  FEEDBACK words; TIER1 decodes as nil. Persisted entry counts at or above the tier-up threshold make a hot body enter the
  optimizing tier at the call that takes its code from the image (the first when the body has a
  loop, otherwise the second). The tier state's counts of compilations and cold refusals belong
  to the training process's optimized code, so they are written as 0; a final refusal (a
  negative state) is kept. TIER1 code and inline caches are not saved:
  the first is process-local machine code, the second holds heap pointers.
- **No machine code.** An image holds bytecode, profiles and streams only ("Code images hold
  bytecode only").
- `RY_CODE_IMAGE=0` ignores images at load; `--ry-no-code-image` skips writing one.
- **Measured** (callgrind, whole process): `library()` of R6, magrittr, glue, rlang, lifecycle,
  cli, vctrs, purrr, assertthat, zoo, jsonlite and digest is 1,201.9M instructions with images
  and 1,200.7M without; a first-result script over rlang, purrr, vctrs, glue and cli 1,323.8M
  against 1,321.9M. A trained image of a loop-heavy function saves its warm-up (12M of the 76M a
  200-call workload adds to an empty run), a constant that does not grow with the run.

## Vectors grow in place into spare capacity

**Status:** built.

- **Previous shape.** `x[[length(x) + 1]] <- v` allocated a vector of the new length and copied
  every element, so a loop that appends n elements was quadratic; rcp's list-for timed out.
  Subscripts other than a local symbol or a constant fell back to `complex_assign`, and a text
  output connection rebuilt its character vector from bytes for every line.
- **Target shape.** Growth uses the heap's resizable vectors (`alloc_resizable_vector`,
  `resize_vector`), the same flag `SETLENGTH` and `IS_GROWABLE` expose to packages. `subset::grow`
  extends an unshared, unclassed vector without dims in place when capacity allows, and otherwise
  copies into a vector with 5% headroom, which keeps a sequence of appends linear at a memory cost
  of one twentieth. Names grow alongside. Tier 0 compiles element stores with any lowerable
  subscript and two-subscript matrix stores (`Op::SubAssignMatrix`) to in-place stores ("Computed
  subscripts stay in bytecode"), and a formal is read at run time so a missing one still assigns
  every element. A text connection's lines are the R vector itself, grown with
  doubling.
- **Impact.** One source of truth for length and capacity (the heap object), no shadow buffers,
  and appends that never copy unless R code holds a second reference. GC sizes an object by
  capacity, so headroom is visible to collection limits. Analysis is unaffected.

## Card marking, frame watermarks and moved list growth

**Status:** built.

- **Previous shape.** The remembered set held whole objects: an old list or character vector that received a young child was rescanned element by element at every minor collection, and so was a remembered spilled frame. Growing a list by `[[<-` copied it and counted every element into the copy, and the sweep counted each one back out of the dead original through a temporary buffer. `unserializeFromConn` read its connection through the read method once per item, a few bytes per call.
- **Cards for large handle vectors.** A list, expression or character vector too large for the small-object pages keeps one card byte per 128 elements of its capacity after its data (`CARD_ELEMENTS`, `Sexp::cards` in `object.rs`). The element barrier (`Heap::element_barrier`) stays one load and one compare for a young or already remembered small parent; a large old parent reaches the slow path on every store while remembered, so the card of each store is dirtied. A minor collection marks only `CARD_YOUNG` cards and downgrades them to `CARD_UNTENURED` for a tenured vector, which only an intermediate collection reads (`scan_cards`). `RY_GC_VERIFY` checks that every younger child sits in a dirty card.
- **Frame watermarks.** A spilled frame keeps `Spilled::scanned`, the number of bindings a minor collection has already marked. Frames only append or compact (`retain` resets it to 0), so a minor collection marks the appended cells and moves the watermark (`Frame::unscanned_cells`, `mark_scanned`). The verifier checks that every cell below the watermark is marked.
- **Moved growth.** `[[<-` that grows an unshared, non-ALTREP list as its final step moves the element pointers into the grown list (`Heap::move_elements`) and empties the old one, so neither counting pass runs. Growth by `[<-` and growth followed by work that can fail still copy, because the original must stay intact on error. `release_swept_lists` releases a dead list's elements in one pass without a buffer.
- **Peeked connection reads.** `connections::peek` lends out the bytes a gzip file, raw, loaded or `gzcon` connection has buffered, and `consume` advances past what was used. The unserialize reader serves small reads from that window and consumes it before a `refhook` call, before an ASCII byte read and when the read ends, so the connection stays exactly after the object (`Reader::settle`). A read longer than the window goes through the connection.
- **Measured** (callgrind, `library(testthat)` and the testthat workload, JIT on; wall best of three for the rcp programs):
  - rcp/list-for 1.146 s to 0.557 s (minor GC 356 ms to 64 ms), list-while 1.036 s to 0.529 s, lapply 0.957 s to 0.843 s; most of the gain is moved growth, because each 5% regrowth makes a young copy that cards cannot help.
  - Load 720.3M to 701.7M instructions: `do_unserialize_from_conn` 53.5M to 34.8M, `collect_keeping` 90.3M to 89.0M. Full workload 3,564M to 3,529M, GC 194M to 186M. Peak RSS per worker 80.7 MB to 78.6 MB.
- **Evidence.** `crates/heap/tests/test_gc.rs` (`a_frame_filled_across_minor_collections_keeps_every_young_binding`, `moved_elements_keep_their_counts_and_survive_in_the_new_vector`, the card test and the fuzzers); oracle cases `builtins__list_growth_moves.R` and `serialize__connection_reads.R`.

## Inline small-vector allocation

**Status:** built.

- **Shape.** `Heap::alloc_vector` is `#[inline]` and allocates a vector of up to 512 data bytes of a common kind (raw, logical, integer, double, `I64`, complex, character, list, expression) without leaving its caller: the element shift comes from `small_vector_shift`, the size class from `SIZE_CLASS_BY_WORDS`, then the young budget check and the slot bump every small object takes. It writes the header and length words whole and initializes only the data: handles to `""` or `NULL`, numbers to zero with two stores for a scalar. Everything else (`CHARSXP`, larger vectors, a collection) goes to `alloc_vector_general`.
- **Why.** The general path called `object_size` through the PLT, and `memset` zeroed the 32 header bytes that the header writes overwrite anyway: about 140 instructions per call, 381k calls in the testthat workload, mostly one-element character vectors from `scalar_string_charsxp`.
- **Measured** (callgrind, testthat workload): 3,528M to 3,450M instructions with the JIT on, 3,514M to 3,435M off; `library(testthat)` 700.7M to 694.2M. `memset` 31.6M to 26.8M.

## Inline marking for minor collections

**Status:** built, except the out-of-line `Space::age`, which "Marking a traced object costs a few table loads" supersedes.

- **Shape.** `Space::mark` is `#[inline(always)]` and only tests and sets the slot's mark bit, with unchecked bitmap indexing (`locate` has already bounds-checked the page, and a slot of the smallest class still has its bit in the last word). A newly marked object ages in the out-of-line `Space::age`, and a large object goes to `Space::mark_large`. `Heap::mark_child::<TENURED>` replaces a `tracing_tenured` flag: `trace` picks the instantiation once per object, so a minor collection's loops carry no younger-child test. A character vector of up to 16 elements skips the 512-byte dedup table, which costs more to clear than its repeats save.
- **Why.** A package load allocates objects that almost all survive, so each minor collection traces about 360k young objects, and most of the children it meets are old and already marked. That check was an out-of-line call through two nested bounds-checked `Vec` lookups, with a store of the bit even when it was set: about 44 instructions per child.
- **Measured** (callgrind, testthat workload, `RY_BENCH_REPEAT=2`): `collect_keeping` 89.0M to 63.4M during `library(testthat)` and 185.8M to 145.1M over the full run; the full run 3,450M to 3,409M instructions with the JIT on, 3,435M to 3,394M off. Peak RSS is unchanged.
- **Left open.** `locate` (page, class, multiplier, word and bit) is about 13 instructions of what remains. The other lever is collecting less often while a namespace loads, since its survivors are retraced only to be promoted (`backlog.md`, "collector throughput").

## Minor collections touch only what changed

**Status:** built.

- **Previous shape.** Every environment was finalizable, so each minor sweep read every dead call frame to drop it, although most closure frames fit their inline binding slots and own no memory. The interpreter's root source walked the whole S3 and S4 method cache on every minor collection; that cache grows to thousands of sites during a package's tests, and its walk went from 12 µs to 1.2 ms per collection over the testthat run. Counting freed objects by type for `--profile-summary` ran inside the sweep and was charged to the collection bucket.
- **Target shape.**
  - `Frame::owns_memory` (spilled bindings, an index or a hash table) decides the finalizable bit. It is set when the environment is allocated or restored from an image, and when `define`, `define_formals` or `set_frame_hashed` makes a frame own memory. Nothing else gives a frame heap memory; the JIT only reads frames.
  - `MethodCache::written` logs the key of each entry stored into either method cache since the last collection (`dispatch::Written`). A minor collection marks the entries those keys still name (`Marker::is_minor`), and any other collection walks every entry. The source clears the log only when `Marker::is_collecting`, because a reconcile also traces sources. The log holds keys rather than objects: logging objects marked entries that were replaced or cleared before the collection, and in a run with only minor collections whatever a minor collection marks is promoted and never freed (that version cost 5 MB of peak RSS on testthat). Past 65,536 keys the log is dropped and the next collection walks everything.
  - Freed-by-type counting is its own pass (`Space::count_dead_by_kind`) under `Activity::Profiler`, which the summary shows as "profile summary bookkeeping" in the overhead column. It runs only while profiling.
- **Measured** (testthat workload, `RY_BENCH_REPEAT=2`, with inline marking, against the main build before it): the collection bucket of `--profile-summary` 0.046 s to 0.021 s (7 interleaved runs), part of it the profiler's own counting moved out; the sum of collection pauses without profiling 36.9 ms to 24.1 ms (`RY_GC_LOG`, 5 runs); `collect_keeping` 185.8M to 137.6M instructions; peak RSS unchanged at 82 MB.
- **Left open.** Toward a tenth of the collection time: the minor collections during `library(testthat)` still trace each load survivor once (about 10 ms), dead returned frames are cleared one cell at a time (about 7 ms), and the sweep visits every touched page (`backlog.md`, "collector throughput").

## Pretenuring lazy-load fetches

**Status:** built.

- **Shape.** `do_lazy_load_fetch` reads its record inside `Heap::begin_pretenuring`/`end_pretenuring` (`pretenured` in `crates/interp/src/serialize.rs`). `Space` swaps in a second set of cursors whose refills file their pages in `pretenured_pages` instead of `touched`, so those pages hold only old objects plus the unflushed pretenured ones (allocated and not marked). The inline allocation paths are unchanged. No collection runs while pretenuring, and the pretenured bytes stay out of the nursery's budget, which bounds young garbage. The next collection (or `promote_young`) flushes them before it marks anything: it ages every pretenured object to `OLD` first, then remembers each one with a younger child (`has_younger_child`), then marks them and files their pages. A large object is promoted when pretenuring ends, with the same check (dirty cards for a carded one). R code the reader calls (the `PERSISTSXP` hook, `findPackageEnv`, `..getNamespace`) runs under `outside_pretenuring` and allocates young.
- **Why the flush checks children instead of a barrier.** The reader stores into objects it has just built without a barrier, which is right for young objects, and a pretenured object looks young until the flush. So stores into it, by the reader or by the mutator before the next collection, are caught by the flush's check of every child, which keeps the old-to-young invariant exact. If nil is still young at the flush (a heap that has not collected yet), it is marked first: tracing skips leaves, so an old leaf pointing at a young nil would later read as a tenured object holding an untenured one (fuzz seed 24). `environments__pretenured_fetch` writes its own lazy-load database, fetches an environment from it and stores young values into it between collections, for `RY_GC_VERIFY=1` and collection stress.
- **Measured** (callgrind, testthat workload, `RY_BENCH_REPEAT=2`, against main): `library(testthat)` runs no collection (one before), 314.3M to 295.6M instructions; the full run 2,824.6M to 2,813.4M with the JIT on and 2,823.6M to 2,811.9M off; `collect_keeping` 78.2M to 66.9M; minor collections 10 to 9. Peak RSS is equal with both builds freshly copied (73.7 to 74.0 MB against 73.5 to 73.9 MB); a freshly written binary has more of its file pages resident, which reads as several MB of RSS, so compare RSS only between copies made the same way.
- **What it did not buy.** The testthat load fetches about 28k objects, a small part of what its collection traced. Counting pretenured bytes toward the nursery budget keeps the load's collection, and it then costs as much as before (18.9M against 18.8M): the flush (about 4.7M before the loops were fused) costs about what tracing those objects did, and 10.3M is tracing about 39k other objects. The win is the collection that pretenured bytes no longer trigger.

## Lazy-load records are read in place from the code image

**Status:** built.

- **Previous shape.** A fetch from a lazy-load database read the whole `.rdb` into an anonymous per-process buffer (`serialize::cached_record`), inflated the record with zlib on every fetch, and unserialized the result. The code image stored a 64-bit FNV hash of each record's compressed bytes to tell that the record it described was still the one in the database, so every fetch through the image also hashed the record.
- **Weakness.** Inflate cost about 18M instructions of a testthat run (`decompress_record`, 475 fetches) and the buffer was private memory in every worker. The per-record hash was a second identity for a record that the database file already identifies.
- **Target shape.**
  - The `.ryc` holds each recorded fetch's uncompressed serialize stream after the code payloads (streams last, so payload offsets stay 32-bit). The file is mapped read-only (`ImageFile`), so worker processes share its pages, and `code_image::read_image_record` unserializes straight from the map.
  - The image is stamped with the database's length and modification time (`DatabaseStamp`). Any mismatch ignores the whole image; a copy that drops modification times only loses the image, never reads a wrong one. Without a modification time no image is written.
  - Each stream carries an FxHash checked on its first read (`Image::streams_verified`), which catches a damaged or truncated image; a mismatch falls back to the database.
  - `ry-rcmd start-image` also writes images for the base-priority packages (`crates/rcmd/src/base.rs`, `BASE_CODE_IMAGES`): a session started from the start image fetches every record of every base `.rdx` by key (start-up forces some base and methods bindings before recording starts, so forcing bindings missed 14 records that testthat_full then inflated) and writes the images with streams only (`ryCodeImageWrite(code = FALSE)`). They carry the build stamp, so they are rewritten with every start image. Full untrained base images (compiled and machine code) measured worse: on testthat_full `jit::ahead::load` went from 0.8M to 7.5M instructions mapping native sections, and decoding code cost 7.7M more while compiling saved only 3.8M, 3M instructions net over not having base images at all, plus 25 MB on disk.
- **Databases are numbered.** `code_image::Databases` replaces both the path-keyed `.rdb` read cache in `serialize.rs` and the path-keyed image map: one column per fact (preserved path `CHARSXP`, path, mapped `.rdb`, image slot, a recording bit), indexed by the database's number. A fetch finds the number by a pointer scan over the interned path `CHARSXP`s (a few dozen per session), then the record by binary search over the image's offset-sorted entries. The `.rdb` itself is mapped rather than read into a private buffer; a database rewritten by an install is replaced by rename, and `lazyLoadDBinsertValue` appends and then unmaps it (`serialize::forget_database`), as `lazyLoadDBflush` does.
- **Why not map heap objects.** Mapped heap pages would need one fixed address per package or relocation of every pointer, plus fix-ups for symbols (interned per process) and for the environments the namespace refhook returns, which run R code. Unserializing from a mapped stream keeps one object layout and one allocation path.
- **Allocation.** A fetch reads inside `serialize::pretenured`, whichever source it reads from, so what it builds is allocated old (see "Pretenuring lazy-load fetches").
- **Key stays `c(offset, length)`.** makeLazy's promise shape is visible to R, so the record is found by binary search on the offset. Everything `do_lazy_load_fetch` spends outside `read_image_record` (database number, search, first-read stream hash) is 1.6M of its 54.2M instructions on testthat_full.
- **Measured** (testthat_full, `RY_BENCH_REPEAT=1`, against main with each build's own start image and package installs):
  - Callgrind: 2,236.1M to 2,207.9M instructions (-1.26%). `do_lazy_load_fetch` inclusive 80.4M to 54.2M, `decompress_record` 26.0M to 0. No `.rdb` is opened at all; main opens 13.
  - Wall time (15 paired rounds): testthat 0.997x, empty run 1.017x, both inside the noise.
  - Memory with the page cache dropped before each run: empty-run peak RSS 34.2 to 34.5 MB. Pss summed over 4 concurrent workers: empty run 76.3 to 74.6 MB, testthat 232.6 to 237.0 MB, of which anonymous 189.1 to 179.1 MB (the private `.rdb` buffers are gone) and file-backed 43.6 to 57.9 MB (mapped streams, which are larger than the compressed records).
  - With a hot page cache the empty run peaks at 38.6 MB on main and 42.1 MB here. The difference is page cache: a freshly written `.ryc` sits in large folios, and touching one byte of `methods.ryc` maps 2048 kB of it, against 64 kB after `drop_caches`. Anonymous memory is 0.8 MB lower.
  - The 14 base images take 14.5 MB on disk.

## Escalation prediction and the dense precious set

**Status:** built.

- **Why `records` escalated.** `tools/bench/real`'s `records` builds a list of trades and keeps it. Each intermediate collection freed under 1%, because what it could free (objects promoted since the last deeper collection) was exactly the structure still being built, and the major collection after it traced the same objects again. Pretenuring lazy-load fetches changed nothing there (17 minor, 3 intermediate, 3 major collections before and after). `escalate_if_full` was 10.2% of the program's instructions.
- **Prediction instead of a fixed ladder.** `Swept.untenured_bytes` counts the dead objects a deeper collection freed outside the tenured generation, and `Space::younger_old_bytes` counts the live younger-old bytes (marks without the tenured bit, by popcount). After each escalation, `younger_dead_share` is the share of the younger-old bytes it found dead (only when they exceed a twentieth of the minimum node limit, since a share of a few objects predicts nothing). When the old small bytes minus that share of the younger-old bytes would still leave the node limit full, the escalation skips the intermediate collection. Vector pressure always tries the intermediate first, because large vectors die independently of the small-object structure.
- **Only an intermediate collection teaches the prediction (settled).** `younger_dead_share` was once also updated after major collections, which made the policy alternate: a major collection found 83% of the younger-old bytes dead, the next escalation trusted that and ran an intermediate collection, and that one freed 10 to 14 KB of 31 MB before the major collection after it. The difference is nepotism: a tenured object that died keeps its younger children alive through the remembered set until a major collection, so what a major collection frees outside the tenured generation overstates what an intermediate one can. Only intermediate collections measure the share, and after `RETRY_INTERMEDIATE_AFTER` (4) escalations in a row that went straight to a major collection on the prediction alone, one intermediate collection runs anyway to measure again. Measured with callgrind (`RY_JIT=on`, `RY_BENCH_REPEAT=5`) on top of the string change below: `team_scores` -0.61% instructions, with one intermediate-then-major pair fewer (3 to 2; `RY_GC_LOG` shows the branch running `in ma`, four predicted majors, then `in ma` again); `records` -0.51%, the same pair fewer; `sales`, `expr_tree` and the testthat workload unchanged, with identical collection sequences. Pinned by `an_escalation_learns_only_from_intermediate_collections` in `crates/heap/tests/test_gc.rs`: its first phase builds a structure that stays live (main ran 15 intermediate collections for 30 majors, the prediction 7 for 29), and its second makes short-lived objects again and asserts that intermediate collections outnumber majors, which they do only because of the retry (without it the phase ran 0 intermediates and 20 majors).
- **The carded path skips repeated strings, and the mark test cannot replace the repeat table (settled).** A character vector large enough for cards was traced by deeper collections one element at a time, because the 64-entry repeat table lived only on the uncarded path. Both paths share `Heap::mark_strings`; a card must learn whether each element is younger than the vector, so the table keeps that flag in the low bit of each CHARSXP address, and a repeat ORs it into its card (a string cannot age between two of its marks, since the first one aged it). Measured per element of a tenured 1M-element vector per major collection, above the 8 instructions of the loop itself: a mark test on an already-marked string costs 25 instructions (finding the page, size class, word and bit, then reading the mark and tenured bitmaps), a table repeat 6, and a table miss 11 more than a plain mark. A cheaper mark test does not close that gap: a flat bitmap over a space holding only strings, found by address range, still needs the range check, the shift and two bitmap reads (about 15), and a mark bit in the header costs a cache miss per element. Moving surviving interned strings to an untraced space was rejected because their death would then wait for a rare full collection, which is an RSS risk. To keep the miss cost off vectors of distinct strings (a column of identifiers), the carded path stops using the table after the first card without a repeat: distinct strings cost 118.8 instructions per element as without the table, repeats 13.4 instead of 33.3. On `sales` this cuts 9.96% of the whole run (`drain`, against main); the other workloads are unchanged. Pinned by `a_string_repeated_across_cards_keeps_each_card_dirty`, which fails when a repeat does not dirty its card, and `a_vector_of_distinct_strings_keeps_a_later_card_dirty`.
- **Rejected: a nursery sized by survival.** Doubling the nursery (up to 64 MB) while a minor collection promotes more than a fifth of its bytes saved only another 0.6% of `records`' instructions on top of the prediction, and raised its peak RSS from 111 to 141 MB; the rule is no peak RSS regression per worker, and four parallel workers would pay it four times. Bounding the growth by the live set buys nothing, because the small-object trigger is already `max(young_limit, old_small_bytes / 2)`.
- **Dense precious set.** `Roots.precious` was an `FxHashMap<Sexp, u32>`, probed on every frame release to ask whether the environment was preserved. It is `Precious { objects, counts }`, two parallel vectors, plus `flag_bits::PRECIOUS` in the header, so the hot question is a flag test. The index is not stored in the header because every gc bit is taken and flag bit 7 was the only spare one; `R_ReleaseObject` scans from the newest entry, as GNU R's precious list does, and the packages that preserve many objects (Rcpp, cpp11) keep their own linked lists and preserve only the list head.
- **Measured** (callgrind, `RY_BENCH_REPEAT=2`, against main): `records` 7,028M to 6,366M instructions (-9.4%), `maybe_collect` 964M to 632M, `escalate_if_full` 720M to 391M; per process at `RY_BENCH_REPEAT=4`, intermediate collections 5 to 2 (26 minor and 5 major on both). Peak RSS on `records` 111 to 97 MB (not profiled; it comes with the skipped intermediate collections); the testthat workload 73.7 MB on both (copies made the same way). The testthat workload 2,813.4M to 2,802.7M instructions with the JIT on and 2,812.2M to 2,802.1M off (the precious-set probe; its 9 minor collections are unchanged).
- **Minor-collection cost by source: nothing in marking is worth building.** A measurement-only probe split each minor collection by where marking starts, with phase times that sum to the collector total (per-build R home, single process, `RY_BENCH_REPEAT=2`, collection counts unchanged by the probe).

  | phase | testthat_full (9 minor, 15.9 ms, 2.2% of the run) | package load of 11 packages (2 minor, 5.3 ms) |
  |---|---|---|
  | root marking (`InterpRoots::trace`, root stacks, sources) | 1.41 ms, 8.9% | 0.21 ms, 3.9% |
  | tracing from roots | 1.74 ms, 10.9%, 47.9k objects | 2.13 ms, 39.9%, 68.2k objects |
  | remembered carded vectors | 0.07 ms, 0.4% | 0.12 ms, 2.3% |
  | remembered environments | 0.27 ms, 1.7% | 0.34 ms, 6.4% |
  | other remembered objects, traced whole | 1.96 ms, 12.3%, 7.7k old parents | 1.25 ms, 23.4%, 11.2k old parents |
  | escaped set | 0 (no escaped object reaches a minor collection) | 0 |
  | after marking (weak references, returned frames, sweep) | 10.44 ms, 65.7% | 1.28 ms, 24.0% |

  The bar for a structural change is 2% of testthat_full. The whole remembered set is about 0.3% of it and the escaped set is empty, so neither a slot-precise remembered set nor a more precise escaped set is built; the old objects a package load traces in its minor collections come from the remembered set (closures, strings, pairlists, calls, lists) and from roots, not from escaped objects. Roots are about 0.45% and `InterpRoots::trace` alone about 0.2%. The largest piece is the work after marking, about 1.45% of testthat_full, which caps any one of its sub-steps below the bar, so it was not split further (`backlog.md`, "Open: collector throughput").

## The start image keeps base R's closures compiled

**Status:** built, except the image layout, which "The start image is compacted in touch order at write time" supersedes.

- **Previous shape.** `image::write` replaced every code object with its source, so a session started from the image compiled each base closure again at its second call (or first, with a loop). Compiling was 12.2M of 50.4M instructions in an empty `ry run`.
- **Weakness.** Code that ran during start-up carries the writer's inline caches, type feedback, profile counters and slot hints, so it could not be imaged as it was; dropping all code threw away work every session repeats.
- **Target shape.** `bytecode::compile_namespaces_for_image` compiles every closure bound in a loaded namespace afresh, never runs it, and installs it as the closure's body; the image keeps exactly that set and stores any other code object as its source. Inline caches are weak-pair lists, which the image writes empty anyway; `PROFILE[RECORDING]` follows whether the writer had a tier, and a process without one clears it at the first entry, as decoded code does. Unforced lazy-load bindings stay promises (backlog: "Unforced base bindings still compile at run time").
- **Why the heap image and not base `.ryc` code.** The `.ryc` route decodes each closure at its first call and costs about what compiling does (see "Lazy-load records are read in place from the code image"); heap pages are mapped as they are, with nothing decoded, and pages of never-called code stay page cache.
- **Layout.** Superseded by "The start image is compacted in touch order at write time". Allocating the fresh code after start-up, where it filled start-up's free slots, cost the empty run 1.6 MB of peak RSS. Rejected along the way: own pages plus `MADV_RANDOM` (a cold empty start ran 1.20x and testthat_full 1.14x, because each touched page becomes its own read); own pages without the advice (no RSS gain, 1.3 MB of holes); ordering only the fresh code (no RSS gain, since start-up reads about 160 KB of objects allocated while compiling cold closures).
- **Image size.** 17.2 MB on main against 19.2 MB: the fresh code is about 0.85 MB of small objects and 1.9 MB of large ones, partly offset by holes it fills. Of the 14.8 MB of small pages, about 37% are holes start-up's collections left.
- **Measured** (release build, against main with each build's own start image):
  - Callgrind (JIT off): empty run 49.2M to 40.0M instructions (-18.8%), testthat_full (`RY_BENCH_REPEAT=1`) 1,923.6M to 1,902.1M (-1.1%).
  - Hot cache, 30 paired rounds on a quiet machine: wall time empty run 0.95x [0.89, 1.01], testthat_full 0.98x [0.93, 1.03]; peak RSS empty 35.3 to 36.9 MB, testthat_full 66.9 to 66.6 MB.
  - Page cache dropped, 10 rounds: wall time empty run 1.02x [0.90, 1.16], testthat_full 1.00x [0.91, 1.09]; peak RSS empty 34.6 to 36.2 MB, testthat_full 66.4 to 66.3 MB.
  - Start image 17.2 to 19.2 MB.

## The start image is compacted in touch order at write time

**Status:** built.

- **Previous shape.** The image held the heap's pages as start-up left them. About 37% of the small pages were holes left by start-up's collections, and an empty start touched most 64 KB chunks, so a cold start mapped never-run code next to the code it ran, through fault-around and readahead.
- **Target shape.** `Heap::write_image` numbers every live object with a stable id (a depth-first walk from seeds that do not depend on addresses, `number_for_image`), ranks the objects by a recorded first-touch order, and then by id, and moves every movable small object into dense pages in that rank (`Space::compact_for_image`). Large objects are laid out in the large section in the same rank. Every handle field, environment frame cell, the intern table, the symbol map and the roots are forwarded (`forward_for_image`). The image format is unchanged.
- **What stays put.** Objects allocated before start-up (`Space::pin_allocated`, called right after `image_precondition`, which the precondition digest covers), fixed objects, symbols, environments and ALTREP objects. Symbols stay because frame filters, slot indexes, hash tables and record layout tags hash symbol addresses; moving them would mean rebuilding all of those.
- **The profile is beside the image, not in it.** `ids`, the touch files and `order` (`development/architecture.md`, "The touch profile") are build inputs that only the layout build reads; the loader never does. Putting them in the image would add 3.7 MB of ids that no session reads. Each file names what it belongs to: `ids` and the touch files carry the image's header checksum, and `ids` and `order` carry the numbering digest. `image_order.py` refuses a touch file for another image, and the second write refuses an order for another numbering, so a stale profile fails the build instead of laying out the image wrongly. Without an order, the image is compacted in id order.
- **The order is regenerated, never committed.** Any change to what start-up allocates renumbers the objects, so a committed order would go stale with most runtime changes. `tools/layout/optimize.sh` (the x86-64 Linux release and nightly builds) records a profile against the binary it is building and writes the second image from it, and a failing training run fails that build, as it does for PGO and BOLT. Every other write uses no order and compacts in id order, which is also the tested fallback (`crates/heap/tests/test_image.rs`, even seeds). Rejected: keying the order on a per-object identity that survives renumbering, because most objects (promises, bytecode constants, pairlists) have no name, and a content hash would be costly and ambiguous for identical objects.
- **Merge rule.** An object's place is the lowest phase in which any run touched it (start-up, namespace load, other), then its earliest position as a fraction of that run's touches, so a long run does not push a short run's objects back.
- **Numbering and forwarding use dense tables, not hash tables.** `Space::slot_index` numbers every small-object slot (`first_slot[page] + slot`, from a snapshot of the page classes taken before anything moves) and then the large objects by sorted address. The writer's seen and fixed sets are bitsets over those numbers (`SlotSet`), and forwarding is a `Vec<Option<Sexp>>` indexed by them, so the old address of a moved object is looked up without hashing and the old slot can be reused as a destination. Measured against the three `FxHashMap`/`FxHashSet` tables it replaced: write time and peak RSS of `ry-rcmd start-image` unchanged within noise (0.58 to 0.75 s, 128 to 135 MB).
- **The loader's relocation table is a sorted array.** When the base tree sits at another path than the one the image was written under (every release, whose tree is extracted into the cache), the loader replaces each string that names a path under it and then forwards the roots that held an old string. The loader has no slot numbering to index a dense table by (building one would walk every page on every start), but the writer emits the strings in address order, so the moves form a sorted `Vec<(Sexp, Sexp)>` that a binary search reads; the loader rejects a section out of order. It replaced an `FxHashMap<Sexp, Sexp>`.
- **An image of an older format is stale, not damaged.** A stamp that differs only in its version byte makes the loader start the slow way silently, as an image from another build does, and assembling the home (`rcmd::refresh_start_image`, run by every new cached home, which a new libR's stamp keys) writes it again. `crates/libr/tests/test_start_image.rs` checks both kinds end to end: the image is rewritten with the current identity, sessions start from it (they record image touches), and a session on a stale image evaluates correctly without loading or rewriting it.
- **The image bytes are a function of the session.** Writing the same tree twice gives byte-identical images, which a reproducible build needs and which makes the touch files' image checksum stable across writes. The writer zeroes every byte that live contents do not determine: a spilled frame's inline storage (an empty frame leaves it uninitialized), the contents of fixed objects the loader replaces with its own (ALTREP class tables and primitives, which hold native addresses), and a vector's bytes past its elements (spare capacity, a large handle vector's write-barrier cards, which are clear once nothing is young, a string's bytes after its NUL, a small slot's tail). The intern table is rebuilt with entries placed in address order (`StringTable::canonicalize`), because its slots otherwise depend on the order strings were interned and removed in. Before this, two writes differed in about 1,300 bytes. Strings that name the install path make the numbering depend on that path's length, so training and the final write must use the same base tree path, as `optimize.sh` does.
- **A stale order is safe.** An order only ranks objects, and every live object is placed whether ranked or not, so even an order whose ids name different objects gives a correct image that is only less compact. The numbering check catches the ordinary case (the write fails, and `optimize.sh` with it); an order forced onto another numbering by rewriting its header passed the oracle under `ry` and `ry-gctorture`.
- **No advice.** `MADV_RANDOM` over the image makes each touched page its own read (see the rejected layouts in "The start image keeps base R's closures compiled"). Compaction removes the cost that advice was meant to address.
- **Measured** on a quiet machine: a release build of the compacting writer with the trained order against main. Each build has its own base R copy, start image, assembled home, cache directory and package library whose code images it wrote, because a build loads only images it wrote itself and a shared home or library compares a fast start against a slow one. Each start is checked: the branch through an `RY_IMAGE_TOUCH` file, main through `strace` showing the image mapped at the heap's fixed address.
  - Image 17,498,112 bytes on main, 16,384,000 compacted; the order covers 158,642 ids, trained on 966 programs.
  - Ratios are wall time, branch over main (below 1 is faster), paired rounds, with 95% intervals.
  - Page cache dropped, 30 rounds: empty run 0.95x [0.89, 1.02], peak RSS 34.7 to 31.3 MB; testthat_full 0.94x [0.90, 0.98], peak RSS 70.2 to 69.3 MB.
  - Hot, 30 rounds: empty run 0.85x [0.81, 0.89], peak RSS 35.2 to 32.1 MB; testthat_full 0.96x [0.90, 1.01], peak RSS 70.6 to 69.7 MB.
  - Held out of training, page cache dropped, 20 rounds: awfy `bounce` 0.97x [0.90, 1.03] and `storage` 0.95x [0.91, 1.00], typed `sales` 1.01x [0.98, 1.04] and `orders` 0.97x [0.91, 1.04], and package programs loading jsonlite 0.95x [0.91, 0.98], cli 0.93x [0.87, 0.99] and glue 0.95x [0.88, 1.04].
  - An earlier measurement with a shared package library (both sides ignoring stale code images) found the same direction: empty start 1320 to 800 image pages touched (4 KB), and callgrind's warm iteration unchanged, so the saving is in start-up.

## Young collection stress keeps survivors untenured

**Status:** built.

- **Gap.** `RY_GC_STRESS=1` runs a minor collection at every allocation. A minor collection ages each survivor into the old generation, and only a deeper collection can free an old object. So an object that survives one collection and is then held only by a Rust local stays safe under stress, but outside stress it is still young, and the next minor collection frees it. The detached argument reference in `eval::detach_reference` was such a bug, and plain stress hid it.
- **Shape.** `GcStress::Young` (`RY_GC_STRESS=young` or `startup-young`) runs an intermediate collection at every allocation, with `promote_bits` limited to `OLD` (`Heap::keep_untenured`), and tenures only every 64th time. An intermediate collection unmarks and re-traces every untenured object, so a survivor stays collectable. A minor collection without promotion is not possible: the mark bits are sticky and mean "old", so a marked object is skipped by the next minor collection. Major collections keep the plain mode's spacing. `Heap::trace` keys its remembered-parent bookkeeping off `Heap::deeper` (any collection that is not minor) instead of the promote bits, because an untenured intermediate still has to re-remember tenured parents.
- **Cost.** A small script runs about five times slower than under plain stress, so the default suite runs a bounded sample (`cases_survive_young_gc_torture`, and `fuzz_programs_survive_gc_torture` runs both engines). `just gc-torture young` and `tools/fuzz-deep.py` run the deep passes.
- **First catch.** A tenured weak-pairs vector left the remembered set when a deeper collection traced it while its value was still young. `trace_children` never visits weak pairs, so tracing found no younger child. The next intermediate collection then freed the value without clearing the pair. `trace_children` now checks a tenured weak-pairs vector's elements for younger children (heap test `deeper_collection_keeps_a_tenured_weak_pair_list_remembered`).
- **Quarantine.** Stress alone missed `datetime__year_overflow`'s stale argument reference: every sweep restarts each cursor at its page start, so a freed slot is handed out again at the next allocation and a stale read finds a valid object. While `RY_GC_STRESS` or `RY_GC_VERIFY` is on (`Heap::refresh_quarantine`, derived from the policy, no separate flag), every freed object is poisoned in any build (`0xDB`, GC bits clear) and held back from reuse: small slots sit in a per-page `quarantined` bitmap that `claim_run` treats as taken, with a FIFO releasing the oldest past 16 MiB, and large objects queue until 64 MiB. Quarantined slots are not in `allocated`, so marking, tenuring, images and the dead counts never see them; a page holding any stays with its class. In debug builds the kind byte is poisoned too, and `Sexp::kind` panics on a byte above `Free`. Release builds compile that check out and poison the kind byte as `Free` (`POISONED_KIND`), because the check in release cost about 10% of instructions on the applications suite. Tests check freed objects with `Sexp::is_poisoned`.
- **Quarantine costs nothing while off.** A stress-only feature must not tax the normal allocator, so the code that runs with quarantine off is the code without quarantine: `refill` tests the quarantine once and runs `refill_from::<false>`, whose inlined `claim_run` reads no held bitmap, or the cold `refill_quarantined`, `sweep_page` dispatches once per page to a `sweep_page_words::<false>` instance that has no poisoning code, and a sweep releases the quarantine before it frees anything so the single sweep-and-file loop stays. Reading the held bitmap inline, and poisoning code that writes through `self` inside the word loop (which makes LLVM reload the page's bitmaps every word), cost 0.1 to 0.2% of release instructions with quarantine off.
- **Quarantine.** Stress alone missed `datetime__year_overflow`'s stale argument reference: every sweep restarts each cursor at its page start, so a freed slot is handed out again at the next allocation and a stale read finds a valid object. While `RY_GC_STRESS` or `RY_GC_VERIFY` is on (`Heap::refresh_quarantine`, derived from the policy, no separate flag), every freed object is poisoned in any build (`0xDB`, GC bits clear) and held back from reuse: small slots sit in a per-page `quarantined` bitmap that `claim_run` treats as taken, with a FIFO releasing the oldest past 16 MiB, and large objects queue until 64 MiB. Quarantined slots are not in `allocated`, so marking, tenuring, images and the dead counts never see them; a page holding any stays with its class. In debug builds the kind byte is poisoned too, and `Sexp::kind` panics on a byte above `Free`. Release builds compile that check out and poison the kind byte as `Free` (`POISONED_KIND`), because the check in release cost about 10% of instructions on the applications suite, against 0.1 to 0.2% with it out. Tests check freed objects with `Sexp::is_poisoned`.

## Evaluated arguments are rooted, and `gc()` runs queued finalizers first

**Status:** built.

- **Previous shape.** The AST evaluator's `collect_evaluated_args` rooted the values of `...` but not those of named or positional arguments. Evaluating a variable returns its value unrooted, held only by the binding. So in `c(x, { x <- 5; 3 })` the second argument rebinds `x`, and the first argument's old value is then held only by the Rust argument list, which the collector cannot see. Plain stress hid this because the value had already been promoted, and only a rare major collection frees an old object. Young stress freed it (`jit__write_back_in_place` and `kernels__elementwise` segfaulted). Separately, `gc()` collected before it ran the finalizers that earlier collections had queued. A queued finalizer is a root until it runs, and its closure can reach a dead frame (through a forced promise's environment) that binds another dead object, so `gc()` left that object alive and finalized one environment instead of two (`bytecode__finalizer_under_wide_window`).
- **Rule.** Every evaluated builtin argument is rooted in the current handle scope as soon as it is evaluated, the same as `...` values. `do_gc` runs the pending finalizers before it collects, so `gc()` finalizes every object that was dead when it was called, however the earlier collections fell.
- **Pins.** `evaluator_survives_young_gc_stress` (interp, `test_rust_allocations`) and `gc_runs_queued_finalizers_before_it_collects` (libr, `test_finalizers`, which uses `ry_set_gc_stress(2)` for young stress). Both fail without their fix.

## Start-up and code images are keyed by build inside a base R tree

**Status:** built.

- **Previous shape.** The start-up image (`library/base/R/ry-start.image`) and each package's code image (`R/<package>.ryc`) had one name per tree, and an image loads only into the build whose identity (`RY_RUNTIME_ID`) its header carries. Assembling a build's R home rewrote the shared tree's start-up and base code images for that build, so every other build sharing `target/base-r` silently started the slow way, and the recommended packages' images (MASS, Matrix, ...) stayed with whichever build ran `just base-r`. `tools/bench/bench.py` refused to time two builds that shared a tree.
- **Weakness.** One file name stood for many builds' images, so the tree held one build's truth at a time and builds displaced each other; a per-build tree copy would duplicate about 50 MiB of identical sources to hold a different 54 MiB of images.
- **Target shape.** `crates/rcmd/src/images.rs` names every image by build: `library/base/R/ry-start.<build>.image` and `R/<package>.<build>.ryc` (also `sysdata.<build>.ryc`). A build that finds no current image of its own (at `just base-r`, `just start-image`, home assembly, or reuse of an assembled home whose image was pruned) writes its start-up image, the base packages' code images and an untrained install-style code image for every other package in the tree (`run_image_writer` in `crates/rcmd/src/base.rs`; a first start that writes them all takes about 2.6 s and peaks at 190 MiB), and leaves every other build's files alone. Writes go to `.partial` and rename, and happen under the tree's `.start-image.lock` with a re-check, so two processes starting at once write once. `crates/repl/build.rs` embeds only the building build's images in a release, and the release smoke test (`tools/smoke-test.sh`) fails when the first start of an installed archive writes or skips an image.
- **Prune rule.** After writing, under the lock, the writer records itself as newest in the tree's ledger (`library/base/R/ry-builds`, one build per line, oldest first, replaced by rename) and keeps the images of `KEPT_BUILDS` = 4 builds: its own and the 3 others the ledger orders newest. Builds on disk but missing from the ledger count as oldest, by name. The order never comes from file times, which `cp`, `tar` and archive extraction reset. Every process holds a shared `flock` on its build's start-up image until it exits (`images::hold_start_image`, taken before the image is checked), and the pruner removes a build only while holding an exclusive lock on its start-up image, so a running build's images are never removed; a start that loses the race finds the file gone and writes it again. Unkeyed names belong to runtimes that predate the keying and are never pruned. `fuzz_prune_keeps_the_most_recent_and_running_builds_whole` checks the exact kept set against random ledgers, file times and running builds.
- **A package installed by another build is named, not rebuilt.** After an upgrade or a runtime rebuild, every package in a user library has only another build's code image, so its functions compile as they run. ry prints one note per library and session that names those packages and says to reinstall them (`note_foreign` in `crates/interp/src/code_image.rs`), and `RY_IMAGE_LOG=1` reports each one as an unused image. Rejected: rebuilding the code image lazily at load. A session would then write into package libraries, which may be read-only or shared by concurrent processes, a first load would pay for compiling the whole package, and every runtime rebuild in a development checkout would leave another set of images in the corpus library with nothing to prune them. Scenario `install-foreign-code-image` pins the note.
- **Benchmarks.** `bench.py`'s `refuse_foreign_images` starts each compared build once before timing (which writes any missing images into the base R tree) and stops when a corpus or `R_LIBS` package has only another build's code image, naming the packages to reinstall. The shared-tree refusal and the header scan of the base tree it replaced are gone, since keyed names let builds share a tree. Every timed process runs with `RY_IMAGE_LOG=1`, and `spawn` fails one that prints `ry: image unused: <reason>` (an image skipped because it is missing, of another build, of another format, of another database version or damaged) or `ry: image written: ...` (a start that wrote images, such as after a prune). That per-run check is the loader's own verdict and catches what a name cannot: damage, a database reinstalled since, and a prune between the preflight and the run.
- **Impact.** Two builds compared on one tree both start from their own images, with no per-build tree and no silent slow start; `two_builds_alternate_starts_without_displacing_each_others_images` in `crates/ry/tests/test_perf.rs` pins it. Disk grows by about 54 MiB per kept build (a 16 MiB start-up image and 38 MiB of code images), bounded at four plus any build still running. No runtime semantics change; the runtime adds the note on standard error.

## Marking a traced object costs a few table loads

**Status:** built, except the "Measuring" advice to give each build its own R home, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Previous shape.** Every marked object paid for decisions recomputed from its kind: `element_size` and `shape` were `match` ladders, `locate` divided by a slot size looked up through a branch on the page class, `age` was an out-of-line call, the leaf check walked the kind ladder again, and `mark_child` wrote a `younger_child` field on the heap that `trace` read back. `vector_cells` computed each small vector's exact cell count from its length. On `records` the collector was 757M of 5.55G instructions, about 1,600 instructions per kept record.
- **Target shape.**
  - Kind facts are two 32-entry tables in `crates/heap/src/object.rs`: `KIND_TRAITS` packs the element width with a `LEAF` bit (no references besides attributes), and `SHAPES` gives the shape. `element_size`, `holds_no_references` and `shape` are one load each.
  - `SLOT_INDEX_MULTIPLIERS` in `space.rs` has 256 entries (zero for bytes that are no size class), so `locate` is a page-class load, a multiply and a shift with no bounds branch.
  - `Space::age` is inlined into `mark` and `mark_beside_tenured`, and returns whether the object is tenured so the caller sets the tenured bit.
  - `mark_child` returns whether the child is younger than the parent instead of storing it in a heap field, and `trace_children` folds those results into the decision to remember the parent. It skips `NULL` before touching the bitmap, because `mark_roots` marks `NULL` first and it is tenured from the first collection; roots go through `Heap::mark`, which never takes that skip.
  - A small vector counts the vector cells of its size class (`SLOT_VECTOR_CELLS`), as GNU R counts a small vector's `Vcells` by its size class; a large vector counts its exact length. The count only feeds `gc()`, `gcinfo` and `max_vector_cells`.
- **Impact.** The collector on `records` went from 619M to 457M instructions (-26%), the whole program from 5,153M to 4,987M (-3.2%); `report` -0.2% and `testthat` -0.5%, where the collector is a small share. A heap-only benchmark that keeps 100,000 named five-element lists through major collections went from 873M to 595M in `collect_keeping` (-32%). No trigger, root or barrier changed: `RY_GC_LOG` is byte-identical to main on all three workloads apart from times, and peak RSS is within 0.3 MB.
- **The floor for this design.** About 50 instructions per traced object remain, spread evenly over `locate`, the mark-bit test, `age` and the stack push and pop; on `records` the drain is 306M of the program's 4,987M. The structural options were weighed against a bar of another 2% of `records` and rejected without a prototype, because neither can reach it: prefetching the next object hides cache misses but adds instructions, and `locate` (`space.rs`, the page-class load, multiply and shifts) is about 60M of the drain, so even a bitmap layout that removed it entirely would save at most 1.2%. Reopen this only with a wall-clock target, where prefetching can pay.
- **Measuring.** The start-up image and the `.ryc` code images in an R home load only into the build that wrote them, so two builds sharing `target/base-r` compare one fast start against one slow start (7 MB of RSS and up to 17% of instructions on `report`). Give each build its own copy of the R home and run `ry-rcmd start-image <home> --libr <libR.so>` for each.

## Peak RSS is judged as anonymous, image and code size, not as one number

**Status:** built.

- **Previous rule.** A change's peak RSS, one number per workload, must not grow against main.
- **Weakness.** Release builds are fat LTO with no function ordering, so the resident pages of `libR.so`'s code depend on where the linker places hot functions. Two changes were blocked by this alone: one showed +2.6 MB peak RSS, all of it `libR.so` resident text (14.2 to 16.6 MB) with identical anonymous and start-image RSS; another showed +0.6 and +1.4 MB of file-backed RSS on two applications with equal anonymous RSS. The number mixed a real signal with layout noise of the same size.
- **Target rule.** `tools/bench/bench.py` reports anonymous RSS, start-image RSS and `libR.so` file RSS separately, plus the size of `libR.so`'s `.text` section (`size -A`). Anonymous and image RSS must not grow. Resident text is not judged per change; `.text` size is, and the reviewer accepts growth that matches the code the change adds. Resident text is checked on the shipped layout (`tools/layout/optimize.sh`, PGO plus BOLT) when a release is cut, and a regression there becomes a backlog item.
- **Impact.** The real memory signal (heap and image) stays strict, and the noisy one is replaced by a deterministic measure. `bench.py` reports the parts (`sample_memory`, which samples `/proc/<pid>/smaps` every 20 ms because a process's smaps cannot be read after it exits, and `text_size`). Recording the measured layout swing of a no-op change in `development/testing.md` is open work.
- `RssFile` depends on how the page cache holds the mapped files. A file that was just written is cached in large folios, and a fault maps a whole folio. A file read back after eviction is cached 16 pages at a time. The same build therefore shows megabytes more `RssFile` right after it wrote its images. `tools/bench/evict.py` walks `base-r` and ry's cache directory recursively and evicts both, and a comparison runs it for both builds first. `/proc/PID/pagemap` shows per mapping which pages are resident.

## A handler stack set aside is traced

**Status:** built.

- **Previous shape.** Signalling replaced `interp().handlers` with the stack below the matched entry while a calling handler ran, and `R_ToplevelExec`, `R_tryEval` and `on.exit` code replaced it with another stack. The replaced stack lived only in a Rust local or a libr thread-local, which the collector never traces. A handler closure that `.addCondHands` received is referenced by its entry alone, so a collection while the handler call was built freed it (young GC stress reproduces it every time).
- **Chosen shape.** `conditions::set_aside(handlers, restarts)` pushes the stacks onto `Interp::suspended_handlers`, which the root scan traces, and returns a guard that truncates the list back to its earlier length when dropped. Every site that hides a stack uses it: the three signalling loops, `run_on_exit`, the help server, libr's `try_eval`, and `ry_handlers_enter`, whose context entry owns the guard until `ry_handlers_leave` drops it. Truncation rather than pop keeps the list consistent when an error return drops a guard or a C context is left out of order.
- **Impact.** One `Vec` push and truncate per signal, no allocation once the list has grown. Pinned by `caught_twice` in `YOUNG_STRESSED`, `crates/interp/tests/test_rust_allocations.rs`.
