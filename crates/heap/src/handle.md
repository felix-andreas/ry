A safepoint is a point where the heap may free an object or foreign code may run: an
allocation, a call into R code, a call into native code. A function that can reach one takes
`&mut Cx`, and a value read out of the heap borrows `&Cx`, so the borrow checker refuses a
value held across a safepoint without a root:

```compile_fail,E0502
use heap::handle::{Cx, List, Local, Real};
fn held_across_an_allocation<'s>(cx: &mut Cx<'s>, list: Local<'s, List>) {
    let first = list.get(cx, 0);
    let copy = cx.alloc::<Real>(1);
    let _ = (first, copy);
}
```

Rooting it is what the rule asks for:

```no_run
use heap::handle::{Cx, List, Local, Real};
fn rooted_across_an_allocation<'s>(cx: &mut Cx<'s>, list: Local<'s, List>) {
    let first = cx.root(list.get(cx, 0));
    let copy = cx.alloc::<Real>(1);
    let _ = (first, copy);
}
```

A slice of a vector's elements borrows the context the same way:

```compile_fail,E0502
use heap::handle::{Cx, Local, Real};
fn slice_across_an_allocation<'s>(cx: &mut Cx<'s>, vector: Local<'s, Real>) {
    let elements = vector.as_slice(cx);
    let copy = cx.alloc::<Real>(1);
    let _ = (elements, copy);
}
```

So do a string's bytes:

```compile_fail,E0502
use heap::handle::{Chars, Cx, Local, Real};
fn bytes_across_an_allocation<'s>(cx: &mut Cx<'s>, text: Local<'s, Chars>) {
    let bytes = text.bytes(cx);
    let copy = cx.alloc::<Real>(1);
    let _ = (bytes, copy);
}
```

A rooted value cannot leave the scope that roots it, except as the scope's result:

```compile_fail
use heap::handle::{Cx, Local, Real};
use heap::HeapError;
fn leaves_its_scope<'s>(cx: &mut Cx<'s>) {
    let mut leaked = None;
    let _ = cx.scope(|cx| -> Result<Local<'_, Real>, HeapError> {
        let inner = cx.alloc::<Real>(1)?.finish();
        leaked = Some(inner);
        Ok(inner)
    });
    let _ = leaked;
}
```

```no_run
use heap::handle::{Cx, Local, Real};
use heap::HeapError;
fn returned_from_its_scope<'s>(cx: &mut Cx<'s>) -> Result<Local<'s, Real>, HeapError> {
    cx.scope(|cx| Ok(cx.alloc::<Real>(1)?.finish()))
}
```

A handle of the enclosing scope is not one of the nested scope's, so code written for one
scope's handles refuses it:

```compile_fail
use heap::handle::{Cx, Local, Real};
use heap::HeapError;
fn copy<'s>(cx: &mut Cx<'s>, vector: Local<'s, Real>) -> Result<Local<'s, Real>, HeapError> {
    let copy = cx.alloc_unfilled::<Real>(vector.len())?;
    Ok(copy.fill(vector.as_slice(cx).iter().copied()).finish())
}
fn copied_in_a_nested_scope<'s>(cx: &mut Cx<'s>, outer: Local<'s, Real>) {
    let _ = cx.scope(|cx| copy(cx, outer));
}
```

`scope_reading` hands the body the proof that the enclosing scope outlives it, which passes
such a handle in, and carries plain data out:

```no_run
use heap::handle::{Cx, Local, Real};
use heap::HeapError;
fn copy<'s>(cx: &mut Cx<'s>, vector: Local<'s, Real>) -> Result<Local<'s, Real>, HeapError> {
    let copy = cx.alloc_unfilled::<Real>(vector.len())?;
    Ok(copy.fill(vector.as_slice(cx).iter().copied()).finish())
}
fn passed_into_the_scope<'s>(cx: &mut Cx<'s>, outer: Local<'s, Real>) -> Result<(), HeapError> {
    cx.scope_reading(|cx, enclosing| {
        let outer = enclosing.pass(outer);
        Ok(copy(cx, outer)?.len())
    })?;
    Ok(())
}
```

A handle of the nested scope is not plain data, so it cannot be the result:

```compile_fail
use heap::handle::{Cx, Real};
fn passed_out_of_the_scope<'s>(cx: &mut Cx<'s>) {
    let _ = cx.scope_reading(|cx, _enclosing| cx.alloc::<Real>(1).map(|fresh| fresh.finish()));
}
```

