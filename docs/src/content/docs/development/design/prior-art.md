---
type: Reference
title: Prior art from new language implementations
description: "What Roc, Bend, goose, Vx and Bun's compiler, the production JITs of JavaScriptCore (Bun), LuaJIT and V8, and Julia, Ř, Truffle, YJIT, Deegen, HHVM, Java's Project Leyden, Static Python, Sorbet, LXR, Riposte and pqR do best (with CPython, PyPy, HotSpot and .NET in the verdict table), and which of their ideas ry's R runtime adopts, defers or rejects"
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
  - id: bun-aot
    resource: https://bun.sh/docs/compiler/aot
    title: "Bun documentation: the ahead-of-time compiler, and its internals page"
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
  - id: pep-659
    resource: https://peps.python.org/pep-0659/
    title: "PEP 659: Specializing Adaptive Interpreter"
  - id: pep-744
    resource: https://peps.python.org/pep-0744/
    title: "PEP 744: JIT Compilation"
  - id: pypy
    resource: https://doc.pypy.org/en/latest/
    title: "Bolz, Cuni, Fijałkowski and Rigo: Tracing the meta-level, PyPy's tracing JIT compiler (ICOOOLPS 2009), and the PyPy documentation"
  - id: julia
    resource: https://docs.julialang.org/en/v1/devdocs/compiler/
    title: "Bezanson et al.: Julia, dynamism and performance reconciled by design (OOPSLA 2018), and the Julia developer documentation"
  - id: rsh-contextual
    resource: https://doi.org/10.1145/3428288
    title: "Flückiger et al.: Contextual Dispatch for Function Specialization (OOPSLA 2020)"
  - id: rsh-brains
    resource: https://doi.org/10.1145/3359619.3359744
    title: "Flückiger et al.: R Melts Brains, an IR for First-Class Environments and Lazy Effectful Arguments (DLS 2019)"
  - id: truffle
    resource: https://doi.org/10.1145/2509578.2509581
    title: "Würthinger et al.: One VM to Rule Them All (Onward! 2013)"
  - id: dotnet-tiered
    resource: https://github.com/dotnet/runtime/blob/main/docs/design/features/tiered-compilation.md
    title: ".NET runtime design document: tiered compilation, and the compilation configuration reference (https://learn.microsoft.com/en-us/dotnet/core/runtime-config/compilation)"
  - id: hotspot
    resource: https://docs.oracle.com/en/java/javase/21/vm/java-hotspot-virtual-machine-performance-enhancements.html
    title: "Java SE 21 HotSpot VM guide: performance enhancements, tiered compilation"
  - id: okf
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2
    last_modified: 2026-08-21T13:08:36-07:00
---

This page studies four recent language implementations, one new compiler, the production JITs of
JavaScriptCore, LuaJIT and V8, and a set of narrower reference points (Julia, Ř, Truffle, YJIT,
Deegen, HHVM, Project Leyden, Static Python, Sorbet, LXR, Riposte and pqR, plus CPython, PyPy,
HotSpot and .NET in the table only) for ideas that ry's R runtime can use, and decides which ones it
takes. One
constraint decides every verdict, and it comes from [the runtime design](/development/design/r-runtime/): R's C API must work unchanged. Any object a
native package can reach is therefore a non-moving heap object with a reference count, reached
through a raw `SEXP` pointer, rooted by `PROTECT`, possibly an ALTREP vector, and possibly written to
by native code without regard for its reference count.

## What ry takes, in one table

The order and status of the untyped-performance work these verdicts feed live in the runtime
design's [roadmap](/development/design/r-runtime/#untyped-performance-roadmap); "step n" below
refers to its steps.

