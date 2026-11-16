---
title: Memory safety in the runtime
description: "Handles and a context parameter that make the interpreter safe Rust: a value held across a collection without a root does not compile, and unsafe stays in the heap and at the native boundary"
---

The interpreter keeps R values as raw pointers and keeps them valid by convention. This page
proposes types that carry the conventions, so the compiler checks them:

- A function that can run a collection takes `&mut Cx`. A value read out of the heap borrows
  `&Cx`, so keeping it across such a call without rooting it is a compile error.
- A rooted value is a plain pointer with a lifetime. Rooting costs the one push it costs today, and
  nothing else in the design costs an instruction the hand-written code does not already spend.
- A pointer knows its kind and whether it has element storage, so a write into an object with no
  storage (an ALTREP wrapper) has no way to be written.
- `unsafe` stays in `heap`, in `libr`'s C entry points, in the helpers that generated code calls,
  and on a short allowlist in the interpreter (operating-system calls, calls into native routines).

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
interpreter calls them about 1,600 times. And an `unsafe fn` body needs no `unsafe` block, so
inside those 3,291 functions every raw read is unmarked.

## The design

### A context parameter is the permission to collect

A **safepoint** is a point where the heap may free an object: an allocation (collections and the
reference-count reconcile both run there), a call into R code, and a call into native code. Between
safepoints nothing is freed, which is the same promise R's C API makes to native code.

`Cx<'s>` is the context: the heap, the interpreter's state, and the current handle scope `'s`.
It replaces the global accessors. The two ways to borrow it are the two capabilities:

| Borrow | Allows | So it is taken by |
| --- | --- | --- |
| `&mut Cx<'s>` | a safepoint | allocation, evaluation, calling a closure or a native routine, materializing an ALTREP vector, any store into an object other code can see |
| `&Cx<'s>` | reading the heap, rooting a value | accessors, slices, comparisons |

### Three pointers

All three are one machine word, the same word a `SEXP` is, because the heap does not move objects.

| Type | Meaning | Made by | Lives until |
| --- | --- | --- | --- |
| `Gc<'a, T>` | read from the heap, not rooted | reading an element, a binding, a field, through `&'a Cx` | the next safepoint: it borrows the context |
| `Local<'s, T>` | rooted in scope `'s` | `cx.root(gc)`, every constructor, every argument | the scope ends |
| `Fresh<'s, T>` | a new object nothing else can see | `cx.alloc_*` | `finish()` turns it into a `Local` |

`Gc` and `Local` are `Copy`. `Fresh` is not: it is the only pointer to its object, which is what
makes writing through it safe.

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

### Kinds and storage

`T` is the object's kind: `Real`, `Int`, `Lgl`, `Cplx`, `Raw`, `Str`, `Chars`, `List`, `Sym`,
`Env`, `Closure`, `Promise`, `Lang`, `Pairlist`, and `Value` for any of them. A `Local<Value>`
becomes a `Local<Real>` through one checked downcast, the test a builtin already makes when it
dispatches on `kind()`. Accessors exist only where they are valid, so `car` of a vector is not an
operation.

A vector kind does not promise element storage, because an ALTREP object has the kind and no
elements. Element access goes through a **dense view**:

```rust
let dense = text.dense(cx)?;      // &mut Cx: materializes an ALTREP vector, may run native code
let chars = dense.get(cx, 0);     // &Cx: a plain load
let reals: &[f64] = x.dense(cx)?.as_slice(cx);   // borrows cx, so no safepoint while it lives
```

The view checks the ALTREP bit once, where today `Sexp::element` checks it on every element. No
operation writes elements through anything but a `Fresh` or a `modify` (below), and neither can be
an ALTREP object, so a write into an object without storage has no spelling.

### Writes

