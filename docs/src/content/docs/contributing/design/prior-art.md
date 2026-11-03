---
type: Reference
title: Prior art from new language implementations
description: "What Roc, Bend, goose and Vx, and the production JITs of JavaScriptCore (Bun), LuaJIT and V8, do best, and which of their ideas ry's R runtime adopts or rejects"
tags: [runtime, memory-management, compilation, jit, type-system, ffi]
status: draft
generated: { by: ry-research-agent/1, at: 2026-10-31T16:00:00Z }
verified: { by: ry-review-agent/1, at: 2026-10-31T16:30:00Z }
sources:
  - id: roc
    resource: https://github.com/roc-lang/roc
    title: "Roc repository: docs/langref, design.md, design/lambda-set-specialization.md"
    last_modified: 2026-10-31T14:11:55+01:00
  - id: perceus
    resource: https://www.microsoft.com/en-us/research/wp-content/uploads/2020/11/perceus-tr-v4.pdf
    title: "Reinking, Xie, de Moura and Leijen: Perceus, Garbage Free Reference Counting with Reuse"
  - id: bendrt
    resource: https://github.com/HigherOrderCO/Bend/blob/main/paper/BendRT.pdf
    title: "Taelin et al.: BendRT, a Parallel Runtime for CPUs and GPUs"
  - id: bend
    resource: https://github.com/HigherOrderCO/Bend
    title: "Bend repository: README.md, guide/EFFECTS.md"
    last_modified: 2026-10-30T17:50:53-03:00
  - id: goose
    resource: https://github.com/aardappel/goose
    title: "goose repository: README.md, docs/implementation.md, docs/reference.md"
    last_modified: 2026-10-30T13:15:56-07:00
  - id: goose-lang
    resource: https://github.com/goose-lang/goose
    title: "goose-lang/goose: a Go subset with a semantics in Coq"
  - id: vx
    resource: https://github.com/vx-lang/Vx
    title: "Vx repository: README.md, ROADMAP.md, docs/language_rules_for_a_parallel_compiler.md, www/book/src/quick-reference.md"
    last_modified: 2026-10-31T07:28:21-07:00
  - id: jsc-speculation
    resource: https://webkit.org/blog/10308/speculation-in-javascriptcore/
    title: "Pizlo: Speculation in JavaScriptCore (WebKit blog, 2020)"
  - id: jsc-b3
    resource: https://webkit.org/blog/5852/introducing-the-b3-jit-compiler/
    title: "Pizlo: Introducing the B3 JIT Compiler (WebKit blog, 2016)"
  - id: jsc-ftl
    resource: https://webkit.org/blog/3362/introducing-the-webkit-ftl-jit/
    title: "Pizlo: Introducing the WebKit FTL JIT (WebKit blog, 2014)"
  - id: bun
    resource: https://bun.sh/docs/bundler
    title: "Bun documentation: the bundler and bytecode caching (`--bytecode`)"
  - id: luajit
    resource: https://github.com/LuaJIT/LuaJIT
    title: "LuaJIT repository: src/vm_x64.dasc, src/lj_obj.h, src/lj_tab.c, src/lj_snap.c, src/lj_opt_sink.c"
  - id: v8-ignition
    resource: https://v8.dev/blog/ignition-interpreter
    title: "V8 blog: Firing up the Ignition interpreter (2016)"
  - id: v8-sparkplug
    resource: https://v8.dev/blog/sparkplug
    title: "V8 blog: Sparkplug, a non-optimizing JavaScript compiler (2021)"
  - id: v8-maglev
    resource: https://v8.dev/blog/maglev
    title: "V8 blog: Maglev, V8's fastest optimizing JIT (2023)"
  - id: v8-turboshaft
    resource: https://v8.dev/blog/leaving-the-sea-of-nodes
    title: "V8 blog: Land ahoy, leaving the Sea of Nodes (2025)"
  - id: v8-hidden-classes
    resource: https://v8.dev/docs/hidden-classes
    title: "V8 documentation: Maps (hidden classes) in V8"
  - id: v8-lite
    resource: https://v8.dev/blog/v8-lite
    title: "V8 blog: A lighter V8 (2019), lazy feedback allocation"
  - id: rust-tail-calls
    resource: https://github.com/rust-lang/rfcs/pull/3407
    title: "Rust RFC 3407: explicit tail calls (`become`)"
  - id: okf
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2
    last_modified: 2026-08-21T13:08:36-07:00
---

This page studies four recent language implementations, and then the production JITs of
JavaScriptCore, LuaJIT and V8, for ideas that ry's R runtime can use, and decides which ones it
takes. One constraint decides every verdict, and it comes from
[the runtime design](/contributing/design/r-runtime/): R's C API must work unchanged. Any object a
native package can reach is therefore a non-moving heap object with a reference count, reached
through a raw `SEXP` pointer, rooted by `PROTECT`, possibly an ALTREP vector, and possibly written to
by native code without regard for its reference count.

## What ry takes, in one table

