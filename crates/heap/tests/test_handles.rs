//! The handle API over a real heap that collects before every allocation and verifies the write
//! barrier, so a store that skipped the barrier or a result that lost its root fails here.

use {
    heap::{
        GcStress, Heap, HeapError,
        handle::{Cx, Fresh, Integer, List, Local, Owned, Real, Str, Value},
    },
    std::ptr::NonNull,
    syntax::testing::{SplitMix64, for_each_seed},
};

fn with_context<T>(stress: GcStress, body: impl for<'s> FnOnce(&mut Cx<'s>) -> T) -> T {
    let mut heap = Heap::new();
    heap.update_policy(|policy| {
        policy.verify = true;
        policy.stress = stress;
    });
    // SAFETY: the heap outlives the context, this thread alone uses it, and nothing releases
    // the handle stack while `body` runs.
    let mut cx = unsafe { Cx::enter(NonNull::from(&mut heap)) };
    body(&mut cx)
}

fn integers<'s>(cx: &mut Cx<'s>, values: &[i32]) -> Result<Local<'s, Integer>, HeapError> {
    let mut fresh: Fresh<'s, Integer> = cx.alloc(values.len())?;
    fresh.as_mut_slice().copy_from_slice(values);
    Ok(fresh.finish())
}

/// Every allocation between two stores collects, which promotes the list, so each later store
/// puts a young vector into an old list.
#[test]
fn a_list_filled_across_collections_keeps_its_elements() {
    for stress in [GcStress::EveryAllocation, GcStress::Young] {
        for_each_seed(24, |seed| {
            let mut rng = SplitMix64(seed);
            let lengths: Vec<usize> = (0..1 + rng.below(40)).map(|_| rng.below(300)).collect();
            with_context(stress, |cx| -> Result<(), HeapError> {
                let mut list = cx.alloc::<List>(lengths.len())?;
                for (index, &length) in lengths.iter().enumerate() {
                    let element = cx.scope(|cx| {
                        let values: Vec<i32> = (0..length as i32)
                            .map(|value| value + index as i32)
                            .collect();
                        integers(cx, &values)
                    })?;
                    list.set(cx, index, element.value());
                }
                let list = list.finish();
                cx.alloc::<Real>(1024)?;
                for (index, &length) in lengths.iter().enumerate() {
                    let element = list
                        .get(cx, index)
                        .standard::<Integer>()
                        .expect("an integer vector");
                    let element = cx.root(element);
                    let expected: Vec<i32> = (0..length as i32)
                        .map(|value| value + index as i32)
                        .collect();
                    assert_eq!(element.as_slice(cx), expected);
                }
                Ok(())
            })
            .expect("allocation");
        });
    }
}

#[test]
fn a_scope_releases_what_it_rooted_and_keeps_its_result() {
    with_context(GcStress::Young, |cx| -> Result<(), HeapError> {
        let mut temporary = None;
        let kept = cx.scope(|cx| {
            temporary = Some(integers(cx, &[1, 2, 3])?.raw());
            integers(cx, &[4, 5, 6])
        })?;
        // Another size class, so neither allocation can take a freed slot back.
        cx.alloc::<Real>(64)?;
        cx.alloc::<Real>(64)?;
        assert_eq!(kept.as_slice(cx), [4, 5, 6]);
        // SAFETY: stress quarantines and poisons a freed object, so its header stays readable.
        assert!(unsafe { temporary.expect("the scope ran").is_poisoned() });
        Ok(())
    })
    .expect("allocation");
}

