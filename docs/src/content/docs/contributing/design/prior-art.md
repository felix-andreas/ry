---
type: Reference
title: Prior art from new language implementations
description: "What Roc, Bend, goose and Vx do best, and which of their ideas ry's R runtime adopts or rejects"
tags: [runtime, memory-management, compilation, type-system, ffi]
status: draft
generated: { by: ry-research-agent/1, at: 2026-10-31T16:00:00Z }
sources:
  - id: roc
    resource: https://github.com/roc-lang/roc
    title: "Roc repository: docs/langref, design/lambda-set-specialization.md, src/README.md"
    last_modified: 2026-10-31T00:00:00Z
  - id: perceus
    resource: https://www.microsoft.com/en-us/research/wp-content/uploads/2020/11/perceus-tr-v4.pdf
    title: "Reinking, Xie, de Moura and Leijen: Perceus, Garbage Free Reference Counting with Reuse"
  - id: bendrt
    resource: https://github.com/HigherOrderCO/Bend/blob/main/paper/BendRT.pdf
    title: "Taelin et al.: BendRT, a Parallel Runtime for CPUs and GPUs"
    last_modified: 2026-10-30T00:00:00Z
  - id: bend
    resource: https://github.com/HigherOrderCO/Bend
    title: "Bend repository: README.md, guide/EFFECTS.md"
    last_modified: 2026-10-30T00:00:00Z
  - id: goose
    resource: https://github.com/aardappel/goose
    title: "goose repository: README.md, docs/implementation.md, docs/reference.md"
    last_modified: 2026-10-30T00:00:00Z
  - id: vx
    resource: https://github.com/vx-lang/Vx
    title: "Vx repository: README.md, ROADMAP.md"
    last_modified: 2026-10-31T00:00:00Z
  - id: okf
    resource: https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md
    title: Open Knowledge Format v0.2
---

This is a design draft, and it is written as an [Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format) v0.2 concept document: the frontmatter carries the `type`, provenance (`sources`) and trust (`generated`) fields that format defines, and each claim about a project cites its source with a footnote keyed to a `sources` entry.[^okf]

It studies four recent language implementations for ideas that ry's R runtime ([the runtime design](/contributing/design/r-runtime/)) can use. The constraint that decides every verdict is the one that page establishes: R's C API must work unchanged, so any object a native package can reach is a non-moving heap object with a reference count, reached through a raw `SEXP` pointer, rooted by `PROTECT`, and possibly an ALTREP vector.

## What ry takes, in one table

| Idea | From | Verdict | Where it lands in ry |
| --- | --- | --- | --- |
| In-place update when the reference count is one ("functional but in place") | Roc (Perceus) | **Adopt** | Copy-on-modify for vectors, attributes and the table type |
| Borrow inference: arguments a callee only reads pay no count traffic | Bend | **Adopt** | The JIT's reference-count elision for R closures |
| Count only what is shared, found by a whole-program analysis | Bend | **Adopt, scoped** | Per-function in the JIT, never for objects native code can see |
| Defunctionalized closures (lambda sets) | Roc | **Adopt, scoped** | Specializing `lapply`, `vapply` and `purrr::map` call sites in the JIT |
| Several backends from one IR: interpreter, fast dev backend, optimizing backend | Roc | **Adopt** | Tier 0 interpreter plus Cranelift, with the same IR feeding both |
| Hindley-Milner inference that never needs hints | Roc | Already ry's model | No change |
| Static dispatch only, no dynamic dispatch | Roc | **Reject** | R's S3, S4 and R6 are dynamic; ry speculates instead |
| Host-provided allocator (platforms) | Roc | **Adopt, narrowly** | Arena allocation for scratch memory in `R_alloc` and the JIT |
| Bounds-check elimination over difference constraints | goose | **Adopt** | Subscript checks in JIT-compiled loops |
| Values built in their final destination | goose | **Adopt** | Result-vector preallocation in fused loops |
| No heap, data stacks only, no reference counts | goose | **Reject** | Incompatible with `SEXP` identity and `R_PreserveObject` |
| Static multi-frame returns instead of an unwinder | goose | **Reject** | R's conditions and restarts are dynamic |
| Compile to one C file, call C through `extern fn` | goose, Bend | **Reject** | ry is a JIT host for the existing C ABI, not a C emitter |
| Even-split parallel forks with no work stealing | Bend | **Reject** | R code makes no balance promise |
| Placement and residency in the type | Vx | **Adopt the idea, not the syntax** | A column is either an Arrow buffer or an R vector, tracked as a fact |
| Content-hashed global identifiers | Vx | **Reject** | Salsa's interned ids already give stable identity |
| LLVM and MLIR as the JIT backend | Vx | **Reject** | Compile latency; Cranelift is the choice |