| Idea | From | Verdict | Where it lands in ry |
| --- | --- | --- | --- |
| Precise reference counts, so the runtime "count is one" check succeeds more often | Roc (Perceus) | **Adopt** | JIT-compiled code; copy-on-modify for vectors and the table type |
| Precise counts and borrowed parameters in the interpreter: decrement at last use and frame exit, and borrow a parameter the bytecode never assigns or passes to a modifying call | Roc (Perceus, borrow inference) | **Adopt, planned** (step 2) | Tier 0, so `x[i] <- v` updates in place more often; reported as copies and peak memory |
| Borrowed arguments: a callee that only reads an argument leaves its count alone | Roc, Bend | **Adopt, as a speculation** | The JIT, for callees it can see entirely |
| Escape analysis, inspired by Bend's share inference | Bend | **Adopt, scoped** | Temporaries in compiled code that native code never sees |
| Call-site specialization of known closures, inspired by lambda sets | Roc | **Adopt, scoped** | `lapply`, `vapply`, `Map`, `Reduce` in compiled code |
| One IR feeding several backends | Roc | **Adopt** | Bytecode feeds the interpreter, the baseline tier and the Cranelift tier |
| A fast dev backend beside an optimizing one | Roc | **Adopt, planned** (step 4) | The Sparkplug-style baseline tier |
| Static dispatch only | Roc | **Reject** | S3, S4 and R6 are dynamic; ry speculates on the class instead |
| A top level that cannot perform effects | Roc | **Adopt, planned for strict mode** | A strict library file's top level holds only definitions and pure constants |
| Purity inference: effectful types use `=>`, effectful names end in `!` | Roc | **Reject** | Freedom from effects stays a runtime body fact the JIT reads, outside the type |
| Reference counting with no tracing collector, for immutable acyclic values | Roc, Koka, Lean 4 | **Adopt for native typed values** | Typed ry's own values; R values stay under the collector (`typed-runtime.md`, "Memory") |
| Bounds-check elimination over difference constraints | goose | **Adopt** | Subscript checks in compiled loops |
| Values built in their final destination | goose | **Adopt** | Result preallocation in fused loops |
| No heap, data stacks only, no reference counts | goose | **Reject** | Incompatible with `SEXP` identity and `R_PreserveObject` |
| Static multi-frame returns instead of an unwinder | goose | **Reject** | R's conditions and restarts are dynamic |
| Compile to one C file, call C through `extern fn` or spliced effects | goose, Bend | **Reject** | ry hosts the existing C ABI; it does not emit C |
| Even-split parallel forks with no work stealing | Bend | **Reject** | R code makes no balance promise |
| Residency of data as a tracked fact | Vx | **Adopt the idea, not the syntax** | Table columns: Arrow buffer or R vector, plus a lint |
| Content-hashed identifiers | Vx | **Reject** | Bytecode and runtime summaries are install-time build output, so no cache needs a key |
| LLVM and MLIR as the JIT backend | Vx | **Reject** | Compile latency; Cranelift is the choice |
| Macros | Vx (`macro_rules`), Bend (templates) | **Reject** | ry has no macros, for checking speed; R's own non-standard evaluation keeps working |
| Types as facts (free) or guesses (one check), with a second compiled copy that reads a guess in place | Bun | **Adopt, keeping errors for non-conforming values** | Typed entry stubs (`typed-runtime.md`, "Facts and guesses") |
| Whole-program value tracking | Bun | **Adopt per namespace** | The typed core of a locked namespace or strict file; untyped code keeps profiles |
| Closed bindings per locked namespace: the bindings are closed though their values are not | Bun | **Adopt** (step 2) | Direct calls to sibling namespace functions and imports, invalidated by `unlockBinding` and `assignInNamespace` |
| Freezing built-ins instead of deoptimizing | Bun | **Adopt for ry's standard library only** | R bindings get invalidation on `unlockBinding`, because mocking rebinds them |
| Ahead-of-time compilation, applied to untyped code at install time | Bun | **Adopt, planned; measure warm-up and start-up before building machine code** | `ry install` writes each package's bytecode, argument-match plans and resolved namespace call targets into a read-only file mapped at load |
| Machine code mapped read-only from the file and shared between processes | Bun | **Adopt** | Typed code files in installed packages and executables |
| Mapped images shared across worker processes | Bun | **Adopt, planned** (step 5) | callr, parallel and doParallel workers map the same package images; measured as resident memory per worker |
| A report of code compiled to a slow path, worst first, with fixes | Bun | **Adopt** | `ry check --explain` |
| Each program runs interpreted, compiled, and compiled with types erased; guesses forced to fail; facts checked | Bun | **Adopt** | Typed differential tests and fuzzing |
| No compiler at run time, so every fallback is compiled at build time | Bun | **Reject** | ry compiles a second copy at the first call that needs it |
| A standalone executable with its machine code mapped from the file | Bun | **Adopt** | `ry build --exe` |
| Inline caches as the profile the optimizer reads | JavaScriptCore, V8 | **Built, extend** | Tier 0 function lookups; tier 1 speculates on the cached cell |
| Shapes as cache keys, adapted to frames: remember slot k, check the symbol there | JavaScriptCore, V8 | **Built** | `Frame::find_hinted`: a slot hint per tier 0 variable and function lookup site |
| Watchpoint sets per binding | JavaScriptCore | **Adopt** | Watched binding cells list dependent bodies; replaces the epoch and stamp check at entry |
| Polymorphic inline caches | JavaScriptCore, V8 | **Adopt, scoped** | S3 dispatch sites, keyed on the class vector; the global method cache as megamorphic fallback |
| Value profiling of call targets and classes | JavaScriptCore | **Adopt** | Beside tier 0's value-kind feedback |
| OSR entry and exit | JavaScriptCore, LuaJIT, V8 | **Built** | Loop headers; deoptimization to tier 0 |
| Register-based interpreter with per-instruction tail-call dispatch and fast paths inside handlers | JavaScriptCore (LLInt), LuaJIT 2 | **Adopt, planned** (step 3); the assembly is rejected | Tier 0 handlers ending in a `become` call through a handler table |
| Snapshots for side exits | LuaJIT | **Built, as per-exit records** | Tier 1's deoptimization exits |
| Allocation sinking | LuaJIT | **Adopt, scoped** | Short vectors and lists a compiled loop builds and consumes |
| Specialization on constant arguments | LuaJIT | **Adopt, scoped** | Literal arguments of base builtins (`na.rm`, `drop`) |
| NaN-boxing of values native code can see | LuaJIT, JavaScriptCore | **Reject** | A `SEXP` must be a pointer; unboxing stays inside compiled code and record slots |
| Trace compilation | LuaJIT | **Reject** | Promises and dispatch make traces fragile; Cranelift is a method JIT |
| Dense feedback slots, allocated lazily | V8 | **Adopt** (lazy half built) | Feedback only for value-producing instructions |
| Fast mid tier on feedback | V8 (Maglev) | **Adopt, scoped** | Tier 1, compiled off the main thread, with lower thresholds |
| Baseline compiler reusing the interpreter's frame | V8 (Sparkplug) | **Adopt, planned** (step 4), preferred over copy-and-patch | Per instruction, a call to tier 0's handler with immediate operands and native jumps |
| Accumulator bytecode | V8 (Ignition) | **Reject** | Tier 0 keeps named registers |
| Sea-of-nodes optimizer, a fourth tier | V8, JavaScriptCore | **Reject** | Tier 1 is the top tier |
| In-place instruction specialization (quickening), de-specialized on a miss | CPython (PEP 659)[^pep-659] | **Adopt the idea** (step 3) | Tier 0 instructions rewrite themselves to a specialized form and back; CPython is not the performance bar, JavaScriptCore is |
| Copy-and-patch JIT | CPython (PEP 744)[^pep-744] | **Reject** | The PEP calls it experimental and build-time only, with modest gains; a baseline that calls tier 0's handlers is simpler for ry |
| Meta-tracing of an interpreter written in RPython | PyPy[^pypy] | **Reject** | A tracing JIT (rejected under LuaJIT), and it needs the interpreter written in RPython |
| Per-signature method specialization driven by type inference | Julia | **Adopt for numeric code** (steps 2 and 4) | Typed ry and ahead-of-time call facts select per-signature compiled copies |
| Environment elision, promise elision for strict effect-free arguments, contextual dispatch | Ř | **Adopt** (steps 2 and 4) | Several compiled versions of a closure per call context, chosen at the call |
| Self-specializing AST nodes and partial evaluation | Truffle, Graal | **Reject as an architecture** | The specialization idea arrives as quickening (step 3) |
| Counter-driven tier-up, call counting, background compilation, precompiled code replaced when hot | HotSpot[^hotspot], .NET[^dotnet-tiered] | **Adopt the policy** (step 4) | Tier thresholds, off-thread compiles, and install-time images that hot code outgrows, as .NET's ReadyToRun code is |
| Lazy basic block versioning: compile a block per combination of value kinds at its entry, only when reached | YJIT | **Adopt, as the baseline's second stage** (step 4) | Baseline blocks versioned on register kinds, once plain handler calls are measured |
| One description of each instruction generates the interpreter and the baseline JIT | Deegen (LuaJIT Remake) | **Adopt the principle** (steps 3 and 4); **reject the generator** | Tier 0's handlers are the semantics, and the baseline calls them |
| Profiles from a training run shipped with precompiled code, so the JIT starts warm | HHVM Jump-Start, Project Leyden | **Adopt, planned** (step 5) | Per-package images carry profiles from the package's tests and examples, run at install time |
| Types added to a large dynamic codebase, then used for speed | HHVM and Hack, Static Python, Sorbet | **Adopt as the strategy** | Typed ry: checked boundaries, per-file strict mode, compiled typed closures |
| Reference counting with Immix and occasional concurrent tracing for cycles | LXR | **Defer**; freeing on count zero is rejected | ry's counts are R's sharing counts, which native code does not keep exact |
| Automatic parallelism of effect-free work | Bend, pqR (helper threads) | **Adopt, scoped** (step 4) | `lapply`, `vapply`, `Map` over compiled effect-free closures, and element-wise ops on long vectors; reductions stay sequential |
| A stackless interpreter, so recursion never uses the C stack | Bend | **Defer** | Tier 0 grows the native stack in segments; revisit with step 3's tail-call handlers |
| Deferred, fused vector arithmetic | Riposte, pqR (task merging) | **Adopt, scoped** (step 3) | A lazy ALTREP vector for `x * y + z` on doubles, computed in one pass when read |

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
- **Precise counts and borrowed parameters in tier 0,** planned as step 2 of the
  [roadmap](/development/design/r-runtime/#untyped-performance-roadmap). The interpreter can apply
  the same two ideas without a JIT. A parameter the bytecode never assigns and never passes to a
  call that may modify it is borrowed, so binding it needs no count increment. A local's count is
  decremented at its last use and at frame exit rather than left to the collector, so the
  `x[i] <- v` that follows finds a count of one, and updates in place, more often. The measure is
  the number of copies and the peak memory of a run.
- **Call-site specialization of known closures.** R code passes functions to `lapply`, `vapply`,
  `Map` and `Reduce` constantly. When the JIT sees `vapply(xs, function(x) x * 2, double(1))` with a
  known closure, it compiles a loop that calls the closure directly. This is ordinary callback
  inlining with a deoptimization exit, inspired by lambda sets rather than a copy of them, because R
  semantics require the closure object to exist if anything reflects on it. It covers base R's
  higher-order functions, which ry implements itself. `purrr::map` runs its loop in purrr's own C
  code, so the JIT cannot specialize it.
- **One IR for every tier.** ry's bytecode feeds the tier 0 interpreter, and tier 1 compiles from
  the same bytecode (through its own SSA form) to Cranelift, as Roc's backends share their
  lower-level IR. Roc's fast dev backend corresponds to the planned Sparkplug-style baseline tier
  (see V8 below).

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

**What ry rejects.**

- **Content hashes as identities,** in memory or on disk. Salsa's interned ids already give every
  item and type a stable identity within a session, and bytecode and runtime summaries leave a
  session only as install-time build output stamped with the ry build (`r-runtime.md`, "Where
  bytecode comes from"), so no on-disk cache needs a key derived from content.
- **MLIR and LLVM as the JIT backend,** for the compile-latency reasons in the runtime design.
- **Placement types as a language feature,** because no benchmark code base targets accelerators.

## Bun's compiler

**What it is.** `bun compile` (shorthand for `bun build --compile --aot`) compiles TypeScript and
JavaScript to machine code at build time and writes one executable with no JIT, no bytecode and no
profiling data in it. Bun documents it as experimental.[^bun-aot] It type-checks the whole program,
tracks values through it, optimizes each function and generates code with the backend of
JavaScriptCore's fastest JIT.[^bun-aot]

**What it does best for ry.** It solves the problem ry's typed runtime has: typed code that must
run fast without trusting values that arrive from untyped code.

- **Facts and guesses.** A type describing a value the program creates is a fact: objects get fixed
  layouts and a property read is one load with no check. A type describing a value from outside
  (`JSON.parse`, `any`, `as`, a `.d.ts`-only package) is a guess and costs one layout check where
  the value arrives. A failed check sends the call to a second copy of the function that treats only
  that parameter as plain JavaScript. A mismatch never throws or converts, so types change speed and
  never behavior.[^bun-aot]
- **Whole-program value tracking.** For each variable, parameter and property it works out which
  objects can reach it, and for each call which functions it can reach. Untyped JavaScript gets fixed
  layouts the same way, and code the compiler cannot resolve looks properties up at run
  time.[^bun-aot]
- **Freezing instead of deoptimizing.** An executable cannot discard compiled code, so replacing or
  deleting a member of a built-in object such as `Array.prototype` throws a `TypeError`, which makes
  the compiler's assumption true. Adding members, or patching `console`, `process` or `fetch`,
  works.[^bun-aot]
- **Shared code.** The machine code and its tables are read-only and mapped straight from the file,
  so processes and workers share them.[^bun-aot]
- **Explaining slow paths.** `--explain` lists code that compiled to a slow path, worst first, with a
  fix for each, and `--explain=json` prints one object per line.[^bun-aot]
- **The test matrix.** Each test runs under `bun run`, compiled, and compiled with every annotation
  erased, and all three must match in output and exit status. Programs are also compiled with every
  guess forced to fail, and a mode checks every fact the executable relies on at run time.[^bun-aot]

**What ry adopts.** All six, fitted to R (`typed-runtime.md` holds the design), and two more
that apply Bun's closed-world reasoning to untyped packages:

- **Facts and guesses,** but with ry's contract: an annotation means the value has the type, so a
  value that does not conform is refused with a type-mismatch error. A conforming R value takes the
  second copy, which reads it in place, because typed ry's own values are native values outside the R
  heap and an R value never has their layout.
- **Value tracking per namespace.** The typed core of a locked namespace or a strict file has no
  `eval` and its bindings are locked, so it is closed apart from the functions that escape to R
  (exports, registered S3 methods and closures it hands out). The values of untyped R code are
  never closed, because `eval`, `get` and `assign` reach any binding, so it keeps allocation
  profiles.
- **Closed bindings per locked namespace,** step 2 of the roadmap. A locked namespace's set of
  bindings is closed even though their values are not: no binding can be added or rebound without
  `unlockBinding` or `assignInNamespace`. So a call from a package function to a sibling function
  of its namespace, or to an import, resolves to its target directly, and those two primitives
  invalidate the code that assumed the binding.
- **Freezing for ry's standard library.** No existing R code rebinds `List.map`, so its namespaces
  cannot be unlocked, and a call the checker resolves to it is direct. R's own locked bindings are
  not frozen, because testthat's `local_mocked_bindings` and `assignInNamespace` rebind them on purpose. Instead,
  the binding-unlock primitive discards the compiled code whose direct calls assumed the binding.
- **Mapped, shared code** for the typed code that `ry install` writes into each installed package,
  which matters for CI machines that run several test processes and are bound by memory, and for
  `ry build --exe`, which writes a standalone executable as `bun compile` does. Worker processes
  started by callr, parallel and doParallel load the same packages, so mapping each package's image
  read-only lets them share it (planned as step 5, measured as resident memory per worker).
- **Ahead-of-time compilation, applied to untyped code at install time** (planned). `ry install`
  writes each package's bytecode, argument-match plans and resolved namespace call targets into a
  read-only file that the loader maps, one image per package, which extends the start-up image.
  Warm-up and start-up are measured with these images before any machine code is written into them.
- **`ry check --explain`** for the slow-path report. With no profile, it ranks by loop depth.
- **The test matrix** for typed differential tests and the typed fuzzer.

**What ry rejects.**

- **Compiling every fallback at build time,** because ry has a compiler at run time and compiles a
  second copy at the first call that needs it.
- **Types that never change behavior,** because ry's annotations are checked at the boundary and
  report mismatches.

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
  and go without moving any stamp. A structure id cannot work for a frame either, because `assign`
  from anywhere would have to update it. ry's frame layout allows a cheaper key: bindings stay in
  insertion order, and removal leaves a slot holding no real symbol instead of shifting, so a
  binding keeps its slot until the frame compacts. Each tier 0 `GetVar`, `SetVar` and function
  lookup site keeps a `u32` slot hint (the `HINTS` vector of the code object), and
  `Frame::find_hinted` in `crates/heap/src/env.rs` tests the frame's bit filter first, then
  compares the symbol in the hinted slot, and only on a mismatch searches the frame (the inline
  pairs, or a hash index past 32 bindings) and stores the slot it found as the new hint. Calls of
  one closure bind their formals and locals in the same order, so a hint from an earlier call
  answers in later frames with one compare. The hint needs no invalidation, because the check reads
  the frame itself, and a frame that binds a symbol twice (duplicate formals) ignores hints, so a
  stale hint never matches the wrong binding. **Built.** Before slot hints, the frame search
  (`Heap::frame_cell`) took 2.9% of the samples by self time on the `validation` workload of
  `just bench-real` with the JIT off.
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

**What ry adopts from the interpreters.** The LLInt is in offlineasm so that it controls register
assignment and dispatch, and shares its frame layout with the JIT tiers; LuaJIT 2's interpreter
does the same in DynASM. ry takes the technique and rejects the assembly. Tier 0 is one dispatch
loop (`interpret` in `crates/interp/src/bytecode.rs`): each instruction word carries its opcode and
its operand count, and every instruction jumps back through one `match`, so all instructions share
one indirect branch, whose target the predictor must guess from one history. Its register window is already shared with
tier 1. The Rust path to LLInt-like dispatch is one handler function per instruction, each ending
in a tail call through a handler table (`become`, Rust's explicit tail calls, nightly
only).[^rust-tail-calls] That keeps the machine state in argument registers across instructions as
hand-written assembly does, and splits the shared branch into one per handler, which the
predictor can learn. Each handler also gets the fast path for the common operand kinds inline, as
LLInt handlers do. ry builds on nightly Rust (`rust-toolchain.toml`), so `become` is available.
**Adopt, planned** as step 3 of the
[roadmap](/development/design/r-runtime/#untyped-performance-roadmap), together with quickening
(CPython, in the table). An earlier profile of a scalar loop with the JIT off (`s <- s + i * 2` and a
comparison, 2e7 iterations) put the shared dispatch jump at about 28% of samples and the
allocation of result scalars at about 8%. It predates the single dispatch loop, so the dispatch
share needs remeasuring.

**What ry rejects.**

- **An interpreter in assembly,** because a Rust handler with a guaranteed tail call gets the same
  dispatch shape and stays portable and checkable.
- **Four tiers.** Each tier adds a compiler to maintain and a set of transitions to test. ry has two
  and plans a third, the baseline described under V8 below.

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
  past 32 bindings. Built.

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
- **An assembly interpreter.** ry adopts its dispatch technique in Rust, as described under
  JavaScriptCore.

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
- **A Sparkplug-style baseline tier, preferred over copy-and-patch.** ry's tier 0 and tier 1
  already share the register window, which is Sparkplug's key property. ry's baseline tier emits,
  per instruction, a call to that instruction's tier 0 handler with its operands as immediates and
  its branches as native jumps. That removes decoding and dispatch, and keeps the allocations,
  guards and frame searches that the other roadmap steps attack. It is simpler to build and
  maintain than copy-and-patch stencils, and it reuses the handlers step 3 writes. **Adopt,
  planned** as step 4 of the roadmap.

**What ry rejects.**

- **The accumulator.** It shortens bytecode for an interpreter that cannot allocate registers.
  ry's tier 0 names every register, tier 1 maps each one to a Cranelift variable, and rewriting the
  bytecode would gain encoding size, not speed.
- **A sea-of-nodes top tier.** V8 left it for a control-flow graph, citing compile time and
  maintenance cost.[^v8-turboshaft]

## Other reference points

These engines answer narrower questions than the three above: how a compiler for a dynamic
language uses inferred types, what an optimizing R compiler removes, what form a baseline tier
takes, how a JIT can start warm, how other dynamic languages added types, how a counted heap can
reclaim memory, and how R's vector code can avoid temporaries. CPython, PyPy, HotSpot and .NET
have one row each in the table and no section here.

### Julia

**What it is.** Julia compiles a method separately for each concrete signature of argument types it
is called with. At the first call with a new signature, type inference runs over the method body
with those argument types, and the inferred types let the compiler unbox numbers, devirtualize
calls and inline, so numeric code compiles much as a statically typed language's
would.[^julia]

**What ry adopts.** Per-signature specialization for numeric code, in steps 2 and 4 of the roadmap.
Typed ry already supplies argument types, and the ahead-of-time call facts give untyped calls the
types the checker can prove, so a closure gets a compiled copy per argument type signature, chosen
at the call. ry does not adopt Julia's model of inferring every call: an untyped call whose types
are unknown keeps tier 0 and feedback.

### Ř

**What it is.** Ř is a research VM for R from Jan Vitek's group, built on a patched GNU R (section 3
of the runtime design). Its optimizing tier compiles to PIR, an SSA IR that models R's environments
and promises explicitly, so the compiler can remove an environment nothing reflects on and a
promise whose argument it can evaluate eagerly.[^rsh-brains] Contextual dispatch keeps several
compiled versions of a closure, each for a call context: which arguments are already evaluated,
which are missing, and their types. The call site picks the most specific version that fits its
context.[^rsh-contextual]

**What ry adopts.** All three ideas, in steps 2 and 4 of the roadmap: environment elision, promise
elision for arguments that are strict and free of effects, and contextual dispatch, with the call
context computed at the call site. Ř's papers are also the evidence behind the runtime design's
finding that R's cost is mostly semantic (environments, promises, argument matching) rather than
instruction dispatch.

**What ry rejects.** Ř's base, a fork of GNU R, for the reason in section 3 of the runtime design:
forks cannot follow upstream R.

### Truffle and Graal

**What it is.** Truffle runs a language as an AST interpreter whose nodes rewrite themselves into
specialized versions for the types they see, and Graal compiles a hot AST by partial evaluation of
the interpreter with the tree as a constant.[^truffle] FastR, the Truffle R, ran R code fast but
emulated the C API, so packages' native code was slow and incomplete (section 3 of the runtime
design).

**What ry rejects.** Truffle as an architecture. It requires GraalVM, and FastR shows where it fails
for R: the package ecosystem's native code. The self-specialization idea is the same as quickening,
which ry adopts in step 3.

@@NEW@@
## About this page's format

This page is an [Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format)
v0.2 concept document.[^okf] The frontmatter carries the `type`, the provenance (`sources`) and the
trust (`generated`, `verified`) fields that format defines, and each claim about a project cites its
source with a footnote keyed to a `sources` entry. The docs site does not render frontmatter, so each
footnote also carries the source's link.

[^bun-aot]: Bun documentation: the ahead-of-time compiler, and its internals page, <https://bun.sh/docs/compiler/aot>, <https://bun.sh/docs/compiler/internals>
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
[^pep-659]: PEP 659: Specializing Adaptive Interpreter, <https://peps.python.org/pep-0659/>
[^pep-744]: PEP 744: JIT Compilation, <https://peps.python.org/pep-0744/>
[^pypy]: Bolz, Cuni, Fijałkowski and Rigo: Tracing the meta-level, PyPy's tracing JIT compiler (ICOOOLPS 2009), and the PyPy documentation, <https://doc.pypy.org/en/latest/>
[^julia]: Bezanson et al.: Julia, dynamism and performance reconciled by design (OOPSLA 2018), and the Julia developer documentation, <https://docs.julialang.org/en/v1/devdocs/compiler/>
[^rsh-contextual]: Flückiger et al.: Contextual Dispatch for Function Specialization (OOPSLA 2020), <https://doi.org/10.1145/3428288>
[^rsh-brains]: Flückiger et al.: R Melts Brains, an IR for First-Class Environments and Lazy Effectful Arguments (DLS 2019), <https://doi.org/10.1145/3359619.3359744>
[^truffle]: Würthinger et al.: One VM to Rule Them All (Onward! 2013), <https://doi.org/10.1145/2509578.2509581>
[^hotspot]: Java SE 21 HotSpot VM guide: performance enhancements, tiered compilation, <https://docs.oracle.com/en/java/javase/21/vm/java-hotspot-virtual-machine-performance-enhancements.html>
[^dotnet-tiered]: .NET runtime design document: tiered compilation, and the compilation configuration reference, <https://github.com/dotnet/runtime/blob/main/docs/design/features/tiered-compilation.md>, <https://learn.microsoft.com/en-us/dotnet/core/runtime-config/compilation>
