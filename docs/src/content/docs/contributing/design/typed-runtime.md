---
title: The typed runtime
description: "Design draft for how typed ry runs: a typed core IR compiled ahead of time, Rust-like layouts and calls, uniqueness-based updates, and boxing only at the R boundary"
---

This draft designs how the typed core of ry runs, built to the runtime contract of the
native-types design (`native-types.md` in this folder, section "Runtime contract"). The bar is the
typed benchmark programs within 1.5 to 2 times the time of equivalent Rust programs. That calls for
Rust-shaped data and calls: values in registers, flat structs, direct calls, no per-value type
checks, and no allocation the program did not ask for.

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

`evaluate` compiles to one machine function that takes a `Node` pointer in a register and returns
a double in a register. `match` loads the layout id and jumps through a table, `v` is a load of an
unboxed double, and the recursive calls are direct calls. Nothing checks a type, nothing allocates,
and no R frame exists. The interpreter sees `evaluate` as an ordinary closure: when untyped R code
calls it, an entry stub checks the argument against `Node` once and calls the same machine function.

## Why the native path is not the end shape

Typed closures run as native functions from their first call (`decisions.md`, "Typed closures run
as native functions over records, tags and match"), and typed `orders` and `expr_tree` run 13 and
80 times faster than GNU R. That path compiles R syntax with types it infers from the first call's
argument values, and it treats those types as speculation:

- Every field read checks the slot's representation, because a record's slot kinds come from
  allocation profiles, not from its type.
- Every native returns a status word, and a failure reruns the whole call in tier 0.
- Every record allocation goes through a runtime call that pushes a handle, and loop back edges
  release handles.
- Strings, lists and the standard library are refused, so a typed program that builds a list runs
  at untyped speed.

The native-types contract makes the typed core sound: no `NA`, no attributes, no promises, no
dynamic scoping, exhaustive `match` and value semantics. A sound type leaves nothing to speculate
on, so the end shape compiles from the checker's types, and the checks above disappear instead of
getting cheaper.

## Compilation pipeline

**The typed core IR.** For each strict-clean typed function, the analysis lowers the body to a
typed core IR: a control-flow graph in SSA form over ry types, with every name resolved and every
`match` compiled to a decision tree. The exhaustiveness checker runs Maranget's usefulness
algorithm; the IR adds the matching compilation from the same paper. Standard library calls such
as `List.map` and `List.walk` are IR operations, not calls, so the backend can fuse and inline them.

**Where the IR lives.** The runtime cannot link the analysis: the loader runs inside `libR` on ry's
runtime (`decisions.md`, "A strict reload re-checks through a persistent salsa database"), and the
JIT never reads the checker's structures (`r-runtime.md`, section 5). So the IR is a serialized
per-function summary that the analysis computes, as a salsa query per function, and stores with the
function's bytecode. A generic function's summary is its generic IR, which the runtime instantiates
when untyped R code calls it with an instantiation the checker never saw.

**Ahead of time, cached.** `ry run` and `ry load` check a strict file before it runs, so they can
compile every typed function in it before the first call. The machine code is cached on disk under
a hash of the IR and the target, which is the one place [prior art](/contributing/design/prior-art/#vx)
adopts content hashes. A cache hit costs a map and a relocation, and a miss costs Cranelift's
compile time, about a millisecond per function, once per edit. Each instantiation the checker sees
is compiled once, as Roc does.

**Backend.** Cranelift, as for the JIT tier (`r-runtime.md`, section 5). LLVM would win some
throughput on numeric loops, but the bar is set on programs that call functions and walk data,
where layout and calls dominate.

## Representations

Each ry type has one machine representation in compiled code and one boxed form, the form the
interpreter and R code see. The boxed forms are the contract's; this section fixes the machine side.

| Type | In registers and stack slots | Boxed |
| --- | --- | --- |
| `F64`, `F32` | one float register | length-one `double` (`F64`), or a native number object |
| `I8` to `I64`, `U8` to `U64` | one integer register | native number object |
| `I128`, `U128`, `Dec` | two integer registers | native number object |
| `Bool` | one byte | length-one `logical` |
| `Str` | pointer to a UTF-8 `CHARSXP` | length-one `character` |
| `List<T>` | pointer to a vector | the same vector |
| record, tuple | its fields, flattened, when they fit in four words, else a pointer | heap record |
| tag union | a layout id plus the largest payload, when that fits in four words, else a pointer | heap record |
| function | a code pointer and a captures pointer | closure |

**Strings.** A `Str` is a `CHARSXP` in canonical UTF-8. `Str.from_r` and `List.from_r` translate
latin1, native and bytes-encoded strings to UTF-8 (copying when the bytes change) and refuse a
string that has no UTF-8 form, so every `Str` is interned by its UTF-8 bytes alone. Equality of two
`Str`s is then exactly a pointer compare. A `CHARSXP` is shared through the intern table, so it is
never written in place: `Str.concat`, `Str.join_with` and interpolation write into a growable byte
buffer that is not a `Str`, and intern once at the end. A loop that appends to a string interns
once per finished string, not once per step.

**Lists.** A `List<T>` is an R vector header followed by its items. `List<F64>`, `List<Bool>` and
`List<Str>` are `REALSXP`, `LGLSXP` and `STRSXP` vectors, as the contract requires, so they pass to
R and C without a copy. A list of another number type is a native number vector at the item's
natural width, a vector kind whose `TYPEOF` is the native number code. C code sees that code and
none of R's accessors accept it, so such a list reaches C only through a conversion. A list of
records, tags, lists or functions is a `VECSXP` of pointers. `List.append` grows in place through
the growable flag and true length R's own vectors use (`gp::GROWABLE`).

**Records and tags.** A record, tuple or tag on the heap is the record object, whose layout id
names the field names, the tag and each slot's representation: an unboxed `F64`, an R integer, an R
logical, a ry number of a given width, or a pointer. The layout fixes the slot order, pointers
first, so the collector traces a prefix and the object carries no per-slot mask. Untyped R code
gets the same layout as typed code when its values have the same representations, so
`(x = 1, y = 2)` built by R and a `Record{x: F64, y: F64}` built by ry are one layout. A typed
parameter therefore checks a record from R by comparing its layout id with the type's, in constant
time and with no conversion. A record whose layout differs is refused with the type-mismatch error
the contract promises. Every builtin that reads records (`$`, `[[`, `identical`, printing,
`as.list`) works on that one object kind.

**Flattening.** A record or tag that never escapes a function is never allocated: its fields are
SSA values. An escaping one that fits in four words is passed and returned in registers, as Rust's
ABI does for small structs. A recursive type such as `Node` is always a pointer, and each `:Num`
costs one 32-byte object: the 16-byte header, the layout id, and the double.

## Calling convention

A compiled typed function takes its arguments flattened by the table above and returns its result
the same way, using Cranelift's multi-value returns for up to four words. A larger result is
written to a destination the caller passes as a hidden first parameter, which is destination
passing as in goose and Roc's reuse analysis. The function takes no environment, no promise and no
argument list.

**Errors are values.** An R error is a value inside Rust, and a `longjmp` across a Rust frame is
undefined behavior (`r-runtime.md`, "Errors across the boundary"). Compiled code sits between
Rust frames, both under it (the interpreter that called the entry stub) and above it (an R function
that typed code calls). So a function that can fail (overflow-checked arithmetic, a bounds-checked
read, or a call of an R function or of another fallible function) returns a failure flag in an
extra register, and its caller tests it after the call and branches to a cold block that returns
the failure in turn. The entry stub turns it into the R error. A function the IR proves infallible
returns no flag, and Cranelift's exception edges (`try_call`, in the pinned version) can replace the
flag test once measured.

**Redefinition.** R code can rebind any name at any time (`r-runtime.md`, section 5). A typed call
is a direct call when its callee is a binding in a locked namespace, which is every function of a
loaded package. In a script, where globals can be rebound, the call loads the code pointer from the
callee's binding cell, so it is one load and an indirect call, and rebinding the name clears the
cell's code pointer, which sends the next call through the interpreter.

**Entry and exit stubs.** A call from untyped R code into typed code goes through an entry stub,
and a call from typed code to an R function boxes the arguments, calls the interpreter, and checks
the result against the call's declared type. The typed core has no lazy evaluation, so an entry
stub forces each argument promise at entry, in order, and checks it once: a type code, a length of
one, no attributes and not `NA` for a scalar, a layout id for a record, and a scan for a list from
R. This differs from checking an annotated argument when it is forced, which is the rule for typed
R code, and the change is recorded in `decisions.md` when it is built. A function-typed parameter
cannot be checked at entry: an untyped closure passed to it gets a wrapper that checks each call's
arguments and result, and a typed closure passes as its code pointer and captures.

**One call machinery.** A typed call from typed code needs no frame. A call that crosses into or
out of typed code uses the frame machinery of the untyped call-path redesign (stack-arena frames
with lazily materialized environments), so typed code adds stubs to that machinery, not a second
call path.

**Closures.** A typed closure that captures by value under the contract's rule is a code pointer
and a heap block of its captured values at their natural width. Where the checker knows the exact
closure at a call site, as for a lambda passed to `List.map` in the same function, the call is
direct and the lambda inlines, which is Roc's lambda-set specialization restricted to what one
function can see. A closure that fails the capture rule keeps R's environment semantics and is
called through the interpreter.

**Loops.** Interrupts are tested at loop back edges with one load. A list read is a bounds check
and a load, and the IR removes bounds checks it proves redundant, with goose's difference
constraints over loop indices (`prior-art.md`, goose). Recursion depth is checked at function
entry against a stack limit the runtime derives from R's `expressions` option and the thread's
stack size.

## Memory

**Allocation.** The heap is one `&mut Heap`, a non-moving mark-sweep collector with sticky marks
and size-class pages (`r-runtime.md`, "Memory management"). Compiled code allocates by an inlined
pop from the size class's free list, with a runtime call only when the list is empty, and it never
moves an object, because a `List<F64>` handed to C must stay put. An in-place store of a pointer
into an existing object goes through the write barrier (`Heap::write_barrier`), because an old
object that gains a young child must be rescanned by the next minor collection.

**Roots.** Compiled code publishes its live pointers through Cranelift's user stack maps, which
spill each live pointer to a stack slot at each safepoint (a call, including an allocation that
calls the runtime). A loop with no call in its body spills nothing. An entry stub pushes an
activation record (the frame pointer and the return address) onto a list the collector walks, so
it finds compiled frames between Rust frames. Every collection, minor ones included, scans every
live compiled frame. This replaces the handle stack that natives push and release.

**Uniqueness.** `xs[[i]] <- v`, `List.set`, `List.append` and a record update `(..r, x = 1)` reuse
their argument's storage when it is uniquely held, and copy otherwise, as the contract requires
(`decisions.md`, "Standard library updates reuse a uniquely held value"). The heap's reference
count is a `u16` that counts references from heap objects and bindings, and registers and stack
slots hold uncounted references. A value is uniquely held when its count is zero and the IR proves
the update is the last use of the only register reference. The IR maintains the counts:

- Storing a pointer into an object or a binding increments its count, and overwriting or removing
  one decrements the old value's count.
- Reading a field out of a record that dies at that point moves the field: the record's slot is
  cleared and the field's count drops. So
  `state <- (..state, items = List.append(state$items, x))` holds `items` uniquely and appends in
  place, instead of copying the list each iteration.
- A parameter the callee only reads is borrowed: passing it changes no count. A variable's last use
  is a move, not a copy.
- A count that reaches its maximum stays there, so the value is shared for good and every later
  update copies.

Counts never free memory: the collector does. Decrements stay on the paths where a container is
overwritten or dies, but no compiled code ever runs a free.

Crossing to R marks the value shared, so a ry update after R has seen a list copies it, and R's
copy-on-modify contract holds for C code that writes into a list it was given.

## The R boundary

The entry and exit stubs are the boundary for calls, and `Num.from_r`, `List.from_r`, `List.to_r`,
`Str.from_r` and `as.record` are the boundary for data. `F64` and `Bool` cross in constant time,
because their boxed form is R's own scalar, and so does a `Str` already in UTF-8. `List<F64>`,
`List<Bool>` and `List<Str>` cross from ry to R without a copy, and from R to ry with one scan for
`NA`, attributes and encoding. Every other value crosses by conversion.

## Measuring against the bar

- **Rust references.** Each typed benchmark program has a Rust twin in `tools/bench/why/rust/`, and
  `just bench-why` reports the typed time against it. The language session's hill climb runs on
  these.
- **Differential checks.** The oracle compares typed against untyped against GNU R on every typed
  benchmark program and on a typed oracle suite. A fuzzer generates typed programs (records, tags,
  lists, strings, closures) and compares the compiled result with the interpreter's implementation
  of the same program.
- **Collector stress.** The same fuzzer runs under `RY_GC_STRESS`, which collects at every
  allocation, to check the stack maps and the activation list.

## What is built in one change

The typed core IR and its lowering in the analysis, its serialized form beside the bytecode, the
Cranelift backend over it, layouts with slot representations in place of per-object masks, the
native number vector kinds, stack-map rooting with the activation list, the entry and exit stubs,
uniqueness inference, the disk cache and the Rust twins. The native path in
`crates/jit/src/native.rs` keeps serving typed code over R types that is not strict-clean.

## Open questions

- **Dropping the profile-driven native path.** Once the typed core compiles ahead of time, the
  native path from R syntax serves only typed code over R types. Whether that is worth keeping
  depends on how much of the benchmark code bases is rewritten in ry's types.
- **Growable lists handed to C.** `List.append` capacity lives in the true length, which
  data.table also reads and writes on vectors it is given. To verify that a list reaching
  data.table through the boundary cannot be corrupted by that use.
- **Eager or lazy compilation at load.** Compiling a 300,000-line code base's typed functions takes
  seconds on a cold cache. Compiling at first call and filling the cache in the background may be
  the better default for `ry run`, with eager compilation for `ry load` and test runs.
- **Untyped records with a mixed layout.** Untyped code may build records of one field set whose
  slots take different representations from call to call, which multiplies layouts. Whether that
  needs a cap with a fallback to all-pointer slots is to be measured on the benchmark code bases.