| Idea | From | Verdict | Where it lands in ry |
| --- | --- | --- | --- |
| Precise reference counts, so the runtime "count is one" check succeeds more often | Roc (Perceus) | **Adopt** | JIT-compiled code; copy-on-modify for vectors and the table type |
| Borrowed arguments: a callee that only reads an argument leaves its count alone | Roc, Bend | **Adopt, as a speculation** | The JIT, for callees it can see entirely |
| Escape analysis, inspired by Bend's share inference | Bend | **Adopt, scoped** | Temporaries in compiled code that native code never sees |
| Call-site specialization of known closures, inspired by lambda sets | Roc | **Adopt, scoped** | `lapply`, `vapply`, `Map`, `Reduce` in compiled code |
| One IR feeding several backends | Roc | **Adopt** | Bytecode feeds the interpreter and the Cranelift tier |
| A fast dev backend beside an optimizing one | Roc | **Keep as an option** | The optional copy-and-patch baseline tier |
| Static dispatch only | Roc | **Reject** | S3, S4 and R6 are dynamic; ry speculates on the class instead |
| Bounds-check elimination over difference constraints | goose | **Adopt** | Subscript checks in compiled loops |
| Values built in their final destination | goose | **Adopt** | Result preallocation in fused loops |
| No heap, data stacks only, no reference counts | goose | **Reject** | Incompatible with `SEXP` identity and `R_PreserveObject` |
| Static multi-frame returns instead of an unwinder | goose | **Reject** | R's conditions and restarts are dynamic |
| Compile to one C file, call C through `extern fn` or spliced effects | goose, Bend | **Reject** | ry hosts the existing C ABI; it does not emit C |
| Even-split parallel forks with no work stealing | Bend | **Reject** | R code makes no balance promise |
| Residency of data as a tracked fact | Vx | **Adopt the idea, not the syntax** | Table columns: Arrow buffer or R vector, plus a lint |
| Content-hashed identifiers | Vx | **Adopt for on-disk caches only** | Cache keys for bytecode and runtime summaries |
| LLVM and MLIR as the JIT backend | Vx | **Reject** | Compile latency; Cranelift is the choice |
| A top level that cannot perform effects | Roc | **Adopt, planned for strict mode** | A strict library file's top level holds only definitions and pure constants |
| Purity inference: effectful types use `=>`, effectful names end in `!` | Roc | **Reject** | Freedom from effects stays a runtime body fact the JIT reads, outside the type |
| Macros | Vx (`macro_rules`), Bend (templates) | **Reject** | ry has no macros, for checking speed; R's own non-standard evaluation keeps working |
| Shapes as cache keys, adapted to frames: remember slot k, check the symbol there | JavaScriptCore, V8 | **Adopt** | Local variable reads, with no invalidation |
| Watchpoint sets per binding | JavaScriptCore | **Adopt** | Watched binding cells invalidate dependent compiled bodies |
| Polymorphic inline caches | JavaScriptCore, V8 | **Adopt, scoped** | S3 dispatch sites, keyed on the class vector |
| Dense, lazily allocated feedback vectors | V8 | **Adopt** | Feedback slots only for value-producing instructions |
| Fast mid tier on feedback, compiled off the main thread | V8 (Maglev) | **Adopt, scoped** | Tier 1 with lower thresholds |
| Baseline compiler reusing the interpreter's frame | V8 (Sparkplug) | **Defer** | The form of a baseline tier, preferred over copy-and-patch |
| NaN-boxing of values native code can see | LuaJIT, JavaScriptCore | **Reject** | A `SEXP` must be a pointer |
| Trace compilation | LuaJIT | **Reject** | Cranelift is a method JIT; promises make traces fragile |

## Roc

