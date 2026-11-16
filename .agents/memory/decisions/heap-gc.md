# Decisions: Heap and collector

The object model, allocation, the garbage collector, and start-up and code images.

## The runtime's object model, collector and native boundary

**Status:** built, except one-step promotion and the 16 MB large-vector budget, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes, and the small `Rf_eval` subset, which "The interpreter and base R" supersedes.

Code: `crates/heap` and `crates/libr` (C half in `src/native.c`, headers in `include/`). Spec: `development/design/r-runtime.md`, section 4.

**Two crates, split by who may see the layout.** `heap` owns layout, allocation, the collector, interning and frames, and knows no C; `libr` owns runtime state, the native boundary and every export. Its cdylib has the `libR.so` soname, because a GNU-R-built package needs that library and it must hold the whole runtime.

**One header, three payload shapes.** A 16-byte header: type code, collector bits, object and ALTREP flags, GNU R's 16 gp bits at GNU R's positions (packages test `LEVELS(x) & 8`), a 16-bit saturating reference count, the attribute pairlist. Vectors store length and capacity, so `INTEGER()` is an interior pointer at offset 32; everything cons-shaped is a triple. Rust-side atomic vectors start zeroed (uninitialized reads are undefined behavior in Rust); `Rf_allocVector` skips it.

**Binding cells are heap objects** (`SexpType::Binding`, code 26, unused by GNU R and never handed to native code). A cell's address is stable while referenced, so compiled code can watch one and hold it after `rm()`. Base keeps its cells on the symbols, as GNU R's `SYMVALUE`, so a base lookup is one load. Each frame keeps a 64-bit symbol filter (a removed binding leaves its bit set, which only costs a scan), and `INLINE_BINDINGS` (8) slots keep a frame in the 192-byte class (`crates/heap/src/env.rs`).

**A promise keeps its expression and carries its bytecode in the attribute slot**, which GNU R never uses for a promise. GNU R's `PRCODE` holding code would send every expression reader (`substitute`, `match.call`, deparse, C's `PRCODE`) through an accessor. The C API hides the slot (`ATTRIB`, `ANY_ATTRIB`, `DUPLICATE_ATTRIB` see none; `SET_PRCODE`/`SET_PRENV` drop the code; serialization writes none). Arguments worth compiling (`compiles_as_promise`) are compiled at the site's first closure call (`compile_promise_arguments`), not with the body: a builtin callee never pays, and eager compilation was exponential in nesting depth. `bytecode::run_promise` restores the srcref current when forcing began.

**`switch` compiles to one table lookup and a chain of identity tests** (`SwitchArm`, then `BranchUnlessSame` per arm), because every op in the JIT's control-flow graph has at most one jump target. Names match by identity first and by text only for another encoding or `NA` (`eval::switch_name_matches`). Rare shapes stay AST calls (`switch_compiles`), both paths read the selector through `eval::switch_selector`, and errors match GNU R's AST `switch`, since GNU R runs a closure's first call through the AST.

**A closure call runs one collection check for its frame**: `frame_environment` reserves the environment, cells and default promises (`Heap::reserve`) and `bind_arguments` uses the `_uncollected` allocators. Safe because the caller keeps argument values alive, defaults are reachable from the closure, and `match_args`' result is computed before the reservation.

**Compiled `x[[i]] <- v` updates in place** (`assign_element_in_place`, `bytecode.rs`) only when the result certainly equals what `[[<-`/`[<-` returns, with `complex_assign`'s sharing checks; otherwise it falls back to `complex_assign`. Pinned by `assignment__element_in_place`.

**The Linux release build is profile-guided.** `just package` on x86_64 Linux runs `tools/layout/optimize.sh`: an instrumented build of every crate linked into libR and `ry` runs `tools/layout/train.py`, the release build uses the merged profile, and BOLT lays out libR from a second training run. Held-out programs gain as much as trained ones, so the profile generalizes.

- **Profile flags go through `RUSTC_WRAPPER`** (`tools/layout/rustc-wrapper.sh`, `RY_LAYOUT_RUSTFLAGS`), not `RUSTFLAGS`, which changes every crate's `-C metadata` hash and so every v0-mangled symbol: the profile silently matched about 1% of functions (LLVM warns on hash mismatch, never on a missing record). Both builds keep `strip` off and use their own target directory, since cargo does not rebuild when only the wrapper's flags change; `-p libr -p rcmd` and `-p ry-lang` build separately, as feature unification differs.
- **The build fails on a stale profile** (`-pgo-warn-missing-function` over 1%, or any hash mismatch). `EMBEDDED_RUNTIME`/`EMBEDDED_BASE` are read through `black_box`, because a `const None` folded an arm away in the profiled build only.
- **BOLT needs LLVM 20+**: BOLT 18 produced a libR that segfaulted in blake3's AVX-512 assembly. Only functions the PGO profile saw run are instrumented (`tools/layout/hot_functions.py`), because all of libR's text needs over 14 GB; `objcopy --remove-relocations` drops the `--emit-relocs` sections that keep the symbol table alive through `strip`.
- **Benchmarks keep the plain build**, so a change is never measured with a profile collected before it. BOLT lays out libR only: `ry` runs a negligible share of instructions.

**S3 dispatch**: `decisions/interp.md`, "S3 methods are found through one row per method symbol".

**S4 dispatch keeps a polymorphic inline cache per generic** (`methods::S4Cache`), valid while the generic's `.SigArgs`, `.SigLength` and `.AllMTable` bindings hold the values seen; up to four entries keyed by the signature arguments' first classes, each holding the table's binding cell and method, so `setMethod`/`removeMethod` make it miss. Inherited methods are cached only once `.InheritForDispatch` stored them. The cache is a GC root, cleared when full (`s4__method_cache_invalidation`). NextMethod passes `.Class` from the class whose method it found and always defines `.Group`, as GNU R does.

**NextMethod runs from a method in slots**, finding the frame by position (`Call::frame_index`) and reading dispatch variables through `eval::pending_name`, so no environment is reified. A closure next method's `parent.frame()` is the method's caller and a builtin one runs in the method's frame, as in GNU R. `MadePromises` holds one counted reference per promise NextMethod makes, so an unforced promise held only there is known dead; otherwise a released dead promise looks forced (`dispatch__next_method_from_slots`).

**The collector: non-moving mark-sweep with sticky mark bits.** A survivor keeps its mark, so outside a collection "marked" means "old"; a minor collection marks from roots, the remembered set and the escaped set, and sweeps only pages allocated into since the last one. Moving was rejected: a moving nursery cannot coexist with raw pointers native code holds. Weak references follow ephemeron rules; the `CHARSXP` intern table is weak.

