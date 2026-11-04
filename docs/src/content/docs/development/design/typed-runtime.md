---
title: The typed runtime
description: "Design draft for how typed ry runs: a typed core IR compiled ahead of time, native values freed by compiler-inserted reference counts, Rust-like layouts and calls, and conversion only at the R boundary"
---

This draft designs how the typed core of ry runs, built to the runtime contract of the
native-types design (`native-types.md` in this folder, section "Runtime contract"). The bar is the
typed benchmark programs within 1.5 to 2 times the time of equivalent Rust programs. That calls for
Rust-shaped data and calls: values in registers, flat structs, direct calls, no per-value type
checks, no tracing collector, and no allocation the program did not ask for.

The contract gives `List<F64>`, `List<Bool>` and `List<Str>` R's vector layouts, so that they
pass to C without a copy, and makes a `Str` a `CHARSXP`. That assumes typed values live on the R
heap. This draft makes them native values freed by reference counts (`decisions.md`, "Typed values
are native, immutable, acyclic and freed by reference counts"), so every list crosses to R by a
copy, and the contract is amended to match.

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

`evaluate` compiles ahead of time to one machine function that takes a `Node` reference in a
register and returns a double in a register. The reference is a 32-bit index with the tag in its
upper bits, so `match` jumps through a table on bits it already holds. Reading `v` is one load
from the `:Num` value column at that index, and the recursive calls are direct calls. `node` is borrowed, so
the function touches no reference count. Nothing checks a type, nothing allocates, and no R frame
exists. The interpreter sees `evaluate` as an ordinary closure: when untyped R code calls it, an
entry stub handles the R value it is given (see "Facts and guesses") and calls a machine function.

## Why the native path is not the end shape

Typed closures run as native functions from their first call (`decisions.md`, "Typed closures run
as native functions over records, tags and match"), and typed `orders` and `expr_tree` run 13 and
80 times faster than GNU R. That path compiles R syntax with types it infers from the first call's
argument values, and it treats those types as speculation:

- Every field read checks the slot's representation, because a record's slot kinds come from
  allocation profiles, not from its type.
- Every native returns a status word, and a failure reruns the whole call in tier 0.
- Every record is an R heap object, allocated through a runtime call that pushes a handle, and
  traced by the collector.
- Strings, lists and the standard library are refused, so a typed program that builds a list runs
  at untyped speed.

The native-types contract makes the typed core sound: no `NA`, no attributes, no promises, no
dynamic scoping, exhaustive `match`, and immutable values. A sound type leaves nothing to speculate
on, so the end shape compiles from the checker's types, and the checks above disappear instead of
getting cheaper. Immutable values also cannot form a cycle, which is what lets the typed core drop
the tracing collector (see "Memory").

## Compilation pipeline

**The typed core IR.** For each strict-clean typed function, the analysis lowers the body to a
typed core IR: a control-flow graph in SSA form over ry types, with every name resolved and every
`match` compiled to a decision tree. The exhaustiveness checker runs Maranget's usefulness
algorithm, and the IR adds the matching compilation from the same paper. Standard library calls
such as `List.map` and `List.walk` are IR operations, not calls, so the backend can fuse and inline
them. Reference-count operations are inserted into the IR (see "Memory"), so the backend sees them
as ordinary instructions it can move and cancel.

**Where the IR lives.** The runtime cannot link the analysis: the loader runs inside `libR` on ry's
runtime (`decisions.md`, "A strict reload re-checks through a persistent salsa database"), and the
JIT never reads the checker's structures (`r-runtime.md`, section 5). So the IR is a serialized
per-function summary that the analysis computes, as a salsa query per function, and stores with the
function's bytecode. A generic function's summary is its generic IR, which the runtime instantiates
when untyped R code calls it with an instantiation the checker never saw.

**Ahead of time.** A typed function never runs in an interpreter. `ry run` and `ry load` check a
strict file before it runs, and a typed function is compiled before its first call: when the
lazy loader first forces its binding, so a session compiles only what it uses, as the mandate for
`ry load` requires. While the program runs, worker threads compile the functions that a compiled
function can call, which value tracking lists, so most calls find their callee compiled.
`ry install` compiles all of a package's typed functions and writes the machine code into the
installed package, beside its R code, as part of the package's build output. Each instantiation the checker
sees is compiled once, as Roc does. Compiling a function at a later call happens only for a generic
instantiation that untyped code creates and for the second copies in "Facts and guesses".

**No code cache.** Compiled code is a build output, not a cache. An installed package holds one
code file for its namespace, written by `ry install` and replaced only by reinstalling, and a
standalone executable holds one for its program. A code file is stamped with the ry build that
wrote it and with the interface hashes of the namespaces it calls into, and a stamp that does not
match at load sends the namespace through compilation in memory with a warning to reinstall. Code that `ry run` and `ry load` compile for a
script or a package under development lives in memory for the process. That costs Cranelift's
compile time, about a millisecond per function on a worker thread, for each function a session
uses, which keeps
the fast path free of a cache to key, invalidate and keep coherent. If start-up time on the
benchmark code bases says otherwise, the fix is a faster baseline compiler (`prior-art.md`, the
copy-and-patch tier), not a cache.

**Mapped, shared code.** A code file is position-independent, and everything that differs between
processes (runtime entry points, interned `CHARSXP` pointers for string constants that cross to R,
R symbols, the base addresses of the value pools) is loaded from a small table that each process
fills when it maps the file. The code pages are mapped read-only straight from the file, so the four
test processes a CI machine runs share one copy of every installed dependency's typed code, and
only the table is private to each. `ry install` writes the file to a temporary name and renames it
into place, so a process never maps a half-written file.

**Standalone executables.** `ry build --exe main.R` writes one executable in the spirit of
`bun compile`: ry's runtime, the bytecode of the script and of the strict files it sources, and the
code file for their typed code, appended as a section that the executable maps read-only at
start. Running the executable runs the script as `ry run main.R` does, with its arguments in
`commandArgs()`. Plain `ry build` on a package writes a source tarball as `devtools::build()` does,
which existing workflows depend on, so the executable is an option of the same command for a
script, which has an entry point, and not its default for a package, which has none. Packages with compiled code
load from the library as they do under `ry run`, because their shared objects are built against
`libR` and are not part of the program.

**Value tracking.** Inside one locked namespace or one strict file, the typed core is a closed
program: it has no `eval` and no dynamic scoping, and its bindings are locked, so the analysis can
work out, for each variable, parameter and field, which allocation sites and which closures can
reach it, and for each call, which functions it can reach. Bun's compiler does this for a whole
program ([prior art](/development/design/prior-art/#buns-compiler)). ry does it per namespace,
because code outside a namespace reaches it through every function that escapes: an exported
function, a registered S3 method, and any closure the namespace hands to R. Those functions'
parameters only ever hold guesses. The results drive closure specialization across functions (see
"Closures" below) and the facts the next section relies on. A locked binding can still be
unlocked, so every result derived from one is discarded with the code that used it (see
"Redefinition" below). Untyped R code is never closed, since `eval`, `get` and `assign` reach any
binding, so it keeps the allocation profiles of the native path.

**Backend.** Cranelift, as for the JIT tier (`r-runtime.md`, section 5). LLVM would win some
throughput on numeric loops, but the bar is set on programs that call functions and walk data,
where layout and calls dominate.

## Facts and guesses

A type in compiled code is either a fact or a guess, a split that comes from Bun's compiler. A
fact describes a value the typed core created, such as a record built by `(x = 1, y = 2)` in typed
code or a list returned by `List.map`. Such a value is a native ry value with the layout its type
fixes, so a field read is one load with no check, and a fact costs nothing at run time. A guess
describes a value that arrives from outside the typed core: an argument from untyped R code, a
value returned by an R function, or a value read from a binding R code can write. Such a value is
an R value under the collector, and a guess costs one check where it arrives, and nowhere else.

The check reads the R value's type code, length and attributes, which takes constant time for a
scalar, a record, a tag or an R vector of the right type. Then:

- a value that does not have the declared type is refused with the type-mismatch error the
  contract promises, so annotations keep their meaning at the boundary;
- a value that has the type goes to a second compiled copy of the function in which only that
  parameter is dynamic. That copy reads the R value in place: a field by its name and its slot's
  recorded representation, a list item through R's accessors. Every other parameter stays a fact,
  and the value is neither converted nor copied. The check for a list's items (no `NA`, the right
  layout for each record) runs as each item is read, so a function that reads three items of a
  long list pays for three.

Bun compiles every second copy at build time, because its executables carry no compiler. ry has a
compiler at run time, so a second copy is compiled at the first call that needs it and kept beside
the first. A function whose parameter receives only R values runs its second copy on every call,
which is slower than converting once when the function reads the value many times. So
`ry check --explain` reports such a parameter (see "Explaining slow paths") with the fix to convert
explicitly where the value enters, with `List.from_r` or `as.record`.

## Representations

Each ry type has one native representation in compiled code and one R form, the form the
interpreter and R code see. Native values live outside the R heap (see "Memory"). The R forms are
the contract's, and this section fixes the native side.

| Type | In registers and stack slots | Storage | R form |
| --- | --- | --- | --- |
| `F64`, `F32` | one float register | none | length-one `double` (`F64`), or a native number object |
| `I8` to `I64`, `U8` to `U64` | one integer register | none | native number object |
| `I128`, `U128`, `Dec` | two integer registers | none | native number object |
| `Bool` | one byte | one bit in a list or record | length-one `logical` |
| `Str` | three words, the bytes inline up to 23 | UTF-8 bytes past 23 in a counted buffer | length-one `character` |
| `List<T>` | a pointer, or inline on the stack | a counted buffer, one column per field of `T` | an R list |
| `Dict<K, V>`, `Set<T>` | a pointer | entry columns and an index array, or a flat array and a bitset for dense keys | ry's `dict` or `set` kind |
| record, tuple | its fields, flattened, when they fit in four words, else a reference | a slot in its type's pool | R record |
| tag union | the tag and payload, when they fit in four words, else a reference with the tag in its upper bits | a slot in its variant's pool | R tag |
| function | a code pointer and a captures reference | a slot in its lambda set's pool | closure |

A **reference** is a 32-bit index into a pool, not a pointer. A pool holds every heap value of one
record type or one variant, as a struct of arrays: one column per field and one column of
reference counts, each a flat array indexed by the reference. A variant is a tag name with its
payload types, and tags are structural, so one `:Num(value: F64)` pool serves every union that
contains that variant. A reference uses its lower 28 bits as the index into its pool, which allows
2^28 live values per pool, and for a value of a closed union its upper 4 bits hold the variant's
index in that union's sorted tag list, so `match` reads the tag without a load. Coercing a value
from one closed union to a wider one re-encodes the upper bits through a table the compiler builds
for that coercion. A union of more than 16 variants, and an open union (`..`), whose variants are
not known, use a 64-bit reference instead: a 32-bit variant id, unique in the process, and the
32-bit index. A pool that outgrows its index space is a run-time error that names the type.

Pools are per process, and each column's base address is in the table each code file reads at
load. A field read is one load at `base + index * width`, with the base kept in a register across
code that cannot allocate from that pool. A column grows by reallocation, so the base is read again
after any call or allocation that may grow it, which costs one load from the table, a cache line
that stays hot.

**Strings.** A `Str` is UTF-8 bytes with no terminator, as in Roc and Rust. A string of up to 23
bytes is stored inline in its three-word value, so most keys, labels and identifiers never
allocate. A longer one points at a counted buffer of bytes, since strings have no fixed size to
pool by. Equality compares the lengths and then
the bytes, and `Str.concat`, `Str.join_with` and interpolation append into a uniquely held string in
place. A string crosses to R by interning its bytes as a UTF-8 `CHARSXP`, and `Str.from_r`
translates a latin1, native or bytes-encoded `CHARSXP` to UTF-8 and copies the bytes, refusing a
string that has no UTF-8 form.

**Lists.** A `List<T>` is a counted buffer with the length, the capacity and the items as a
struct of arrays: doubles and integers at their natural width, `Bool` as a bitset, strings as
three-word values, and a record or tag item as one column per field, so a
`List<Record{x: F64, y: F64}>` is an array of `x` values followed by an array of `y` values, and a
loop that sums `x` reads contiguous doubles. A list of a union type whose values fit in four words
stores a tag column, one byte per item, and payload columns overlaid across variants: payload
column `k` holds the `k`th field of whichever variant the item is, at the widest width any variant
gives that position. An item of a type that is a reference, a recursive union such as `Node`
included, is a single column of references, which already carry their tags. `List.append` grows in place when the list is uniquely held and its capacity
allows, and doubles the capacity otherwise. A list that does not escape its function, and is
passed only to borrowing parameters, gets inline capacity on the stack, eight items, when the IR
bounds its length by that constant or the list is built by a loop with no proven bound. It moves
to a heap buffer if it grows past the inline capacity. A stack list is never dropped: its storage
ends with the frame, and passing it to an owning parameter is an escape, which gives it a heap
buffer from the start.

**Dicts and sets.** When the key type is `Bool`, `U8`, `I8` or a union of payload-free tags, which
have at most 256 values, a `Dict` is a flat array of values indexed by the key, a bitset of the
keys present, and a byte array of the keys in insertion order, and a `Set` drops the value array.
A lookup is a bit test and a load with no hashing, and iteration walks the order array, so the
order is insertion order as `Dict.keys` promises.
Every other `Dict` keeps its entries in insertion order as a key column and a value column, plus
an open-addressing index array of 32-bit entries, which is the layout of Roc's `Dict` and of
Python's. The lower 25 bits of an index entry are the position of its entry in the columns, and
the upper 7 bits are a fragment of the key's hash, so a probe rejects almost every wrong slot
without reading the key column. An entry of all ones marks an empty slot, and a `Dict` past 2^25
entries moves to 64-bit index entries. Iteration walks the dense columns and a lookup probes the
index. Removal marks the entry's position in a bitset of removed entries instead of moving
another entry into it, so iteration stays in insertion order, and the columns are compacted when
removed entries reach half of them. Iteration order is insertion order, so the hash seed never
reaches output. The seed is drawn once per process, which keeps input from a web request or a
parsed file from forcing collisions on purpose, and the test harnesses fix it from their one seed
so a failing run replays. A hashed `Set<T>` is a `Dict<T, {}>` with no value column.

**Records and tags.** A record's fields are stored in name order, the order R-side records use
(`crates/interp/src/records.rs`), so converting between the two never reorders. A tag stores its
tag as the index of its name in the union's sorted tag list, as `records-and-tags.md` assigns it,
and each variant has its own pool with exactly that variant's fields. A slot has no header: its
count is in the pool's count column and its type is the pool's, which compiled code knows
statically, so a value carries no layout id and no type word.

**Flattening.** A record or tag that never escapes a function is never allocated: its fields are
SSA values. An escaping one that fits in four words is passed and returned in registers, as Rust's
ABI does for small structs, and stored inline in lists and other records. A recursive type such as
`Node` is always a reference, and each `:Num` costs 12 bytes, a double in the value column and a
count in the count column, while an `:Add` costs two 4-byte references and a count. Evaluating a
tree reads only the columns of the variants it meets.

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
extra register, and its caller tests it after the call and branches to a cold block that drops its
live native values and returns the failure in turn. The entry stub turns it into the R error. A
function the IR proves infallible returns no flag, and Cranelift's exception edges (`try_call`, in
the pinned version) can replace the flag test once measured.

**Redefinition.** R code can rebind any name at any time (`r-runtime.md`, section 5). A typed call
is a direct call when its callee is a binding in a locked namespace, which is every function of a
loaded package. In a script, where globals can be rebound, the call loads the code pointer from the
callee's binding cell, so it is one load and an indirect call, and rebinding the name clears the
cell's code pointer, which sends the next call through the interpreter.

Bun cannot discard compiled code, so it makes its assumptions true by freezing built-ins: replacing
a member of `Array.prototype` throws. ry cannot freeze R's bindings, because existing code changes
locked bindings on purpose: testthat's `local_mocked_bindings` (through rlang's
`env_binding_unlock`) and `assignInNamespace` both unlock a namespace binding and rebind it. So each
compiled file records which locked bindings its direct calls, closure sets and facts assume, and
the binding-unlock primitive (`R_unlockBinding`, which the R function `unlockBinding` and rlang
both reach) discards the code that assumed the binding. That code recompiles with a cell load at the
affected call. A test suite that mocks in every test recompiles every function that called a mocked
binding directly, once per test, and code that never unlocks pays nothing.

ry's own standard library is frozen instead: its namespaces cannot be unlocked, and no existing R
code can depend on rebinding them. A program or package may still define its own `List.map`, which
comes before the standard library's binding, so a call is direct and inlinable only where the
checker resolves the name to the standard library. Elsewhere it takes the binding-cell path.

**Entry and exit stubs.** A call from untyped R code into typed code goes through an entry stub,
and a call from typed code to an R function converts the arguments to their R forms, calls the
interpreter, and checks the result against the call's declared type. The typed core has no lazy
evaluation, so an entry stub forces each argument promise at entry, in order, and checks it once
as "Facts and guesses" describes. This differs from checking an annotated argument when it is
forced, which is the rule for typed R code, and the change is recorded in `decisions.md` when it
is built. A function-typed parameter cannot be checked at entry: an untyped closure passed to it
gets a wrapper that checks each call's arguments and result, and a typed closure passes as its
code pointer and captures.

**One call machinery.** A typed call from typed code needs no frame. A call that crosses into or
out of typed code uses the frame machinery of the untyped call path (stack-arena frames with lazily
materialized environments), so typed code adds stubs to that machinery, not a second call path. The
stubs reach that machinery through one narrow interface (enter an R call with these arguments,
return this value or this failure), so the frame redesign can land without touching compiled code.

**Closures.** A typed closure that captures by value under the contract's rule is a code pointer
and a reference to a slot in its lambda set's pool, which holds the captured values at their
natural width, one column per capture. Where the lambda set is not known, as for a function value
received from outside the namespace, the code pointer points just past a small header that holds
the closure's drop function, so dropping the value calls through it. A closure never captures
itself: recursive and mutually recursive local functions are lifted to top-level definitions that
call each other by name and take their captures as extra parameters, as in Roc, which keeps
closures acyclic. Where the checker knows the exact closure at a call site, as for
a lambda passed to `List.map` in the same function, the call is direct and the lambda inlines,
which is Roc's lambda-set specialization. Value tracking extends it across functions in one
namespace: a function-typed parameter that only lambdas of its own namespace reach is specialized
on that set, so each call is a direct call or a jump on a small closure id. A parameter that also
receives closures from outside, which every exported function's parameters do, keeps the indirect
call through the code pointer. A closure that fails the capture rule keeps R's environment
semantics and is called through the interpreter.

**Loops.** Interrupts are tested at loop back edges with one load. A list read is a bounds check
and a load, and the IR removes bounds checks it proves redundant, with goose's difference
constraints over loop indices (`prior-art.md`, goose). Recursion depth is checked at function
entry against a stack limit the runtime derives from R's `expressions` option and the thread's
stack size.

## Memory

Typed values are immutable and acyclic, so a reference count is exact: a value is freed when its
count reaches zero, and nothing can keep a dead value alive in a cycle. Native typed values are
therefore freed deterministically by reference counts that the compiler inserts, in the style of
Perceus (Koka), Roc and Lean 4, and no tracing collector ever sees them. R values that typed code
receives or produces stay R heap objects under ry's mark-sweep collector.

**Allocation.** A fixed-size value takes a slot in its type's pool: compiled code pops a free index
from the pool's free stack, a flat array of indices, or bumps the pool's length when the stack is
empty, and calls the runtime only to grow the columns. Freeing pushes the index back. A list
buffer or a long string comes from a size-class allocator apart from the R heap, with the same
inlined pop and push. Neither returns memory while a program runs, so a steady loop reuses the
slots the previous iteration freed and touches only warm memory.

**Counts.** The IR inserts a `dup` where a value gains an owner and a `drop` where an owner's last
use passes, and then removes what it can:

- **Borrowing.** A parameter the callee only reads, or passes on to a borrowing parameter, is
  borrowed: the caller keeps ownership, and neither side touches the count. Lean 4's borrow
  inference decides this per parameter from the function's body, so most reads of a list, a
  record or a string touch no count.
- **Moves.** A variable's last use is a move, so passing it costs nothing, and a `dup` followed by
  a `drop` of the same value cancels.
- **Field moves.** An update `(..r, f = e)` whose source `r` dies at that point takes every field
  out of `r` instead of duplicating them, and the reads of `r` inside `e` read the taken fields. So
  in `state <- (..state, items = List.append(state$items, x))`, `items` is held uniquely and appends
  in place, and the new record reuses the old one's slot, instead of copying the list each
  iteration.
- **Drop specialization.** A `drop` of a value whose type is known inlines the decrement and, on
  zero, drops each field and pushes the slot on its free stack. A recursive type is freed by a loop
over a work list,
  not by recursion, so dropping a long chain of `Node`s cannot overflow the stack.

**Reuse.** `xs[[i]] <- v`, `List.set`, `List.append`, `Dict.insert`, `Str.concat` and a record update
`(..r, x = 1)` update their argument in place when its count is one, and copy it otherwise, as
the contract requires (`decisions.md`, "Standard library updates reuse a uniquely held value").
Beyond those, a slot that dies at a point where a value of the same type or variant is built is
reused for the new value without going through the free stack: a `match` arm that takes a `:Neg`
apart and builds a `:Neg` writes into the old slot. This is Perceus's reuse analysis, which Roc applies the
same way.

**References go one way.** A native value can hold an R value, through the explicit boundary types
such as an opaque `R` value field, but an R value never holds a native one: every native value
that crosses to R is converted to its R form, a typed closure included, whose R form is an R closure
over the R forms of its captures that calls the compiled body through an entry stub. Without
references from R into the native heap, no cycle can run through both heaps, so a preserved R value
can never keep the native value that preserves it alive.

**R values inside typed code.** An R field in a native value holds a preserved reference: it is
registered with the collector's precious set when stored and released when its value is dropped.
An R value in a compiled function's locals is rooted on the collector's handle stack: the entry
stub, an exit stub's result and every conversion to an R form push it, and the function releases to
its handle mark at return and on the failure path. Native values need no rooting at all, so compiled
frames carry no stack maps, and the roots `r-runtime.md` lists for JIT frames are handle-stack
entries for typed code.

**Threads.** R evaluation is single-threaded, so counts are plain 32-bit integers with no atomic
operations. A count that would overflow saturates, and a saturated value is never freed, which
leaks it rather than corrupting memory, and is reported by the leak check below.

## The R boundary

The entry and exit stubs are the boundary for calls, and `Num.from_r`, `List.from_r`, `List.to_r`,
`Str.from_r` and `as.record` are the boundary for data. A guess that a function only reads crosses
with no conversion, through the second copy. Converting copies: an R vector becomes a native list
in one pass that also checks for `NA`, attributes and encoding, and a native list becomes an R
vector in one pass. `F64` and `Bool` cross in constant time in both directions, because their R
form is R's own scalar.

A `List<F64>` that crosses to R by conversion becomes a `REALSXP` copy. An ALTREP vector over the
native buffer would avoid the copy for R code that only reads, but most C code asks for a writable
pointer through `REAL()`, which would force the copy anyway, so the zero-copy wrapper is deferred
until the benchmark code bases show the copy matters.

## Explaining slow paths

Typed code can still run slowly where it meets R, and the reason is invisible in the source. So
`ry check --explain` lists every place in strict-clean typed code that compiles to a slow path,
worst first, with the fix for each, as Bun's `--explain` does. It reads the same facts the backend
uses, so it needs no run and no profile. The places it reports are:

- a parameter that receives only R values and runs its second copy, with the fix to convert once
  with `List.from_r` or `as.record` where the value enters;
- a conversion inside a loop, such as a `List.to_r` whose result goes back into typed code;
- a call from typed code to an R function, which converts its arguments and checks its result;
- a closure that fails the capture rule, which runs through the interpreter;
- a function-typed parameter called indirectly because closures from outside the namespace reach it;
- a call through a binding cell, in a script whose globals can be rebound.

Worst first means a slow path inside more nested loops ranks higher, because no profile exists to
rank by measured time. `--explain=json` prints one finding per line, for editors and CI. The
user-facing wording and the diagnostic codes follow the native-types contract.

## Measuring against the bar

- **Rust references.** Each typed benchmark program has a Rust twin in `tools/bench/why/rust/`, and
  `just bench-why` reports the typed time against it.
- **Three runs per program.** Every typed benchmark program, every typed oracle program and every
  program the fuzzer generates (records, tags, lists, strings, dicts, closures) runs three ways: in
  the interpreter with annotations checked, compiled, and compiled with every annotation erased,
  which runs it as untyped ry. All three must print the same output and exit with the same status,
  and the oracle adds GNU R where the erased program is plain R. This is Bun's test matrix.
- **Every guess fails.** `RY_FORCE_GUESS_FAIL` makes every fast-path check at a boundary choose the
  second copy, so every second copy runs, and the three runs must still agree.
- **Every fact checked.** `RY_CHECK_FACTS` compiles a check before each use of a fact: a count of
  one before each in-place update and each reuse, a live count before each read, and the bound
  before each list read whose bounds check was removed. In this mode every pool also keeps a
  generation column, bumped when a slot is freed, and every reference carries the generation it
  was made with, so a read through a reference to a freed and reused slot fails the check instead
  of reading another value. A failed check panics with the function and the fact, so a soundness
  bug in the IR shows up as a precise failure instead of corrupt memory.
- **No leaks.** Reference counting is deterministic and R holds no references into the native
  heap, so a program that returns no native value frees every native value it allocated. Each run
  in the matrix ends by asserting that the native pools hold no live slot apart from saturated
  ones, which catches a missing `drop` on the run that introduced it.

## What is built in one change

The typed core IR and its lowering in the analysis, its serialized form beside the bytecode, the
native representations (pools with their columns and free stacks, column lists, bitsets, stack
lists, dense and hashed dicts), count insertion with borrow inference, field moves, drop
specialization and reuse, the Cranelift backend over the IR, the entry and exit stubs with the
second copies, value tracking per namespace, lazy compilation with background workers, mapped code
files with their stamps and per-process table, compilation at `ry install` into the installed
package, `ry build --exe`, the invalidation of direct calls on unlocking, `ry check --explain`,
the test modes and the Rust twins. The native path in
`crates/jit/src/native.rs` keeps serving typed code over R types that is not strict-clean.

The build starts with native `List`, `Str` and `Dict` and ahead-of-time compilation of the typed
benchmark programs, because those give the largest measured wins: the typed programs that gain
nothing from the native path today (`bond_book`, `report`, `closures`) are the ones that need
lists, strings and captures.

## What exists

`crates/typed` builds the first slice. It compiles a strict typed closure at its first call for
the argument types it sees, which covers scalars, vectors and Arrays of numbers, logicals and
strings, lists of scalars, local functions (inlined), `lapply` and `vapply` over a local function,
and the string builtins of the benchmark programs. It differs from this draft in four ways:

- The IR is lowered at run time from the closure's R syntax, not from a serialized summary the
  analysis computes, and its global calls are guarded by the bindings they resolved to.
- A `Str` is two words with up to 15 bytes inline, not three, so it returns in two registers.
- A `List` header points at its items, so an R vector crosses into compiled code as a view with no
  copy, and writing to a view copies it first.
- Code lives in memory for the process. Code files, `ry install` compilation, `ry build --exe`,
  value tracking, records, tags, pools and the background workers are not built.

A call whose arguments are unforced promises, a default argument or a construct the lowering does
not know runs on the native path in `crates/jit/src/native.rs` instead.

## Open questions

- **Dropping the profile-driven native path.** Once the typed core compiles ahead of time, the
  native path from R syntax serves only typed code over R types. Whether that is worth keeping
  depends on how much of the benchmark code bases is rewritten in ry's types.
- **Wider references.** A 32-bit reference limits a variant to 2^(32 - tag bits) live values. If
  a benchmark code base approaches that, references widen to 64 bits for that type, with the tag
  still in the upper bits.
- **Zero-copy lists to R.** An ALTREP wrapper over a native `List<F64>`, which hands out a
  read-only pointer and copies only when C asks to write, is the candidate if conversion cost shows
  up at the boundary.
- **Packages in the executable.** `ry build --exe` loads packages from the library. Embedding the
  R code of the program's packages, and their shared objects, would make the executable portable
  between machines.
- **First-call compile time.** A function compiled at its first call costs about a millisecond on
  that call unless a worker compiled it first. Whether a 300,000-line code base under development
  needs the copy-and-patch baseline for its first calls is to be measured.
- **Untyped bytecode** is install-time build output and part of the base image, not a cache
  (`r-runtime.md`, "Where bytecode comes from").