| Write | API | Why it is sound |
| --- | --- | --- |
| Fill a new vector | `fresh.as_mut_slice()`, `fresh.init(i, gc)` | `Fresh` is the only pointer; it takes `&mut self` and no context, so other objects stay readable through `&Cx` |
| Modify in place | `cx.modify(unique, \|elements, reader\| ...)` | `Unique<'s, T>` comes from `cx.make_unique(local)`, which copies a shared or ALTREP object; the closure holds `&mut Cx`, and `reader` refuses to hand out a slice of the object being written (one pointer compare per source, not per element) |
| Store one value into a visible object | `cx.set_element(list, i, value)`, `cx.define(env, sym, value)` | takes `&mut Cx` and rooted arguments; runs the barrier and the counts inside `heap` |

This puts the write barrier and the reference counts behind the only functions that can store a
pointer, and it makes in-place modification require the token that proves it is allowed.

`do_nchar` in this shape:

```rust
fn do_nchar<'s>(cx: &mut Cx<'s>, call: &Call<'s>) -> R<Local<'s, Value>> {
    let text = character_vector(cx, call.arg(0))?.dense(cx)?;
    let mut result = cx.alloc_int(text.len())?;
    for (slot, chars) in result.as_mut_slice().iter_mut().zip(text.iter(cx)) {
        *slot = chars.bytes().len() as i32;
    }
    copy_shape(cx, call.arg(0), result.finish())
}
```

### Scopes, results and unwinds

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
This is `interp::scope` with the lifetime that proves what its comment asks for. The bytecode loop
opens one scope per instruction, as it releases handles per instruction today.

An unwind (`Error`, `Return`, `Break`) carries values out of scopes that are being released. Its
values move into a traced slot of the context when it is raised, and `Unwind` itself holds no
pointer, so `?` needs no rooting.

Values kept in the interpreter's own state (frames, the handler stack, caches) sit in `Held<T>`
fields, readable as `Gc` through `&Cx`. Each such structure has one `unsafe impl Trace` that marks
its fields.

### Where `unsafe` remains

| Place | Why | Bound |
| --- | --- | --- |
| `heap` | the object layout, the allocator, the collector, and the handle types themselves | the crate |
| `libr` C entry points | a `SEXP` from C carries no proof; each entry makes a `Cx` and adopts its arguments | one block per entry point |
| Helpers called from JIT and typed code | generated code passes raw words | one block per helper |
| Calling a native routine, operating-system calls | foreign code | an allowlist of files in `interp` |
| The bytecode loop's register access | only if bounds checks measure above noise; the bytecode is checked when it is lowered | one accessor |

Everything else in `interp`, `stats`, `graphics` and `frames` is safe Rust, and
`#![deny(unsafe_op_in_unsafe_fn)]` makes each remaining operation carry a block and a `SAFETY`
comment.

### Cost

| Construct | Machine cost | Today |
| --- | --- | --- |
| `Gc`, `Local`, `Fresh` | one word, passed in a register | `Sexp`, the same word |
| `cx.root` | one push | `heap.root`, the same push |
| `&mut Cx` parameter | one register | a load from a static on each `heap()` call |
| Downcast | one compare on the kind byte | the `match kind()` the builtin already has |
| Dense view | one test of the ALTREP bit per vector | one test per element |
| `modify` | one pointer compare per source vector | hand-written alias checks |
| Scope | a truncate and one push | the same, in `interp::scope` |
| Slice access | bounds checks where an index is not an iterator | mostly slices already |

No row adds work, and two remove some. That is a claim to measure, not to trust: each migration
change reports `just bench hot`, the kernel and code-base suites, and tier 1's instruction counts
(`test_tiers`) before and after, and a regression is fixed by changing the API, not by keeping the
raw version.

## Candidates