## Roc

**What it is.** Roc ([roc-lang/roc](https://github.com/roc-lang/roc)) is a pure functional language by Richard Feldman and contributors, whose compiler is being rewritten in Zig. It has no 0.1 release yet.[^roc]

**What it does best for ry.** Roc's memory model is the closest match to R's own value semantics. Lists and other builtins are reference counted, and Roc's compiler applies the Perceus "functional but in place" scheme: a builtin that receives a value whose count is one updates it in place, and a shared value is shallowly cloned first, so `List.set` and `List.append` on a unique list cost nothing extra.[^roc][^perceus] That is exactly R's copy-on-modify contract (R decides by `MAYBE_SHARED`, which reads the reference count), expressed as a compiler discipline instead of a runtime check repeated at every call.

Roc compiles higher-order functions by **lambda set specialization**, a form of defunctionalization: by code generation time a closure is a tagged union whose tag names the procedure and whose payload is the captured environment, so every call is direct.[^roc] Its type inference is Hindley-Milner, and the language reference states that inference never needs hints, which is the same commitment ry made for its checker. Its only ad hoc polymorphism is static dispatch on nominal types; dynamic dispatch is unsupported by design.[^roc]

Roc's backends are an interpreter, a "dev" backend that emits machine code directly for fast builds, an LLVM backend for optimized builds, and WebAssembly.[^roc] Its platforms supply the allocator: the host implements `malloc` and `free` for the Roc code it embeds, which lets a web server platform allocate each request into an arena and reset it afterwards.[^roc]

**What ry adopts.**

- **Perceus-style reference counting in the JIT.** The interpreter keeps R's count updates at every binding, which is what native code observes. The JIT tracks counts precisely inside a compiled function, drops increments and decrements that cancel, and turns an update of a uniquely owned vector into a store, so `x[i] <- v` in a loop never copies.
- **Lambda sets at call sites that pass closures.** R code passes functions to `lapply`, `vapply`, `Map`, `purrr::map` and `Reduce` constantly. When the JIT sees a call like `vapply(xs, function(x) x * 2, double(1))` with a known closure, it specializes the loop for that closure and calls it directly instead of through a closure object. R semantics require that the closure object still exist if anything reflects on it, so this is a speculation with a deoptimization exit, not a whole-program transformation.
- **A shared IR for every tier,** as Roc shares its post-check IR between backends. ry's bytecode feeds the interpreter, and its SSA IR feeds Cranelift.
- **Arena allocation where nothing escapes.** `R_alloc` memory is already freed at the end of a `.Call`, so it is an arena by contract; ry implements it as a bump allocator reset at the call boundary. The JIT uses the same technique for temporaries it proves do not escape.

**What ry rejects.** Static-only dispatch, because R's S3 dispatch on `class(x)`, S4 generics and R6 methods are dynamic by definition, and existing code depends on that. ry gets most of the benefit by speculating on the class seen at a call site, guarded by the class attribute. Roc's whole-program monomorphization is also out of reach, because R loads code at runtime.

## Bend

**What it is.** Bend ([HigherOrderCO/Bend](https://github.com/HigherOrderCO/Bend)) is Higher Order Company's pure functional language with an affine dependent type system. The current implementation (in `bend2/`) is no longer the interaction-combinator design of HVM: its runtime paper says so explicitly, and drops HVM's graph reduction in favor of ownership.[^bendrt] The compiler emits one C file that runs on one thread, on CPU threads, or on the GPU through Metal or CUDA, over one heap, with no garbage collector.[^bendrt] Bend also checks proofs of user-declared laws (`LAWS.bend`), which is outside ry's scope.[^bend]

**What it does best for ry.** Bend's runtime is an argument that knowing ownership statically removes most runtime costs. Because the type system is affine, a value has one owner and the `match` that consumes it frees it, so deallocation is compiled code.[^bendrt] Two whole-program analyses decide the rest of the memory traffic: **share inference** finds the constructor types that need reference counts at all (a type used more than once, or reachable from one that is), and **borrow inference** finds arguments a callee only reads, which it lends raw so that "a fold over a reused tree costs no count traffic."[^bendrt] A term is one 64-bit word, and small values (a machine word, a constructor with one word-sized field) are packed into the word itself and never touch the heap.[^bendrt]

Parallelism is explicit and cheap: a parallel `let` binding several calls means "run these in parallel, they split the work evenly," and the scheduler trusts that promise, with no shared queue, no locks and no work stealing.[^bendrt] On the authors' benchmarks the sequential build runs within 0.8 to 1.5 times the time of hand-written C, sixteen threads give 9 to 12 times one thread, and an integrated GPU up to 67 times.[^bendrt] Foreign functionality enters through effects: an effect is a definition of type `IO(R)` whose body imports a `.c` (and a `.js`) file that registers a C implementation, spliced into the generated program.[^bend]

**What ry adopts.**

- **Borrow inference, per compiled function.** ry's checker already knows, per function, which parameters are read and which escape. The runtime type summary in the runtime design carries that fact, and the JIT passes a borrowed argument without touching its reference count. This is safe for R because a borrowed value is still a valid, rooted `SEXP` for the duration of the call.
- **Share inference, scoped to values native code cannot see.** A vector that a compiled loop creates and consumes, and that never reaches an environment, an attribute or a native call, needs no count, and often no heap object at all. The JIT's scalar replacement and fusion already cover the common case, and Bend's analysis is the template for the general one.
- **Word-packed small values in the JIT's frames,** which is what unboxing length-one vectors means in ry's design. They are boxed back into real `SEXP`s the moment they reach anything native code can observe.

**What ry rejects.**

- **Ownership as the memory model.** R values are freely aliased, and the C API exposes aliasing directly (`R_PreserveObject`, external pointer protection slots, environments). Affine typing cannot be retrofitted onto existing R code, so ry keeps a tracing collector plus reference counts and uses ownership only as an optimization inside compiled code.
- **Even-split forks without work stealing.** The balance contract is reasonable for a language designed around it, but R's parallel code (`foreach`, `parallel`, `future`) makes no such promise, and table operations need a work-stealing pool, which Polars already brings.
- **Effects spliced into generated C,** because ry hosts existing compiled packages through the C ABI rather than emitting C.

## goose

**Which project.** Several projects are called goose. This page studies [aardappel/goose](https://github.com/aardappel/goose), Wouter van Oortmerssen's memory-safe systems language, because it is the one that is a language implementation in the sense the comparison needs. The others are `goose-lang/goose`, a Go subset with a Coq semantics for verification, and smaller hobby languages.[^goose]

**What it does best for ry.** goose has no heap at all. Every dynamic value lives inline on data stacks the compiler assigns statically, growth advances a pointer, and memory is freed when its scope ends; there is no allocator, garbage collector, reference counting or destructor, and the type rules make the model memory safe without lifetime annotations.[^goose] Values are built in their final destination through any depth of calls, so `items.push(parse(line))` writes the parsed record directly into the array.[^goose] Its bounds-check elimination pass runs a flow-sensitive difference-constraint analysis (facts of the form `l <= r + c` over integer variables and array lengths, solved as shortest paths) and removes every check it can prove cannot fire.[^goose] The README reports that over sixteen benchmarks goose runs 1.16 times as fast as hand-optimized C++ and 1.12 times as fast as the best safe Rust, while using less memory.[^goose] It compiles to one C file and calls C through `extern fn`, with a bundled TinyCC backend that compiles and runs a program in-process.[^goose]

**What ry adopts.**

- **Bounds-check elimination over difference constraints.** R's subscripts are checked (an out-of-range read returns `NA`, a write extends the vector), so a JIT-compiled `for (i in seq_along(x)) y[i] <- x[i] * 2` carries checks on every iteration unless they are proven redundant. goose's domain is the right one: `i` ranges over `1..length(x)`, `length(y) >= length(x)` after preallocation, so both checks go. The pass must be sound only, as in goose, so that a program's meaning never depends on it.
- **Construction in place.** When the JIT fuses `z <- x * y + 1` it allocates `z` once, at its final size, and the fused loop writes into it. When a function's result is assigned straight into a list element or a column, the JIT can pass the destination down.

**What ry rejects.** The heapless memory model, static multi-frame returns, and shared-nothing threads all assume a language designed around them. R needs stable object identity, native code needs pointers that outlive scopes (`R_PreserveObject`), and R's condition system is dynamic. TinyCC-style in-process C compilation is interesting for compiling package code quickly in development, but it does not replace building packages with the platform's real compiler, which the C++-heavy part of the benchmark closure requires.

## Vx

**What it is.** Vx ([vx-lang/Vx](https://github.com/vx-lang/Vx)) is a research systems language, written in Rust and compiling through MLIR and LLVM, that puts hardware topology and memory placement into the type system. Its README describes it as an early research compiler at version 0.0.3.[^vx]

**What it does best for ry.** A Vx tensor's type carries its element type, its dimensions and its residency, for example `Tensor<f32, [4, 4], Memory::GPU_HBM>`, and the compiler rejects host code that reads device memory, transfers with no declared hardware path, and working sets that exceed a device's declared capacity.[^vx] Machines are described declaratively (memory capacities, bandwidths, links), so a program can be checked against a target it is not running on.[^vx] Its frontend identifies every symbol and instantiation by a 256-bit content hash and type-checks modules in parallel without locks, with output that is byte-identical across thread counts.[^vx] Its JIT is LLVM run through `lli`.[^vx]

**What ry adopts.** The idea that where a value's data lives is a fact worth tracking. ry's table columns live either in an Arrow buffer owned by Polars or in an R vector, and moving between the two costs a conversion when nulls are present (the runtime design, section 7). The runtime tracks residency per column and materializes an R vector only when R code or native code asks for one, much as Vx makes a transfer explicit. ry does not add residency to its user-visible type syntax, because R users should not have to write it.

**What ry rejects.** Content-hashed global identifiers, because the salsa database already gives every item and type an interned, stable identity, and a second identity scheme would be a second source of truth. MLIR and LLVM as the JIT backend, for the compile-latency reasons in the runtime design. Placement types as a language feature, because no benchmark code base targets accelerators.

[^okf]: Open Knowledge Format v0.2
[^roc]: Roc repository: docs/langref, design/lambda-set-specialization.md, src/README.md
[^perceus]: Reinking, Xie, de Moura and Leijen: Perceus, Garbage Free Reference Counting with Reuse
[^bendrt]: Taelin et al.: BendRT, a Parallel Runtime for CPUs and GPUs
[^bend]: Bend repository: README.md, guide/EFFECTS.md
[^goose]: goose repository: README.md, docs/implementation.md, docs/reference.md
[^vx]: Vx repository: README.md, ROADMAP.md
