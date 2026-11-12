---
title: Install-time compilation of package closures
description: "Compiling every closure of an installed package to machine code at install, so package code runs natively from its first call"
---

This is a design draft, not a contract. It proposes how `ry install` compiles every closure of a
package to machine code, and how that code stays correct when a test suite rebinds the functions
it was compiled against.

## Why

Package code runs almost entirely in tier 0. T0's sampler puts the share of time in machine code
at 0.1% on `testthat_full`, 0.0% on rlang's tests and 2.2% on cli's (reported by T0, not rerun
here). Between 60% and 85% of the time is in bodies that the run-time JIT refuses as too rarely
called (`analysis::RARE`) or that never reach its entry threshold. Lowering the threshold does
not help: the optimum is about 3,000 entries, and at that optimum a per-session JIT reaches at
most 45% to 55% native time, because every compile is paid again in every process and most
package closures run a few hundred times per session.

Compiling at install moves that cost out of the session. Each package is compiled once, on the
machine that installs it, and every process maps the result read-only. The target is at least
80% of the time on `testthat_full`, rlang and cli in machine code, and a lower wall time and
instruction count on `testthat_full` than tier 0 alone.

## What exists

- **Code images** (`r-runtime.md`, "Code images of installed packages") hold each closure's
  tier-0 code beside the lazy-load database, and a native section that is mapped read-only and
  shared between processes.
