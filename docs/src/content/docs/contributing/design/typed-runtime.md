---
title: The typed runtime
description: "Design draft for how typed ry runs: a typed core IR compiled ahead of time, Rust-like layouts and calls, uniqueness-based updates, and boxing only at the R boundary"
---

This draft designs how the typed core of ry runs, to the [runtime contract](/contributing/design/native-types/#runtime-contract)
of the native-types design. The bar is the typed benchmark programs within 1.5 to 2 times the time
of equivalent Rust programs, which means Rust-shaped data and calls: values in registers, flat
structs, direct calls, no per-value type checks, and no allocation that the program did not ask for.

## The shape in one example

```r
type Node = :Num(value: F64) | :Neg(inner: Node) | :Add(left: Node, right: Node)

evaluate <- function(node: Node) -> F64 {
  match (node) {
    :Num(value = v) => v
    :Neg(inner = e) => -evaluate(e)
    :Add(left = a, right = b) => evaluate(a) + evaluate(b)
  }
}
```

`evaluate` compiles, when its file loads, to one machine function `evaluate(node: *Node) -> f64`.
`Node` is a heap object of three words, a tag word and two payload words, so `match` is a load of
the tag and a jump table, `v` is a load of an unboxed double, and the recursive calls are direct
calls with the pointer in a register. Nothing checks a type, nothing allocates, and no R frame
exists. The interpreter sees `evaluate` as an ordinary closure: when untyped R code calls it, an
entry stub checks the argument against `Node` once and calls the same machine function.

## What exists, and why it is not the end shape

Typed closures already run as native functions (`decisions.md`, "Typed closures run as native
functions over records, tags and match"), and typed `orders` and `expr_tree` run 13 and 80 times
faster than GNU R. That path compiles R syntax with types it infers itself from the first call's
argument values, and it treats the types as speculation:

- Every field read checks the slot's representation, because a record's slot kinds come from
  allocation profiles, not from its type.
- Every native returns a status word, and a failure reruns the whole call in tier 0.
- Every record allocation goes through a runtime call that pushes a handle, and every loop back
  edge releases handles.
- Records live in the generic record object: 16-byte slots with a per-object unboxed mask.
- Strings, lists and the standard library are refused, so a typed program that builds a list runs
  at untyped speed.

The native-types contract makes the typed core sound: no `NA`, no attributes, no promises, no
dynamic scoping, exhaustive `match`, value semantics. With a sound type there is nothing to
speculate on, so the end shape compiles from the checker's types, not from runtime profiles, and
the checks above disappear instead of getting cheaper.

## Compilation pipeline

**The typed core IR.** For each strict-clean typed function, `semantics` lowers the body to a
typed core IR: a control-flow graph in SSA form over ry types, with every name resolved, every
generic instantiated and every `match` compiled to a decision tree (Maranget's algorithm, which
the exhaustiveness checker already runs). Standard library calls such as `List.map` and
`List.walk` are IR operations, not calls, so the backend can fuse and inline them. The IR is a
salsa query per function and instantiation, so an edit recompiles only the functions whose IR
changed. The runtime never reads the checker's internal structures (`r-runtime.md`, section 5);
the IR is the summary it reads.

**Ahead of time, cached.** `ry run` and `ry load` already check a strict file before it runs, so
they lower and compile every typed function in it before the first call, in parallel across
functions. The machine code is cached on disk under a hash of the IR and the target, which is the
one place [prior art](/contributing/design/prior-art/#vx) adopts content hashes. A cache hit costs
a map and a relocation; a miss costs Cranelift's compile time, about a millisecond per function,
paid once per edit.

**Generics.** Each instantiation the checker sees is compiled once, as Roc does. A generic function
called from untyped R code with an instantiation the checker never saw compiles at that call, from
the argument types the entry stub checked.

**Backend.** Cranelift, as for the existing JIT tier (`r-runtime.md`, section 5). LLVM would win
some throughput on numeric loops, but the bar is set on programs that call functions and walk
data, where layout and calls dominate.

## Representations

Each ry type has one machine representation in compiled code and one boxed form, the form the
interpreter and R code see. The boxed forms are the contract's; this section fixes the machine
side.

| Type | In registers and stack slots | Boxed |
| --- | --- | --- |
| `F64`, `F32` | one float register | length-one `double` (`F64`); native number object |
| `I8` to `I64`, `U8` to `U64` | one integer register | native number object |
| `I128`, `U128`, `Dec` | two integer registers | native number object |
| `Bool` | one byte | length-one `logical` |
| `Str` | pointer to a UTF-8 `CHARSXP` | length-one `character` |
| `List<T>` | pointer to a vector | the same vector |
| record, tuple | its fields, flattened, when it fits in four words; otherwise a pointer | a heap record |
| tag union | a tag word plus the largest payload, when that fits in four words; otherwise a pointer | a heap record |
| function | a code pointer and a captures pointer | a closure |

**Strings.** A `Str` is a `CHARSXP` pointer, which R already interns, so equality of two strings
from the same session is usually a pointer compare and the byte compare runs only when the
pointers differ. The bytes are UTF-8 by the contract, so `Str.count_utf8_bytes` is the stored
length. Building a string (`Str.concat`, `Str.join_with`) writes into a growable byte buffer and
interns once at the end, so a loop that appends to a string does not intern each step.

**Lists.** A `List<T>` is an R vector header followed by contiguous items at their natural width:
`REALSXP` for `F64`, `LGLSXP` for `Bool`, `STRSXP` for `Str` (as the contract requires, so those
pass to R and C without a copy), and a new vector kind per other number width, whose `TYPEOF` is
the native number code. A list of records or tags holds pointers, one per item, in a `VECSXP`. The
header's length and capacity fields make `List.append` amortized constant time, using the same
growable flag R's own vectors use.

**Records and tags on the heap.** A boxed record or tag is a new heap object kind with a static
layout: a header, the layout id, and the fields at their natural width, in an order the layout
fixes (pointers first, so the collector traces a prefix). The layout's pointer count is all the
collector needs, so the object carries no per-slot mask. This replaces the generic 16-byte-slot
record object for values whose type is a ry type; a record built by untyped R code keeps the
generic object, because its slots can hold any R value. The two kinds share the layout table, so
`match` on either reads the same layout id, and the typed entry stub converts a generic record to
the static form when a typed parameter receives one.

**Flattening.** A record or tag that never escapes a function is never allocated: its fields are
SSA values. An escaping one that fits in four words is passed and returned in registers, as Rust's
ABI does for small structs. A recursive type such as `Node` is always a pointer, because its size
is unbounded; its `:Num` case still costs one allocation of three words, not a record object of
16-byte slots.

## Calling convention

A compiled typed function takes its arguments flattened by the table above and returns its result
the same way, with Cranelift's multi-value returns for up to four words and a caller-provided
destination for anything larger (destination passing, as in goose and Roc's reuse analysis). It
has no environment parameter, no promise, no argument matching and no status word.

**Errors.** Integer overflow, a list index out of range, and an error raised by an R call at the
boundary are the only failures in typed code. Each jumps to a cold block that calls the runtime's
error routine, which unwinds to the nearest R context exactly as an R error does. The unwind
crosses no Rust frame, because compiled code calls only `extern "C"` runtime entries that are
themselves boundaries (`r-runtime.md`, "Errors across the boundary"). There is no retry in tier 0,
because a sound type cannot be wrong.

**Interrupts and recursion depth.** A loop back edge tests one flag, and a function entry compares
the stack pointer against a limit the runtime sets from R's `expressions` option and the thread's
stack size, so deep recursion raises R's error instead of overflowing the machine stack.

**Closures.** A typed closure that captures by value (the contract's rule) is a code pointer and a
heap block of its captured values at their natural width. Where the checker knows the exact
closure at a call site, as for a lambda passed to `List.map` in the same function, the call is
direct and the lambda inlines, which is Roc's lambda-set specialization restricted to what one
function can see. A closure that fails the capture rule keeps R's environment semantics and is
called through the interpreter.

**One call machinery.** A typed call from typed code is a direct machine call and needs no frame
at all. A call that crosses into or out of typed code uses the frame machinery the untyped
call-path redesign builds (stack-arena frames with lazily materialized environments), so typed
code adds entry and exit stubs to that machinery, not a second call path. An entry stub checks each
argument against its ry type once (a type code, a length of one, no attributes, not `NA` for a
scalar; a scan for a list from R), unboxes it, and calls the machine function. An exit stub boxes
the result.

## Memory

**Allocation.** A typed allocation is a bump in a thread-local nursery page of the heap's
size-segregated pages, inlined into compiled code, with a call into the runtime only when the page
is full. The collector stays non-moving (`r-runtime.md`, "Memory management"), because a `List<F64>`
handed to C must not move.

**Roots.** Compiled frames publish their live pointers through Cranelift's stack maps at
safepoints (calls and allocations), which the collector reads when it walks the machine stack. This
replaces the handle stack that natives push and release today, so a loop that allocates pays
nothing per iteration for rooting.

**Uniqueness.** `xs[[i]] <- v`, `List.set`, `List.append`, a record update `(..r, x = 1)` and the
string builders reuse their argument's storage when its reference count is one, and copy
otherwise, as the contract requires. The IR makes counts precise the way Roc's Zig compiler does
(`prior-art.md`, Roc): it infers which parameters are only borrowed, so passing a list to a function
that reads it changes no count, and it moves the last use of a variable instead of copying it, so a
loop `xs <- List.append(xs, x)` holds the only reference and appends in place. Where the IR proves
a value unique, the count check disappears; elsewhere it is one load and compare. Crossing to R
marks the value shared, so a ry update after R has seen a list copies it.

**Reference counts and the collector together.** Counts decide only whether an update may reuse
storage; the tracing collector frees memory. A typed program therefore never frees on a count
reaching zero, which keeps decrements off the hot path, and the collector reclaims dead nursery
pages in bulk.

## The R boundary

A typed function's entry and exit stubs are the boundary for calls; `Num.from_r`, `List.from_r`,
`List.to_r`, `Str.from_r` and `as.record` are the boundary for data. `F64`, `Bool` and `Str` cross
in constant time, because their boxed form is R's own scalar. `List<F64>`, `List<Bool>` and
`List<Str>` cross from ry to R without a copy, and from R to ry with one scan for `NA` and
attributes. Every other value crosses by conversion. A typed function that calls an R function
boxes its arguments, calls through the interpreter, and checks the result against the declared type
of the call, which is where an R function's wrong return surfaces as an error at a precise source
range.

## Measuring against the bar

- **Rust references.** Each typed benchmark program has a Rust twin in `tools/bench/why/rust/`, and
  `just bench-why` reports the typed time against it. The language session's hill climb runs on
  these.
- **Differential checks.** The oracle compares typed against untyped against GNU R on every typed
  benchmark program and on a typed oracle suite, and a fuzzer generates typed programs (records,
  tags, lists, strings, closures) and compares the compiled result with the interpreter's
  correct-but-slow implementation of the same IR.
- **Collector stress.** The same fuzzer runs under `RY_GC_STRESS`, which collects at every
  safepoint, to check the stack maps.

## What is built in one change

The typed core IR and its lowering from `semantics`, the Cranelift backend over it, the static
record and tag object kind, the native number vector kinds, stack-map rooting, the entry and exit
stubs, uniqueness inference, the disk cache, and the Rust twins. The existing native path in
`crates/jit/src/native.rs` keeps serving typed code that is not strict-clean, which still has
`#:`-annotated R types and speculation, and shrinks as more code is written in ry's own types.

## Open questions

- **Dropping the profile-driven native path.** Once the typed core compiles ahead of time, the
  native path from R syntax serves only typed code over R types. Whether that is worth keeping
  depends on how much of the benchmark code bases is rewritten in ry's types.
- **Where `List` capacity lives.** R vectors carry a true length for growable vectors, which gives
  `List.append` its capacity for free; a `List` handed to C then shows only its length, which is
  what C expects. To verify against data.table's use of `TRUELENGTH`.
- **Parallel compile at load.** Compiling a 300,000-line code base's typed functions takes seconds
  on a cold cache. Compiling lazily at first call with the cache filled in the background may be
  the better default for `ry run`, with the eager form for `ry load` and test runs.