**What it is.** Roc ([roc-lang/roc](https://github.com/roc-lang/roc)) is a pure functional language
whose compiler is being rewritten in Zig. It has no 0.1 release yet.[^roc]

**What it does best for ry.** Roc's memory model is the closest match to R's own value semantics.
Lists and other builtins are reference counted, and a builtin that receives a value whose count is
one updates it in place, while a shared value is shallowly cloned first.[^roc] So `List.set` on a
unique list costs nothing extra. The older Rust compiler does this with the Perceus "functional but
in place" scheme (`inc_dec.rs`, `borrow.rs`, `reset_reuse.rs`), and its reuse still tests the count at
runtime.[^perceus] The Zig compiler replaces Perceus with its own pass (`src/lir/arc.zig`), which runs
after all specialization: it infers borrowed parameters, moves fields out of aggregates that die, and
reuses dying allocations as the destination of a new value. In optimized builds it also infers
uniqueness, so where it can prove a value is unique it removes the `refcount == 1` check
altogether.[^roc] Roc's alias analysis (Morphic) was built to decide in-place updates for list
operations, but every optimization level calls its trivial solver, which answers "immutable", because
of a loop bug and little measured gain.[^roc] So in practice uniqueness is either proven by the ARC
pass or checked at runtime, which is R's copy-on-modify contract (R decides by `MAYBE_SHARED`, which
reads the count) with a compiler that keeps the counts tight.

When a function only reads an argument, passing it leaves its count unchanged, and the compiler works
this out by itself.[^roc] Higher-order functions compile by **lambda set specialization**, a form of
defunctionalization: by code generation time a closure is a tagged union whose tag names the
procedure and whose payload is the captured environment, so calls through it are direct (Roc also
has boxed erased callables for the cases this does not cover).[^roc] The Zig compiler monomorphizes
by default, and it has an experimental `--specialize=no` mode, `.boxy`, that passes runtime type
descriptors instead, which trades speed for compile time.[^roc] Its inference is Hindley-Milner,
and its only ad hoc polymorphism is static dispatch on nominal types; dynamic dispatch is
unsupported by design.[^roc]

Roc has an interpreter, a dev backend that emits machine code directly for fast builds (also for
wasm32), and LLVM for optimized builds, all fed from shared lower-level IRs.[^roc] Its platforms
supply the allocator: the host implements `malloc` and `free` for the Roc code it embeds, which lets
a web server allocate each request into an arena and reset it afterwards.[^roc] Roc has no stackless
coroutines or continuation transform: its interpreter recurses on the native stack, tail calls
compile through tail-recursion-modulo-cons and a trampoline, and asynchronous IO lives only in hosts
(basic-cli blocks on a tokio runtime for each hosted call).[^roc] Effectful functions are direct calls
into the host through the C ABI.

**What ry adopts.**

- **Monomorphized natives with an unboxed calling convention.** A closure typed `#: (x: double) ->
  double` compiles, at its first call, to a native function per argument type list. Its arguments and
  result travel as machine words, with no promises, no environment and no argument matching, and only
  the boundary to untyped code boxes them. Records, tuples and tags travel as pointers whose field
  types come from an allocation-site profile, and `match` compiles to a branch on the layout id. This
  is Roc's specialization applied at runtime, where R loads code: the cache key is the closure and
  the argument types seen, not a whole-program instantiation (`decisions.md`, "Typed closures run as
  native functions over records, tags and match").
- **Precise counts in compiled code.** The interpreter keeps R's count updates at every binding,
  which is what native code observes. The JIT tracks counts precisely inside a compiled function and
  drops increments and decrements that cancel. Three rules keep this correct for R. A binding in an
  elided environment still counts as a reference, so `y <- x; x[1] <- 0` copies. Every elided count is
  materialized before a native call. Deoptimization rebuilds the exact counts. The effect on a loop
  like `for (i in ...) x[i] <- v` is that it copies at most once, on entry if `x` is shared, and the
  per-iteration sharing check moves out of the loop. (GNU R already updates an unshared local in
  place; the gain is the hoisted check and dispatch.)
- **Call-site specialization of known closures.** R code passes functions to `lapply`, `vapply`,
  `Map` and `Reduce` constantly. When the JIT sees `vapply(xs, function(x) x * 2, double(1))` with a
  known closure, it compiles a loop that calls the closure directly. This is ordinary callback
  inlining with a deoptimization exit, inspired by lambda sets rather than a copy of them, because R
  semantics require the closure object to exist if anything reflects on it. It covers base R's
  higher-order functions, which ry implements itself. `purrr::map` runs its loop in purrr's own C
  code, so the JIT cannot specialize it.
- **One IR for every tier.** ry's bytecode feeds the tier 0 interpreter, and tier 1 compiles from
  the same bytecode (through its own SSA form) to Cranelift, as Roc's backends share their
  lower-level IR. Roc's fast dev backend corresponds to the optional copy-and-patch baseline tier in
  the runtime design.

**What ry rejects.**

- **Static-only dispatch,** because R's S3 dispatch on `class(x)`, S4 generics and R6 methods are
  dynamic by definition, and existing code depends on that. ry gets most of the benefit by
  speculating on the class seen at a call site, guarded by the class attribute.
- **Whole-program monomorphization,** because R loads code at runtime. ry specializes per closure and
  argument type list at its first call instead.
- **Runtime type descriptors in place of specialization** (`.boxy`), because ry's tier 0 interpreter
  already plays that role and the native tier exists to remove it.
- **Purity inference.** Roc infers which functions are effectful, lets only those call each other,
  and so guarantees that a top-level constant performs no effect and can be evaluated at compile
  time. It has no effect polymorphism, so a higher-order function exists twice, as `Try.map_ok` and
  `Try.map_ok!`.[^roc] R cannot split `lapply` or `Map` that way, effect polymorphism would put an
  extra variable in the types R code calls most, and an inferred effect in a function's type would
  let a body edit change every caller's interface. ry takes the guarantee without the inference:
  strict mode is planned to limit a library file's top level to definitions and constants that call
  only functions declared pure.
- **A host-provided allocator as a design principle.** R already has the parts it needs: `R_alloc`
  is an arena by R's own contract (freed when the foreign call returns), and `allocVector3` with an
  `R_allocator_t` is R's hook for custom vector allocation. ry implements both as specified.

## Bend

**What it is.** Bend ([HigherOrderCO/Bend](https://github.com/HigherOrderCO/Bend)) is Higher Order
Company's pure functional language with an affine dependent type system. Its current runtime is no
longer the interaction-combinator evaluator of HVM: the runtime paper says it "keeps the goals and
drops the mechanism", replacing graph reduction with ownership.[^bendrt] The compiler emits one C
file that runs on one thread, on CPU threads, or on the GPU through Metal or CUDA, over one heap,
with no garbage collector.[^bendrt] Bend also checks proofs of user-declared laws (`LAWS.bend`),
which is outside ry's scope.[^bend]

**What it does best for ry.** Bend's runtime is an argument that knowing ownership statically
removes most runtime costs. Because the type system is affine, a value has one owner, and the `match`
that consumes it frees it, so deallocation is compiled code.[^bendrt] Two whole-program analyses
decide the remaining memory traffic.

- **Share inference** finds the constructor types that need reference counts at all. A type is "hot"
  when some binder of it is used more than once, when a hot type's live field reaches it, or when it
  is boxed in an array; every other type is built and consumed with plain stores and loads.[^bendrt]
- **Borrow inference** finds the arguments a callee only reads. Each argument starts borrowed and
  flips to owned on any escape, to a fixpoint, and a borrowed argument is lent raw, so "a fold over a
  reused tree costs no count traffic".[^bendrt]

A term is one 64-bit word, and small values (a machine word, a constructor with one word-sized
field) are packed into the word and never touch the heap.[^bendrt] Parallelism is explicit: a
parallel `let` that binds several calls means, in the paper's words, "run these calls in parallel;
they split the work into roughly equal parts", and the scheduler trusts that promise, with no shared
queue, no locks and no work stealing.[^bendrt] On the authors' benchmarks, the sequential build runs
within 0.8 to 1.5 times the time of hand-written C, sixteen threads give 9 to 12 times one thread,
and an integrated GPU up to 67 times.[^bendrt] Foreign functionality enters through effects: an
effect is a definition of type `IO(R)` whose body imports a `.c` (and a `.js`) file that registers a
C implementation, which the compiler splices into the generated program.[^bend]

Bend's runtime is also stackless: user code never grows the C stack. A call compiles to a tail call,
a cut or a fork of a segment (with `preserve_none` and `musttail` in the generated C), a pending
continuation is a heap node, and IO is written in continuation-passing style.[^bendrt] Its
predecessor HVM2 packs a term into 32-bit ports with 24-bit numbers, and balances work through
per-thread redex bags that neighbouring threads steal from.[^bend]

**What ry adopts.**

- **Borrowed arguments, as a speculation.** Bend can borrow soundly because it is affine and its
  analysis sees every escape. R's escape facts are not sound in general: a callee can capture an
  argument through `<<-` or `assign`, by storing it in a list or attribute, through a closure over
  its frame, through `parent.frame()`, `sys.frames()`, `eval` or `delayedAssign`, through the promise
  itself (`substitute`, `match.call`), or in native code (`R_PreserveObject`, an external pointer's
  protected slot). If a borrowed value were captured, a later in-place update by the caller would
  mutate the captured copy. So ry borrows only when the whole callee is compiled and visible, treats
  any call to unknown R code or to `.Call` or `.External` as an escape, and undoes the borrow on
  deoptimization by restoring the counts. The runtime design's body facts (section 8 there) are where
  the "only reads" fact will live; the checker does not compute it today.
- **Escape analysis for temporaries, inspired by share inference.** Share inference classifies
  types, which does not transfer to R. What does transfer is its premise that a value nobody else
  can see needs no count. A vector that a compiled loop creates and consumes, and that never reaches
  an environment, an attribute or a native call, needs no count, and often no heap object at all.
  The JIT's scalar replacement and fusion are meant to handle that case.

**What ry rejects.**

- **Ownership as the memory model.** R values are freely aliased, and the C API exposes aliasing
  directly (`R_PreserveObject`, external pointer protection slots, environments). Affine typing
  cannot be retrofitted onto existing R code, so ry keeps a tracing collector plus reference counts
  and uses ownership only as an optimization inside compiled code.
- **Even-split forks without work stealing.** The balance contract suits a language designed around
  it, but R's parallel code (`foreach`, `parallel`, `future`) makes no such promise, and table
  operations need a work-stealing pool, which Polars already brings.
- **Effects spliced into generated C,** because ry hosts existing compiled packages through the C
  ABI rather than emitting C.

## goose

**What it is.** This page studies [aardappel/goose](https://github.com/aardappel/goose), a
memory-safe systems language by Wouter van Oortmerssen.[^goose] The name is ambiguous. The most
prominent "goose" on GitHub is Block's AI agent, which is not a language, and `goose-lang/goose` is a
subset of Go with a semantics in Coq, built for verification rather than as an implementation to
learn runtime techniques from.[^goose-lang] aardappel/goose is the one that matches the comparison.

**What it does best for ry.** goose has no heap at all. Every dynamic value lives inline on data
stacks the compiler assigns statically, growth advances a pointer, and memory is freed when its scope
ends. There is no allocator, garbage collector, reference counting or destructor, and the type rules
make the model memory safe without lifetime annotations.[^goose] Values are built in their final
destination through any depth of calls, so `items.push(parse(line))` writes the parsed record
directly into the array.[^goose]

Its bounds-check elimination pass removes every check it can prove cannot fire. It uses a
flow-sensitive domain of difference constraints, facts of the form `l <= r + c` over integer
variables and array lengths, and answers a query as a shortest path over those facts. Every base,
whether a variable or an array length, carries a generation, so a fact dies when the variable or
array it describes changes.[^goose] The pass is "required
only to be sound": a program's meaning never depends on it.[^goose] The README reports that over
sixteen benchmarks goose runs 1.16 times as fast as hand-optimized C++ and 1.12 times as fast as the
best safe Rust, while using less memory.[^goose] It compiles to one C file and calls C through
`extern fn`, with a bundled TinyCC backend that compiles and runs a program in-process.[^goose]

**What ry adopts.**

- **Bounds-check elimination over difference constraints.** R's subscripts are checked (an
  out-of-range read with `[` returns `NA`, and an out-of-range write extends the vector), so a
  compiled `for (i in seq_along(x)) y[i] <- x[i] * 2` carries checks on every iteration unless they
  are proven redundant. goose's domain fits: `for` evaluates `seq_along(x)` once, so `i` ranges over
  `1..length(x)`, and after preallocation `length(y) >= length(x)`, which removes both checks. The
  facts need goose's generations, because rebinding `x` or `y` in the body kills them. Only positive
  in-range subscripts are bounds facts. Zero, negative and `NA` subscripts, `[[` (which errors out of
  range), and a classed vector with its own `[<-` method all stay behind guards. Like goose's, the
  pass need only be sound.
- **Construction in place.** When the JIT fuses `z <- x * y + 1`, it allocates `z` once, at its final
  size, and the fused loop writes into it. When a function's result is assigned straight into a list
  element or a column, the JIT can pass the destination down.

**What ry rejects.**

- **The heapless memory model,** because R needs stable object identity and native code needs
  pointers that outlive scopes (`R_PreserveObject`).
- **Static multi-frame returns,** because R's condition system is dynamic.
- **Shared-nothing threads as the language model,** because R code already shares through
  environments, and ry keeps R-level evaluation single-threaded.
- **In-process C compilation as the build path.** It is attractive for compiling package code
  quickly during development, but it does not replace building packages with the platform's real
  compiler, which the C++-heavy part of the benchmark dependency set requires.

## Vx

**What it is.** Vx ([vx-lang/Vx](https://github.com/vx-lang/Vx)) is a research systems language,
written in Rust and compiling through MLIR and LLVM, that puts hardware topology and memory placement
into the type system. Its README describes it as an early research compiler at version 0.0.3.[^vx]

**What it does best for ry.** A Vx tensor's type carries its element type, its dimensions and its
residency, for example `Tensor<f32, [4, 4], Memory::GPU_HBM>`, and the compiler rejects host code
that reads device memory, transfers with no declared hardware path, and working sets that exceed a
device's declared capacity.[^vx] Machines are described declaratively (memory capacities,
bandwidths, links), so a program can be checked against a target it is not running on.[^vx] Its
frontend identifies every symbol and instantiation by a 256-bit content hash and type-checks modules
in parallel without locks, with output that is byte-identical across thread counts.[^vx] An identifier
packs a module hash, a symbol hash, a generic hash and flags; an instantiation gets an arena index,
and content digests are used only inside evaluation, never merged into the identity.[^vx] Its "JIT"
compiles the module with `llc` to an object file, links it with `clang` and runs the executable;
`lli` appears only in its roadmap.[^vx]

**What ry adopts.**

- **Residency as a tracked fact.** ry's table columns live either in an Arrow buffer owned by Polars
  or in an R vector. Integer and double columns without nulls can be read in place, while logical,
  character and dictionary columns, and any column native code wants to write, must become R vectors
  first (section 7 of the runtime design). Vx makes every transfer explicit in source and checks it
  at compile time; ry keeps the transfer implicit, because R users should not have to write it, and
  borrows the checking side instead: a lint when typed code forces a column to materialize inside a
  loop.
- **Content hashes as on-disk cache keys.** The runtime caches bytecode and per-function summaries
  across processes. Salsa's interned ids are stable only inside one database, so these caches need a
  key derived from content, which is what Vx's identifiers are. A hash derived from content is not a
  second source of truth.

**What ry rejects.**

- **Content hashes as the in-memory identity,** because salsa's interned ids already give every item
  and type a stable identity within a session.
- **MLIR and LLVM as the JIT backend,** for the compile-latency reasons in the runtime design.
- **Placement types as a language feature,** because no benchmark code base targets accelerators.

## Production JITs: JavaScriptCore, LuaJIT and V8

The four languages above know their types or ownership statically. ry's untyped tiers face a different problem, the one production JavaScript and Lua engines solve:
running dynamically typed code fast while anything may be redefined at any time. The performance bar
in the mandate is Bun, which is JavaScriptCore, and ry's tier 0 runs real R code slower than GNU R's
bytecode interpreter: ggplot2, lattice and PerformanceAnalytics render 1.2 to 1.9 times slower than
on GNU R, which the knowledge base attributes to interpreter call cost. On the nine
production-shaped workloads of `just bench-real`, untyped R runs at 0.95 times GNU R's speed with
the JIT off and 0.96 times with it on (geometric means, `benchmarks.mdx`). `ry run
--profile-summary` splits that time into tier-0 instructions (23 to 44%), closure call setup (12
to 24%), builtins (about 25%) and S3 method lookup (up to 17%), with tier 1 under 3% on all but one
workload, because most hot closures run too few times to reach it.
This part studies the three engines for ideas that close that gap.

R adds two constraints to the `SEXP` one at the top of this page. Environments are first-class and
mutable from anywhere (`assign`, `eval`, `sys.function`, `parent.frame`, and native code through
`Rf_defineVar`), so no frame layout is ever final. Arguments are lazy promises, so whether and when
an argument's expression runs depends on the callee.

### JavaScriptCore and Bun

**What it is.** JavaScriptCore runs code in four tiers. The LLInt is an interpreter written in
offlineasm, a portable assembly language that compiles to each target's assembler. The Baseline JIT
emits a fixed template per bytecode. The DFG is a speculative optimizing compiler, and the FTL is a
slower, stronger backend for the hottest code, built on the B3 compiler since B3 replaced LLVM
there.[^jsc-speculation][^jsc-b3][^jsc-ftl] Code moves up the tiers by counters, enters optimized
code at loop headers (OSR entry), and leaves it at a failed speculation (OSR exit) for the Baseline
tier.[^jsc-speculation]

**What it does best for ry.** Speculation is fed by three mechanisms. A **structure** (a hidden
class) names the layout of an object's properties, so a property read caches "structure S, offset
k" and becomes a compare and a load. An inline cache that sees several structures becomes a
**polymorphic** cache with one case each, and past a limit it goes megamorphic. Every value-producing
bytecode keeps a **value profile**, and the inline caches double as profiles, so the DFG reads the
structures a site saw from its cache. **Watchpoints** cover facts that rarely change, such as "this
global holds this function": compiled code depends on a watchpoint set instead of checking, and
when the fact changes the set fires and throws away the dependent code.[^jsc-speculation] Bun adds
fast startup (a transpiler cache, and bytecode caching with `bun build --bytecode`) and implements
its hot builtins in Zig and C++.[^bun]

**What ry adopts.**

- **Inline caches as the profile.** ry already does this for functions: tier 0's lookup cache holds
  the binding cell it found, keyed on the enclosure and the symbol's lookup stamp
  (`cached_cell` in `crates/interp/src/bytecode.rs`), and tier 1 speculates on that cell.
- **Shapes, adapted to frames.** An environment is R's dictionary object, as an object is
  JavaScript's, but ry's lookup cache skips function frames entirely, because their bindings come
  and go without moving any stamp. So every read of a local variable searches the frame
  (`Frame::find` in `crates/heap/src/env.rs`): a bit-filter test, then a scan of the inline
  symbol and cell pairs, or a hash probe past 16 bindings. A structure id cannot work for a frame,
  because `assign` from anywhere would have to update it. ry's frame layout allows a cheaper key:
  bindings stay in insertion order, and removal leaves a tombstone instead of shifting, so a
  binding keeps its slot until the frame compacts. A cache that remembers "slot k" for a symbol at
  an instruction checks `bindings[k].symbol == symbol` and reads the cell, one compare and two
  loads, and needs no invalidation, because the check reads the frame itself. A miss falls back to
  the search. A closure's formals sit in the leading slots, so the compiler knows their slots
  before the first call. **Adopt.**
  On the `validation` workload of `just bench-real` with the JIT off, the frame search
  (`Heap::frame_cell`) takes 2.9% of the run's samples by self time, and the inline-cache misses
  that fall back to a full lookup are 1.5% of the time split in `ry run --profile-summary`.
- **Watchpoint sets per binding.** Today a compiled body checks its function speculations on every
  entry: one global binding epoch, and when that moved, the lookup stamp of each speculated symbol
  (`valid` in `crates/jit/src/runtime.rs`). The epoch moves on every top-level binding, which a
  package load or a test suite does constantly, so the per-symbol path runs often. A watched
  binding cell can instead list the compiled bodies that depend on it (the cell's third field
  holds nothing for an ordinary binding), and assigning or removing it invalidates them, so entry
  checks one flag. One R-specific check stays: a function frame can gain a binding that shadows
  the speculated one (`assign` into `parent.frame()`), and that path, which the `rooted` check
  covers today, is not a watched cell. Section 5 of the runtime design plans watch sets; this is
  their concrete shape. **Adopt.**
  How often `valid()` reaches the per-symbol loop on a real test suite is not yet measured.
- **Polymorphic inline caches for S3 dispatch.** S3 method lookup goes through one global table
  (`MethodCache` in `crates/interp/src/dispatch.rs`) that is cleared whenever the binding epoch or
  the collection count changes. A per-site cache in the style of a polymorphic property cache would
  hold up to four entries, keyed on the class attribute: first by the pointer of the class vector,
  which objects from one constructor usually share (unverified), then by comparing its elements,
  which are interned `CHARSXP`s and so compare by pointer. An object without a class attribute is
  keyed on its implicit class (type and `dim`). Past four classes the site goes megamorphic and
  uses the global table. The entries are invalidated by the lookup stamps of the method names,
  which per-binding watch sets make precise. **Adopt, scoped** to `UseMethod` and the internally
  dispatching builtins.
- **Value profiling, extended.** Tier 0 records a bit set of eleven value kinds per instruction
  (`feedback` in `bytecode.rs`). JavaScriptCore also profiles call targets and structures; ry's
  equivalents are the closure a call site called and the class a dispatch site saw, which the
  inlining and S3 items above need. **Adopt.**
- **OSR entry and exit** are built: tier 1 enters at loop headers and deoptimizes to tier 0.
- **Bun's two non-JIT reasons,** builtins in a systems language and cached bytecode, are already
  section 5 of the runtime design.

**What ry rejects or defers.**

- **An interpreter in assembly.** The LLInt is in offlineasm so that it controls register
  assignment and dispatch, and shares its frame layout with the JIT tiers. ry's tier 0 is a
  `match` in a Rust loop with `step` inlined into it, and its register window is already shared
  with tier 1. The cheap path to LLInt-like dispatch in Rust is one handler function per
  instruction, each ending in a tail call through a handler table (`become`, Rust's explicit tail
  calls, nightly only), which keeps the machine state in argument registers across instructions as
  hand-written assembly does.[^rust-tail-calls] On a scalar loop with the JIT off
  (`s <- s + i * 2` and a comparison, 2e7 iterations), the shared indirect jump that decodes the
  next instruction and branches to its handler takes about 28% of all samples, the largest single
  cost, because every instruction mispredicts through the same branch. Allocation of result
  scalars takes about 8%. A per-handler tail call would split that one branch into one per
  instruction, which is what the LLInt gets from offlineasm. It needs nightly Rust, and tier 1
  already runs such loops about 6 times faster than GNU R, so the gain lands only on code tier 1
  rejects. **Defer** until `become` is stable, and revisit first if tier 1 coverage stalls.
- **Four tiers.** Each tier adds a compiler to maintain and a set of transitions to test. ry has two,
  and the V8 section below argues what a third would be.

### LuaJIT

**What it is.** LuaJIT is Mike Pall's Lua implementation. Its interpreter is hand-written assembly,
generated with DynASM (`vm_x64.dasc`), which pins the bytecode base, the program counter and the
dispatch table to fixed machine registers and ends every handler by decoding and jumping to the
next one. A value is one 64-bit word: a double is itself, and every other type is encoded in the
unused NaN space, with pointers in the low bits (`lj_obj.h`). A table has an array part for dense
integer keys and a hash part for the rest (`lj_tab.c`).[^luajit]

**What it does best for ry.** The JIT is a trace compiler: it records the path a hot loop actually
takes, through calls, as a linear SSA trace, specializing on the constants it sees, including the
identity of each called function. Every guard carries a **snapshot**, a compact map from interpreter
slots to the trace's values at that point, which is all a side exit needs to rebuild the
interpreter's state (`lj_snap.c`). A hot side exit gets a side trace of its own. **Allocation
sinking** moves an allocation the trace does not let escape onto the side exits that need it, so
the hot path never allocates (`lj_opt_sink.c`).[^luajit]

**What ry adopts.**

- **Snapshots** are built in spirit: each tier 1 guard has an exit record listing the registers and
  slots tier 0 will read and their machine representations, and a cold block that stores them
  before one shared exit (`emit_deopts` in `crates/jit/src/codegen.rs`). Recompiling with updated
  feedback, up to four times per body, plays the role of side traces in a method JIT.
- **Allocation sinking,** extended from scalars to small vectors and lists. Compiled code already
  boxes an unboxed scalar only on exit. The same rule for a short vector or a list a loop builds
  and consumes is the escape analysis already adopted from Bend above, and LuaJIT's pass is the
  concrete design for it. **Adopt, scoped.**
- **Specialization on constants.** A call such as `sum(x, na.rm = TRUE)` or `x[i, , drop = FALSE]`
  passes a literal that selects the builtin's path. Tier 1 can pick that path at compile time, as
  LuaJIT specializes a trace on a constant key. **Adopt, scoped** to literal arguments of base
  builtins.
- **Array and hash parts.** ry's frames have the same split by size: inline pairs, with a hash index
  past 16 bindings. Built.

**What ry rejects.**

- **NaN-boxing for any value native code can see.** A `SEXP` is a pointer to a header, and native
  code dereferences it, so a register, a binding, a list element or an argument that can reach the C
  API must hold a real pointer. Inside ry the idea is already used where native code cannot look:
  compiled code keeps scalars unboxed in machine registers, and a ry record stores an integer or a
  logical slot under a quiet-NaN prefix (`Slot` in `crates/heap/src/object.rs`). Tagging tier 0's
  registers too would remove the allocation of scalar results, but every builtin call would box its
  operands. The knowledge base's plan, reusing a register's scalar in place when it is unshared,
  removes most of those allocations with one representation, so it comes first.
- **A trace JIT.** Cranelift is a method compiler, and ry already has OSR and deoptimization around
  it. R's control flow also suits traces badly: a hot loop calls closures whose arguments are
  promises, so a trace through a call has to record which promises the callee forces and in what
  order, and S3 dispatch, `tryCatch` and `on.exit` multiply the paths. Riposte, the one tracing JIT
  for R, was a research prototype for vector code that ran no packages (section 3 of the runtime
  design).
- **An assembly interpreter,** for the reasons given under JavaScriptCore.

### V8

**What it is.** Ignition, V8's interpreter, runs a register-machine bytecode with an implicit
accumulator: most instructions read or write the accumulator, which shortens their
operands.[^v8-ignition] Sparkplug, the baseline compiler, walks the bytecode once and emits, for each
instruction, a fixed sequence that mostly calls a builtin, with no IR and no register allocation. It
keeps Ignition's frame layout, so switching between the two at any instruction needs no frame
translation, and the post reports a gain of 5 to 15 percent on real-world benchmarks.[^v8-sparkplug]
Maglev is a mid-tier SSA compiler that specializes on feedback; the post puts its compile speed at
about ten times slower than Sparkplug and ten times faster than TurboFan.[^v8-maglev] TurboFan is
the top tier, whose sea-of-nodes IR V8 has since replaced with a control-flow graph
(Turboshaft).[^v8-turboshaft] Objects have hidden classes (maps), as JavaScriptCore's have
structures.[^v8-hidden-classes] Each function's feedback lives in a **feedback vector**, a side
array with one slot per inline-cache site, assigned when the bytecode is generated and allocated
only once the function has run a few times.[^v8-lite]

**What ry adopts.**

- **Dense, lazily allocated feedback.** ry's lazy half is built: tier 0 records feedback only once a
  body is warm (`warm` in `bytecode.rs`). Its storage is one integer per code word, operands
  included, so most of the array is never written. Assigning feedback slots at compile time, only to
  the instructions that produce values, gives a dense side array with no unused entries, which is
  the data-oriented layout the mandate asks for. **Adopt.**
  The share of code words that carry feedback over a package's bytecode is not yet measured.
- **A fast mid tier.** Tier 1 already has Maglev's shape: it builds a typed plan straight from the
  bytecode and its feedback, with no R-level IR. Its thresholds (3000 entries, 10,000 back edges)
  are high because Cranelift compiles slowly for code that runs only a few thousand times. Maglev's
  lesson is that compile speed decides how early a tier can pay off. ry can build the plan on the
  main thread, which reads the heap, and run Cranelift on another thread, which does not, and then
  lower the thresholds. **Adopt, scoped.**
  On the `validation` workload, tier 1 compiles 73 bodies in about 15 ms of the run's 3 s, about
  0.2 ms per body including planning, so compile time is not what keeps code in tier 0 there: the
  thresholds are. The split between planning and Cranelift is not yet measured.

**What ry rejects or defers.**

- **The accumulator.** It shortens bytecode for an interpreter that cannot allocate registers.
  ry's tier 0 names every register, tier 1 maps each one to a Cranelift variable, and rewriting the
  bytecode would gain encoding size, not speed.
- **A Sparkplug-style baseline tier, deferred, but preferred over copy-and-patch.** ry's tier 0 and
  tier 1 already share the register window, which is Sparkplug's key property. A baseline tier for
  ry would emit, per instruction, a call to the existing handler (`step_one`) with its operands as
  immediates and its branches as native jumps. That removes decoding and dispatch, and keeps the
  allocations, guards and frame searches that dominate tier 0. It is simpler to build and maintain
  than copy-and-patch stencils, so if measurements call for a baseline tier, this is its form.
- **A sea-of-nodes top tier.** V8 left it for a control-flow graph, citing compile time and
  maintenance cost.[^v8-turboshaft]

### Verdicts

| Idea | From | Verdict | Where it lands in ry |
| --- | --- | --- | --- |
| Inline caches as the profile the optimizer reads | JavaScriptCore, V8 | **Built, extend** | Tier 0 function lookups; tier 1 speculates on the cached cell |
| Shapes as cache keys, adapted to frames: remember slot k, check the symbol there | JavaScriptCore, V8 | **Adopt** | Local variable reads in tier 0 and in compiled code that keeps the environment |
| Watchpoint sets per binding | JavaScriptCore | **Adopt** | Watched binding cells list dependent bodies; replaces the epoch and stamp check at entry |
| Polymorphic inline caches | JavaScriptCore, V8 | **Adopt, scoped** | S3 dispatch sites, keyed on the class vector; the global method cache as megamorphic fallback |
| Value profiling of call targets and classes | JavaScriptCore | **Adopt** | Beside tier 0's value-kind feedback |
| OSR entry and exit | All three | **Built** | Loop headers; deoptimization to tier 0 |
| Snapshots for side exits | LuaJIT | **Built, as per-exit records** | Tier 1's deoptimization exits |
| Allocation sinking | LuaJIT | **Adopt, scoped** | Short vectors and lists a compiled loop builds and consumes |
| Specialization on constant arguments | LuaJIT | **Adopt, scoped** | Literal arguments of base builtins (`na.rm`, `drop`) |
| NaN-boxing of values native code can see | LuaJIT, JavaScriptCore | **Reject** | A `SEXP` must be a pointer; unboxing stays inside compiled code and record slots |
| Interpreter in assembly with fixed-register dispatch | JavaScriptCore, LuaJIT | **Defer** | Tail-call dispatch in Rust, after tier 0's allocation and lookup costs |
| Trace compilation | LuaJIT | **Reject** | Promises and dispatch make traces fragile; Cranelift is a method JIT |
| Dense feedback slots, allocated lazily | V8 | **Adopt** (lazy half built) | Feedback only for value-producing instructions |
| Fast mid tier on feedback | V8 (Maglev) | **Adopt, scoped** | Tier 1, compiled off the main thread, with lower thresholds |
| Baseline compiler reusing the interpreter's frame | V8 (Sparkplug) | **Defer; preferred over copy-and-patch** | Calls to tier 0's handlers, if a baseline tier is built |
| Accumulator bytecode | V8 (Ignition) | **Reject** | Tier 0 keeps named registers |
| Sea-of-nodes optimizer, a fourth tier | V8, JavaScriptCore | **Reject** | Two tiers, measured before a third |

## About this page's format

This page is an [Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format)
v0.2 concept document.[^okf] The frontmatter carries the `type`, the provenance (`sources`) and the
trust (`generated`, `verified`) fields that format defines, and each claim about a project cites its
source with a footnote keyed to a `sources` entry. The docs site does not render frontmatter, so each
footnote also carries the source's link.

[^okf]: Open Knowledge Format v0.2, <https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md>
[^roc]: Roc repository: docs/langref, design.md, design/lambda-set-specialization.md, <https://github.com/roc-lang/roc>
[^perceus]: Reinking, Xie, de Moura and Leijen: Perceus, Garbage Free Reference Counting with Reuse, <https://www.microsoft.com/en-us/research/wp-content/uploads/2020/11/perceus-tr-v4.pdf>
[^bendrt]: Taelin et al.: BendRT, a Parallel Runtime for CPUs and GPUs, <https://github.com/HigherOrderCO/Bend/blob/main/paper/BendRT.pdf>
[^bend]: Bend repository: README.md, guide/EFFECTS.md, <https://github.com/HigherOrderCO/Bend>
[^goose]: goose repository: README.md, docs/implementation.md, docs/reference.md, <https://github.com/aardappel/goose>
[^goose-lang]: goose-lang/goose: a Go subset with a semantics in Coq, <https://github.com/goose-lang/goose>
[^vx]: Vx repository: README.md, ROADMAP.md, docs/language_rules_for_a_parallel_compiler.md, www/book/src/quick-reference.md, <https://github.com/vx-lang/Vx>
[^jsc-speculation]: Pizlo: Speculation in JavaScriptCore (WebKit blog, 2020), <https://webkit.org/blog/10308/speculation-in-javascriptcore/>
[^jsc-b3]: Pizlo: Introducing the B3 JIT Compiler (WebKit blog, 2016), <https://webkit.org/blog/5852/introducing-the-b3-jit-compiler/>
[^jsc-ftl]: Pizlo: Introducing the WebKit FTL JIT (WebKit blog, 2014), <https://webkit.org/blog/3362/introducing-the-webkit-ftl-jit/>
[^bun]: Bun documentation: the bundler and bytecode caching, <https://bun.sh/docs/bundler>
[^luajit]: LuaJIT repository: src/vm_x64.dasc, src/lj_obj.h, src/lj_tab.c, src/lj_snap.c, src/lj_opt_sink.c, <https://github.com/LuaJIT/LuaJIT>
[^v8-ignition]: V8 blog: Firing up the Ignition interpreter (2016), <https://v8.dev/blog/ignition-interpreter>
[^v8-sparkplug]: V8 blog: Sparkplug, a non-optimizing JavaScript compiler (2021), <https://v8.dev/blog/sparkplug>
[^v8-maglev]: V8 blog: Maglev, V8's fastest optimizing JIT (2023), <https://v8.dev/blog/maglev>
[^v8-turboshaft]: V8 blog: Land ahoy, leaving the Sea of Nodes (2025), <https://v8.dev/blog/leaving-the-sea-of-nodes>
[^v8-hidden-classes]: V8 documentation: Maps (hidden classes) in V8, <https://v8.dev/docs/hidden-classes>
[^v8-lite]: V8 blog: A lighter V8 (2019), <https://v8.dev/blog/v8-lite>
[^rust-tail-calls]: Rust RFC 3407: explicit tail calls, <https://github.com/rust-lang/rfcs/pull/3407>