- **Install-time native functions** (`crates/jit/src/ahead.rs`, `decisions.md` "Install-time
  machine code from whole-program facts, beside the JIT") compile scalar numeric closures with
  guessed argument types. They cover 57 of rlang's 1,281 closures, 112 of cli's 868 and 38 of
  testthat's 1,089, and measured neutral on real packages. They established the section format,
  the link table for runtime helpers, the manifest of checks, and the check epoch.
- **Tier 1** (`crates/jit/src/analysis.rs`, `codegen.rs`) plans any body that is not on the
  never-native list, runs the instructions it cannot compile through tier 0's steps
  (`Action::Step`), and speculates on the types and call targets that tier 0's feedback recorded.
  Its code embeds heap addresses as immediates, so it lives only as long as its process.

The gap between the second and third rows is the design: tier 1's coverage of general bodies,
with the second row's guarantee that the code needs nothing from the process that compiled it.

## The shape

`ry install` compiles every closure in the code image with tier 1's planner in a mode without
feedback, and writes the code into the image's native section. That code is called the
**install tier** below. It differs from run-time tier 1 in three ways:

- **It speculates on nothing it cannot test with a branch.** No feedback exists at install, so a
  value's type is known only from a literal, a declared annotation or a base builtin's result
  type. Every other value is a boxed `SEXP`. An operation on one tests the common case inline (one
  masked header-word compare for a plain scalar, as tier 0's quickened handlers do) and calls the
  generic helper otherwise. A function binding the code was compiled against is tested before each
  use, and a failed test takes the generic call path for that one call.
- **It has no deoptimization exits.** Because every assumption is a branch with a generic side,
  the code never has to leave for tier 0 in the middle of a body, so it needs no frame-state maps.
  Tier 0's activation and register window are the frame, as for tier 1 and the baseline, so stack
  introspection, `sys.function`, `parent.frame` and unwinding behave the same in every tier.
- **It holds no heap address.** Everything the code refers to is reached through a table filled in
  the running process, as described in "Position independence" below.

The run-time JIT stays. A body whose install-tier code turns out to be hot is handed back to tier
0 for a recording window and then compiled by tier 1 with feedback, as Bun pairs its
ahead-of-time code with its JIT ("Tiering with the run-time JIT" below).

## What the code assumes, and how each assumption is tested

| Assumption | Where it comes from | Test | When the test fails |
| --- | --- | --- | --- |
| A name in the package resolves to the sibling closure the image holds | locked namespace at install | the image's proof table, below | that call goes through the generic call path |
| A name resolves to a base function (`length`, `is.null`, `vapply`) | no binding of the name in the namespace or its imports | the same proof table, with the frames the lookup walks | generic lookup and call |
| A name resolves to an imported function | the imports environment at install | the same proof table, keyed on the import's binding | generic lookup and call |
| A callee carries no debug or trace flag | the closure at install | part of the proof | generic call, which honors the flag |
| A value is a plain scalar of some type | nothing | inline header test at each operation | the generic helper computes the result |
| A namespace variable that is not a function has some value | nothing: never assumed | none; read through the binding's cell each time | |

The proof table is per image, one row per name the image's code resolves outside its own frames:
the symbol, the closure or base function expected, and the cells of every frame the lookup walks.
A row is proven when every one of those cells is locked, the expected object is the one bound,
and the object carries no debug or trace flag. The table records the check epoch
(`interp::code_image::check_epoch`) at which it was last proven.

A use of a row is one load and one compare: the epoch equals the table's proven epoch, and the
row's proven bit is set. When the epoch has moved, the use calls `reprove(table)`, which rechecks
every row once and stores the new epoch, so a test suite that unlocks bindings a thousand times
pays a thousand linear passes over each image it calls into, not one per call. A row whose
bindings are unlocked is never kept proven, so it is rechecked on each use for as long as it stays
unlocked.

## The hazard: an assumption breaks while compiled code runs

Compiled code can be in the middle of a body when an assumption breaks. A test that mocks a
function calls code that calls the mocked function, so the code was proven before the mock and
runs after it. The rule that makes this safe is that **every operation that can run R code ends
any proof the code holds**: the code tests the epoch again before the next use of a proof table
row, not once at entry. Because the epoch moves on every event below, a running activation sees
the change at its next call.

| Event | What moves the epoch | What the running code does |
| --- | --- | --- |
| `assignInNamespace("f", g, "pkg")` | `unlockBinding` on `f`'s binding | the next call of `f` reproves its row, finds `g` bound instead of the image's `f`, and calls `g` through the generic path; every other row reproves and stays direct |
| `trace("f", quote(...), where = asNamespace("pkg"))` | the binding is unlocked and the closure replaced by a traced one, which also carries the trace flag | as above; the generic call runs the trace |
| `testthat::local_mocked_bindings(f = mock, .package = "pkg")` | testthat unlocks `f`'s binding in the namespace, its imports environment, its S3 methods table and the attached package environment, binds the mock in each, and on exit binds the original and then locks again | while the mock is bound, `f`'s row is unlocked, so each call rechecks it and calls the mock; after the restore the next reproof finds the original closure locked and keeps the row proven |
| `debug(f)`, `debugonce(f)`, `undebug(f)` | flagging a closure for the debugger | the row fails on the flag, and the generic call enters the browser |
| `unlockBinding` and `assign` from C (`R_unLockBinding`), or C clearing a frame's lock bit (`SET_ENVFLAGS`, `SETLEVELS`, as rlang and pkgload do) | the C entry points move the epoch | as for `assignInNamespace` |
| `environment(f) <- e`, `body(f) <- b`, `formals(f) <- a` | nothing: they make a new closure | the new closure has no install-tier code unless its body and formals match an image entry (the existing code-image rule) and its enclosure resolves every row the same way |

S3 and S4 dispatch, `match.fun`, `get`, `do.call` and every lookup through an environment the
code does not own make no assumption: they go through tier 0's helpers, which read the method
tables and bindings as they are.

An oracle case per row of this table pins the behavior against GNU R, and a scenario runs a
package's testthat suite that mocks its own functions with images on and off and compares the
output.

## Position independence

The code is compiled in one process and runs in others, so it may hold no heap address. Three kinds
of reference cover everything tier 1 embeds today (`codegen.rs` has 14 places that emit a pointer
immediate):

| Reference | Today in tier 1 | In the install tier |
| --- | --- | --- |
| Objects of the body: the call expressions passed to helpers, srcrefs, formals, constants | address immediate | load from the code object's constants vector at an index fixed at install; the vector is decoded from the same image, so its layout is the one the code was compiled against |
| Runtime helpers | function address immediate | the section's link table, placed 64 KiB past the code and filled when the section is mapped (built) |
| Heap objects outside the body: symbols, base functions, sibling closures, the proof table | address immediate | a per-image cell table, one row per object, resolved when a function first runs and read with one PC-relative load |

Start-image objects keep fixed addresses for one build, but an install-tier image must also work
when the session started without the start image (`RY_IMAGE=0`, a profile that forces the slow
start), so base objects go through the cell table like everything else.

Records stay out of install-tier code, as for native functions, because each process numbers
record layouts anew.

## Code generation

The planner and Cranelift code generator are tier 1's, with three changes:

- **A no-feedback mode** in which a register's type comes only from static facts, function
  lookups resolve through the namespace frames as installed, and a speculation becomes a guarded
  branch (`Action::Guarded`, a new action) instead of an exit.
- **Native actions for the operations package code spends its time in**, each with its generic
  side calling the tier-0 helper that implements it: variable reads and writes on slots and through
  cells; closure calls through the call-setup contract below; builtin and `.Internal` calls with
  their arguments in registers; `if` and `while` on a generic logical; `[[`, `$`, `[` and their
  replacement forms on plain lists and vectors; `missing`, `is.null`, `length`, `inherits` and
  the other predicates package code calls in nearly every function. Tier 1's refusals that exist to
  protect the session's compile budget (`RARE`, `MOSTLY_TIER0`, `BRIEF`) do not apply, because
  the install pays the compile; the refusal that remains is a body whose code would run slower than
  tier 0, which the report names.
- **An object module as the target.** Code goes into `ssa::ObjectModule` and through
  `ssa::section`, as native functions do, instead of a JIT module.

A body that the compiler refuses a slot table (`assign`, `eval`, `local`, `environment()`) runs in
a heap frame, and its install-tier code reads every variable through the environment's cells. Such
bodies are still compiled, because the dispatch they save does not depend on the frame kind.

`ry install --explain` lists, per closure, whether it compiled, the instructions left to tier 0's
steps, and the proof-table rows it uses.

## Calls

A call from install-tier code to a closure goes through the call-setup contract (`decisions.md`,
"Closure call setup: the 10x end shape"): `enter(plan, function, call, args_start, sys_parent)`
and `leave(depth, result)`. For a sibling, the plan is built at install, because the callee's
formals are known and fixed: the code writes each argument into its slot with no matching at run
time. Constants go in as values, pure arguments (`native::pure_value`) as values computed in the
caller, and everything else as pooled references over the caller's slots, exactly as tier 0
passes them. When the callee has install-tier code, `enter` jumps to it directly; otherwise the
callee runs in tier 0 on the window `enter` set up.

A call whose target is not a proven row uses the site's run-time plan, as tier 0 does.

## Tiering with the run-time JIT

Install-tier code counts entries and backward jumps in the body's profile words, as tier 0 does.
It records no type feedback. When a body reaches tier 1's entry threshold, its next entries run in
tier 0 with recording on, and tier 1 compiles it from that feedback and replaces the install-tier
entry; the same happens for a loop at its back-edge threshold, which the install-tier code leaves
at the loop header. That exit needs no frame-state map, because install-tier code keeps every
tier-0 register in the window at loop headers. A body that tier 1 refuses keeps its install-tier
code.

## Memory and load cost

- **Shared code.** The code is file-backed and read-only, so it counts toward a process's
  resident set only for the pages it touches and is shared between the workers of one machine.
  It never counts as anonymous memory.
- **Per-process state.** Each image's cell table and proof table are allocated on the image's first
  call into native code, sized by the manifest (8 bytes per cell, 16 per proof row). A function's
  entry address goes into its code object's constants when the code object is decoded; there is no
  per-function heap allocation like tier 1's `Compiled`.
- **Load.** Mapping a section and filling its link table is the only work at load. The manifest is
  read in place; a function's cells are resolved at its first call. Untrained base images measured
  7.5M instructions in `jit::ahead::load` on `testthat_full` because they did more than this at
  load, which is the cost this layout avoids.

The acceptance rule is the RSS rule: anonymous RSS and start-image RSS on `testthat_full` and
`empty` must not grow.

## Copied libraries: CPU and build

Library trees move between machines (an renv cache, a Docker layer, a shared NFS library), so a
native section must never run where it cannot.

- **CPU.** The install tier emits code for a fixed baseline, never the host's own features:
  x86-64-v2 (SSE4.2, POPCNT, CMPXCHG16B), the level `ssa::section` already targets
  (`TARGET_X86_64_V2`). The section records its target, and the loader checks it against the
  processor (`section::host_runs`) before mapping. A section for a level the processor lacks is
  ignored silently and the package runs in tier 0 and the run-time JIT, so a copied library never
  dies on `SIGILL`. Vector kernels that want AVX2 or AVX-512 select a variant at run time inside
  the runtime's own helpers, which the code calls through the link table, so the section itself
  stays at the baseline. aarch64 gets the ARMv8.0 baseline with its own target number once
  `ssa::section` has a linker for it; until then an aarch64 install writes no native section.
- **Build.** The whole code image is keyed by the runtime build that wrote it: the identity covers
  the ry commit, so it covers the link table's helper list and order, the Cranelift version and
  the tier-0 instruction set together. With build-keyed names (#490) the file name carries the build
  too (`R/<pkg>.<build>.ryc`), so builds sharing a library each keep their own image. The link table
  is also resolved by helper name, so a section that names a helper this build lacks is refused
  even under a matching identity. Base R's identity does not key the section: base objects are
  reached by name through the cell table, and a name that resolves to something else fails its
  proof row and takes the generic path.
- **On a mismatch** the section is ignored and the package runs in tier 0, silently, because a
  copied library is normal and not damage; `RY_IMAGE_LOG=1` (#490) names each image a session ignores
  and why. A damaged section (a hash mismatch) warns once on the console, as damaged images do today.
  The repair for either is to reinstall the package with this build; whether `ry` should offer a
  command that rewrites a library's images in place is an open question below.

## Packages without install-tier code

A package with no native section runs correctly in tier 0 and the run-time JIT, with no error and
no warning. That covers a library installed by GNU R (no code image at all), by an older ry or
another build (the image is ignored, above), on a processor below the baseline, and with
compilation turned off. The install tier only adds an entry to a closure's code; nothing else in
the runtime depends on it being there.

The switches, for debugging and for measuring:

| Switch | Effect |
| --- | --- |
| `RY_INSTALL_TIER=0` | a session ignores every native section; code images still give tier-0 code and streams |
| `RY_CODE_IMAGE=0` | a session ignores code images entirely (exists) |
| `R CMD INSTALL --ry-no-install-tier` | the install writes the image without a native section |
| `R CMD INSTALL --ry-no-code-image` | the install writes no image (exists) |
| `RY_INSTALL_TIER=force` | every call of a closure with install-tier code enters it and never tiers up, for the tests below |

## Base and recommended packages

`testthat_full` also spends time in base closures (`tryCatch`, `match.arg`, `vapply`'s R-level
callers, `structure`). Those live in the start image, not in a `.ryc`. The same compiler writes
their code into a native section beside the start image when `ry-rcmd start-image` writes it,
with the same cell-table rule, so a session started without the image still runs. This is the
second step, after package images measure as intended.

## Relation to native functions

The scalar native functions of `ahead.rs` remain as specializations: a closure whose arguments
match a native function's types enters it, and any other call enters the closure's install-tier
code. Both share the manifest, the link table and the proof table, which replaces native functions'
per-function check list.

## Robustness

These land with the code, per `robustness.md`:

- **Oracle package suites under the install tier.** The package suites run with
  `RY_INSTALL_TIER=force` against packages installed with native sections, and must match GNU R
  with no new skips.
- **One scenario per hazard-table row, triggered mid-activation.** Each scenario installs a small
  package whose function `outer` proves its rows, calls something that rebinds or flags `inner`
  (`assignInNamespace`, `trace`, `debug`, `local_mocked_bindings`, `R_unLockBinding` from C), and
  then calls `inner` again in the same activation. The output must show the new binding taking
  effect at that second call, and the original again after a restore.
- **GC stress.** The tier tests and the package suites above also run with the collector at every
  allocation (the `gctorture` engine) over install-tier bodies, which checks that every value the
  code holds across a helper call is in the window or rooted.
- **Damaged sections.** A fuzzer flips, truncates and extends the native section of a real image
  (manifest, cell table description, link-table names, code bytes) and loads it. Every input must
  load cleanly or be refused with the damage warning and run in tier 0; none may crash. The
  section's hash catches flipped code bytes; the parser checks every offset against the section's
  length. Pinned regression inputs land with any bug it finds.

## Measures

Each change is measured against main in one sitting, each side with its own base R, start image
and package library written by its own build:

- native share on `testthat_full`, rlang's tests and cli's tests, with T0's sampler;
- `testthat_full` wall time (paired rounds) and instructions (callgrind);
- the three engines of the oracle with no skips;
- anonymous and start-image RSS on `empty` and `testthat_full`;
- install time and image size for rlang, cli and testthat.

Native share counts a sample as native when the innermost frame that belongs to R code is machine
code (install tier, tier 1 or a native function), including the time in builtins and runtime
helpers that machine code called. It is pending T0's confirmation that its sampler classifies the
same way, so that the numbers here and in T0's tables mean the same thing.

The first build passes when, on the cloud VM:

| Measure | Pass line |
| --- | --- |
| native share, `testthat_full` / rlang / cli | at least 80% each |
| `testthat_full` instructions and paired wall time | below main |
| anonymous and start-image RSS, `empty` and `testthat_full` | not above main |
| image size (the whole `.ryc`) | at most 2x main's, for rlang, cli and testthat |
| install time of rlang | at most 2x main's, and at most 20 s more |

Install time and image size are also measured over the benchmark dependency set: every package in
`tools/bench/packages/lock.tsv` (the 27 package suites of `packages.txt` and their dependencies,
about 120 packages), installed by main and by the branch from the same
sources. The pass lines there are a total install time of at most 1.5x main's, no single package
above 2x main's or 60 s more, and a total library size of at most 1.5x main's.

## Open questions

- **Which operations to make native first.** The order comes from T0's sampler, taken on tier 0
  with the instructions grouped by opcode, so the first native actions are the ones with the most
  time behind them.
- **Install time.** Tier 1 compiles at about 10 µs per bytecode word, which for rlang's closures
  is a few seconds (an estimate, not measured). Compiling functions on several threads, each with
  its own Cranelift context, keeps a large package's install within that range.
- **Image size.** Machine code is larger than tier-0 code; how much larger, and whether rarely
  called closures should share a smaller generic stub, is measured on the first build.
- **A command to rewrite images.** A library copied from another build or CPU runs in tier 0
  until each package is reinstalled; a `ry` command that rewrites every image of a library in place
  would make that one step.
- **Packages loaded with `ry load`.** Their namespaces are not installed, so they get no
  install-tier code; whether the loader should compile and cache it is a separate question.
