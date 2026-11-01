---
type: Reference
title: Prior art from new language implementations
description: "What Roc, Bend, goose and Vx do best, and which of their ideas ry's R runtime adopts or rejects"
tags: [runtime, memory-management, compilation, type-system, ffi]
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
  - id: okf
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2
    last_modified: 2026-08-21T13:08:36-07:00
---

This page studies four recent language implementations for ideas that ry's R runtime can use, and
decides which ones it takes. One constraint decides every verdict, and it comes from
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

## Roc

**What it is.** Roc ([roc-lang/roc](https://github.com/roc-lang/roc)) is a pure functional language
whose compiler is being rewritten in Zig. It has no 0.1 release yet.[^roc]

**What it does best for ry.** Roc's memory model is the closest match to R's own value semantics.
Lists and other builtins are reference counted, and a builtin that receives a value whose count is
one updates it in place, while a shared value is shallowly cloned first.[^roc] So `List.set` on a
unique list costs nothing extra. Roc applies the Perceus "functional but in place" scheme to do
this.[^perceus] The decision is still made at runtime: Roc's design notes say that "runtime mutation
still uses `refcount == 1` to decide whether in-place mutation is allowed". The compiler's part is to
make the counts precise, inserting increments and decrements exactly where they are needed and
solving which arguments are only borrowed, so the runtime check succeeds as often as it can.[^roc]
That is R's copy-on-modify contract (R decides by `MAYBE_SHARED`, which reads the count), with a
compiler that keeps the counts tight.

When a function only reads an argument, passing it leaves its count unchanged, and the compiler works
this out by itself.[^roc] Higher-order functions compile by **lambda set specialization**, a form of
defunctionalization: by code generation time a closure is a tagged union whose tag names the
procedure and whose payload is the captured environment, so calls through it are direct (Roc also
has boxed erased callables for the cases this does not cover).[^roc] Its inference is Hindley-Milner,
and its only ad hoc polymorphism is static dispatch on nominal types; dynamic dispatch is
unsupported by design.[^roc]

Roc has an interpreter, a dev backend that emits machine code directly for fast builds (also for
wasm32), and LLVM for optimized builds, all fed from shared lower-level IRs.[^roc] Its platforms
supply the allocator: the host implements `malloc` and `free` for the Roc code it embeds, which lets
a web server allocate each request into an arena and reset it afterwards.[^roc]

**What ry adopts.**

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
- **Whole-program monomorphization,** because R loads code at runtime.
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
in parallel without locks, with output that is byte-identical across thread counts.[^vx] Its JIT is
LLVM run through `lli`.[^vx]

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