| Design | How it keeps a value alive | Verdict for ry |
| --- | --- | --- |
| Handle scopes with indirect handles (V8) | every value is a pointer to a slot the collector updates | **Reject.** The indirection exists because V8 moves objects. ry cannot move an object native code holds, so it would pay a slot and a load per value for nothing. Scopes are also unchecked: a handle that outlives its scope is a use after free |
| Branded arena lifetimes with collection only between mutations (`gc-arena`) | nothing needs rooting, because the collector runs only when no mutator frame is on the stack | **Reject as the mechanism, adopt the brand.** R evaluation is recursive Rust that re-enters from C (`Rf_eval` inside `.Call`), so there is no point with an empty stack to collect at. The invariant-lifetime brand is what `Cx<'s>` and `Local<'s, T>` use |
| `Rooted` and `Handle`, with a static analysis for hazards (SpiderMonkey) | explicit roots on the stack; a separate tool finds values held across a call that can collect | **Adopt the model, replace the tool.** SpiderMonkey's Rust bindings mark "can collect" with `&mut JSContext`; borrowing does the hazard analysis in the compiler |
| `CAMLparam` and `CAMLlocal` (OCaml) | every local is registered by macro | **Reject.** It is the `PROTECT` discipline, unchecked. `ocaml-interop` shows the borrow-checked form of the same idea |
| Reference counting with in-place mutation when unique (Roc) | the count | **Reject for liveness, adopt uniqueness.** Environments and closures form cycles, and native code holds pointers it never counts, so counts cannot free on zero. `Unique` is the type-level form of "the count is one" |
| Conservative stack scanning (JavaScriptCore, Ruby) | the collector treats every word on the native stack as a possible pointer | **Reject.** It needs no roots and would remove the handle pushes, and the heap can afford it because nothing moves. But a dead value in a stale stack slot stays alive for as long as the compiler happens to leave it there, which breaks finalizer timing that R6 and lgr rely on and that the `evaluation__*finalizer*` cases pin, and it makes that timing depend on code generation. It also covers one rule of six, does not see values in Rust heap containers, and rests on the optimizer keeping a recognizable pointer |
| Borrow-checked safepoints with scope-branded direct handles | a root-stack slot, proved by the type | **Adopt.** Described above |

The expectation going in was branded handles at no runtime cost, and it holds with one correction.
The brand alone (a lifetime on a rooted pointer) proves a rooted value does not outlive its scope.
It does not catch the unrooted value, which is the bug that has happened. That needs the second
half: unrooted values borrow the context, and safepoints take it mutably.

Arena indices stay where the layout already uses them (frames in the frame arena, registers in the
window, slots in a record). They are not a replacement for object pointers: an index needs a table
between it and the object, and native code needs the object's address.

## What the types do not check

| Rule | Why not | Caught by |
| --- | --- | --- |
| Each `unsafe impl Trace` marks every field | a missed field is an unrooted value in the interpreter's state | seeded stress with verification over the oracle cases, each run twice in one process |
| A `libr` entry point adopts what C gave it correctly | C can pass anything | `libr` `test_fuzz_native`, the `PROTECT` balance check |
| Generated code keeps its values in the register window across helpers | machine code has no types | `ry-jit-gctorture`, the tier differential |
| A frame's slots agree with its environment | a relation between two structures | the replacement and shadowing contracts in `interp` `test_fuzz`, the differential fuzzer's repeated calls |
| A count is not undercounted | counts only gate copies and deferred frees; the type is `Unique`, the proof is the count | `RY_GC_VERIFY` on every reconcile |

[Robustness](/development/robustness) describes those harnesses.

## Migration

The handle types and `Cx` land in `heap` with compile-fail tests for each rule above. The builtin
table accepts both signatures while files move. A migrated function that calls an unmigrated one
does it in an `unsafe` block, so the boundary between the two halves is visible and counted.

1. `heap`: the handle module, `Cx`, typed constructors, dense views, `modify`, scopes.
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
- Whether `modify`'s reader is enough for the replacement paths that read and write the same
  object on purpose (`x[i] <- x[j]`), or whether they want a copy-within operation.
- How `Held<T>` fields in structures that native code also reaches (the precious list, external
  pointer protection) share one definition with `libr`.