#[test]
fn a_scope_reading_its_enclosing_scope_keeps_both() {
    with_context(GcStress::EveryAllocation, |cx| -> Result<(), HeapError> {
        let outer = integers(cx, &[1, 2, 3])?;
        let mut sums = Vec::new();
        for round in 0..8 {
            let sum = cx.scope_reading(|cx, enclosing| {
                let outer = enclosing.pass(outer);
                let shifted: Vec<i32> = outer
                    .as_slice(cx)
                    .iter()
                    .map(|value| value + round)
                    .collect();
                let inner = integers(cx, &shifted)?;
                cx.alloc::<Real>(64)?;
                Ok(outer.as_slice(cx).iter().sum::<i32>() + inner.as_slice(cx).iter().sum::<i32>())
            })?;
            sums.push(sum);
        }
        assert_eq!(outer.as_slice(cx), [1, 2, 3]);
        assert_eq!(sums, (0..8).map(|round| 12 + 3 * round).collect::<Vec<_>>());
        Ok(())
    })
    .expect("allocation");
}

#[test]
fn an_enclosed_scope_returns_its_result_rooted_in_the_enclosing_scope() {
    with_context(GcStress::EveryAllocation, |cx| -> Result<(), HeapError> {
        let outer = integers(cx, &[1, 2, 3])?;
        let doubled = cx.scope_enclosed(|cx, enclosing| {
            let outer = enclosing.pass(outer);
            let values: Vec<i32> = outer.as_slice(cx).iter().map(|value| value * 2).collect();
            cx.alloc::<Real>(64)?;
            integers(cx, &values)
        })?;
        cx.alloc::<Real>(64)?;
        assert_eq!(doubled.as_slice(cx), [2, 4, 6]);
        assert_eq!(outer.as_slice(cx), [1, 2, 3]);
        Ok(())
    })
    .expect("allocation");
}

#[test]
fn a_write_reads_other_vectors_and_refuses_its_own_storage() {
    with_context(GcStress::EveryAllocation, |cx| -> Result<(), HeapError> {
        let target = integers(cx, &[1, 2, 3, 4])?;
        let source = integers(cx, &[10, 20, 30, 40])?;
        // SAFETY: `target` is visible to this test alone.
        let mut owned = unsafe { Owned::assume(target) };
        let mut write = owned.write(cx);
        let (elements, reader) = write.split();
        elements[0] = reader.slice(source).expect("another vector")[3];
        assert!(reader.slice(target).is_none());
        elements.copy_within(1..2, 3);
        assert_eq!(target.as_slice(cx), [40, 2, 3, 2]);
        Ok(())
    })
    .expect("allocation");
}

#[test]
fn a_list_written_in_place_keeps_young_elements() {
    with_context(GcStress::EveryAllocation, |cx| -> Result<(), HeapError> {
        let list = cx.alloc::<List>(3)?.finish();
        // Collections promote the list before the store.
        cx.alloc::<Real>(8)?;
        cx.alloc::<Real>(8)?;
        let young = integers(cx, &[7, 8])?;
        {
            // SAFETY: `list` is visible to this test alone.
            let mut owned = unsafe { Owned::assume(list) };
            let mut write = owned.write(cx);
            let (mut stores, reader) = write.split_elements();
            stores.set(0, young.value());
            stores.set(2, reader.element(list, 0));
        }
        cx.alloc::<Real>(8)?;
        for index in [0, 2] {
            let element = list
                .get(cx, index)
                .standard::<Integer>()
                .expect("an integer vector");
            assert_eq!(cx.root(element).as_slice(cx), [7, 8]);
        }
        Ok(())
    })
    .expect("allocation");
}

#[test]
fn a_downcast_checks_the_kind() {
    with_context(GcStress::Off, |cx| -> Result<(), HeapError> {
        let value: Local<'_, Value> = integers(cx, &[1])?.value();
        assert!(value.standard::<Integer>().is_some());
        assert!(value.standard::<Real>().is_none());
        assert!(value.standard::<List>().is_none());
        let strings = cx.alloc::<Str>(2)?.finish();
        assert!(strings.value().standard::<Str>().is_some());
        assert_eq!(strings.get(cx, 1).bytes(), b"");
        Ok(())
    })
    .expect("allocation");
}
