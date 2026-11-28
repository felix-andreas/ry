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

The runtime has seven such rules. Each has produced a crash or a wrong answer that a test now pins:

| Rule kept by hand | What breaks | Pinned by |
| --- | --- | --- |
| A value held across an allocation or a call is rooted | a freed object is read | `datetime__year_overflow`, `apply__lapply_closure_results_stay_rooted` |
| A store into an old object runs the write barrier | a young object is freed while an old one points at it | `datatable__setcolorder_young_columns`, `datatable__setorder_young_strings` |
| An element read or write goes to an object of the right kind that has element storage | the write lands on the next object, or the read takes another type's bytes for pointers | `assignment__long_argument_copy_element_stores`, `strings__encoding_rejects_non_character` |
| An object is modified in place only when nothing else can see it | another variable changes | `assignment__dollar_in_place` |
| A frame's slots and its environment agree about each variable | a stale or missing binding | `environments__unread_loop_variable` |
| A new object is filled before anything can read or trace it | uninitialized elements are traced | the collector's seeded tests (`heap` `test_gc`) |
| Two strings are compared by their text, not their address | a name that is there is not found | `subset__non_ascii_names` |

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

### Five pointers

All five are one machine word, the same word a `SEXP` is, because the heap does not move objects.

| Type | Meaning | Made by | Lives until |
| --- | --- | --- | --- |
| `Gc<'a, T>` | read from the heap, not rooted | a load through `&'a Cx` | the next safepoint: it borrows the context |
| `Local<'s, T>` | rooted in scope `'s` | `cx.root(gc)`, every constructor, every argument, every register operand | the scope ends |
| `Fresh<'s, T>` | a new object nothing else can see | `cx.alloc::<T>(n)`, `handles::duplicate` | `finish()` turns it into a `Local` |
| `Owned<'s, T>` | an object nothing else refers to | `fresh.owned()`, or the interpreter's ownership checks | `finish()` turns it into a `Local` |
| `Write<'a, 's, T>` | exclusive access to an owned vector's elements | `owned.write(cx)` | it borrows the context and the `Owned` mutably |

`Gc` and `Local` are `Copy`. `Fresh`, `Owned` and `Write` are not: each is the only way to reach
its object while it lives, which is what makes writing through it safe.

Holding an unrooted value across a safepoint does not compile:

```rust
let first = list.get(cx, 0);        // Gc<'_, Value>, borrows cx
let copy = cx.alloc::<Real>(n)?;    // error[E0502]: cannot borrow `*cx` as mutable
use_both(first, copy);              //   because it is also borrowed as immutable
```

The fix is the root the hand-written rule asks for, and it is the same push:

```rust
let first = cx.root(list.get(cx, 0));   // Local<'s, Value>
let copy = cx.alloc::<Real>(n)?;
```

`cx.root` takes `&Cx`, not `&mut Cx`: pushing onto the handle stack is not a safepoint, and it must
be callable while the `Gc` it roots still borrows the context.

### A kind names a representation

`T` is the object's kind: `Real`, `Integer`, `Logical`, `Complex`, `Raw`, `Str`, `Chars`, `List`,
`Expression`, `Symbol`, `Environment`, `Closure`, `Promise`, `Language`, `Pairlist`, `Alt` and
`Value` for any of them. A vector kind
means a standard vector, with its length in the header and its elements behind it. An ALTREP
object is an `Alt`, whatever R type it reports, so a `Local<Real>` always has elements and
`len()`, `get(cx, i)` and `as_slice(cx)` on it are plain loads.

A `Local<Value>` becomes a typed pointer through one checked test, the one a builtin already
makes when it dispatches on `kind()`. For a kind whose objects are never ALTREP (`Exact`: a symbol,
an environment, a `CHARSXP`) the test is `downcast`. A vector kind has no `downcast`, because an
ALTREP vector of its type is not a standard vector, and a `None` that silently meant "absent" read
`names(x) <- 1:2` as having no names. The caller chooses instead: `standard::<Real>()` leaves ALTREP
vectors out, for a fast path that falls back to one that expands them, and code that accepts "a
double vector" asks for the view it can work with:

```rust
match reals(cx, value)? {                             // one test of the kind and the ALTREP bit
    Reals::Standard(vector) => sum(vector.as_slice(cx)),
    Reals::Sequence { first, step, len } => closed_form(first, step, len),
    Reals::Lazy(alt) => each(cx, alt),                // alt.get(cx, i) takes &mut Cx
}
let dense = dense::<Real>(cx, value)?;                // &mut Cx: Local<Real>, materialized once
let slice: &[f64] = dense.as_slice(cx);               // borrows cx, so no safepoint while it lives
```

The views live in `interp::handles`, because expanding an ALTREP object runs the hook `libr`
installs, and `heap` knows only that an `Alt` is not a vector. `dense` always returns a standard
vector, as `values::materialize` does: the vector itself, the
one a wrapper wraps, the one a compact sequence or deferred string vector expands into and keeps,
or a copy for a class a package registers. No slice ever points at memory a class owns. A loop
that only reads a few elements uses the `Lazy` arm, so a class's `Elt` method runs for the
elements asked for and a long sequence is not expanded.

