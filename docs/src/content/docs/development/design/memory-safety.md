---
title: Memory safety in the runtime
description: "Handles and a context parameter that make the interpreter safe Rust: a value held across a collection without a root does not compile, and unsafe stays in the heap and at the native boundary"
---

The interpreter keeps R values as raw pointers and keeps them valid by convention. This page
proposes types that carry the conventions, so the compiler checks them:

- A function that can run a collection takes `&mut Cx`. A value read out of the heap borrows
  `&Cx`, so keeping it across such a call without rooting it is a compile error.
- A rooted value is a plain pointer with a lifetime, and rooting costs the one push it costs today.
- A pointer's type names its representation, so a write into an object with no element storage (an
  ALTREP wrapper) has no way to be written.
- An object is written in place only through a guard that holds the context, so no R code and no
  other reader runs while it is being written.
- `unsafe` stays in `heap`, in `libr`'s C entry points, in the helpers that generated code calls,
  and on a short allowlist in the interpreter.

The design aims at no added instruction on a hot path. [Cost](#cost) lists the constructs where
that has to be measured before it is believed.

## What goes wrong by hand

A builtin looks like this (shortened from `do_nchar`):

```rust
unsafe fn do_nchar(call: &Call<'_>) -> R<Sexp> {
    let text = character_vector(call.arg(0))?;
    let result = alloc(SexpType::Integer, length(text))?;
    let counts = values::ints_mut(result);
    for (index, slot) in counts.iter_mut().enumerate() {
        *slot = text.element(index).bytes().len() as i32;
    }
    copy_shape(call.arg(0), result)?;
    Ok(result)
}
```

Every line relies on a rule nothing checks. `text` must be rooted, because `alloc` can collect.
`ints_mut` returns a slice with no lifetime, so `counts` can outlive `result`. `result` must be an
integer vector with element storage. `text.element` runs a native method when `text` is an ALTREP
string vector, and that method can allocate while `counts` is borrowed. The whole function is
`unsafe fn`, so the compiler marks none of these.

The runtime has six such rules. Each has produced a crash or a wrong answer that a test now pins:

| Rule kept by hand | What breaks | Pinned by |
| --- | --- | --- |
| A value held across an allocation or a call is rooted | a freed object is read | `datetime__year_overflow`, `apply__lapply_closure_results_stay_rooted` |
| A store into an old object runs the write barrier | a young object is freed while an old one points at it | `datatable__setcolorder_young_columns`, `datatable__setorder_young_strings` |
| An element write goes to an object of the right kind that has element storage | the write lands on the next object | `assignment__long_argument_copy_element_stores` |
| An object is modified in place only when nothing else can see it | another variable changes | `assignment__dollar_in_place` |
| A frame's slots and its environment agree about each variable | a stale or missing binding | `environments__unread_loop_variable` |
| A new object is filled before anything can read or trace it | uninitialized elements are traced | the collector's seeded tests (`heap` `test_gc`) |

The scale is the reason review does not hold these rules. Counted over each crate's `src`:

| Crate | Lines | `unsafe fn` | `unsafe` blocks | `SAFETY` comments |
| --- | --- | --- | --- | --- |
| `interp` | 137,354 | 3,291 | 593 | 216 |
| `libr` | 26,946 | 1,282 | 118 | 36 |
| `heap` | 13,336 | 356 | 227 | 64 |
| `typed` | 24,194 | 391 | 4 | 7 |
| `graphics` | 18,702 | 397 | 26 | 16 |
| `stats` | 22,177 | 249 | 8 | 8 |
| `jit` | 18,748 | 222 | 106 | 7 |
| `frames` | 4,304 | 144 | 3 | 2 |

Two more facts make the current code unsound even where it is careful. `interp::heap()` and
`interp::interp()` are safe functions that return `&'static mut`, so any two calls alias, and the
interpreter calls them about 1,600 times. And each runtime crate's root sets
`#![allow(unsafe_op_in_unsafe_fn)]`, so inside those 3,291 functions no raw read carries an
`unsafe` block. The migration deletes that line crate by crate.

## The design

### The context is the permission to collect

A **safepoint** is a point where the heap may free an object or foreign code may run: an
allocation (collections and the reference-count reconcile both run there), a call into R code, and
a call into native code, which includes every ALTREP method. Between safepoints nothing is freed,
which is the same promise R's C API makes to native code.

`Cx<'s>` is the context: the heap, the interpreter's state, and the current handle scope `'s`.
It replaces the global accessors. The two ways to borrow it are the two capabilities:

| Borrow | Allows | So it is taken by |
| --- | --- | --- |
| `&mut Cx<'s>` | a safepoint | allocation, evaluation, calling a closure or a native routine, any ALTREP method, a variable lookup that can force a promise or fill an inline cache, defining a variable |
| `&Cx<'s>` | one load from the heap, rooting, a store with its barrier | field and element reads of standard objects, slices, comparisons, element stores into an object being built |

A read takes `&Cx` only when it is a single load. Anything that can dispatch (an element of an
ALTREP vector, the length of one, a variable whose binding is active or a promise) takes
`&mut Cx`.

`Cx` is a capability, not a reference to the heap's memory. It holds raw pointers to the heap and
to the interpreter's state, and each method dereferences them for one operation. A C entry point
reached from native code (`Rf_eval` inside `.Call`) therefore makes its own `Cx` without creating a
second `&mut` to the same memory, and the caller, which lent its `&mut Cx` to the native call,
holds no unrooted value or slice across it.

### Four pointers

All four are one machine word, the same word a `SEXP` is, because the heap does not move objects.

| Type | Meaning | Made by | Lives until |
| --- | --- | --- | --- |
| `Gc<'a, T>` | read from the heap, not rooted | a load through `&'a Cx` | the next safepoint: it borrows the context |
| `Local<'s, T>` | rooted in scope `'s` | `cx.root(gc)`, every constructor, every argument, every register operand | the scope ends |
| `Fresh<'s, T>` | a new object nothing else can see | `cx.alloc_*`, `cx.duplicate` | `finish()` turns it into a `Local` |
| `Write<'a, 's, T>` | exclusive access to an existing object | the interpreter's ownership checks | it borrows the context mutably |

`Gc` and `Local` are `Copy`. `Fresh` and `Write` are not: each is the only way to reach its object
while it lives, which is what makes writing through it safe.

Holding an unrooted value across a safepoint does not compile:

```rust
let first = list.get(cx, 0);        // Gc<'_, Value>, borrows cx
let copy = cx.alloc_real(n)?;       // error[E0502]: cannot borrow `*cx` as mutable
use_both(first, copy);              //   because it is also borrowed as immutable
```

The fix is the root the hand-written rule asks for, and it is the same push:

```rust
let first = cx.root(list.get(cx, 0));   // Local<'s, Value>
let copy = cx.alloc_real(n)?;
```

`cx.root` takes `&Cx`, not `&mut Cx`: pushing onto the handle stack is not a safepoint, and it must
be callable while the `Gc` it roots still borrows the context.

### A kind names a representation

`T` is the object's kind: `Real`, `Int`, `Lgl`, `Cplx`, `Raw`, `Str`, `Chars`, `List`, `Sym`,
`Env`, `Closure`, `Promise`, `Lang`, `Pairlist`, `Alt` and `Value` for any of them. A vector kind
means a standard vector, with its length in the header and its elements behind it. An ALTREP
object is an `Alt`, whatever R type it reports, so a `Local<Real>` always has elements and
`len()`, `get(cx, i)` and `as_slice(cx)` on it are plain loads.

A `Local<Value>` becomes a typed pointer through one checked downcast, the test a builtin already
makes when it dispatches on `kind()`. Code that accepts "a double vector" asks for the view it can
work with:

```rust
match cx.reals(value)? {                              // one test of the kind and the ALTREP bit
    Reals::Standard(vector) => sum(vector.as_slice(cx)),
    Reals::Sequence { first, step, len } => closed_form(first, step, len),
    Reals::Lazy(alt) => each(cx, alt),                // alt.get(cx, i) takes &mut Cx
}
let dense = cx.dense_reals(value)?;                   // &mut Cx: Local<Real>, materialized once
let slice: &[f64] = dense.as_slice(cx);               // borrows cx, so no safepoint while it lives
```

`dense_*` always returns a standard vector, as `values::materialize` does: the vector itself, the
one a wrapper wraps, the one a compact sequence or deferred string vector expands into and keeps,
or a copy for a class a package registers. No slice ever points at memory a class owns. A loop
that only reads a few elements uses the `Lazy` arm, so a class's `Elt` method runs for the
elements asked for and a long sequence is not expanded.

This replaces a test of the ALTREP bit on every `Sexp::element` call with one test per vector,
and it is the rule that removes the write into an object without storage: element writes exist
only on standard kinds.

### Writes

| Write | API | Why it is sound |
| --- | --- | --- |
| Fill a new atomic vector | `fresh.as_mut_slice()` | `Fresh` is the only pointer and takes `&mut self`. No context is needed, because the object holds no pointers |
| Fill a new list, string vector or pairlist | `fresh.set(cx, i, value)` with `&Cx` | the store runs the barrier and the count inside `heap`. A safepoint between two stores can promote the object, and the barrier covers that. There is no `&mut [Gc]` to fill without it |
| Change an existing object in place | `let (elements, reader) = write.split()` | `Write` holds `&mut Cx`, so no slice taken earlier is alive, no R code runs, and nothing can share the object meanwhile |
| Store into an object with reference semantics | `cx.define(env, sym, value)` (`&mut Cx`), promise and external-pointer fields (`&Cx`) | the barrier and the count run inside `heap` |

A store is not a safepoint: a count that drops to zero is only noted, and the object is freed at
the next allocation. That is why element stores need only `&Cx`, and why copying elements from one
list into a new one roots nothing:

```rust
let mut out = cx.alloc_list(n)?;
for index in 0..n {
    out.set(cx, index, input.get(cx, index));
}
```

Lists and string vectors hand out no slice of their elements, only single loads, so a store
through `&Cx` never invalidates a borrowed slice.

**In-place modification.** Whether an object may be changed in place is a fact about the
interpreter: its count is one, and no register, handle or escaped list holds it uncounted
(`slot_owns`, a call's `reusable` test). The functions that establish that fact live in `interp`
and are the only callers of `Cx::assume_exclusive`, an `unsafe` constructor that returns the
`Write`. Because a `Write` borrows the context mutably, the fact cannot go stale: no R code runs
between the check and the write. When the check fails, the caller duplicates (`cx.duplicate`
returns a `Fresh`, never an ALTREP wrapper), stores the copy in the variable and checks again.

`write.split()` gives the element storage and a `reader` for the sources. For an atomic vector
the reader compares storage addresses and returns `None` for a source that shares the target's
storage, which also covers a wrapper around the target. The caller then reads from the mutable
slice itself, so `x[i] <- x[j]` is `elements.copy_within(..)`. For a list the reader returns
single elements, and the same object may be read and written.

```rust
let mut write = own_slot(cx, state, slot)?;       // interp: the ownership check
let (elements, reader) = write.split();
match reader.reals(value) {
    Some(source) => elements[position] = source[0],
    None => elements.copy_within(0..1, position),
}
```

`Write` also carries the operations that change an object's shape: setting attributes, and
shortening or growing within capacity. `Lgl` and `Int` share a storage trait, so a copy that is
generic over the element type serves both.

`do_nchar` in this shape:

```rust
fn do_nchar<'s>(cx: &mut Cx<'s>, call: &Call<'s>) -> R<Local<'s, Value>> {
    let text = cx.dense_strings(character_vector(cx, call.arg(0))?)?;
    let mut result = cx.alloc_int(text.len())?;
    for (slot, chars) in result.as_mut_slice().iter_mut().zip(text.iter(cx)) {
        *slot = chars.bytes().len() as i32;
    }
    copy_shape(cx, call.arg(0), result.finish())
}
```

### Scopes, registers and unwinds

A constructor returns its object already rooted in the current scope, as `values::alloc` does, so
returning a value from a `&mut Cx` function does not extend the borrow. A callee that wants to
release its temporaries runs in a nested scope:

```rust
let total = cx.scope(|cx| {            // cx: &mut Cx<'inner>
    let parts = split(cx, input)?;     // Local<'inner, _>
    sum(cx, parts)                     // the result is rooted again in the outer scope
})?;
```

The closure is generic over `'inner`, so a `Local<'inner, _>` cannot leave it except as the result.
This is `interp::scope` with the lifetime that proves what its comment asks for. An unwind
(`Error`, `Return`, `Break`) carries values under the same brand and leaves a scope the way a
result does, rooted again in the enclosing one.

The bytecode loop does not pay that second root. Its registers are traced, so an instruction that
can root runs in `cx.scope_to(register, ..)`, which writes the result into the destination
register and releases the handles, as the loop's `release` does by hand.

Operands need no handle either. Only the running activation's own instructions write its
expression registers, so a value in one stays put for the length of the instruction that reads
it. The loop's operand accessor returns it as a `Local` under the instruction's brand, and a
builtin's arguments arrive the same way. That accessor is one `unsafe` adoption resting on the
bytecode's own invariant. A variable slot is different: a nested closure's `<<-` can overwrite
it during a call, so a slot read is a `Gc` and reaches an operand through an expression register.

Values kept in the interpreter's own state (frames, the handler stack, caches) sit in `Held<T>`
fields, readable as `Gc` through `&Cx`. Each such structure has one `unsafe impl Trace` that marks
its fields.

### Where `unsafe` remains

| Place | Why | Bound |
| --- | --- | --- |
| `heap` | the object layout, the allocator, the collector, and the handle types themselves | the crate |
| `libr` C entry points | a `SEXP` from C carries no proof; each entry makes a `Cx` and adopts its arguments | one block per entry point |
| Helpers called from JIT and typed code | generated code passes raw words | one block per helper |
| Ownership checks | they assert the fact `Write` rests on | the few functions that call `assume_exclusive` |
| The bytecode loop's registers | operand adoption, the raw register pointers an activation keeps, and unchecked indexing only if bounds checks measure above noise | one accessor module |
| Calling a native routine, operating-system calls | foreign code | an allowlist of files in `interp` |
| `unsafe impl Trace` | the collector trusts it | one per structure that holds values |

Everything else in `interp`, `stats`, `graphics` and `frames` is safe Rust, and
`#![deny(unsafe_op_in_unsafe_fn)]` makes each remaining operation carry a block and a `SAFETY`
comment.

### Cost

| Construct | Machine cost | Today | Status |
| --- | --- | --- | --- |
| `Gc`, `Local`, `Fresh` | one word, passed in a register | `Sexp`, the same word | equal by construction |
| `cx.root` | one push | `heap.root`, the same push | equal by construction |
| Register operands | none: adopted as `Local` | none: registers are traced | equal by construction |
| Instruction scope | a compare and a truncate | the same, in `release` | equal by construction |
| Downcast | one compare on the kind byte | the `match kind()` the builtin already has | equal by construction |
| Views | one test of the ALTREP bit per vector | one test per element | less work |
| Element stores | barrier and count | the same | equal by construction |
| `Write` | one address compare per source vector | hand-written alias checks | to measure |
| `&mut Cx` parameter | one more live register through every call | a load from a static in `heap()`; the bytecode loop keeps the heap pointer in its activation | **to measure**: register pressure has cost this interpreter before (`lookup_outside` is forced inline for that reason) |
| Slice access | bounds checks where an index is not an iterator | mostly slices already | to measure |

Each migration change reports `just bench hot`, the kernel and code-base suites, and tier 1's
instruction counts (`test_tiers`) before and after. A regression is fixed by changing the API (for
the context parameter: keeping it in the activation, or making `Cx` a zero-sized token whose
methods load the static as `heap()` does), not by keeping the raw version.

## Candidates

| Design | How it keeps a value alive | Verdict for ry |
| --- | --- | --- |
| Handle scopes with indirect handles (V8) | every value is a pointer to a slot the collector updates | **Reject.** The indirection exists because V8 moves objects. ry cannot move an object native code holds, so it would pay a slot and a load per value for nothing. Scopes are also unchecked: a handle that outlives its scope is a use after free. V8's direct handles drop the slot and pair with conservative stack scanning, the row below (not checked against current V8) |
| Branded arena lifetimes with collection only between mutations (`gc-arena`) | nothing needs rooting, because the collector runs only when no mutator frame is on the stack | **Reject as the mechanism, adopt the brand.** R evaluation is recursive Rust that re-enters from C (`Rf_eval` inside `.Call`), so there is no point with an empty stack to collect at. The invariant-lifetime brand is what `Cx<'s>` and `Local<'s, T>` use |
| `Rooted` and `Handle`, with a static analysis for hazards (SpiderMonkey) | explicit roots on the stack; a separate tool finds values held across a call that can collect | **Adopt the model, replace the tool.** SpiderMonkey's Rust bindings mark "can collect" with `&mut JSContext`; borrowing does the hazard analysis in the compiler. Josephine (Servo) and shifgrethor have the same shape, a context borrow plus branded roots, and their known difficulty is a second context made on re-entry from foreign code, which is why `Cx` is a capability and not a reference |
| `CAMLparam` and `CAMLlocal` (OCaml) | every local is registered by macro | **Reject.** It is the `PROTECT` discipline, unchecked. `ocaml-interop` shows the borrow-checked form of the same idea |
| Reference counting with in-place mutation when unique (Roc) | the count | **Reject for liveness, adopt uniqueness.** Environments and closures form cycles, and native code, registers and handles hold pointers they never count, so a count cannot free at the decrement: ry frees from counts only at an allocation, after checking the roots. `Write` is the type-level form of Roc's "mutate when unique" |
| Conservative stack scanning (JavaScriptCore, Ruby) | the collector treats every word on the native stack as a possible pointer | **Reject.** It needs no roots and would remove the handle pushes, and the heap can afford it because nothing moves. But a dead value in a stale stack slot stays alive for as long as the compiler happens to leave it there, which breaks finalizer timing that R6 and lgr rely on and that the `evaluation__*finalizer*` cases pin, and it makes that timing depend on code generation. It also covers one rule of six, does not see values in Rust heap containers, and rests on the optimizer keeping a recognizable pointer |
| Borrow-checked safepoints with scope-branded direct handles | a root-stack slot, proved by the type | **Adopt.** Described above |

The expectation going in was branded handles at no runtime cost, and it holds with one correction.
The brand alone (a lifetime on a rooted pointer) proves a rooted value does not outlive its scope.
It does not catch the unrooted value, the first rule in the table at the top. That needs the second
half: unrooted values borrow the context, and safepoints take it mutably. And lifetimes do nothing
for the third rule, a write into an object without storage, which needs the kind to name the
representation.

Arena indices stay where the layout already uses them (frames in the frame arena, registers in the
window, slots in a record). They are not a replacement for object pointers: an index needs a table
between it and the object, and native code needs the object's address.

## What the types do not check

| Rule | Why not | Caught by |
| --- | --- | --- |
| Each `unsafe impl Trace` marks every field | a missed field is an unrooted value in the interpreter's state | seeded stress with verification over the oracle cases, each run twice in one process |
| An ownership check is right before it calls `assume_exclusive` | the fact depends on counts, registers and frames | the replacement contract in `interp` `test_fuzz`, `assignment__dollar_in_place` and its siblings |
| An expression register is written only by its own activation | a property of the bytecode the compiler emits | the tier differential, seeded stress |
| Native code keeps the C API's rules | foreign code can do anything | `libr` `test_fuzz_native` for ry's side of the boundary; nothing checks a package's C code |
| A `libr` entry point adopts what C gave it correctly | C can pass anything | `libr` `test_fuzz_native`, the `PROTECT` balance check |
| Generated code keeps its values in the register window across helpers | machine code has no types | `ry-jit-gctorture`, the tier differential |
| A frame's slots agree with its environment | a relation between two structures | the replacement and shadowing contracts in `interp` `test_fuzz`, the differential fuzzer's repeated calls |
| A count is not undercounted | counts gate copies and deferred frees, and the stores that keep them sit inside `heap` | `RY_GC_VERIFY` on every reconcile |

[Robustness](/development/robustness) describes those harnesses.

## Migration

The handle types and `Cx` land in `heap` with compile-fail tests for each rule above. The builtin
table accepts both signatures while files move. A migrated function that calls an unmigrated one
does it in an `unsafe` block, so the boundary between the two halves is visible and counted.

1. `heap`: the handle module, `Cx`, typed constructors, views, `Write`, scopes.
2. `interp` leaf builtins (strings, numbers, sorting, arithmetic kernels), then `values.rs`, then
   subsetting and assignment, then the evaluator and the bytecode loop.
3. `stats`, `graphics`, `frames`.
4. `libr`, `jit`, `typed`: entry points make a `Cx`; raw access leaves their bodies.
5. `heap` stops exporting the raw accessors outside a `raw` module that only the boundary imports,
   and `interp::heap()` and `interp::interp()` are deleted.

A budget file records the count of `unsafe` per source file. `just gate` fails when a file's count
rises, and a change that lowers one lowers the budget with it, so the count only falls.

## Open questions

- Whether the bytecode loop's register access measures the same with bounds checks.
- Which form of the context parameter measures best on the call path: a pointer in a register, a
  field of the activation, or a zero-sized token.
- How `Held<T>` fields in structures that native code also reaches (the precious list, external
  pointer protection) share one definition with `libr`.
- The spelling for linking a pairlist cell after it is visible to other code, which the evaluator
  does when it builds argument lists incrementally.