While an object is written in place, the context is borrowed mutably, so nothing allocates
and no slice taken earlier is alive:

```compile_fail,E0499
use heap::handle::{Cx, Owned, Real};
fn allocates_during_a_write<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>) {
    let mut write = vector.write(cx);
    let (elements, _reader) = write.split();
    let copy = cx.alloc::<Real>(1);
    elements[0] = 1.0;
    let _ = copy;
}
```

```compile_fail,E0502
use heap::handle::{Cx, Owned, Real};
fn reads_an_earlier_slice_during_a_write<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>) {
    let before = vector.get().as_slice(cx);
    let mut write = vector.write(cx);
    let (elements, _reader) = write.split();
    elements[0] = before[1];
}
```

```no_run
use heap::handle::{Cx, Local, Owned, Real};
fn reads_through_the_reader<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>, source: Local<'s, Real>) {
    let mut write = vector.write(cx);
    let (elements, reader) = write.split();
    match reader.slice(source) {
        Some(source) => elements[0] = source[1],
        None => elements.copy_within(1..2, 0),
    }
}
```

A new object is the only one that can be filled without a check, and it is not `Copy`:

```compile_fail,E0382
use heap::handle::{Cx, Integer};
use heap::HeapError;
fn written_after_it_is_visible<'s>(cx: &mut Cx<'s>) -> Result<(), HeapError> {
    let mut fresh = cx.alloc::<Integer>(4)?;
    let visible = fresh.finish();
    fresh.as_mut_slice()[0] = 1;
    let _ = visible;
    Ok(())
}
```

An object with no element storage has no element access. An ALTREP object is never a vector
kind, so a write into one has to go through a copy the allocator made:

```compile_fail,E0599
use heap::handle::{Cx, Owned};
fn written_without_storage<'s>(cx: &mut Cx<'s>, mut copy: Owned<'s>) {
    copy.write(cx).split().0[0] = 1.0;
}
```

Whether a vector may be written where it is, is one value. An [`Owned`] comes from a new
vector or from the one `unsafe` claim that nothing else refers to an existing one, so a
shared value has to be copied first, and the copy has to be one with storage:

```compile_fail,E0599
use heap::handle::{Cx, Local, Real};
fn written_while_shared<'s>(cx: &mut Cx<'s>, shared: Local<'s, Real>) {
    shared.write(cx).split().0[0] = 1.0;
}
```

```no_run
use heap::handle::{Cx, Local, Owned, Real};
use heap::HeapError;
fn written_into_a_copy<'s>(
    cx: &mut Cx<'s>,
    shared: Local<'s, Real>,
) -> Result<Local<'s, Real>, HeapError> {
    let mut copy = cx.alloc::<Real>(shared.len())?;
    copy.as_mut_slice().copy_from_slice(shared.as_slice(cx));
    let mut owned: Owned<'s, Real> = copy.owned();
    owned.write(cx).split().0[0] = 1.0;
    Ok(owned.finish())
}
```

A pointer has no `==`. Two objects are compared with `is`, and strings are not, because one
text can be several objects:

```compile_fail,E0369
use heap::handle::{Chars, Gc};
fn compared_by_address(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    left == right
}
```

```compile_fail,E0277
use heap::handle::{Chars, Gc};
fn compared_by_identity(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
    left.is(right)
}
```

```no_run
use heap::handle::{Gc, Symbol};
fn same_symbol(left: Gc<'_, Symbol>, right: Gc<'_, Symbol>) -> bool {
    left.is(right)
}
```

An ALTREP vector, such as a compact `1:n` or a deferred `as.character(x)`, has the type of
a vector but no element storage, so no one test can say whether an object is a vector of a
kind. A [`Vector`] kind is therefore not [`Exact`] and has no `downcast`:

```compile_fail,E0277
use heap::handle::{Local, Str};
fn names(value: Local<'_>) -> Option<Local<'_, Str>> {
    value.downcast::<Str>()
}
```

The caller chooses: `standard` leaves ALTREP vectors out, for a fast path that falls back to
one that expands them, and `interp::handles::dense` expands them.

```no_run
use heap::handle::{Local, Str};
fn standard_names(value: Local<'_>) -> Option<Local<'_, Str>> {
    value.standard::<Str>()
}
```