This replaces a test of the ALTREP bit on every `Sexp::element` call with one test per vector,
and it is the rule that removes the write into an object without storage: element writes exist
only on standard kinds.

**A copy says which kind it returns.** R has two copies, and by hand they look alike:

| Copy | Raw function | Typed form | Returns |
| --- | --- | --- | --- |
| An owning copy, for a caller that writes elements | `values::duplicate`, `values::shallow_duplicate` | `handles::duplicate(cx, vector)`, `handles::shallow_duplicate(cx, vector)` | `Fresh<'s, T>`: a standard vector nothing else sees |
| A copy for a caller that changes only attributes | `values::shallow_duplicate_attr` | `handles::attribute_copy(cx, value)` | `Local<'s, Value>`: for a vector of 64 or more elements, a wrapper that shares the original's data |

The second has no storage of its own when it is a wrapper, so a `Sexp::slice_mut` on it is an
empty slice, and an indexed write through that slice lands on the next object in the heap. That
is the write `assignment__long_argument_copy_element_stores` pins: tier 0's matrix element store
copied a shared argument with the attribute copy and then stored into it.

With the typed forms that store has no spelling. A `Local<Value>` has no element access at all.
The two ways from it to a mutable slice both refuse a wrapper:

- `standard::<Real>()` returns `None` for an ALTREP object, so there is no `Local<Real>` to hand
  to an ownership check, and `Owned::write` exists only for vector kinds.
- `dense::<Real>` returns the standard vector the wrapper shares, as a `Local<Real>`, which reads
  but does not write. Writing needs an `Owned`, and the ownership check fails because the wrapper
  holds a reference to that vector, so the caller takes `handles::shallow_duplicate`, the owning copy.

The attribute setters accept any `Local<Value>`, so a caller that only changes attributes keeps
the cheap copy.

### Writes

| Write | API | Why it is sound |
| --- | --- | --- |
| Fill a new atomic vector | `fresh.as_mut_slice()` | `Fresh` is the only pointer and takes `&mut self`. No context is needed, because the object holds no pointers |
| Fill a new list, string vector or pairlist | `fresh.set(cx, i, value)` with `&Cx` | the store runs the barrier and the count inside `heap`. A safepoint between two stores can promote the object, and the barrier covers that. There is no `&mut [Gc]` to fill without it |
| Fill a new vector from one iterator | `cx.alloc_unfilled::<T>(n)?`, then `unfilled.fill(elements)` for numbers or `unfilled.fill_elements(cx, elements)` for a list or string vector | `Unfilled` is the only pointer and offers nothing but the fill, so no element is read before it is written. The elements borrow `&Cx`, so the iterator cannot reach a safepoint, the slots are written directly, and the references are counted once afterwards |
| Change an existing object in place | `owned.write(cx)`, then `write.split()`, or `write.split_elements()` for a list | `Owned` says nothing else refers to the object, and `Write` holds `&mut Cx`, so no slice taken earlier is alive, no R code runs, and nothing can share the object meanwhile |
| Store into an object with reference semantics | `cx.define(env, sym, value)` (`&mut Cx`), promise and external-pointer fields (`&Cx`) | the barrier and the count run inside `heap` |

A store is not a safepoint: a count that drops to zero is only noted, and the object is freed at
the next allocation. That is why element stores need only `&Cx`, and why copying elements from one
list into a new one roots nothing:

```rust
let mut out = cx.alloc::<List>(n)?;
for index in 0..n {
    out.set(cx, index, input.get(cx, index));
}
```

Lists and string vectors hand out no slice of their elements, only single loads, so a store
through `&Cx` never invalidates a borrowed slice.

An allocation that cannot collect is not a safepoint either. `cx.reserve(bytes)` takes `&mut Cx`
and runs the one collection check for a group of objects, and the allocators that follow it take
`&Cx` (`new_pending_frame_uncollected`, `define_locals_uncollected`), so the frame and slot values a
slot frame's environment is built from stay `Gc` reads across those allocations.

A function that returns a `Gc` from `&'a mut Cx` holds the context until the `Gc` dies, so a
caller that needs both roots the answer (`cx.root_answer`), and code that reads an object or
else makes it goes through `cx.read_or_make`, which tries the read under `&Cx` first. The
argument reference pool hands out positions for the same reason: `take_reference` returns an
index, the caller writes the target through it, and only then reads the reference.