**Small objects live in bitmap pages** (`crates/heap/src/space.rs`): objects up to 2 KiB in one reserved range (64 GiB, halved on failure down to 1 GiB for capped address spaces such as valgrind's), 64 KiB pages of 17 size classes, with mark, allocated and finalizable bits in side bitmaps. Sweeping is word operations (`dead = allocated & !marks`) that never read a dead object except a dead environment, to drop its Rust-side frame. Empty pages return to a pool any class can take. Large objects are allocated individually and keep a header mark bit.

**libR keeps freed memory until `gc()`.** mimalloc's purge delay is `PURGE_DELAY_MS` (1e9 ms, `crates/libr/src/libr.rs`), because returning pages after 10 ms made the kernel re-zero them on reuse. Not -1, because mimalloc schedules purges only for a non-negative delay and `gc()`'s `mi_collect(true)` (`frame::release_worker_memory`) carries out scheduled ones, so `gc()` returns free pages as GNU R's does.

**Roots** are the `PROTECT` stack, the precious set, the handle stack, registered globals, interned symbols, the weak-reference list and every `RootSource`. A function that allocates while holding an unrooted handle must root it first; collection stress exposes a miss. The four root stacks are `RootStack`s with a scan watermark, so a minor collection marks only entries pushed since; permanent objects go through `Heap::add_global_root`. Under stress, majors are spaced by live-object count, which keeps total stress time linear.

**Peak memory comes from what the heap keeps alive, not from the allocator** (glibc `malloc` gave the same peaks).

- `Heap::mark` sets a leaf's bit without pushing it, or a large character vector grows the mark stack to megabytes; a non-leaf's attribute slot is always traced, which compiled promises depend on.
- Builtins write into the result vector rather than copying from Rust vectors.
- **A large vector beyond memory plus swap is refused up front** (`memory_ceiling`, `CannotAllocate`) on Linux unless `vm.overcommit_memory` is 1, because mimalloc reserves without committing, so `numeric(1e10)` would fail only when touched, where GNU R reports "cannot allocate vector of size". macOS and Windows have no bound, matching GNU R there.

**Removing a binding is O(1) amortized**: the slot keeps a sentinel symbol and the frame compacts once half its slots are removed. Shifting made `rm()` over a large frame quadratic.

**The handle scope roots what the API hands out, not every allocation, and only recently.** During a native call, a handle an API function returns goes to the handouts stack (`Heap::hand_out`), truncated on return. Rooting every allocation would keep all of `Rf_eval`'s garbage when native code loops over it.

- A call keeps only its latest `HANDOUT_WINDOW` (1,024) to twice that, dropping the older half in one `drain`. Invariant: ry allocates for native code only where GNU R does, so code correct under GNU R never needs an older handout. Results that would break it stay out of the window: a view of something GNU R stores in the argument (`FRAME`'s pairlist) is `Pinned` to the call's handle stack, and a field read (`CAR`, `TAG`, `PRINTNAME`, ...) is `Reachable` and takes no slot.
- Only the outermost API function hands out (`Runtime::api_depth`). A C context saves `api_depth` and the window base with the `PROTECT` depth, so a jump cannot strand handouts. Handouts have their own stack because the window drops entries from the middle of a call's region.

**Accessors are inline in `Rinternals.h` and mirrored in libR.** The header reads the kind byte, ALTREP flag, length, elements and cell fields at fixed offsets that `heap/src/object.rs` pins with const assertions, and the exported functions take the same fast paths before `enter`, so binaries built against GNU R's headers skip the boundary too. An opaque `SEXP` was rejected: it saves nothing, since every accessor becomes a real function. Element pointers into lists and character vectors go through libR ("Read-only element pointers escape too").

**Each native call owns a `PROTECT` region.** `UNPROTECT` below its base is an error (GNU R allows it silently); an imbalance on return warns and cuts the region back, so a leak never accumulates.

**The boundary rule.** Inside Rust every non-local exit is an `Err(Unwind)`. Native code runs in a C trampoline with a jump target (`ry_call_native` and per-signature trampolines). An API function runs under `enter`, which catches the `Err` and any panic, drops everything, then calls `ry_jump`, so the skipped frames hold nothing with a destructor, the condition under which `longjmp` over Rust is defined. API callbacks (`R_UnwindProtect`, `R_ToplevelExec`, `R_tryCatch`, ...) run from C. A jump restores its context's `PROTECT` depth and `R_alloc` watermark, as GNU R does. Wrapping every API function in C instead was rejected: every signature in two languages for no safety gain.

**Exporting C symbols from a Rust cdylib.** rustc's version script exports only Rust symbols, so `build.rs` writes a second one from `RY_EXPORT` markers in `native.c`, which lld merges (GNU ld rejects a second script). Variadic functions (`Rf_error`, `Rprintf`) must be C.

**Finalizers run at safe points, never inside a collection**: ready ones run when a native call returns, on `R_gc` and on `R_RunPendingFinalizers`, as if at top level.

**ry presents the version of the base R it runs** (revision at least 89194, which keeps data.table on its modern branches) with UTF-8 as native encoding (`decisions/libr-rcmd.md`, "The presented R version comes from the base package the runtime runs").

## An escape lasts until the outermost native call returns

**Status:** built.

- `Heap::retire_escaped` runs when the outermost native call returns (`crates/libr/src/boundary.rs`): native code may store through an element pointer only during that call, so each old escaped vector that may hold a younger child is then remembered, rescanned once and becomes ordinary. A permanent escape made every minor collection reread data.table's huge string columns after one `STRING_PTR`.
- During the call an old escaped character vector is not rescanned; a minor collection marks every young `CHARSXP` instead, sound because a `CHARSXP` points at nothing.
- **Minor budget**: the larger of `GcPolicy::young_limit` (8 MB) and half the old small objects, as GNU R's cons-cell trigger grows with its heap.

## Read-only element pointers escape too

**Status:** built.

- **Why.** data.table's `setcolorder`/`setorder` permute columns with `memcpy` through `DATAPTR_RO`. A large list is remembered per 128-element card, so moving a young column from a dirty card to a clean one hid it from the next minor collection. GNU R remembers whole objects, so the fix cannot rely on the read-only contract.
- **Shape.** Every pointer into a handle vector's elements reaches libR (inline `DATAPTR`/`DATAPTR_RO` only for atomic vectors) and escapes through `Heap::escape(vector, writable)`. A writable escape of an old vector remembers it at once. At retire an old vector is dirtied and remembered only if it may hold a younger child: it is remembered, or a collection ran during the escape (`Heap::escaped_collected`). Dirtying every escaped vector was rejected: data.table takes read-only pointers to every column it reads.
- **Reference counts.** Each counting escaped list gets an `EscapedList` entry that `retire_escaped` settles:
  - A writable escape snapshots the elements; retire counts what the list holds and releases what the snapshot names. A counted store in between counts the new value, adds it to the snapshot and releases nothing (`Heap::count_escaped_store`), so each value is released exactly once; releasing as usual could free a value another list holds. Collections mark snapshot entries until retire.
  - A read-only escape records an order-independent digest: a permutation keeps it, any other store changes it, and then every element is counted once more (counts err high, never free early). The first counted change under a digest takes the snapshot (`Heap::escaped_snapshot`), since the slot may hold an uncounted value.
  - A list that dies with an entry pending releases what the entry counted (`release_swept_lists`).
  - Rejected: deferring counts to retire (copy-on-modify would see stored values as unshared), patching the snapshot by index (wrong after a raw permutation), snapshotting read-only escapes (vctrs and purrr take `DATAPTR_RO` per call). Lookup is a linear scan because all header GC bits are taken.
- **Not covered:** a pointer kept past the outermost call, and ALTREP storage outside `data1`/`data2`. Packages built against older headers are refused (`decisions/libr-rcmd.md`, "A package ry compiled records its headers' binary interface").
- `RY_GC_VERIFY=1` checks the invariant before each non-major collection. Pinned by `element_pointers_keep_permuted_young_elements_alive` (`crates/libr/tests/test_native.rs`), `element_pointer_permutations_keep_the_write_barrier` (`test_gc.rs`), `datatable__setcolorder_young_columns`, `datatable__setorder_young_strings`, `native_cases_keep_the_write_barrier_under_seeded_gc_torture`.

## data.table's C runs at or below GNU R's time: barrier, pacing and string kernels

**Status:** built.

- Native code fills vectors one store at a time and allocates many large columns before filling them, so: the write barrier reads the parent's header OLD bit; `Heap::copy_elements` decides the barrier once per range (`Rf_duplicate` uses it); large vectors have their own young budget (`GcPolicy::young_large_limit`), so `fread` does not collect per column; the major limit shrinks by at most a fifth per collection, so an explicit `gc()` does not buy a full collection mid-operation.
- `duplicated`/`unique` hash an all-ASCII character vector by `CHARSXP` pointer, since interning makes equal ASCII text one object (`crates/interp/src/sort.rs`).
- **The ASCII flag is a hint only**: native code can set any gp bit through `SETLEVELS`, so only `flag_bits::VALID_UTF8`, set at interning and out of C's reach, licenses `from_utf8_unchecked`.

## What keeps a dropped object alive: registers, handles, caches and dying finalizers

**Status:** built.

GNU R finalizes an object at the first full `gc()` after its last reference goes, and R6, lgr's buffered appenders and processx depend on that. Each place below held a reference GNU R does not have.

- **Registers and handles.** `Op::EndStatement dst start end` clears a statement's temporaries (registers are allocated in stack order), and `bytecode::execute_code` releases the activation's handles after every instruction, safe because live values sit in registers between instructions. The JIT lowers it to `NULL` stores.
- **Inline caches** hold a start environment and cell, so their list carries `WEAK_PAIRS` (`Heap::mark_weak_pairs`, `clear_dead_weak_pairs` nils a dead pair, a cache miss). Emptying every cache per collection was rejected: every site would pay a full lookup after each one.
- **Tier-1 code** embeds its enclosure, so it is held through a weak reference keyed on the enclosure.
- **Removed binding cells** get the unbound marker, so a stale cached cell reaches nothing.
- **Dying finalizers.** `process_weak_refs` takes every dying reference off the list before marking any, as GNU R's `CheckFinalizers`; marking one at a time could revive another's key after the ephemeron fixpoint and sweep its value while it stayed registered.
- **Visibility.** Reading a variable can force a promise that decides visibility, so compiled code sets the flag before the load and treats it as unknown after a load that may force.
- `eval::eval_all` releases each expression's handles before the next; a pending compiled unwind is traced (`Interp::held_unwinds`), because a deoptimization exit allocates before tier 0 raises it.
- Pinned by the `evaluation__*finalizer*` oracle cases and `conditions__exiting_handler_from_compiled_code`.

## libR commits mimalloc arenas lazily, and handle vectors are not zeroed before their fill

**Status:** built.

- libR's own mimalloc sets `mi_option_arena_eager_commit` to 0 from an `.init_array` constructor (`crates/libr/src/libr.rs`): an eagerly committed arena is backed by transparent huge pages, so re-touching purged parts made the kernel zero and compact whole huge pages.
- `Rf_allocVector` allocates numeric vectors uninitialized (`Heap::alloc_vector_uninit`), as GNU R does, and `Heap::alloc_vector` zeroes only the header of a vector it fills with handles. Purging stays on, or resident memory would only grow.

## The young generation allocates by bumping a cursor

**Status:** built.

- **Shape** (`crates/heap/src/space.rs`, `heap.rs`). Each size class has a cursor over a run of free slots claimed by `claim_run`, marked allocated up front so an unhanded slot sweeps like a dead one; a collection first frees each run's unhanded tail (`release_cursors`). `Heap::allocate` is `#[inline(always)]` (trigger check, bump, prefix zeroing, header store); the collection check and `keep` slice live on the cold `allocate_small_slow`, and `refill` is cold.
- `small_trigger` is derived state refreshed only in `Heap::new`, after collections and in `update_policy` (which replaced `policy_mut`, so the policy cannot change without a refresh), to keep its computation off the hottest path.
- **The barrier reads `gc_bits::OLD`**, set at an object's first mark and never cleared: in a non-moving heap "marked once" is exactly "old", so the fast path `parent & (OLD | REMEMBERED) != OLD` is one load from a header the store touches anyway. `verify_barrier` cross-checks OLD against the mark bit.
- `Heap::reserve(bytes, keep)` runs one collection check for a batch the `_uncollected` allocators then fill. The reservation is a budget, not memory: overrunning it only moves the next collection earlier.
- **Size classes are the per-type pools**: triples share the 40-byte class, environments alone fill 192 bytes. Splitting the triple class by `SEXPTYPE` was rejected, because a call allocates promises, cells and conses interleaved and one cursor keeps them adjacent.

## The string intern table is a packed open-addressed array

**Status:** built.

- **Shape** (`crates/heap/src/strings.rs`). One `Vec<u64>` with linear probing; a slot packs the address divided by 8 (44 bits, enough for 47-bit user addresses) with 20 hash bits, so a probe touches the object only on a tag match. A table of pointers cost a cache miss per candidate, so `formatC` slowed as live strings grew while GNU R stayed level. Growth at 7/8 load counting tombstones keeps memory near GNU R's.
- **Dead young strings** are found through an `interned` bitmap beside the mark bits (`Space::set_interned`): a minor collection removes `interned & !marks` on touched pages, so no list of new strings is kept. Strings above the largest size class use the short `young_large` list.
- **One probe per new string.** `find_or_vacancy` returns the string or a `Vacancy` that `insert_at` fills. The `CHARSXP` allocation between them may collect, so the vacancy carries the table's `generation`, bumped by every fill, rebuild and `load_image`; a stale vacancy falls back to `insert`. A collection that only makes tombstones keeps the generation, since a tombstone or empty slot on the path is still a correct place for an absent string.
- **A `CHARSXP` is aligned to 8**, so a short string fits the 40-byte class.
- `values::StringMemo` stays in `substr` and `formatC`, whose results repeat: a small per-call table stays in cache.

## Symbols stay in a hash map keyed by their `CHARSXP`

**Status:** built.

- `Heap::symbols` is an `FxHashMap<Sexp, Sexp>` from print name to symbol; a lookup compares pointers and never reads the name.
- **Rejected: the string table's packed slots.** Packing the symbol cost a cache miss per hit on `PRINTNAME`; packed name keys with a parallel symbol array probed longer than hashbrown's control-byte groups at 7/8 load.
- **Avoid installs instead**: constant names come from `values::symbols()`, and `values::install_charsxp_text` installs a character element without copying. `fuzz_interning_agrees_with_a_reference_map` (`test_gc.rs`) checks both tables against `HashMap`s.

## `gc()`, `gcinfo()` and `gc.time()` report the collector's real counters

**Status:** built, except the single byte limit behind `gc()`'s trigger columns, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes.

- `heap::GcStats` keeps live objects (`Ncells`), live vector data in 8-byte cells (`Vcells`), their maxima since `gc(reset = TRUE)`, and collection time while `GcPolicy::timing` is on (`gc.time(TRUE)`). `Heap::usage` gives `gc()`'s table. `gcinfo(TRUE)` installs `GcPolicy::reporter`, which prints GNU R's one-line report after each collection while collection is still inhibited.

## Collections run on GNU R's schedule, and dead large vectors go back to the kernel

**Status:** built, except `heap_limit` bounding all bytes and the single old generation, which "Two old generations, separate vector and node limits, and a pool as large as the vector limit" supersedes.

- **Escalation.** A minor collection that leaves less than a fifth of `min_heap_limit` (64 MB, `R_VSIZE`) free runs a major one (`R_MinFreeFrac`). After a major, `HeapGrowth::next_limit` applies `AdjustHeapSize`'s fractions, never closer than that fifth to the need, or every later minor collection would escalate. `HeapGrowth::from_environment` reads GNU R's `R_GC_*` variables with its presets and ranges (`reference/cli.md`, "Memory").
- **The small-object trigger is half the old small bytes**, as GNU R's cons-cell trigger ignores vector memory; counting large vectors let small garbage pile up.
- **Vectors of at least `large::MAPPED_BYTES` (1 MB) get their own mapping**, unmapped when they die (`crates/heap/src/large.rs`). `MappedPool` keeps freed mappings and resizes them with `mremap`; with none long enough it grows the longest and unmaps the rest, so it never holds idle memory while pages are added. It is a pool of address space, not a cache: nothing in it can go stale. `MADV_HUGEPAGE` was rejected for it (huge-page zeroing and compaction); huge pages are used only by table operations (`decisions/frames-table.md`, "Table operations map their large blocks on huge pages").
- **libR's global allocator is `heap::large::Mapped<MiMalloc>`**, so a builtin's 1 MB+ scratch buffer goes back to the kernel instead of staying in mimalloc, where mapped vectors cannot reuse it.
- `rep`, `rep.int`, `rep_len` (`RepPlan`, `crates/interp/src/arith.rs`) and negative subscripts (`gather_kept`, `subset.rs`) stream positions instead of building an 8-byte position per element.
- Pinned by `runtime_peak_memory_stays_near_gnu_r` (`crates/ry/tests/test_perf.rs`).

## Two old generations, separate vector and node limits, and a pool as large as the vector limit

**Status:** built, except `jit_expand_sequence`: the JIT's `for` walks a compact `1:n` without expanding it (`jit_loop_sequence` in `crates/jit/src/runtime.rs`).

- **Why.** With one old generation a temporary live at one minor collection died only in a full one; GNU R's second old generation frees promoted temporaries without a full mark.
- **Generations** (`crates/heap/src/heap.rs`): a minor collection makes a young survivor `OLD`, a deeper one makes an old one `TENURED`. An intermediate collection frees untenured old objects, treating tenured ones as roots through the remembered set; a deeper collection empties the remembered set first and tracing re-adds each tenured object that still points at a younger one (`mark_child::<true>`). A full heap escalates minor, intermediate, major (`escalate_if_full`), as GNU R's `num_old_gens_to_collect`.
- **Limits.** `heap_limit` counts large vectors (`R_VSize`) and `node_limit` small objects (`R_NSize`, at least 32 MiB), each grown after a major. The need includes a fifth of the smallest limit (`R_MinVFree`), so a live set just under the grow fraction cannot pin the limit where every collection escalates. A major that escalation forced before a limit's worth of vectors was allocated counts as thrashing and grows the limit as if the survivors filled it, because each one tenures the in-flight working set.
- **`MappedPool::limit` equals the vector limit**: a smaller pool made large temporaries page-fault fresh mappings every call, and kept mappings never lift the peak, since the pool maps nothing while it holds a mapping.
- **Repeated strings.** Tracing a character vector skips already-marked elements through a 64-entry address table (`trace_children`): columns of a few repeated strings are common, and a skipped `CHARSXP` has no references.
- `runtime_speed_stays_ahead_of_gnu_r` (`crates/ry/tests/test_perf.rs`) holds each program above a floor; open work: `backlog.md`, "Open: collector throughput on the benchmark suites".

## Large temporaries: freed from their reference counts, tracing stays for cycles

**Status:** built.

- **Why.** With tracing alone, a temporary a minor collection saw live was promoted, so a loop's dead large vectors stayed resident until a major collection.
- **Shape** (`crates/heap/src/heap.rs`: `note_zero`, `reconcile`, `shallow_roots`, `free_released`). A counted store releases the old value; an object whose count falls to zero enters `Heap::zero_count` (a flat `Vec`, `ZERO_LISTED` header bit against duplicates) only if it is a non-ALTREP list or large atomic vector. Strings and small vectors stay with tracing, since their pages are not returned one at a time.
- **Reconcile only at an allocation** (inside `maybe_collect`), because C code may hold an unprotected `SEXP` until its next allocation, as GNU R allows. It runs past `RECONCILE_BYTES` (4 MiB) or 65,536 entries, sets `HELD` on everything the shallow roots reach one level deep (root stacks, root sources, weak references, fields of untracked objects (`untrack_references`, R's `DISABLE_REFCNT`), elements of escaped lists), and releases every candidate still at zero and not held: a list gives up its elements, a large vector's pages are freed at once.
- **Invariant: counts only overcount.** Environments, closures, promises and external pointers stay with tracing, which handles cycles (the shape of CPython's cycle collector and Nim's ORC), and a missed decrement only defers a free to tracing. A sweep decrements children only for swept zero-listed lists (`release_swept_lists`) and pending frames.
- A freed vector's bytes stay counted until the next collection recounts them; uncounting them delayed collections, and dead temporaries only a collection finds (forced promise values) piled up.
- `RY_GC_VERIFY=1` makes a reconcile free nothing and run a full collection, panicking if anything it would free is reachable; `fuzz_reconciles_free_only_unreachable_vectors` and `fuzz_temporaries_are_freed_and_live_vectors_kept` (`test_gc.rs`) run it.

## Frames released at return

**Status:** built.

- **Why.** Every binding of a dead frame lived until a collection found the frame dead, so dead `[.data.frame` frames held subset columns across majors.
- **Shape** (`Heap::release_frame`, `release_returned`, `clear_frame`, called from `run_closure` and `apply_internal_method`), following GNU R's `R_CleanupEnvir` and `unpromiseArgs`. Enclosures are counted stores, so a frame's count is exactly its heap references. At return, a frame that is not the result, precious or a symbol frame, and whose count equals its self-cycles (refcount-1 promises bound in it whose environment is the frame), is cleared at once.
- Otherwise it waits on `Heap::returned` with the closures it made (`Interp::frame_closures`, at most `FRAME_CLOSURES`) and is cleared at the next reconcile once no non-handle shallow root reaches it and its count equals its cycles plus its dead closures; only a root scan proves a closure dead, and one scan per reconcile is cheap where one per return is not.
- **Closures never count as self-cycles at return**: a root that counts nothing (native state keeping an R callback, such as the loader's deferred-S4 closure) may hold one. Counting them cleared the loader's frame under a live callback.
- **Arguments are not released after the call**: they travel in an uncounted slice, and an untracked cell may bind a promise without counting, so a zero count does not prove it unreachable.
- `RY_GC_VERIFY=1` checks each reconcile against a full collection.

## Start-up memory: release relocation tables and preflight code pages

**Status:** built.

- An `.init_array` constructor in `crates/libr/src/embed.rs` applies `MADV_DONTNEED` to every loaded object's `DT_RELA` table of 64 KiB or more (the loader never rereads it; lazy `DT_JMPREL` binding is left alone), and `ry run` advises away the preflight's code (`release_executable_code`, `crates/ry/src/main.rs`). Both are clean file-backed pages that fault back from the file.
- **Rejected:** RELR relocations need glibc 2.36; release binaries promise 2.35.

## The start-up image is build output of the base tree, and every start re-reads the environment

**Status:** built, except the single `ry-start.image` name that nothing writes at run time, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Decision.** `ry-rcmd build-base` writes the image as its last step into the base tree, in an emptied environment; a release embeds it and nothing writes it at run time. Its header carries `RY_RUNTIME_ID`, and another build's image is skipped silently (`just start-image` refreshes it).
- **Rejected: an image written at run time** and keyed by every input. It was a cache with a cache's failures: keys that missed inputs (a shared `.Random.seed`, stale databases outside R home, relative `R_LIBS`, warnings shown only on the writing run).
- **Every start re-runs the environment-reading steps** after the load (options, `.Machine`, `.Platform`, base R's profile, the `.onLoad` hooks of utils, grDevices and stats; `interp::session::start_from_image`). The image holds only namespaces and their attachment, which depend on nothing but `image::LOADING_VARIABLES`; setting one starts the slow way. Strings under the writer's R home are relocated (`heap::image::Relocation`). The writer fails the build if start-up warned or left `.Random.seed`. User packages get no image (`decisions/libr-rcmd.md`, "Installed packages load through native fast paths, not an image").
- Rust state holding heap objects across start-up must be a fixed object, a heap root, or saved by `image::save_state` (`start-up-image.scenario`).

## The start-up image carries the host's environment, regex automata are built lazily, and the heap uses huge pages

**Status:** built, except the `MAP_POPULATE` file mapping and the per-object image reader, which "The start-up image is the heap's pages at a fixed address, mapped in place" supersedes.

- `ImageHost::environment` builds `tools:ry` at image write; `image::save_state` carries it and `loader_backend::attach` takes it from `image::host_environment`.
- `Matcher::Automaton` holds a `LazyAutomata` built on first use with `dfa(false)`: the full DFA was most of the build cost and the lazy DFA searches as fast, and a `sub`/`gsub` with group references (backtracking only) never builds it.
- Page faults were the larger start-up cost, so the heap's reservation is 2 MiB aligned, commits 2 MiB at a time and is advised `MADV_HUGEPAGE`. `MADV_POPULATE_WRITE` on small pages was rejected: it pays the same per-page cost up front.

## The start-up image is the heap's pages at a fixed address, mapped in place

**Status:** built.

- **Why.** A per-object image decoded, per start, a result that already had a memory layout and was the same in every process.
- **Shape.** The first heap reserves its small-object space at `IMAGE_BASE` and the writer moves large objects to `IMAGE_LARGE_BASE`, so handles in the pages are valid as is. The writer collects, tenures everything and writes pages, per-page metadata and the large section; the loader maps them `MAP_PRIVATE | MAP_FIXED` (`crates/interp/src/image.rs`; Windows reads them). Soundness (`crates/heap/src/image.rs`, module doc):
  - **Same starting state.** `Heap::image_precondition` digests allocation state, table counts and fixed objects' addresses; equal digests mean the pages replace every object the loader holds. A mismatch, or a heap not at `IMAGE_BASE`, starts the slow way.
  - **No process-local addresses in the pages**: spilled frames, builtin-routine pointers and native payloads of fixed objects are cleared and restored from metadata.
- The header is checksummed, the pages are not, since reading them would fault every page in and undo sharing. The file is replaced by rename, because truncating a mapped file raises SIGBUS.
- **Mapping, not reading**: a private mapping faults lazily and shares unwritten pages across parallel test workers. Image objects are tenured, so marking writes no small-object header and collections leave those pages clean.

## Code images: compiled code beside the lazy-load database

**Status:** built, except the file name `R/<pkg>.ryc`, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Shape.** `R CMD INSTALL` writes a code image beside `R/<pkg>.rdb` (`crates/interp/src/code_image.rs`, driver `crates/rcmd/src/code-image.R`). A fresh session records every fetch from the package's databases (`ryCodeImageRecord`), loads the namespace, forces every binding, optionally trains (`--ry-train=tests|examples|<script>`, `ry install --train`), and compiles and writes each recorded closure (`ryCodeImageWrite`). A failure warns and leaves the package without an image. `RY_CODE_IMAGE=0` ignores images at load; `--ry-no-code-image` skips writing.
- **Identity without a second key.** A closure is named by its record's database offset plus the order the stream reader creates closures (`serialize::unserialize_with_closures`), which depends on the record bytes alone; the image is stamped with the database's length and modification time ("Lazy-load records are read in place from the code image") and with `RY_RUNTIME_ID` plus a layout magic, so another build reads it as absent, quietly.
- **No second copy of the source.** Every language object a code object points at is encoded as its preorder number in the closure's body (`serialize::walk_source`). Nested function bodies are compiled at install (`bytecode::compile_nested_functions`).
- **Decoded where compiling would happen, not at the fetch.** A fetch notes where each closure's code lies (`code_image::State::pending`, keyed by body, the body preserved so its address cannot be reused), and `bytecode::run_body` decodes it at the call that would compile. A namespace fetches many closures it never calls (every registered S3 method), and decoding costs about what compiling does, so untrained tier-0 code is not a speedup: the image's value is the trained profile.
- **Pending code is checked against formals.** Compiler output depends on formals (`local_symbols`, `effects::summarize`, `tail_updates`, the missing-subscript path), and `formals<-`/`as.function` share the body, so `take_pending` hands code only to a closure with the fetched closure's formals. Keying by closure would miss `environment<-` copies, which are safe to share.
- **A damaged image only costs its speedup.** An unreadable file, hash mismatch, mismatched record or rejected code warns once (`code_image::give_up`), marks the database as having no image and drops its pending code; raising an error made the package unloadable and retried at every fetch.
- **Decoded code is verified, not trusted**, because tier 0 indexes operands, registers and jump targets unchecked and the hash only detects accidents: `bytecode::verify` checks opcodes, operand bounds, jump targets against an instruction-start bitset, side tables and switch tables linearly. The encoder refuses Record ops (process-local layout ids), writes shape caches empty and masks constant gp bits to the S4 bit. `fuzz_code_image_load` runs mutated, rehashed payloads. Residual trust: register dataflow and the JIT.
- Images are written to `.partial`, fsynced and renamed, because a mapped file that shrinks raises SIGBUS; `lazyLoadDBflush` drops an image and its pending entries.
- **Training** keeps PROFILE counters, inline-cache hints and FEEDBACK words, so a hot body tiers up at the call that takes its code from the image. Compilation and cold-refusal counts are written as 0 (they describe the trainer's code); a final refusal is kept. Machine code and inline caches are process-local and not saved (`decisions/jit-ssa.md`, "No baseline tier, and code images hold bytecode only").

## Vectors grow in place into spare capacity

**Status:** built.

- **Shape.** Growth uses the heap's resizable vectors (`alloc_resizable_vector`, `resize_vector`), the flag `SETLENGTH` and `IS_GROWABLE` expose to packages. `subset::grow` extends an unshared, unclassed vector without dims in place when capacity allows, otherwise copies with 5% headroom, which keeps appends linear for a twentieth more memory; names grow alongside. Tier 0 compiles element stores with any lowerable subscript and two-subscript matrix stores (`Op::SubAssignMatrix`) to in-place stores (`decisions/jit-ssa.md`, "Computed subscripts stay in bytecode"); a formal is read at run time so a missing one still assigns every element. A text connection's lines are the R vector itself, grown by doubling.
- **Why.** Copying per append made append loops quadratic. Length and capacity live only in the heap object, and the collector sizes it by capacity.

## Card marking, frame watermarks and moved list growth

**Status:** built.

- **Cards for large handle vectors.** A list, expression or character vector too large for the small-object pages keeps one card byte per `CARD_ELEMENTS` (128) elements of its capacity after its data (`Sexp::cards`, `object.rs`), so a remembered large vector is not rescanned whole at every minor collection. `Heap::element_barrier` stays one load and compare for a young or remembered small parent; a large old parent takes the slow path on every store while remembered, so each store's card is dirtied. A minor collection marks only `CARD_YOUNG` cards and downgrades them to `CARD_UNTENURED` for a tenured vector, which only an intermediate collection reads (`scan_cards`). `RY_GC_VERIFY` checks every younger child sits in a dirty card.
- **Frame watermarks.** A spilled frame keeps `Spilled::scanned`, the bindings a minor collection already marked. Frames only append or compact (`retain` resets it), so a minor collection marks only appended cells (`Frame::unscanned_cells`, `mark_scanned`). The verifier checks every cell below the watermark is marked.
- **Moved growth.** `[[<-` that grows an unshared, non-ALTREP list as its final step moves the element pointers (`Heap::move_elements`) and empties the old list, so no counting pass runs; cards cannot help here, since each regrowth makes a young copy. Growth by `[<-`, or growth followed by work that can fail, still copies, because the original must stay intact on error. `release_swept_lists` releases a dead list's elements in one pass.
- **Peeked connection reads.** Unserialize serves small reads from the bytes a connection has buffered (`connections::peek`, `consume`) and consumes them before a `refhook` call, an ASCII byte read and at the end, so the connection stays exactly after the object (`Reader::settle`).
- Pinned by `test_gc.rs` (`a_frame_filled_across_minor_collections_keeps_every_young_binding`, `moved_elements_keep_their_counts_and_survive_in_the_new_vector`, the card test, the fuzzers) and oracle cases `builtins__list_growth_moves.R`, `serialize__connection_reads.R`.

## Inline small-vector allocation

**Status:** built.

- `Heap::alloc_vector` is `#[inline]` and allocates a vector of up to 512 data bytes of a common kind without leaving its caller: shift from `small_vector_shift`, class from `SIZE_CLASS_BY_WORDS`, then the young budget check and slot bump. It writes header and length words whole and initializes only the data (handles to `""` or `NULL`, numbers to zero). Everything else goes to `alloc_vector_general`.
- **Why.** The general path called `object_size` through the PLT and `memset` zeroed 32 header bytes the header writes overwrite anyway; the hot callers are one-element character vectors from `scalar_string_charsxp`.

## Inline marking for minor collections

**Status:** built, except the out-of-line `Space::age`, which "Marking a traced object costs a few table loads" supersedes.

- `Space::mark` is `#[inline(always)]` and only tests and sets the mark bit, with unchecked bitmap indexing (`locate` has bounds-checked the page, and a slot of the smallest class still has its bit in the last word). Large objects go to `Space::mark_large`. `Heap::mark_child::<TENURED>` replaces a runtime flag: `trace` picks the instantiation once per object, so a minor collection's loops carry no younger-child test. A character vector of up to 16 elements skips the dedup table, which costs more to clear than its repeats save.
- **Why.** A package load's objects almost all survive, so each minor collection meets mostly old, already-marked children, and the check per child was an out-of-line call through two bounds-checked lookups that stored the bit even when set.
- **Left open.** Collecting less often while a namespace loads, since its survivors are retraced only to be promoted (`backlog.md`, "Open: collector throughput on the benchmark suites").

## Minor collections touch only what changed

**Status:** built.

- **Finalizable frames only when they own memory.** `Frame::owns_memory` (spilled bindings, an index or a hash table) sets the finalizable bit at allocation, at image restore, and when `define`, `define_formals` or `set_frame_hashed` makes a frame own memory; nothing else gives a frame heap memory (the JIT only reads frames). Most closure frames fit their inline slots, so the sweep no longer reads every dead call frame.
- **Method caches log writes.** `MethodCache::written` logs the key of each entry stored into either method cache since the last collection (`dispatch::Written`). A minor collection marks the entries those keys still name (`Marker::is_minor`); any other walks every entry. The log is cleared only when `Marker::is_collecting`, because a reconcile also traces sources. It holds keys, not objects: logging objects marked entries replaced before the collection, and with only minor collections whatever they mark is promoted and never freed. Past 65,536 keys the log is dropped and the next collection walks everything.
- Freed-by-type counting is its own pass (`Space::count_dead_by_kind`), run only while profiling, so `--profile-summary` does not charge it to collection.

## Pretenuring lazy-load fetches

**Status:** built.

- **Shape.** `do_lazy_load_fetch` reads inside `Heap::begin_pretenuring`/`end_pretenuring` (`pretenured`, `crates/interp/src/serialize.rs`). `Space` swaps in a second set of cursors whose refills file pages in `pretenured_pages` instead of `touched`; the inline allocation paths are unchanged. No collection runs while pretenuring, and pretenured bytes stay out of the nursery budget, since the win is the collection they no longer trigger. The next collection (or `promote_young`) flushes them before marking anything: it ages each to `OLD`, remembers each with a younger child (`has_younger_child`), then marks them and files their pages. A large object is promoted when pretenuring ends, with the same check (dirty cards for a carded one). R code the reader calls (the `PERSISTSXP` hook, `findPackageEnv`, `..getNamespace`) runs under `outside_pretenuring` and allocates young.
- **Why the flush checks children instead of a barrier.** The reader stores into objects it just built without a barrier, and a pretenured object looks young until the flush, so the flush checks every child, keeping the old-to-young invariant exact. If nil is still young at the flush, it is marked first: tracing skips leaves, so an old leaf pointing at a young nil would later read as a tenured object holding an untenured one (fuzz seed 24).
- Pinned by `environments__pretenured_fetch` under `RY_GC_VERIFY=1` and stress, and `pretenured_objects_are_old_and_keep_the_young_objects_they_hold` (`test_gc.rs`).

## Lazy-load records are read in place from the code image

**Status:** built.

- **Shape.** The code image holds each recorded fetch's uncompressed serialize stream after the code payloads (streams last, so payload offsets stay 32-bit). The file is mapped read-only (`ImageFile`), so workers share its pages, and `code_image::read_image_record` unserializes straight from the map: no per-fetch inflate, no private copy of the `.rdb`.
- **The database file is the record's identity.** The image is stamped with the database's length and modification time (`DatabaseStamp`); any mismatch ignores the whole image, so a copy that drops modification times loses the image but never reads a wrong one, and without a modification time none is written. A per-record hash was a second identity and is gone. Each stream carries an FxHash checked on first read (`Image::streams_verified`), which catches damage or truncation and falls back to the database.
- **Base-priority packages get stream-only images** (`crates/rcmd/src/base.rs`, `BASE_CODE_IMAGES`, `ryCodeImageWrite(code = FALSE)`): a session from the start image fetches every record of every base `.rdx` by key, because start-up forces some bindings before recording starts and forcing bindings missed records. Full untrained base images (compiled and machine code) were rejected: mapping native sections and decoding cost more than compiling saved, plus disk.
- **Databases are numbered.** `code_image::Databases` keeps one column per fact indexed by database number, found by a pointer scan over the interned paths; a record is then a binary search over offset-sorted entries, since the key stays `c(offset, length)` (makeLazy's promise shape is visible to R). The `.rdb` is mapped; `lazyLoadDBinsertValue` appends then unmaps it (`serialize::forget_database`).
- **Rejected: mapping heap objects per package**: it needs a fixed address per package or relocation, plus fix-ups for per-process symbols and refhook environments. Unserializing from a mapped stream keeps one layout and one allocation path.

## Escalation prediction and the dense precious set

**Status:** built.

- **Why.** A program that builds and keeps a large structure made every intermediate collection free under 1%: what it could free (objects promoted since the last deeper collection) was exactly the structure being built, and the major after it traced them again.
- **Prediction instead of a fixed ladder.** `Swept.untenured_bytes` counts dead objects a deeper collection freed outside the tenured generation, and `Space::younger_old_bytes` the live younger-old bytes (marks without the tenured bit, by popcount). After each escalation `younger_dead_share` is the share found dead, updated only when the younger-old bytes exceed a twentieth of the minimum node limit. When old small bytes minus that share of the younger-old bytes would still leave the node limit full, escalation skips the intermediate collection. Vector pressure always tries the intermediate first, because large vectors die independently of small-object structure.
- **Only an intermediate collection teaches the prediction (settled).** Learning from majors made the policy alternate, because of nepotism: a dead tenured object keeps its younger children alive through the remembered set until a major, so what a major frees outside the tenured generation overstates what an intermediate can. After `RETRY_INTERMEDIATE_AFTER` (4) predicted majors in a row, one intermediate runs anyway to measure again; without that retry a workload that returns to short-lived objects never runs an intermediate again. Pinned by `an_escalation_learns_only_from_intermediate_collections` (`test_gc.rs`).
- **The carded path skips repeated strings too (settled)** through `Heap::mark_strings`: the repeat table keeps each element's younger-than-the-vector flag in the low address bit, and a repeat ORs it into its card. A cheaper mark test cannot replace the table (a strings-only bitmap still needs a range check and two reads; a header bit costs a cache miss per element), and moving interned strings to an untraced space would delay their death to a rare full collection. The table is dropped after the first card without a repeat, so distinct identifiers pay no misses. Pinned by `a_string_repeated_across_cards_keeps_each_card_dirty`, `a_vector_of_distinct_strings_keeps_a_later_card_dirty`.
- **Rejected: a nursery sized by survival**: little gain over the prediction, and a peak RSS rise every parallel worker pays.
- **Dense precious set.** `Precious { objects, counts }` plus `flag_bits::PRECIOUS`, so "is this frame preserved?" at every frame release is a flag test (every gc bit is taken, so the index cannot live in the header). `R_ReleaseObject` scans from the newest entry, as GNU R's list does; Rcpp and cpp11 preserve only their own list heads.
- **No precise remembered or escaped set.** Each marking source of a minor collection is far below a 2%-of-testthat_full bar (the escaped set is empty at minor collections); the largest piece is the work after marking (`backlog.md`, "Open: collector throughput on the benchmark suites").

## The start image keeps base R's closures compiled

**Status:** built, except the image layout, which "The start image is compacted in touch order at write time" supersedes.

- **Shape.** `bytecode::compile_namespaces_for_image` compiles every closure bound in a loaded namespace afresh, never runs it, and installs it as the body; the image keeps exactly that set and stores any other code object as its source. Code that ran during start-up carries the writer's inline caches, feedback, counters and slot hints, so it cannot be imaged as is. Inline caches are weak-pair lists, written empty anyway; `PROFILE[RECORDING]` follows whether the writer had a tier, and a process without one clears it at first entry. Unforced lazy-load bindings stay promises (`backlog.md`, "Unforced base bindings still compile at run time").
- **Why the heap image and not base `.ryc` code.** Decoding costs about what compiling does ("Code images: compiled code beside the lazy-load database"); heap pages are mapped as they are, and pages of never-called code stay in the page cache.
- **Rejected layouts:** own pages plus `MADV_RANDOM` (each touched page becomes its own read, so cold starts slowed); own pages without advice (no RSS gain, holes); ordering only the fresh code (start-up also reads objects allocated while compiling cold closures).

## The start image is compacted in touch order at write time

**Status:** built.

- **Shape.** `Heap::write_image` numbers every live object with a stable id (a depth-first walk from address-independent seeds, `number_for_image`), ranks objects by recorded first-touch order then id, and moves every movable small object into dense pages in that rank (`Space::compact_for_image`); large objects follow the same rank. Every handle field, frame cell, the intern table, the symbol map and the roots are forwarded (`forward_for_image`). This removes the holes start-up's collections leave and keeps never-run code off pages a cold start faults in. The format is unchanged.
- **What stays put.** Objects allocated before start-up (`Space::pin_allocated`, right after `image_precondition`, whose digest covers them), fixed objects, environments, ALTREP objects and symbols. Symbols stay because frame filters, slot indexes, hash tables and record layout tags hash their addresses.
- **The profile is beside the image, not in it.** `ids`, the touch files and `order` (`development/architecture.md`, "The touch profile") are build inputs only the layout build reads. Each names what it belongs to (`ids` and touch files carry the image's header checksum; `ids` and `order` the numbering digest), so `tools/layout/image_order.py` refuses a touch file for another image and the second write refuses an order for another numbering: a stale profile fails the build. Without an order the image is compacted in id order, the tested fallback (`crates/heap/tests/test_image.rs`, even seeds).
- **The order is regenerated, never committed.** Any change to what start-up allocates renumbers objects. `tools/layout/optimize.sh` records a profile against the binary it builds and writes the second image from it; a failing training run fails the build, as for PGO and BOLT. Rejected: an order keyed on an identity that survives renumbering, since most objects (promises, constants, pairlists) have no name and a content hash is costly and ambiguous for identical objects.
- **Merge rule.** An object's place is the lowest phase in which any run touched it (start-up, namespace load, other), then its earliest position as a fraction of that run's touches, so a long run does not push a short run's objects back.
- **Dense tables, not hash tables.** `Space::slot_index` numbers slots from a page-class snapshot taken before anything moves; seen and fixed sets are bitsets (`SlotSet`) and forwarding is a `Vec<Option<Sexp>>`. The loader has no slot numbering (building one walks every page per start), so its relocation table is a sorted `Vec<(Sexp, Sexp)>` the writer emits in address order.
- **An older-format image is stale, not damaged**: it starts the slow way silently and home assembly (`rcmd::refresh_start_image`) rewrites it (`crates/libr/tests/test_start_image.rs`).
- **The image bytes are a function of the session**, as reproducible builds and stable touch checksums need: the writer zeroes every byte live contents do not determine and rebuilds the intern table in address order (`StringTable::canonicalize`). Path strings make numbering depend on the install path's length, so training and the final write use the same base tree path.
- **A stale order is safe**: every live object is placed ranked or not, so it only costs compactness. No `MADV_RANDOM` advice ("The start image keeps base R's closures compiled").

## Young collection stress keeps survivors untenured

**Status:** built.

- **Gap.** `RY_GC_STRESS=1` runs a minor collection at every allocation, which ages each survivor into the old generation, so an object then held only by a Rust local is safe under stress but freed by the next minor collection outside it (`eval::detach_reference` was such a bug).
- **Shape.** `GcStress::Young` (`RY_GC_STRESS=young` or `startup-young`) runs an intermediate collection at every allocation with `promote_bits` limited to `OLD` (`keep_untenured`), tenuring only every 64th time, so a survivor stays collectable. A minor collection without promotion is impossible: mark bits are sticky and mean "old". Majors keep the plain mode's spacing. `Heap::trace` keys remembered-parent bookkeeping off `deeper` (any non-minor collection), not the promote bits, because an untenured intermediate still has to re-remember tenured parents.
- About five times slower than plain stress, so the default suite runs a sample (`cases_survive_young_gc_torture`); `just gc-torture young` runs the deep pass.
- **Weak pairs.** `trace_children` checks a tenured weak-pairs vector's elements for younger children, since tracing never visits weak pairs and the vector otherwise left the remembered set with a young value (`deeper_collection_keeps_a_tenured_weak_pair_list_remembered`).
- **Quarantine.** A sweep restarts cursors at page start, so a freed slot is reused at once and a stale read finds a valid object (stress alone missed `datetime__year_overflow`). Under `RY_GC_STRESS` or `RY_GC_VERIFY` (`Heap::refresh_quarantine`), freed objects are poisoned (`0xDB`) and held back: small slots in a per-page `quarantined` bitmap `claim_run` treats as taken (FIFO past 16 MiB), large objects to 64 MiB; they are not in `allocated`, so nothing else sees them. Debug builds panic in `Sexp::kind` on a poisoned kind; release builds poison it as `Free` (`POISONED_KIND`), because the check cost about 10% of instructions. Tests use `Sexp::is_poisoned`.
- **Quarantine costs nothing while off**: `refill_from::<false>` and `sweep_page_words::<false>` contain no quarantine code, and `refill`/`sweep_page` dispatch once; inline bitmap reads or poisoning through `self` in the word loop (LLVM reloads bitmaps every word) taxed normal builds.

## Evaluated arguments are rooted, and `gc()` runs queued finalizers first

**Status:** built.

- **Every evaluated builtin argument is rooted** in the current handle scope as soon as it is evaluated, as `...` values are. Evaluating a variable returns its value held only by the binding, so in `c(x, { x <- 5; 3 })` the first argument's old value was held only by the Rust argument list (`collect_evaluated_args`); plain stress hid it because the value was already promoted.
- **`do_gc` runs pending finalizers before it collects**, so `gc()` finalizes every object dead when it was called. A queued finalizer is a root until it runs, and its closure can reach a dead frame binding another dead object.
- Pinned by `evaluator_survives_young_gc_stress` (`crates/interp/tests/test_rust_allocations.rs`) and `gc_runs_queued_finalizers_before_it_collects` (`crates/libr/tests/test_finalizers.rs`); both fail without their fix.

## Start-up and code images are keyed by build inside a base R tree

**Status:** built.

- **Why.** One file name per tree held one build's images at a time, so builds sharing `target/base-r` displaced each other and silently started the slow way; a per-build tree copy would duplicate the identical sources.
- **Shape.** `crates/rcmd/src/images.rs` names every image by build: `library/base/R/ry-start.<build>.image`, `R/<package>.<build>.ryc`, `sysdata.<build>.ryc`. A build with no current image of its own (at `just base-r`, `just start-image`, home assembly, or reuse of a pruned home) writes its start-up image, the base packages' code images and an untrained code image for every other package in the tree (`run_image_writer`, `crates/rcmd/src/base.rs`), and leaves other builds' files alone. Writes go to `.partial` and rename under the tree's `.start-image.lock` with a re-check, so concurrent starts write once. `crates/repl/build.rs` embeds only the building build's images, and `tools/smoke-test.sh` fails when an installed archive's first start writes or skips an image.
- **Prune rule.** Under the lock, the writer records itself newest in the ledger (`library/base/R/ry-builds`, oldest first, replaced by rename) and keeps the images of `KEPT_BUILDS` = 4 builds. Builds missing from the ledger count as oldest, by name; order never comes from file times, which `cp`, `tar` and extraction reset. Every process holds a shared `flock` on its start-up image until exit (`images::hold_start_image`, taken before the check), and the pruner removes a build only under an exclusive lock on it, so a running build's images survive; a start that loses the race rewrites them. Unkeyed names belong to older runtimes and are never pruned. Pinned by `fuzz_prune_keeps_the_most_recent_and_running_builds_whole`.
- **A package installed by another build is named, not rebuilt**: one note per library and session says to reinstall it (`note_foreign`, `crates/interp/src/code_image.rs`; scenario `install-foreign-code-image`). Rebuilding at load was rejected: it writes into possibly read-only or shared libraries, makes a first load compile the whole package, and leaves unpruned images.
- **Benchmarks.** `bench.py`'s `refuse_foreign_images` starts each build once before timing and refuses packages with only another build's image; timed processes run with `RY_IMAGE_LOG=1`, and one printing `ry: image unused:` or `ry: image written:` fails, since only the loader's verdict catches damage or a prune between preflight and run.
- Pinned by `two_builds_alternate_starts_without_displacing_each_others_images` (`crates/ry/tests/test_perf.rs`). Disk grows by about 54 MiB per kept build, bounded at four plus running builds.

## Marking a traced object costs a few table loads

**Status:** built, except the "Measuring" advice to give each build its own R home, which "Start-up and code images are keyed by build inside a base R tree" supersedes.

- **Shape.** Every decision a marked object needs is a table load instead of a recomputation from its kind:
  - `KIND_TRAITS` and `SHAPES` (`crates/heap/src/object.rs`) make `element_size`, `holds_no_references` and `shape` one load each; `SLOT_INDEX_MULTIPLIERS` (`space.rs`) makes `locate` a load, multiply and shift with no bounds branch.
  - `Space::age` is inlined into `mark` and returns whether the object is tenured; `mark_child` returns whether the child is younger instead of storing a heap field. `trace_children` skips `NULL` before the bitmap (it is tenured from the first collection); roots go through `Heap::mark`, which never skips.
  - A small vector counts the vector cells of its size class (`SLOT_VECTOR_CELLS`), as GNU R counts small `Vcells`; a large one its exact length. The count only feeds `gc()`, `gcinfo` and `max_vector_cells`.
- **The floor for this design.** About 50 instructions per traced object remain, spread over `locate`, the mark test, `age` and the stack push and pop. Prefetching (adds instructions) and a bitmap layout without `locate` were rejected without a prototype, because `locate` is too small a share of the drain for either to reach a 2% bar. Reopen only with a wall-clock target, where prefetching can pay.

## Peak RSS is judged as anonymous, image and code size, not as one number

**Status:** built.

- **Rule.** `tools/bench/bench.py` reports anonymous RSS, start-image RSS and `libR.so` file RSS separately, plus the size of `libR.so`'s `.text` (`size -A`; `sample_memory`, `text_size`). Anonymous and image RSS must not grow. Resident text is not judged per change; `.text` size is, and growth matching the added code is accepted. Resident text is checked on the shipped layout (`tools/layout/optimize.sh`) when a release is cut, and a regression becomes a backlog item.
- **Why.** Release builds are fat LTO without function ordering, so resident `libR.so` text depends on where the linker puts hot functions; one number mixed a real signal with layout noise of the same size, and blocked changes on noise alone.
- `sample_memory` samples `/proc/<pid>/smaps` every 20 ms, because smaps cannot be read after exit.
- **`RssFile` depends on the page cache**: a just-written file sits in large folios and a fault maps a whole folio, so a build shows megabytes more right after writing its images. A comparison first runs `tools/bench/evict.py` for both builds.
- Open: record the layout swing of a no-op change in `development/testing.md`.

## A handler stack set aside is traced

**Status:** built.

- **Rule.** A handler stack replaced while a calling handler, `R_ToplevelExec`, `R_tryEval` or `on.exit` code runs must stay traced: a handler closure `.addCondHands` received is referenced by its entry alone, and a Rust local or thread-local is never traced.
- **Shape.** `conditions::set_aside(handlers, restarts)` pushes the stacks onto `Interp::suspended_handlers`, which the root scan traces, and returns a guard that truncates the list to its earlier length on drop. Every site that hides a stack uses it: the three signalling loops, `run_on_exit`, the help server, libr's `try_eval`, and `ry_handlers_enter`, whose context entry owns the guard until `ry_handlers_leave`. Truncation rather than pop keeps the list consistent when an error drops a guard or a C context is left out of order.
- Pinned by `caught_twice` in `YOUNG_STRESSED`, `crates/interp/tests/test_rust_allocations.rs`.