**In-place modification.** Whether an object may be changed in place is a fact about the
interpreter: its count is one, and no register, handle or escaped list holds it uncounted
(`slot_owns`, a call's `reusable` test). That fact is a value, `Owned<'s, T>`. It has two sources.
A new vector is owned by construction (`fresh.owned()`). An existing one is owned when the code
that holds its variable has tested the counts, and that test ends in `Owned::assume`, the one
`unsafe` constructor. A store asks its caller for the claim and copies when there is none:

```rust
let owned = claim(vector.value());                       // Option<Owned>: the holder's test
let mut vector = handles::writable(cx, vector, owned)?;  // the vector itself, or a Fresh copy
vector.write(cx).split().0[position] = item;
Ok(vector.finish())                                      // the value the variable holds afterwards
```

`handles::writable` returns the owned vector, or `handles::shallow_duplicate(..).owned()` when the
claim is `None`. No path leads from a shared vector to a write, and none from the attribute copy
either, because that copy is a `Local<Value>` and never an `Owned`. A `Write` borrows the context
mutably, so the fact cannot go stale: no R code runs between the claim and the write.

Tier 0's element stores (`x[i] <- v`, `x[[i]] <- v`, `x$name <- v`, `m[i, j] <- v` in
`bytecode.rs`) are written this way. The raw entry the bytecode loop calls adopts its operands and
passes the ownership test as a closure. The store itself (`store_element`, `store_matrix_element`)
is safe code that is generic over the vector kind, so the same lines serve doubles, integers,
logicals, strings and lists.

`write.split()` gives an atomic vector's element storage and a `reader` for the sources. The
reader compares storage addresses and returns `None` for a source that shares the target's
storage, which also covers a wrapper around the target. The caller then reads from the mutable
slice itself, so `x[i] <- x[j]` is `elements.copy_within(..)`. For a list,
`write.split_elements()` gives barriered single stores and a reader of single elements, and the
same object may be read and written.

```rust
let mut write = owned.write(cx);
let (elements, reader) = write.split();
match reader.slice(value) {
    Some(source) => elements[position] = source[0],
    None => elements.copy_within(0..1, position),
}
```

`Write` also carries the operations that change an object's shape: setting attributes, and
shortening or growing within capacity. `Logical` and `Integer` have the same `Atomic::Element`,
so a copy that is generic over the element type serves both.

`nchar` is written in this shape (`crates/interp/src/strings.rs`). Its core:

```rust
fn do_nchar<'s>(context: &mut Cx<'s>, arguments: &Args<'_, 's>) -> R<Local<'s>> {
    let x = arguments.value(0);
    let text = handles::character_vector(context, x)?;      // a standard Local<Str>
    let mut result = context.alloc::<Integer>(text.len())?;
    for (slot, chars) in result.as_mut_slice().iter_mut().zip(text.iter(context)) {
        *slot = chars.bytes().len() as i32;
    }
    handles::copy_shape(context, x, &mut result)?;          // attributes go onto the Fresh
    Ok(result.finish().value())
}
```

### Strings compare by text, symbols by identity

Two strings with the same text can be two objects: the heap interns a text once per encoding
mark, so `"é"` marked latin1 and `"é"` marked UTF-8 differ as pointers. A name lookup that
compares pointers therefore misses a name that is there (`subset__non_ascii_names`). The
bug is not a memory error, but it has the same cause: a `Sexp` compares with `==`, and nothing
says whether identity is the question.

A handle has no `==`. `Gc` and `Local` implement `Copy` and nothing else, and each comparison
names what it compares:

| Question | Spelling | Defined for |
| --- | --- | --- |
| Do two strings hold the same text? | `handles::same_text(left, right)` | `Chars` |
| Is a string `NA`? | `handles::is_na(chars)` | `Chars` |
| Are these the same object? | `left.is(right)` | every kind except `Chars` |

`same_text` is `values::same_string`: equal pointers are equal text, two interned ASCII strings
with different pointers differ, `NA` equals only itself, and any other pair compares its decoded
text, so the common case stays one compare.

A `Symbol` compares by identity, and that is correct by construction: the only way to make one
from a string is `handles::install(cx, chars)`, which installs a string of any encoding under its
UTF-8 text. Two spellings of one name are then one symbol, so code that matches names against
symbols (argument matching, `$` on an environment) needs no text comparison.

A `Gc<Chars>` still widens to `Gc<Value>`, and `is` accepts that, so an identity comparison of
two strings can be written on purpose. It cannot be written by reaching for `==`.

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
(`Error`, `Return`, `Break`) carries its values unbranded, and the scope it leaves roots them
again in the enclosing one, as it does a result: the error type implements `Reroot`, whose one
`unsafe impl` for `Unwind` says that every object it gives access to is rooted afterwards.

A body that reads handles of the enclosing scope runs in `cx.scope_reading`, which also hands it
an `Enclosing<'s, 'inner>`. Its `pass` turns a `Local<'s, _>` into a `Local<'inner, _>`, which is
sound because the enclosing scope's roots stay on the handle stack for all of the nested one, and
only `scope_reading` makes the token. Its result is plain data rather than a handle: the nested
lifetime is higher-ranked, so the result type cannot name it, and nothing is allocated to carry an
element's answer out. A loop that runs R code for each element (`lengths`, which may call a
class's `[[` and `length` methods) uses it to release one element's roots before the next.

A kernel that only moves the elements of a list or character vector (a permutation, a recycling
placement) fills a new vector through `Unfilled::fill_from`. It sees the source's and the
target's slots as `Item`s, which it can copy but not read, so `t`, `aperm`, `matrix` and `array`
run the same slice code on pointers as on numbers and count the references once.

The bytecode loop does not pay that second root. Its registers are traced, so an instruction that
can root runs in `cx.scope_to(register, ..)`, which writes the result into the destination
register and releases the handles, as the loop's `release` does by hand.

Operands need no handle either. Only the running activation's own instructions write its
expression registers, so a value in one stays put for the length of the instruction that reads
it. The loop's operand accessor returns it as a `Local` under the instruction's brand, and a
builtin's arguments arrive the same way. That accessor is one `unsafe` adoption resting on the
bytecode's own invariant. A variable slot is different: a nested closure's `<<-` can overwrite
it during a call, so a slot read is a `Gc` and reaches an operand through an expression register.

A builtin's fast path (its unary or direct entry, which answers the common case without the
call's argument matching) takes no scope and no root of its own. Its caller holds the operands
and releases the handle stack once the entry returns: tier 0 after the instruction, compiled code
and the evaluator's direct call around the entry. So the entry adopts its operands without a
root (`handles::held`), enters the context in the caller's scope (`handles::in_caller_scope`),
and returns an answer that borrows the context:

```rust
fn unary_names<'a, 's>(cx: &'a mut Cx<'s>, x: Local<'s>) -> R<Option<Gc<'a>>>
```

A value the fast path read from the heap goes back unrooted, because the signature proves that
nothing can collect between the read and the return. Where the answer is either read or built,
`cx.read_or(read, build)` tries the read under a shared borrow and runs `build` with the context
only when the read declines. A `Gc<'a>` that a `&'a mut Cx` function returned cannot be rooted
afterwards, since the context is borrowed for as long as the value lives, so a helper that both
kinds of caller need exists in a rooted and an unrooted form (`handles::class_of`,
`handles::class_read`).

Values kept in the interpreter's own state (frames, the handler stack, caches) sit in `Held<T>`
fields, readable as `Gc` through `&Cx`. Each such structure has one `unsafe impl Trace` that marks
its fields.

### Where `unsafe` remains

| Place | Why | Bound |
| --- | --- | --- |
| `heap` | the object layout, the allocator, the collector, and the handle types themselves | the crate |
| `libr` C entry points | a `SEXP` from C carries no proof; each entry makes a `Cx` and adopts its arguments | one block per entry point |
| Helpers called from JIT and typed code | generated code passes raw words | one block per helper |
| Ownership checks | they assert the fact `Owned` rests on | the few functions that call `Owned::assume` |
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
| `&mut Cx` parameter | one more live register through every call | a load from a static in `heap()`; the bytecode loop keeps the heap pointer in its activation | no cost in a builtin (measured below); **to measure** on the call path and in the bytecode loop, where register pressure has cost this interpreter before (`lookup_outside` is forced inline for that reason) |
| Slice access | bounds checks where an index is not an iterator | mostly slices already | no cost in an element walk (measured below) |

`nchar` is measured against its raw form, both compiled into one release build and counted with
callgrind:

| Program | Raw form | Safe form |
| --- | --- | --- |
| `nchar(s)` on one string, 200,000 calls: instructions in the builtin per call | 249 | 233 |
| The same, with the `.Internal` call around it | 1,390 | 1,374 |
| `sum(nchar(y))` on 100,000 strings, 20 times: instructions per element | 284.8 | 278.8 |

The safe form is shorter because a dense view tests the ALTREP bit once per vector, where the raw
accessors test it per element. One rule came out of the measurement: a wrapper in
`interp::handles` whose common case is a test keeps the test `#[inline]` and its slow path
`#[inline(never)]`. With the whole wrapper generic and out of line (the attribute copy for a
value that has no attributes, the coercion of an argument that is already a character vector),
the safe `nchar` ran 42 instructions per call longer than the raw one.

The attribute builtins and their fast-path entries run a few hundred instructions per scalar
call, so ten instructions show in a row of `just bench-mechanisms`. Their first safe forms ran
`attr` 52% longer than the raw one, `names` 30%, `dim` 24% and `attr<-` 21%. Each cause was a
shape, and changing the shape removed it:

- **A fast-path entry takes no scope and roots nothing.** Its caller releases the handle stack,
  so a scope of the entry's own is a second mark and release per call, and an answer that
  borrows the context needs no push (see "Scopes, registers and unwinds").
- **A wrapper called from a very large function is `#[inline(always)]`.** LLVM declines
  `#[inline]` once the caller is large, and the call saves registers on every use
  (`install_name_at`, `attributes_unshared`, the text buffer helpers).
- **An attribute list is walked through an iterator over its cells** (`attribute_cells`). A walk
  that first collects the cells builds a list its caller reads once.
- **A rooted allocation has one out-of-line body with the allocator inlined into it**
  (`Heap::alloc_vector_rooted` under `Cx::alloc`, `values::alloc` for raw code). Two layers each
  save registers, 14 instructions per allocation.
- **Text kept for the length of a call is copied to a stack buffer.** `Cow::into_owned` is a
  `malloc` and a `free`, about 83 instructions per call of `sub`.
- **A general step that a hot caller uses for one case is compiled into that caller, with the
  parts the caller does not need switched off by a constant.** Growing a vector pads the new
  elements, tests for room and copies attributes. An append stores into the one new element,
  usually has room, and usually has no attributes but names, so `append` compiles the growth
  step in (`grow_in_caller`) without the padding, asks the vector's header for room
  (`Owned::has_room`) before the out-of-line resize, and stores the new name into the names the
  growth copied. Through the general growth function an append ran about 77 instructions
  longer than the raw form.
- **A builtin answers its scalar case before it splits its arguments.** `x[[i]]` with two
  untagged arguments reads them from the call and returns a list's element
  without a root, because the builtin's caller receives a raw pointer either way. Splitting
  first builds the subscript list and roots the element, about 160 instructions per call.

With those rules, the forms on handles against the raw forms, in instructions per operation
(`just bench-mechanisms`, tier 1 off): of 103 rows, 75 are within half a percent, 17 are lower and
11 are higher. The rows that differ by 1% or more:

| Operation | Raw | On handles | Change |
| --- | ---: | ---: | ---: |
| `levels(x) <- value` | 1,822 | 1,480 | -18.7% |
| `class(x) <- value` | 1,455 | 1,333 | -8.4% |
| `m[i, j] <- value` | 769 | 709 | -7.8% |
| `substr` | 5,223 | 4,850 | -7.1% |
| `names` | 241 | 225 | -6.6% |
| `inherits` | 753 | 710 | -5.7% |
| two matrix elements of an argument, copied | 2,685 | 2,570 | -4.3% |
| the same, on 100 elements | 2,888 | 2,774 | -4.0% |
| `dim(x) <- value` | 2,286 | 2,205 | -3.5% |
| `startsWith` | 1,124 | 1,086 | -3.4% |
| string element of a local vector | 582 | 566 | -2.7% |
| `grepl`, fixed | 5,638 | 5,500 | -2.4% |
| `attributes` | 1,034 | 1,009 | -2.4% |
| `names(x) <- value` | 3,221 | 3,153 | -2.1% |
| `regexpr` | 10,614 | 10,426 | -1.8% |
| `oldClass(x) <- value` | 1,295 | 1,281 | -1.1% |
| `nchar` | 1,614 | 1,636 | +1.4% |
| `holder$value <- i` on an environment | 795 | 807 | +1.5% |
| `dim` | 209 | 213 | +1.9% |
| `attr` | 390 | 400 | +2.6% |

The four higher rows, by cause:

- **`attr`**: the entry and the lookup run 121 instructions where the raw pair ran 111. The
  lookup matches the name through the cell iterator.
- **`dim`**: the entry runs 27 instructions where the raw one ran 23.
- **`nchar`**: the builtin is on handles on both sides, and the difference is the allocation.
  Its result comes from the rooted allocation body, 95 instructions, where `Cx::alloc` with the
  root pushed inline called the allocator, 79. The rule that helps a builtin with several
  allocations costs one with a single allocation about ten instructions.
- **`holder$value <- i`**: the store function executes ten more register moves and spills. It is
  11 kB of machine code with every store kind inlined, and the allocation of its registers
  changes with unrelated code. Moving the fallback to the replacement builtin out of line made
  the row six instructions longer, because the split passes the operands again.

`[`, `[[`, `$`, `@` and their replacement forms measure the same way: of 105 rows, 95 are within
half a percent of the raw forms, 7 are lower and 3 are higher.

| Operation | Raw | On handles | Change |
| --- | ---: | ---: | ---: |
| `x[name]` | 1,639 | 1,576 | -3.8% |
| `names(x) <- value` | 3,275 | 3,198 | -2.4% |
| `length(x) <- value` | 1,255 | 1,231 | -1.9% |
| a new field of a list argument | 3,304 | 3,266 | -1.2% |
| three appends to a new vector | 4,649 | 4,602 | -1.0% |
| `nchar` | 1,521 | 1,506 | -1.0% |
| string element of a local vector | 572 | 567 | -0.9% |
| `x$name` dispatched to a method | 4,246 | 4,275 | +0.7% |
| `unlist` of a short list | 6,346 | 6,391 | +0.7% |
| `list` with a name | 1,279 | 1,294 | +1.2% |

The three higher rows, by cause:

- **`unlist`**: each element goes through the store that checks the target's type and takes
  its write view, 97 instructions where the raw store ran 43.
- **`x$name` to a method**: the dispatch with the allocation of its name argument runs 214
  instructions where the raw pair ran 181.
- **`list` with a name**: the names vector comes from the rooted allocation body, as `nchar`'s
  result does.

Each migration change measures its builtins the same way before the raw form is deleted, and a
change that reaches the call path or the bytecode loop reports `just bench-mechanisms` and tier
1's instruction counts (`test_tiers`) before and after. A regression is fixed by changing the API
(for the context parameter: keeping it in the activation, or making `Cx` a zero-sized token whose
methods load the static as `heap()` does), not by keeping the raw version.

## Candidates

| Design | How it keeps a value alive | Verdict for ry |
| --- | --- | --- |
| Handle scopes with indirect handles (V8) | every value is a pointer to a slot the collector updates | **Reject.** The indirection exists because V8 moves objects. ry cannot move an object native code holds, so it would pay a slot and a load per value for nothing. Scopes are also unchecked: a handle that outlives its scope is a use after free. V8's direct handles drop the slot and pair with conservative stack scanning, the row below (not checked against current V8) |
| Branded arena lifetimes with collection only between mutations (`gc-arena`) | nothing needs rooting, because the collector runs only when no mutator frame is on the stack | **Reject as the mechanism, adopt the brand.** R evaluation is recursive Rust that re-enters from C (`Rf_eval` inside `.Call`), so there is no point with an empty stack to collect at. The invariant-lifetime brand is what `Cx<'s>` and `Local<'s, T>` use |
| `Rooted` and `Handle`, with a static analysis for hazards (SpiderMonkey) | explicit roots on the stack; a separate tool finds values held across a call that can collect | **Adopt the model, replace the tool.** SpiderMonkey's Rust bindings mark "can collect" with `&mut JSContext`; borrowing does the hazard analysis in the compiler. Josephine (Servo) and shifgrethor have the same shape, a context borrow plus branded roots, and their known difficulty is a second context made on re-entry from foreign code, which is why `Cx` is a capability and not a reference |
| `CAMLparam` and `CAMLlocal` (OCaml) | every local is registered by macro | **Reject.** It is the `PROTECT` discipline, unchecked. `ocaml-interop` shows the borrow-checked form of the same idea |
| Reference counting with in-place mutation when unique (Roc) | the count | **Reject for liveness, adopt uniqueness.** Environments and closures form cycles, and native code, registers and handles hold pointers they never count, so a count cannot free at the decrement: ry frees from counts only at an allocation, after checking the roots. `Write` is the type-level form of Roc's "mutate when unique" |
| Conservative stack scanning (JavaScriptCore, Ruby) | the collector treats every word on the native stack as a possible pointer | **Reject.** It needs no roots and would remove the handle pushes, and the heap can afford it because nothing moves. But a dead value in a stale stack slot stays alive for as long as the compiler happens to leave it there, which breaks finalizer timing that R6 and lgr rely on and that the `evaluation__*finalizer*` cases pin, and it makes that timing depend on code generation. It also covers one rule of seven, does not see values in Rust heap containers, and rests on the optimizer keeping a recognizable pointer |
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
| An ownership check is right before it calls `Owned::assume` | the fact depends on counts, registers and frames | the replacement contract in `interp` `test_fuzz`, `assignment__dollar_in_place` and its siblings |
| An expression register is written only by its own activation | a property of the bytecode the compiler emits | the tier differential, seeded stress |
| Native code keeps the C API's rules | foreign code can do anything | `libr` `test_fuzz_native` for ry's side of the boundary; nothing checks a package's C code |
| A `libr` entry point adopts what C gave it correctly | C can pass anything | `libr` `test_fuzz_native`, the `PROTECT` balance check |
| Generated code keeps its values in the register window across helpers | machine code has no types | `ry-jit-gctorture`, the tier differential |
| A frame's slots agree with its environment | a relation between two structures | the replacement and shadowing contracts in `interp` `test_fuzz`, the differential fuzzer's repeated calls |
| A count is not undercounted | counts gate copies and deferred frees, and the stores that keep them sit inside `heap` | `RY_GC_VERIFY` on every reconcile |

[Robustness](/development/robustness) describes those harnesses.

## Migration

`heap::handle` holds the handle types and `Cx`, with a compile-fail doctest for each rule above
and `crates/heap/tests/test_handles.rs` running them on a heap that collects before every
allocation. `interp::handles` is the interpreter's side: a safe builtin has the signature
`fn(&mut Cx<'s>, &Args<'_, 's>) -> R<Local<'s>>`, the `safe!` macro makes the raw entry the
builtin table holds, and each value helper a safe builtin calls has a safe form there that wraps
the raw one and says why the handle types make the call sound. A native routine of a builtin
library has the same form over `NativeArgs`, whose errors name the calling closure as GNU R's do,
and `safe!(native ...)` makes its raw entry, so a crate above the interpreter writes its routines
without `unsafe`. The wrappers are where a migrated
function meets an unmigrated one, so that boundary is visible and counted, and a wrapper
disappears when the helper under it is migrated.

Written against the handle types: tier 0's element and matrix stores and its replacement forms
(`x[i] <- v`, `x[[i]] <- v`, `x$name <- v`, `x[i, j] <- v` and `f(x) <- v`, on a frame slot or a
binding); subscript resolution, element reads and stores, `[`, `[[`, `$`, `@` with their replacement
forms, the array builders (`matrix`, `array`, `t`, `aperm`, `asplit`, `max.col`), `cbind`, `rbind`
and `lengths` (`subset.rs`); the attribute readers and setters with their fast-path entries (`attr`,
`attributes`, `names`, `dim`, `dimnames`, `class`, `oldClass`, `levels<-`, `comment`, `length<-`,
`inherits`, `unclass` and the replacement forms); the class readers `class_of`, `implicit_class`,
`.class2` and `.cache_class` (`dispatch.rs`); NextMethod's argument patch and the POSIXlt readers
and builtins (`system.rs`); the value copies (`duplicate`, `shallow_duplicate`, the wrapper and
replacement copies), the attribute setters and copiers, pairlist names and the pairlist and list
conversions behind `values.rs`, whose raw names are thin entries into `handles.rs`; `strings.rs` but
for `subject_at` and `flag`, which `agrep` calls raw; the bit conversions in `arith.rs`; the host
hooks (`interp::Hooks`) that `libr` fills, which take and return handles, so `libr` roots what an
ALTREP class method returns before the interpreter sees it (only `connection_method`,
`enter_from_native` and `guard_native` stay `unsafe`, because they run native code); the `arima0`
and `ppr` routines of the stats crate; and the tree-walking evaluator's control flow and language
specials (`{`, `(`, `if`, `for`, `while`, `repeat`, `break`, `next`, `return`, `function`, `&&`,
`||`, `switch`, `missing`, `on.exit`, `nargs`, `substitute` with the methods package's
`do_substitute_direct`, `.Primitive`, `~`, `delayedAssign`, `makeLazy`, `call`, `expression` and
`all.names`); the call specials `.Internal`, `forceAndCall`, `Tailcall`, `Exec`, `Recall` and
`eval`; and assignment (`<-`, `=`, `<<-` and the replacement chain of `f(x) <- v`, which tier 0
enters through the raw entry `eval::compiled_complex_assign`); argument matching (`match_args`, the
match plan a compiled body keeps in its constant pool, and `match.call`); and the slot binders
(`bind_slots`, `bind_plan`, `enter_method`, `direct_sources` and `matched_sources`); and the
variable lookups (symbol evaluation, `findVar`, `findFun`, a frame's own variable and the outward
assignment of `<<-`), which return `Found::Unbound` for an unbound name instead of `R_UnboundValue`;
and promise forcing (`promise_value`, `forced_value`, the promises over a slot frame's pending
environment and the argument references that read a slot frame's variable) with the readers that
settle a promise without running code (`settle_without_code`, `peeked_variable`,
`referenced_variable`); and the frame machinery: the argument reference pool (`slot_reference`,
`take_reference`, `return_references`, `detach`), a slot frame's pending environment and frame
environment (`pending_environment`, `frame_environment_of`, `caller_environment`), the
closures over its slots (`closure_over_slots`), the readers of its slots
(`frame_dots`, `variable_register`, `formal_slots`) and the lifting of its slots into an
environment (`lift`, `box_slots`); the predicates that ask whether a slot or a reference is a
missing argument (`value_is_missing`, `reference_is_missing`, `symbol_is_missing`); a heap frame's
binding of its formals and defaults (`bind_environment`, whose default promises are built after
one `reserve` with the `_uncollected` allocators); the forcing of a typed call's arguments
(`force_typed_arguments`); a frame's exit (`catch_return`, `run_on_exit`, `finish_frame`,
`pop_frame`); and closure application (`apply_closure_in`): the native entry and the native tier
it tries first, the checks that choose a slot frame, and the general path that runs a heap frame
or a frame that must be run whole (`run_heap_frame`, `run_general`, `run_closure_body`), whose raw
names (`apply_closure`, `run_on_heap_frame`, `run_frame_general`) are thin entries; and the
collection of a closure call's arguments (`eval::collect_arguments`, one walk over the argument
cells whose mode chooses promises, eager values or argument references, and tier 0's
`collect_slot_arguments` over a slot frame), with the closure arms of the evaluator's call,
`forceAndCall`, `lapply`'s forced call and tier 0's generic call, whose planned calls run through
`handles::call_with_plan`. A dispatched
call's method variables cross into handles as `handles::MethodVariables`, a view over the
dispatcher's pairs that every layer down to the frame push takes, so they are never copied.
Entering and leaving a frame (`eval::enter`, `eval::leave`) stay `unsafe fn` by design: a frame's lifetime is not lexical, because `leave` truncates the handle stack to the mark
`enter` recorded, so no borrow of the context can span it. Both are built from safe pieces, and the
lexical paths that wrap them take a `Cx`.
A promise is rooted for as long as its code runs, because that code can remove the last binding
that holds it. A reader reaches a live frame through `handles::LiveFrame`, which borrows the
context, so a slot read through it cannot outlive an allocation; `handles::pending_frame` panics
when the environment's frame has returned.
The specials reach the loop and function contexts, the srcref and the closure maker through wrappers
in `handles.rs` (`in_loop` and `loop_jump` hold a loop's environment, `Args::register_on_exit`
stores into the frame), because the bytecode loop and the JIT share those raw helpers. An
interpreter frame holds its call, function, environment, `sys.parent`, pending environment, srcref
and `on.exit` code in `Held<T>` fields, so writing one takes `unsafe` (`Held::adopt`). Raw code
reads a field with `Held::raw`, and migrated code reads it as a `Gc` that borrows the context
(`Held::get`). The bytecode loop's register accessor is not built. Two narrow forms of that accessor
reach a frame's slots. The replacement forms use `RunningFrame` and `Slot` in `bytecode.rs`: a slot
read is a `Gc` that borrows the context, a store takes a rooted value, and an in-place write needs
the `Owned` that `claim` returns only when the slot holds the value itself and the value is
unshared. The binders store a new frame's formals through `handles::Window`, which borrows the
context while it is open, so everything that allocates (the `...` list, the compiled defaults, the
match plan) happens before it opens, and a window dropped without `close` gives back the references
its stores counted and the registers. `Window::fill` checks the slot count once for a whole run of
stores, so a binder's per-formal loop costs what stores into a plain array cost. A method's frame
reads the generic's slots through `Window::under`. A binding is claimed the same way
(`handles::claim_binding`), so a variable bound to a promise is copied before its first write
instead of written through the promise.

An unmigrated caller reaches a migrated function through a raw entry, an `unsafe fn` over `Sexp`
that states what the caller must hold. Which helper the entry uses decides its cost:

| Entry | Helper | Roots |
| --- | --- | --- |
| Returns no object | `handles::with` | a scope of its own, released on return |
| Returns an object to a caller that treats it as rooted | `handles::with_result` | a scope of its own, and the result rooted again in the caller's |
| Runs once per instruction or per loop iteration, and its caller releases the handle stack | `handles::in_caller_scope` with `handles::held` operands | none of its own: the caller's roots hold the operands, and what the entry roots stays until the caller's release |
| Reads one object its caller holds, on every dispatch (`class_of_raw`, `implicit_class_raw`) | `handles::held_in_caller_scope` | none of its own: the caller holds the object, which holds its class attribute, and a new answer stays rooted until the caller's release |
| Finds a binding and runs no code in the common case (`eval_symbol_reporting`, `find_var`, `find_called_fun`, `set_var`) | `handles::reading`, then `handles::in_caller_scope` out of line for the rare cases (`...`, `..N`, an unbound name, an active binding where the caller cannot call it raw) | none on the common path: the binding holds the value it returns or the cell it writes, and symbol evaluation forces a promise through `force_promise`, which roots its result |

The third form is what `subset::vector_element` (the element read of `for` and the apply
functions) and tier 0's `$` entries use, because a scope and a second root per call measured 15
to 50 instructions on those paths.

The order:

1. `interp`, by risk and not by ease: the paths that keep a value across an allocation or write
   in place go first (tier 0's stores, the replacement and attribute builtins, name matching,
   string encoding), then `values.rs`, the evaluator and the bytecode loop, and the leaf builtins
   last, because a leaf rarely holds a value across a safepoint. The frame machinery writes a
   slot frame's registers through `LiveFrame` (`store_slot`, `move_slots_to_cells`), which borrows the
   context; the bytecode loop's own register accessor comes next.
2. `stats`, `graphics`, `frames`.
3. `libr`, `jit`, `typed`: entry points make a `Cx`; raw access leaves their bodies.
4. `heap` stops exporting the raw accessors outside a `raw` module that only the boundary imports,
   and `interp::heap()` and `interp::interp()` are deleted.

`tools/unsafe-baseline.txt` records the count of `unsafe` per source file outside the crates that
own it. `just gate` and CI's `just ci` fail when a file's count rises (`tools/unsafe.py check`),
and a change that lowers one lowers the baseline with it (`tools/unsafe.py baseline`, which
lowers counts and refuses to raise one), so the count only falls.

A count rises only by a hand edit of its line. `check` compares each line with two references:
the baseline's first version, and on a branch the baseline at the commit the branch left main
at. It accepts a line above either only when `.agents/memory/backlog.md` names the functions
that hold the new `unsafe` and allows at least that count:

```
- `crates/interp/src/handles.rs` 107 to 149: `evaluate`, `force`, `frame_value`
```

The expected case is `interp::handles`: a migrated builtin that calls a raw helper with no safe
form adds the helper's wrapper there, and the backlog line stands until the helper itself takes
handles and the wrapper goes.

The guarantee covers a function that names neither `Sexp` nor `interp::heap()`. While the raw API
is public (until step 4), safe code can leave the handle types and come back: `raw()` on any
pointer yields a `Sexp`, `Sexp` compares by address, `interp::heap()` is a safe function that
returns the whole heap, and an unwind carries raw values in public fields. A migrated function
that does any of these compiles, and nothing but review rejects it. Step 4 removes the paths
(`raw()` becomes visible only to the boundary crates, and an unwind holds `Held` values), and
until then a migrated file is one in which `tools/unsafe.py` counts no `unsafe` and a reader finds
no `Sexp`.

## Open questions

- Whether a kind should refuse a vector that carries the array flag. `dense` and `standard`
  admit one, so a migrated fast path over a slice skips the strict checks the array code makes
  unless it tests the flag itself, as the raw fast paths must. A kind per flagged representation
  would make the test a type.
- Whether the bytecode loop's register access measures the same with bounds checks.
- Which form of the context parameter measures best on the call path: a pointer in a register, a
  field of the activation, or a zero-sized token.
- How `Held<T>` fields in structures that native code also reaches (the precious list, external
  pointer protection) share one definition with `libr`.
- The spelling for linking a pairlist cell after it is visible to other code, which the evaluator
  does when it builds argument lists incrementally.
- How `strsplit` fills its result. It stores each new piece vector in the result before making
  the vector's strings, so neither takes a handle, and `Unfilled::fill_elements` cannot express
  that: its iterator borrows `&Cx` and cannot allocate the strings.
