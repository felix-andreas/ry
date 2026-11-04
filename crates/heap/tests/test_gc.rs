//! The collector against a model. A generator builds random object graphs, drops roots, stores
//! young objects into old ones and collects at random points; after every collection each object
//! the model still reaches must hold exactly what the model says. `FUZZ_ITERS` scales the budget.

use {
    heap::{
        Collection, ENVIRONMENT_BYTES, GcStress, Heap, HeapError, TRIPLE_BYTES,
        arena::Arena,
        object::{NA_REAL, Sexp, SexpType, Slot, gp, is_na_real},
        records::{Fields, Layout},
        strings::Encoding,
    },
    std::collections::HashSet,
    syntax::testing::{SplitMix64, for_each_seed, fuzz_iterations},
};

#[derive(Clone)]
enum Node {
    Integers(Vec<i32>),
    List(Vec<usize>),
    Char(Vec<u8>),
    Env(Vec<(usize, usize)>),
    /// A tuple whose slots hold live nodes or raw doubles.
    Record(Vec<Item>),
}

#[derive(Clone, Copy)]
enum Item {
    Child(usize),
    Raw(f64),
}

struct Model {
    nodes: Vec<Option<(Sexp, Node)>>,
    roots: Vec<usize>,
    symbols: Vec<Sexp>,
    strings: Vec<Vec<u8>>,
}

impl Model {
    fn live(&self) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut stack: Vec<usize> = self.roots.clone();
        while let Some(index) = stack.pop() {
            if !seen.insert(index) {
                continue;
            }
            match self.nodes[index].as_ref().map(|(_, node)| node) {
                Some(Node::List(children)) => stack.extend(children.iter().copied()),
                Some(Node::Env(bindings)) => stack.extend(bindings.iter().map(|(_, value)| *value)),
                Some(Node::Record(items)) => {
                    stack.extend(items.iter().filter_map(|item| match item {
                        Item::Child(child) => Some(*child),
                        Item::Raw(_) => None,
                    }))
                }
                _ => {}
            }
        }
        let mut live: Vec<usize> = seen.into_iter().collect();
        live.sort_unstable();
        live
    }
}

/// The handle stack is the model's root set: rebuilt from `roots` before each collection.
unsafe fn sync_roots(heap: &mut Heap, model: &Model) {
    heap.release_handles(0);
    for &root in &model.roots {
        if let Some((sexp, _)) = &model.nodes[root] {
            heap.root(*sexp);
        }
    }
}

unsafe fn verify(heap: &mut Heap, model: &mut Model) {
    // Interning every string ever made, dead or alive, must give a string with those bytes: a
    // dead entry left in the table would hand out a freed or reused slot.
    for bytes in model.strings.clone() {
        let interned = unsafe { heap.mk_char(&bytes, Encoding::Utf8).expect("interning") };
        unsafe {
            assert_eq!(
                interned.kind(),
                SexpType::Char,
                "the table kept a dead string"
            );
            assert_eq!(interned.bytes(), bytes.as_slice());
        }
    }
    let live: HashSet<usize> = model.live().into_iter().collect();
    for index in 0..model.nodes.len() {
        if !live.contains(&index) {
            model.nodes[index] = None;
        }
    }
    for index in &live {
        let (sexp, node) = model.nodes[*index].clone().expect("a live node");
        unsafe {
            match node {
                Node::Integers(values) => {
                    assert_eq!(sexp.kind(), SexpType::Integer, "node {index} changed type");
                    assert_eq!(
                        sexp.slice::<i32>(),
                        values.as_slice(),
                        "node {index} lost data"
                    );
                }
                Node::List(children) => {
                    assert_eq!(sexp.kind(), SexpType::List);
                    assert_eq!(sexp.length() as usize, children.len());
                    for (position, child) in children.iter().enumerate() {
                        let expected = model.nodes[*child].as_ref().expect("a live child").0;
                        assert_eq!(sexp.element(position), expected, "node {index}[{position}]");
                    }
                }
                Node::Char(bytes) => {
                    assert_eq!(sexp.kind(), SexpType::Char);
                    assert_eq!(sexp.bytes(), bytes.as_slice());
                    let again = heap.mk_char(&bytes, Encoding::Utf8).expect("interning");
                    assert_eq!(again, sexp, "a live string must stay interned");
                }
                Node::Env(bindings) => {
                    assert_eq!(sexp.kind(), SexpType::Environment);
                    for (symbol, value) in bindings {
                        let cell = heap.frame_cell(sexp, model.symbols[symbol]).expect("bound");
                        let expected = model.nodes[value].as_ref().expect("a live value").0;
                        assert_eq!(cell.cdr(), expected);
                    }
                }
                Node::Record(items) => {
                    assert_eq!(sexp.kind(), SexpType::Record, "node {index} changed type");
                    assert_eq!(sexp.record_slot_count(), items.len());
                    for (position, item) in items.iter().enumerate() {
                        let expected = match item {
                            Item::Child(child) => {
                                Slot::Handle(model.nodes[*child].as_ref().expect("a live child").0)
                            }
                            Item::Raw(value) => Slot::Real(*value),
                        };
                        assert_eq!(
                            sexp.record_slot(position),
                            expected,
                            "node {index}.{position}"
                        );
                    }
                }
            }
        }
    }
}

/// `verify` runs the barrier check before every minor collection and makes each reconcile prove
/// what it would free unreachable with a full collection instead of freeing it. Without it, a
/// reconcile frees for real, and the contents check after each collection catches a live vector
/// freed and reused.
fn run(seed: u64, steps: usize, stress: bool) {
    run_checked(seed, steps, stress, true);
}

fn run_checked(seed: u64, steps: usize, stress: bool, verify_reconcile: bool) {
    let mut rng = SplitMix64(seed);
    let mut heap = Heap::new();
    heap.update_policy(|policy| {
        policy.young_limit = 4096 + rng.below(64 * 1024);
        policy.young_large_limit = 4096 + rng.below(256 * 1024);
        // Checks the barrier, and that each header's old bit agrees with its mark, before
        // every minor collection.
        policy.verify = verify_reconcile;
        if stress {
            policy.stress = GcStress::EveryAllocation;
        }
    });
    let mut model = Model {
        nodes: Vec::new(),
        roots: Vec::new(),
        symbols: Vec::new(),
        strings: Vec::new(),
    };
    unsafe {
        for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"] {
            model
                .symbols
                .push(heap.install(name.as_bytes()).expect("install"));
        }
        for _ in 0..steps {
            let live = model.live();
            sync_roots(&mut heap, &model);
            // Without the extra handles, a live node the roots reach only through a counted
            // field is what a reconcile must leave alone.
            if verify_reconcile {
                for &index in &live {
                    heap.root(model.nodes[index].as_ref().expect("live").0);
                }
            }
            match rng.below(10) {
                0..=2 => {
                    // A reconcile frees large vectors only, and runs at a large allocation.
                    let large = if verify_reconcile { 10 } else { 2 };
                    let bound = if rng.chance(1, large) { 600 } else { 12 };
                    let length = rng.below(bound);
                    let values: Vec<i32> = (0..length).map(|_| rng.next_u64() as i32).collect();
                    let sexp = heap
                        .alloc_vector(SexpType::Integer, length as isize)
                        .expect("alloc");
                    sexp.slice_mut::<i32>().copy_from_slice(&values);
                    model.nodes.push(Some((sexp, Node::Integers(values))));
                    model.roots.push(model.nodes.len() - 1);
                }
                3 | 4 if !live.is_empty() => {
                    let children: Vec<usize> = (0..rng.below(5))
                        .map(|_| live[rng.below(live.len())])
                        .collect();
                    let sexp = heap
                        .alloc_vector(SexpType::List, children.len() as isize)
                        .expect("alloc");
                    for (position, child) in children.iter().enumerate() {
                        let child = model.nodes[*child].as_ref().expect("live").0;
                        heap.set_element(sexp, position, child);
                    }
                    // A list built from temporaries, as `list(id[keep], v[keep])` is, holds the
                    // only references to them.
                    if !verify_reconcile && rng.chance(1, 2) {
                        model.roots.retain(|root| !children.contains(root));
                    }
                    model.nodes.push(Some((sexp, Node::List(children))));
                    model.roots.push(model.nodes.len() - 1);
                }
                5 => {
                    let bytes: Vec<u8> = (0..rng.below(6))
                        .map(|_| b"abcxyz\xc3\xa9"[rng.below(8)])
                        .collect();
                    let sexp = heap.mk_char(&bytes, Encoding::Utf8).expect("mk_char");
                    if !model.strings.contains(&bytes) {
                        model.strings.push(bytes.clone());
                    }
                    let existing = live.iter().find(|index| {
                        matches!(&model.nodes[**index], Some((other, Node::Char(_))) if *other == sexp)
                    });
                    match existing {
                        Some(index) => model.roots.push(*index),
                        None => {
                            model.nodes.push(Some((sexp, Node::Char(bytes))));
                            model.roots.push(model.nodes.len() - 1);
                        }
                    }
                }
                6 if !live.is_empty() => {
                    // Store a fresh object into an existing, possibly old, list or environment.
                    let target = live[rng.below(live.len())];
                    let child = live[rng.below(live.len())];
                    let child_sexp = model.nodes[child].as_ref().expect("live").0;
                    let source = model.nodes[child].clone();
                    match model.nodes[target].clone() {
                        // Copy a run of another list's elements, as `Rf_duplicate` does.
                        Some((sexp, Node::List(mut children)))
                            if child != target
                                && rng.chance(1, 3)
                                && matches!(&source, Some((_, Node::List(from))) if !from.is_empty()) =>
                        {
                            let Some((from_sexp, Node::List(from))) = source else {
                                unreachable!("matched above")
                            };
                            let count = rng.below(children.len().min(from.len()) + 1);
                            let to_start = rng.below(children.len() - count + 1);
                            let from_start = rng.below(from.len() - count + 1);
                            heap.copy_elements(sexp, to_start, from_sexp, from_start, count);
                            children[to_start..to_start + count]
                                .copy_from_slice(&from[from_start..from_start + count]);
                            model.nodes[target] = Some((sexp, Node::List(children)));
                        }
                        Some((sexp, Node::List(mut children))) if !children.is_empty() => {
                            let position = rng.below(children.len());
                            heap.set_element(sexp, position, child_sexp);
                            children[position] = child;
                            model.nodes[target] = Some((sexp, Node::List(children)));
                        }
                        Some((sexp, Node::Env(mut bindings))) => {
                            let symbol = rng.below(model.symbols.len());
                            heap.define(sexp, model.symbols[symbol], child_sexp)
                                .expect("define");
                            bindings.retain(|(bound, _)| *bound != symbol);
                            bindings.push((symbol, child));
                            model.nodes[target] = Some((sexp, Node::Env(bindings)));
                        }
                        _ => {}
                    }
                }
                9 if !live.is_empty() && rng.chance(1, 2) => {
                    // A tuple, sometimes wider than the unboxed mask, mixing handles and raw
                    // doubles; the doubles are fresh scalars, rooted until the tuple holds them.
                    // A length-one integer child would be stored raw, so children skip those.
                    let boxed: Vec<usize> = live
                        .iter()
                        .copied()
                        .filter(|index| {
                            !matches!(&model.nodes[*index], Some((_, Node::Integers(values))) if values.len() == 1)
                        })
                        .collect();
                    let wide = !boxed.is_empty() && rng.chance(1, 8);
                    let width = if wide {
                        64 + rng.below(8)
                    } else {
                        rng.below(6)
                    };
                    let mut items = Vec::new();
                    let mut values = Vec::new();
                    for position in 0..width {
                        if position < 64 && (boxed.is_empty() || rng.chance(1, 2)) {
                            let value = (rng.next_u64() % 1000) as f64 / 8.0;
                            let scalar = heap.alloc_vector(SexpType::Real, 1).expect("alloc");
                            *(scalar.data() as *mut f64) = value;
                            heap.root(scalar);
                            items.push(Item::Raw(value));
                            values.push(scalar);
                        } else {
                            let child = boxed[rng.below(boxed.len())];
                            items.push(Item::Child(child));
                            values.push(model.nodes[child].as_ref().expect("live").0);
                        }
                    }
                    let layout = heap.layouts.intern(Layout {
                        tag: None,
                        fields: Fields::Positional(width as u32),
                    });
                    let sexp = heap.alloc_record(layout, &values).expect("alloc");
                    model.nodes.push(Some((sexp, Node::Record(items))));
                    model.roots.push(model.nodes.len() - 1);
                }
                7 => {
                    let nil = heap.nil();
                    let sexp = heap.alloc_environment(nil).expect("env");
                    model.nodes.push(Some((sexp, Node::Env(Vec::new()))));
                    model.roots.push(model.nodes.len() - 1);
                }
                8 if !model.roots.is_empty() => {
                    let drop = rng.below(model.roots.len());
                    model.roots.swap_remove(drop);
                }
                // Collections sweep what a reconcile would free, so the reconcile variant
                // collects less often.
                _ if !verify_reconcile && rng.chance(2, 3) => {}
                _ => {
                    sync_roots(&mut heap, &model);
                    let kind = match rng.below(4) {
                        0 => Collection::Major,
                        1 => Collection::Intermediate,
                        _ => Collection::Minor,
                    };
                    heap.collect(kind);
                    verify(&mut heap, &mut model);
                }
            }
        }
        sync_roots(&mut heap, &model);
        heap.collect(Collection::Minor);
        verify(&mut heap, &mut model);
        heap.collect(Collection::Major);
        verify(&mut heap, &mut model);
    }
}

#[test]
fn fuzz_object_graphs_survive_collection() {
    for_each_seed(40, |seed| run(seed, 400, false));
}

#[test]
fn fuzz_object_graphs_survive_stress_collection() {
    for_each_seed(8, |seed| run(1000 + seed, 150, true));
}

#[test]
fn fuzz_reconciles_free_only_unreachable_vectors() {
    for_each_seed(40, |seed| run_checked(seed, 400, false, false));
}

#[test]
fn fuzz_reconciles_free_only_unreachable_vectors_under_stress() {
    for_each_seed(10, |seed| run_checked(seed, 200, true, false));
}

/// Past the bytes a reconcile waits for, so one dead vector runs it.
const BIG: isize = 600_000;

/// A large vector, filled so a freed and reused one would show.
unsafe fn big_vector(heap: &mut Heap, fill: f64) -> Sexp {
    unsafe {
        let vector = heap.alloc_vector(SexpType::Real, BIG).expect("alloc");
        vector.slice_mut::<f64>().fill(fill);
        vector
    }
}

/// A list holding `vector` as its only counted reference, with the list rooted and `vector`
/// not, so dropping it from the list leaves it at count zero.
unsafe fn held_by_list(heap: &mut Heap, vector: Sexp) -> Sexp {
    unsafe {
        let list = heap.alloc_vector(SexpType::List, 1).expect("alloc");
        heap.add_global_root(list);
        heap.set_element(list, 0, vector);
        list
    }
}

#[derive(Clone)]
enum Held {
    Empty,
    Vector(Sexp, f64),
    /// A list of vectors stored into the holder, so its release cascades to them.
    Nested(Sexp, Vec<(Sexp, f64)>),
}

/// Temporaries the way R code makes them: fresh large vectors stored into rooted lists, shared
/// between slots, overwritten, nested in lists of their own and protected for a while. After
/// every step each slot must hold the vector it was given, intact, and over the run the
/// reconciles must have freed some, without any collection having to.
#[test]
fn fuzz_temporaries_are_freed_and_live_vectors_kept() {
    for_each_seed(fuzz_iterations(20), |seed| {
        let mut rng = SplitMix64(seed);
        let mut heap = Heap::new();
        unsafe {
            if rng.chance(1, 4) {
                heap.update_policy(|policy| policy.stress = GcStress::EveryAllocation);
            }
            let nil = heap.nil();
            const HOLDERS: usize = 4;
            const SLOTS: usize = 4;
            let holders: Vec<Sexp> = (0..HOLDERS)
                .map(|_| {
                    let list = heap
                        .alloc_vector(SexpType::List, SLOTS as isize)
                        .expect("alloc");
                    heap.add_global_root(list);
                    list
                })
                .collect();
            let mut model = vec![vec![Held::Empty; SLOTS]; HOLDERS];
            let mut protected: Vec<(Sexp, f64)> = Vec::new();
            let fresh = |heap: &mut Heap, rng: &mut SplitMix64| {
                let length = 600 + rng.below(30_000) as isize;
                let fill = rng.below(1000) as f64;
                let vector = heap.alloc_vector(SexpType::Real, length).expect("alloc");
                vector.slice_mut::<f64>().fill(fill);
                (vector, fill)
            };
            for step in 0..300 {
                let holder = rng.below(HOLDERS);
                let slot = rng.below(SLOTS);
                let held = match rng.below(8) {
                    0..=2 => {
                        let (vector, fill) = fresh(&mut heap, &mut rng);
                        Held::Vector(vector, fill)
                    }
                    3 => model[rng.below(HOLDERS)][rng.below(SLOTS)].clone(),
                    4 => {
                        let mark = heap.handle_mark();
                        let count = 1 + rng.below(3);
                        let list = heap
                            .alloc_vector(SexpType::List, count as isize)
                            .expect("alloc");
                        heap.root(list);
                        let mut items = Vec::new();
                        for position in 0..count {
                            let (vector, fill) = fresh(&mut heap, &mut rng);
                            heap.set_element(list, position, vector);
                            items.push((vector, fill));
                        }
                        heap.release_handles(mark);
                        Held::Nested(list, items)
                    }
                    5 => {
                        if let Held::Vector(vector, fill) = model[holder][slot] {
                            heap.roots.protect.protect(vector).expect("protect");
                            protected.push((vector, fill));
                        }
                        continue;
                    }
                    6 if !protected.is_empty() => {
                        heap.roots.protect.unprotect(1).expect("unprotect");
                        protected.pop();
                        continue;
                    }
                    _ => Held::Empty,
                };
                let value = match &held {
                    Held::Empty => nil,
                    Held::Vector(vector, _) | Held::Nested(vector, _) => *vector,
                };
                heap.set_element(holders[holder], slot, value);
                model[holder][slot] = held;
                if rng.chance(1, 40) {
                    heap.collect(match rng.below(3) {
                        0 => Collection::Major,
                        1 => Collection::Intermediate,
                        _ => Collection::Minor,
                    });
                }
                let intact = |vector: Sexp, fill: f64| {
                    let values = vector.slice::<f64>();
                    values.first() == Some(&fill) && values.last() == Some(&fill)
                };
                for (holder, slots) in model.iter().enumerate() {
                    for (slot, held) in slots.iter().enumerate() {
                        let stored = holders[holder].element(slot);
                        match held {
                            Held::Empty => assert_eq!(stored, nil),
                            Held::Vector(vector, fill) => {
                                assert_eq!(stored, *vector, "seed {seed} step {step}");
                                assert!(intact(*vector, *fill), "seed {seed} step {step}");
                            }
                            Held::Nested(list, items) => {
                                assert_eq!(stored, *list, "seed {seed} step {step}");
                                for (position, (vector, fill)) in items.iter().enumerate() {
                                    assert_eq!(list.element(position), *vector);
                                    assert!(intact(*vector, *fill), "seed {seed} step {step}");
                                }
                            }
                        }
                    }
                }
                for &(vector, fill) in &protected {
                    assert!(
                        intact(vector, fill),
                        "seed {seed} step {step}: a protected vector"
                    );
                }
            }
            assert!(
                heap.stats().vectors_released > 0,
                "seed {seed}: nothing was released"
            );
        }
    });
}

#[test]
fn a_dropped_large_vector_is_freed_at_the_next_large_allocation() {
    let mut heap = Heap::new();
    unsafe {
        let mark = heap.handle_mark();
        let vector = big_vector(&mut heap, 1.5);
        heap.root(vector);
        let list = held_by_list(&mut heap, vector);
        heap.release_handles(mark);
        let nil = heap.nil();
        heap.set_element(list, 0, nil);
        big_vector(&mut heap, 2.5);
        let stats = heap.stats();
        assert_eq!(stats.vectors_released, 1);
        assert_eq!(stats.minor_collections + stats.major_collections, 0);
    }
}

#[test]
fn a_dead_list_gives_up_its_large_elements() {
    let mut heap = Heap::new();
    unsafe {
        let mark = heap.handle_mark();
        let inner = heap.alloc_vector(SexpType::List, 3).expect("alloc");
        heap.root(inner);
        for slot in 0..3 {
            let vector = big_vector(&mut heap, slot as f64);
            heap.set_element(inner, slot, vector);
        }
        let outer = held_by_list(&mut heap, inner);
        heap.release_handles(mark);
        let nil = heap.nil();
        heap.set_element(outer, 0, nil);
        big_vector(&mut heap, 9.0);
        assert_eq!(heap.stats().vectors_released, 3);
    }
}

#[test]
fn a_protected_vector_at_count_zero_is_kept_until_unprotected() {
    let mut heap = Heap::new();
    unsafe {
        let vector = big_vector(&mut heap, 1.5);
        heap.roots.protect.protect(vector).expect("protect");
        let list = held_by_list(&mut heap, vector);
        let nil = heap.nil();
        heap.set_element(list, 0, nil);
        big_vector(&mut heap, 2.5);
        assert_eq!(heap.stats().vectors_released, 0);
        assert!(vector.slice::<f64>().iter().all(|value| *value == 1.5));
        // Unprotecting stores nothing, so the vector waits for the next reconcile, which the
        // next dead vector runs.
        heap.roots.protect.unprotect(1).expect("unprotect");
        let mark = heap.handle_mark();
        let other = big_vector(&mut heap, 3.5);
        heap.root(other);
        heap.set_element(list, 0, other);
        heap.release_handles(mark);
        heap.set_element(list, 0, nil);
        big_vector(&mut heap, 4.5);
        assert_eq!(heap.stats().vectors_released, 2);
    }
}

/// Fields of an untracked cell (`DISABLE_REFCNT`) do not count, so the cell is a root for the
/// reconcile.
#[test]
fn an_untracked_holder_keeps_a_vector_at_count_zero() {
    let mut heap = Heap::new();
    unsafe {
        let nil = heap.nil();
        let cell = heap.cons(nil, nil).expect("cons");
        heap.add_global_root(cell);
        heap.untrack_references(cell);
        let mark = heap.handle_mark();
        let vector = big_vector(&mut heap, 1.5);
        heap.root(vector);
        heap.set_car(cell, vector);
        let list = held_by_list(&mut heap, vector);
        heap.release_handles(mark);
        heap.set_element(list, 0, nil);
        big_vector(&mut heap, 2.5);
        heap.collect(Collection::Minor);
        assert_eq!(heap.stats().vectors_released, 0);
        assert!(cell.car().slice::<f64>().iter().all(|value| *value == 1.5));
    }
}

/// Native code storing through an escaped list's element pointer counts nothing, so the escaped
/// list's elements are roots until the escape retires and recounts them.
#[test]
fn an_escaped_list_keeps_what_native_code_stored_into_it() {
    let mut heap = Heap::new();
    unsafe {
        let escaped = heap.alloc_vector(SexpType::List, 1).expect("alloc");
        heap.add_global_root(escaped);
        heap.escape(escaped);
        let mark = heap.handle_mark();
        let vector = big_vector(&mut heap, 1.5);
        heap.root(vector);
        *escaped
            .slice_mut::<Sexp>()
            .first_mut()
            .expect("one element") = vector;
        let list = held_by_list(&mut heap, vector);
        heap.release_handles(mark);
        let nil = heap.nil();
        heap.set_element(list, 0, nil);
        big_vector(&mut heap, 2.5);
        assert_eq!(heap.stats().vectors_released, 0);
        heap.retire_escaped();
        assert_eq!(vector.refcount(), 1);
        heap.set_element(escaped, 0, nil);
        big_vector(&mut heap, 3.5);
        assert_eq!(heap.stats().vectors_released, 1);
    }
}

#[test]
fn unrooted_objects_are_freed_and_their_slots_reused() {
    let mut heap = Heap::new();
    unsafe {
        let kept = heap.alloc_vector(SexpType::Real, 3).expect("alloc");
        kept.slice_mut::<f64>()
            .copy_from_slice(&[1.5, NA_REAL, -0.0]);
        heap.root(kept);
        let garbage = heap.alloc_vector(SexpType::Real, 3).expect("alloc");
        let before = heap.stats().objects_freed;
        heap.collect(Collection::Minor);
        assert!(heap.stats().objects_freed > before);
        // Only debug builds poison freed slots, since it costs a store per dead byte.
        if cfg!(debug_assertions) {
            assert_eq!(
                garbage.kind(),
                SexpType::Free,
                "a freed slot is poisoned as FREESXP"
            );
        }
        let reused = heap.alloc_vector(SexpType::Real, 3).expect("alloc");
        assert_eq!(reused, garbage, "the free list hands the slot out again");
        assert!(is_na_real(kept.slice::<f64>()[1]));
        assert_eq!(kept.slice::<f64>()[0], 1.5);
    }
}

#[test]
fn the_write_barrier_keeps_young_objects_stored_into_old_ones() {
    let mut heap = Heap::new();
    unsafe {
        let old = heap.alloc_vector(SexpType::List, 1).expect("alloc");
        heap.root(old);
        heap.collect(Collection::Minor);
        let young = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        *young.slice_mut::<i32>().first_mut().expect("one element") = 42;
        heap.set_element(old, 0, young);
        heap.collect(Collection::Minor);
        assert_eq!(old.element(0).kind(), SexpType::Integer);
        assert_eq!(old.element(0).slice::<i32>(), &[42]);
        assert_eq!(young.refcount(), 1, "a store counts as one reference");
    }
}

#[test]
fn a_tenured_object_keeps_the_younger_old_objects_stored_into_it() {
    let mut heap = Heap::new();
    heap.update_policy(|policy| policy.verify = true);
    unsafe {
        let tenured = heap.alloc_vector(SexpType::List, 2).expect("alloc");
        heap.root(tenured);
        heap.collect(Collection::Major);
        // Stored while young: a minor collection promotes the child only to the younger old
        // generation, so the tenured parent must stay remembered for the intermediate one.
        let first = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        *first.slice_mut::<i32>().first_mut().expect("one element") = 1;
        heap.set_element(tenured, 0, first);
        heap.collect(Collection::Minor);
        // Stored while old but untenured, after its only other holder let go of it.
        let holder = heap.alloc_vector(SexpType::List, 1).expect("alloc");
        heap.root(holder);
        let second = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        *second.slice_mut::<i32>().first_mut().expect("one element") = 2;
        heap.set_element(holder, 0, second);
        heap.collect(Collection::Minor);
        heap.set_element(tenured, 1, second);
        heap.set_element(holder, 0, heap.nil());
        heap.collect(Collection::Intermediate);
        assert_eq!(tenured.element(0).slice::<i32>(), &[1]);
        assert_eq!(tenured.element(1).slice::<i32>(), &[2]);
        heap.collect(Collection::Minor);
        heap.collect(Collection::Major);
        assert_eq!(tenured.element(1).slice::<i32>(), &[2]);
        assert_eq!(heap.stats().intermediate_collections, 1);
    }
}

#[test]
fn an_intermediate_collection_frees_promoted_garbage_but_not_tenured_garbage() {
    let mut heap = Heap::new();
    heap.update_policy(|policy| policy.verify = true);
    unsafe {
        let holder = heap.alloc_vector(SexpType::List, 2).expect("alloc");
        heap.root(holder);
        let tenured = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        heap.set_element(holder, 0, tenured);
        heap.collect(Collection::Major);
        let promoted = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        heap.set_element(holder, 1, promoted);
        heap.collect(Collection::Minor);
        heap.set_element(holder, 0, heap.nil());
        heap.set_element(holder, 1, heap.nil());
        let before = heap.stats().objects_freed;
        heap.collect(Collection::Intermediate);
        assert_eq!(heap.stats().objects_freed, before + 1);
        if cfg!(debug_assertions) {
            assert_eq!(promoted.kind(), SexpType::Free);
        }
        assert_eq!(tenured.kind(), SexpType::Integer);
        heap.collect(Collection::Major);
        assert_eq!(heap.stats().objects_freed, before + 2);
    }
}

#[test]
fn stores_through_an_escaped_pointer_survive_minor_collections() {
    let mut heap = Heap::new();
    unsafe {
        let strings = heap.alloc_vector(SexpType::String, 2).expect("alloc");
        let list = heap.alloc_vector(SexpType::List, 1).expect("alloc");
        heap.root(strings);
        heap.root(list);
        heap.collect(Collection::Minor);
        heap.escape(strings);
        heap.escape(list);
        // Native code stores through the raw element pointers, bypassing the barrier.
        let text = heap
            .mk_char(b"escaped text", Encoding::Native)
            .expect("mk_char");
        *strings
            .slice_mut::<Sexp>()
            .first_mut()
            .expect("two elements") = text;
        let young = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        *young.slice_mut::<i32>().first_mut().expect("one element") = 7;
        *list.slice_mut::<Sexp>().first_mut().expect("one element") = young;
        heap.collect(Collection::Minor);
        assert_eq!(strings.element(0).bytes(), b"escaped text");
        assert_eq!(list.element(0).slice::<i32>(), &[7]);

        // The call returns: the stores made before it are caught once more, then the vectors
        // are ordinary objects again.
        let later = heap
            .mk_char(b"stored before return", Encoding::Native)
            .expect("mk_char");
        *strings
            .slice_mut::<Sexp>()
            .get_mut(1)
            .expect("two elements") = later;
        heap.retire_escaped();
        heap.collect(Collection::Minor);
        assert_eq!(strings.element(1).bytes(), b"stored before return");
        heap.collect(Collection::Major);
        assert_eq!(strings.element(0).bytes(), b"escaped text");
        assert_eq!(strings.element(1).bytes(), b"stored before return");
        assert_eq!(list.element(0).slice::<i32>(), &[7]);
    }
}

#[test]
fn large_objects_are_allocated_and_freed_individually() {
    let mut heap = Heap::new();
    unsafe {
        let big = heap.alloc_vector(SexpType::Real, 100_000).expect("alloc");
        assert!(
            big.slice::<f64>().iter().all(|value| *value == 0.0),
            "zeroed"
        );
        assert_eq!(heap.stats().large_objects, 0, "stats refresh at collection");
        heap.root(big);
        heap.collect(Collection::Major);
        assert_eq!(heap.stats().large_objects, 1);
        heap.release_handles(0);
        heap.collect(Collection::Major);
        assert_eq!(heap.stats().large_objects, 0);
    }
}

#[test]
fn strings_are_interned_per_encoding_and_ascii_is_unmarked() {
    let mut heap = Heap::new();
    unsafe {
        let native = heap.mk_char(b"abc", Encoding::Native).expect("mk_char");
        let utf8 = heap.mk_char(b"abc", Encoding::Utf8).expect("mk_char");
        assert_eq!(native, utf8, "ASCII text is one object in every encoding");
        let e_utf8 = heap
            .mk_char("é".as_bytes(), Encoding::Utf8)
            .expect("mk_char");
        let e_bytes = heap
            .mk_char("é".as_bytes(), Encoding::Bytes)
            .expect("mk_char");
        assert_ne!(e_utf8, e_bytes);
        assert_eq!(Encoding::of(e_utf8), Encoding::Utf8);
        assert_eq!(Encoding::of(native), Encoding::Native);
        let na = heap.mk_char(b"NA", Encoding::Native).expect("NA");
        assert_ne!(heap.known().na_string, na);
        assert_eq!(
            heap.mk_char(b"a\0b", Encoding::Native),
            Err(HeapError::EmbeddedNul {
                text: "a\\0b".into()
            })
        );
        for bytes in [&b"0123\x00567abcdefgh"[..], b"abcdefgh01234\0"] {
            assert!(
                matches!(
                    heap.mk_char(bytes, Encoding::Native),
                    Err(HeapError::EmbeddedNul { .. })
                ),
                "a NUL inside a word or in the tail is found"
            );
        }
        let long_ascii = heap
            .mk_char(b"abcdefgh\x01\x7f", Encoding::Utf8)
            .expect("mk_char");
        assert!(long_ascii.has_gp(gp::ASCII));
        let high = heap
            .mk_char(b"\x80\x01\xff\x01abcdefgh", Encoding::Latin1)
            .expect("high bytes next to 0x01 are not NUL");
        assert_eq!(Encoding::of(high), Encoding::Latin1);
        let count = heap.string_count();
        heap.collect(Collection::Major);
        assert!(
            heap.string_count() < count,
            "unreferenced strings leave the table"
        );
    }
}

/// The scan that hashes a string also finds its NUL and non-ASCII bytes, reading words that end at
/// the last byte and may overlap. Every length up to several words and every byte position must
/// still be seen, and equal bytes must still meet in the table.
#[test]
fn every_byte_position_is_scanned_at_every_length() {
    let mut heap = Heap::new();
    unsafe {
        heap.inhibit_collection();
        for length in 1..=48usize {
            let text: Vec<u8> = (0..length).map(|index| b'a' + (index % 26) as u8).collect();
            let first = heap.mk_char(&text, Encoding::Utf8).expect("mk_char");
            assert!(first.has_gp(gp::ASCII), "length {length} is ASCII");
            assert_eq!(first.bytes(), &text[..]);
            let copy = text.clone();
            let again = heap.mk_char(&copy, Encoding::Latin1).expect("mk_char");
            assert_eq!(first, again, "length {length} interns once");
            for position in 0..length {
                let mut with_nul = text.clone();
                with_nul[position] = 0;
                assert!(
                    matches!(
                        heap.mk_char(&with_nul, Encoding::Native),
                        Err(HeapError::EmbeddedNul { .. })
                    ),
                    "a NUL at {position} of {length} is found"
                );
                let mut high = text.clone();
                high[position] = 0xE9;
                let latin1 = heap.mk_char(&high, Encoding::Latin1).expect("mk_char");
                assert_eq!(
                    Encoding::of(latin1),
                    Encoding::Latin1,
                    "a high byte at {position} of {length} is not ASCII"
                );
                assert_ne!(latin1, first, "one changed byte at {position} of {length}");
                let bytes = heap.mk_char(&high, Encoding::Bytes).expect("mk_char");
                assert_ne!(latin1, bytes, "the encoding is part of the key");
            }
        }
        heap.allow_collection();
    }
}

/// The allocator reserves address space lazily, so without a bound a petabyte vector would be
/// handed out and fail only when touched. GNU R reports it as an allocation failure up front.
#[cfg(target_os = "linux")]
#[test]
fn a_vector_larger_than_the_machine_is_refused() {
    if std::fs::read("/proc/sys/vm/overcommit_memory").is_ok_and(|mode| mode.trim_ascii() == b"1") {
        return;
    }
    let mut heap = Heap::new();
    unsafe {
        let length = 1isize << 47;
        assert!(matches!(
            heap.alloc_vector(SexpType::Real, length),
            Err(HeapError::CannotAllocate { .. })
        ));
        assert!(heap.alloc_vector(SexpType::Real, 1 << 20).is_ok());
    }
}

#[test]
fn weak_references_follow_ephemeron_rules() {
    let mut heap = Heap::new();
    unsafe {
        let nil = heap.nil();
        let key = heap.alloc_environment(nil).expect("env");
        let value = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
        heap.root(key);
        let weak = heap
            .alloc_weak_ref(key, value, nil, 0, false)
            .expect("weak");
        heap.root(weak);
        heap.collect(Collection::Major);
        assert_eq!(
            weak.weak_value(),
            value,
            "the value lives while the key does"
        );
        heap.release_handles(0);
        heap.root(weak);
        heap.collect(Collection::Major);
        assert_eq!(
            weak.weak_key(),
            nil,
            "a dead key without a finalizer clears the reference"
        );
        assert_eq!(weak.weak_value(), nil);

        let key = heap.alloc_environment(nil).expect("env");
        let weak = heap
            .alloc_weak_ref(key, nil, nil, 0x1234, false)
            .expect("weak");
        heap.collect(Collection::Major);
        let ready = heap.take_ready_finalizers();
        assert_eq!(ready, vec![weak]);
        assert_eq!(
            weak.weak_key(),
            key,
            "the key survives until its finalizer runs"
        );
        assert_eq!(key.kind(), SexpType::Environment);
    }
}

#[test]
fn environments_bind_look_up_and_remove() {
    let mut heap = Heap::new();
    unsafe {
        let global = heap.known().global_env;
        let values: Vec<Sexp> = (0..20)
            .map(|index| {
                let symbol = heap
                    .install(format!("v{index}").as_bytes())
                    .expect("install");
                let value = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
                *value.slice_mut::<i32>().first_mut().expect("one") = index;
                heap.define(global, symbol, value).expect("define");
                value
            })
            .collect();
        heap.collect(Collection::Major);
        for (index, value) in values.iter().enumerate() {
            let symbol = heap
                .install(format!("v{index}").as_bytes())
                .expect("install");
            assert_eq!(
                heap.frame_cell(global, symbol).expect("bound").cdr(),
                *value
            );
        }
        let v3 = heap.install(b"v3").expect("install");
        assert_eq!(heap.remove_binding(global, v3), Ok(true));
        assert!(heap.frame_cell(global, v3).is_none());
        let v4 = heap.install(b"v4").expect("install");
        assert_eq!(heap.frame_cell(global, v4).expect("bound").cdr(), values[4]);

        // Removing most bindings compacts the frame; the survivors keep their insertion order
        // and stay reachable through a collection.
        let symbol = |heap: &mut Heap, index: usize| {
            heap.install(format!("v{index}").as_bytes())
                .expect("install")
        };
        for index in (5..20).filter(|index| index % 4 != 0) {
            let removed = symbol(&mut heap, index);
            assert_eq!(heap.remove_binding(global, removed), Ok(true));
            assert_eq!(heap.remove_binding(global, removed), Ok(false));
        }
        heap.collect(Collection::Major);
        let survivors = [0, 1, 2, 4, 8, 12, 16];
        let expected: Vec<Sexp> = survivors
            .iter()
            .map(|&index| symbol(&mut heap, index))
            .collect();
        assert_eq!(heap.bound_symbols(global), expected);
        for &index in &survivors {
            let bound = symbol(&mut heap, index);
            assert_eq!(
                heap.frame_cell(global, bound).expect("bound").cdr(),
                values[index]
            );
        }
        heap.define(global, v3, values[3]).expect("define");
        assert_eq!(heap.bound_symbols(global).last(), Some(&v3));

        let base = heap.known().base_env;
        let pi = heap.install(b"pi").expect("install");
        heap.define(base, pi, values[0]).expect("define");
        assert_eq!(
            pi.cdr().cdr(),
            values[0],
            "base bindings hang off the symbol"
        );
        assert_eq!(
            heap.frame_cell(heap.known().base_namespace, pi),
            heap.frame_cell(base, pi)
        );
        assert!(heap.frame_cell(global, pi).is_none());
    }
}

#[test]
fn the_protect_stack_reports_imbalance_in_gnu_r_words() {
    let mut heap = Heap::new();
    let nil = heap.nil();
    let stack = &mut heap.roots.protect;
    stack.protect(nil).expect("protect");
    let saved = stack.enter();
    stack.protect(nil).expect("protect");
    assert_eq!(
        stack.unprotect(2).map_err(|error| error.to_string()),
        Err("unprotect(): only 1 protected items, can't unprotect 2".into())
    );
    stack.protect(nil).expect("protect");
    assert_eq!(
        stack.leave(saved),
        3,
        "the depth before the region is cut back"
    );
    assert_eq!(stack.depth(), 1);
    stack.limit = 2;
    stack.protect(nil).expect("protect");
    assert_eq!(stack.protect(nil), Err(HeapError::ProtectOverflow));
}

#[test]
fn fuzz_arena_watermarks_nest() {
    for_each_seed(200, |seed| {
        let mut rng = SplitMix64(seed);
        let mut arena = Arena::default();
        let mut marks = Vec::new();
        let mut blocks: Vec<(usize, u8, usize)> = Vec::new();
        for _ in 0..60 {
            match rng.below(4) {
                0 => marks.push((arena.watermark(), blocks.len())),
                1 if !marks.is_empty() => {
                    let (mark, count) = marks.swap_remove(rng.below(marks.len()));
                    marks.retain(|(_, other)| *other <= count);
                    arena.reset(mark);
                    blocks.truncate(count);
                }
                _ => {
                    let large = rng.chance(1, 8);
                    let size = if large { 40_000 } else { 1 + rng.below(300) };
                    let block = arena.allocate(size).expect("allocate");
                    assert_eq!(
                        block.as_ptr() as usize % 16,
                        0,
                        "R_alloc blocks are aligned"
                    );
                    let fill = rng.next_u64() as u8;
                    unsafe { std::ptr::write_bytes(block.as_ptr(), fill, size) };
                    blocks.push((block.as_ptr() as usize, fill, size));
                }
            }
            for &(address, fill, size) in &blocks {
                let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, size) };
                assert!(
                    bytes.iter().all(|byte| *byte == fill),
                    "a live block was overwritten"
                );
            }
            let encoded = Arena::encode(arena.watermark());
            assert_eq!(Arena::decode(encoded), arena.watermark());
        }
    });
}

#[test]
fn bump_runs_fill_the_holes_a_collection_leaves() {
    let mut heap = Heap::new();
    unsafe {
        // Frees what the heap's own start left behind, so the only holes are the test's.
        heap.collect(Collection::Major);
        let count = 3000;
        let kept = heap
            .alloc_vector(SexpType::List, count as isize)
            .expect("alloc");
        heap.root(kept);
        for index in 0..count * 2 {
            let scalar = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
            scalar.slice_mut::<i32>()[0] = index as i32;
            // Every other object dies, so the survivors leave runs of one free slot.
            if index % 2 == 0 {
                heap.set_element(kept, index / 2, scalar);
            }
        }
        heap.collect(Collection::Minor);
        let pages = heap.stats().pages;
        let fresh = heap
            .alloc_vector(SexpType::List, count as isize)
            .expect("alloc");
        heap.root(fresh);
        for index in 0..count {
            let scalar = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
            scalar.slice_mut::<i32>()[0] = -(index as i32) - 1;
            heap.set_element(fresh, index, scalar);
        }
        heap.collect(Collection::Minor);
        assert_eq!(
            heap.stats().pages,
            pages,
            "the holes left room for every new object"
        );
        for index in 0..count {
            assert_eq!(kept.element(index).slice::<i32>(), &[(index * 2) as i32]);
            assert_eq!(fresh.element(index).slice::<i32>(), &[-(index as i32) - 1]);
        }
    }
}

#[test]
fn a_reserved_batch_survives_the_collections_that_follow_it() {
    let mut heap = Heap::new();
    unsafe {
        heap.update_policy(|policy| {
            policy.stress = GcStress::EveryAllocation;
            policy.verify = true;
        });
        let count = 200;
        let kept = heap
            .alloc_vector(SexpType::List, count as isize)
            .expect("alloc");
        heap.root(kept);
        for index in 0..count {
            let value = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
            value.slice_mut::<i32>()[0] = index as i32;
            // Only `value` and `kept` are rooted while the batch is built, so the reservation's
            // own collection must keep `value` and nothing in the batch may collect.
            heap.reserve(ENVIRONMENT_BYTES + 2 * TRIPLE_BYTES, &[value]);
            let nil = heap.nil();
            let env = heap.alloc_environment_uncollected(nil, 1).expect("alloc");
            let inner = heap.cons_uncollected(value, nil).expect("alloc");
            let outer = heap.cons_uncollected(env, inner).expect("alloc");
            heap.set_element(kept, index, outer);
        }
        heap.collect(Collection::Major);
        for index in 0..count {
            let outer = kept.element(index);
            assert_eq!(outer.car().kind(), SexpType::Environment);
            assert_eq!(outer.cdr().car().slice::<i32>(), &[index as i32]);
        }
    }
}

/// The intern table against a model through growth, removal by minor and major collections,
/// and the rebuilds that drop its tombstones: a rooted string always interns to itself, and
/// after a major collection the table holds exactly the strings still rooted.
#[test]
fn fuzz_the_intern_table_keeps_exactly_the_surviving_strings() {
    let mut random = SplitMix64(0x5EED_7AB1E);
    let mut heap = Heap::new();
    unsafe {
        let base = heap.string_count();
        let base_mark = heap.handle_mark();
        let mut rooted: Vec<(Vec<u8>, Sexp)> = Vec::new();
        for round in 0..fuzz_iterations(24) {
            for _ in 0..random.below(20_000) {
                // One string in a hundred is longer than the largest small-object slot, so the
                // large strings' own young list is exercised too.
                let padding = if random.chance(1, 100) { 3000 } else { 0 };
                let bytes = format!("s{round}_{}{}", random.below(50_000), "x".repeat(padding))
                    .into_bytes();
                let string = heap.mk_char(&bytes, Encoding::Native).unwrap();
                if random.chance(1, 4) {
                    heap.root(string);
                    rooted.push((bytes, string));
                }
            }
            rooted.retain(|_| !random.chance(1, 3));
            // The kept strings move into one list while every string is still rooted.
            let holder = heap
                .alloc_vector(SexpType::List, rooted.len() as isize)
                .unwrap();
            for (index, (_, string)) in rooted.iter().enumerate() {
                heap.set_element(holder, index, *string);
            }
            heap.release_handles(base_mark);
            heap.root(holder);
            let kind = if random.chance(1, 3) {
                Collection::Major
            } else {
                Collection::Minor
            };
            heap.collect(kind);
            for (bytes, string) in &rooted {
                assert_eq!(heap.mk_char(bytes, Encoding::Native).unwrap(), *string);
            }
            if kind == Collection::Major {
                let distinct: HashSet<&[u8]> =
                    rooted.iter().map(|(bytes, _)| bytes.as_slice()).collect();
                assert_eq!(heap.string_count(), base + distinct.len(), "round {round}");
            }
        }
    }
}

/// Large vectors that live across a minor collection are promoted, and in a loop like
/// `keep <- v > 0.5; s <- v[keep]; rm(s, keep)` they die right after. The heap must not let them
/// pile up past GNU R's bound: a limit that grows by a fifth once the live data fills 70% of it.
#[test]
fn dead_large_vectors_stay_within_the_heap_limit() {
    dead_large_vectors_stay_within_the_limit(false);
}

/// Collection stress spaces its major collections by live objects, so with many small live
/// objects a loop of large vectors runs only minor collections unless the byte escalation of
/// the ordinary schedule applies to stress too.
#[test]
fn dead_large_vectors_stay_within_the_heap_limit_under_stress() {
    dead_large_vectors_stay_within_the_limit(true);
}

fn dead_large_vectors_stay_within_the_limit(stress: bool) {
    const MEGABYTE: isize = 1 << 20;
    let mut heap = Heap::new();
    unsafe {
        let live = heap
            .alloc_vector(SexpType::Real, 48 * MEGABYTE / 8)
            .expect("alloc");
        heap.add_global_root(live);
        if stress {
            let small = heap.alloc_vector(SexpType::List, 20_000).expect("alloc");
            heap.add_global_root(small);
            for slot in 0..20_000 {
                let element = heap.alloc_vector(SexpType::Integer, 1).expect("alloc");
                heap.set_element(small, slot, element);
            }
            heap.update_policy(|policy| policy.stress = GcStress::EveryAllocation);
        }
        let mut peak = 0;
        for round in 0..40 {
            let mark = heap.handle_mark();
            // A mask, then two selections of it, each rooted while the round runs.
            for megabytes in [8, 4, 8] {
                let length = (megabytes + (round % 3)) * MEGABYTE / 8;
                let vector = heap.alloc_vector(SexpType::Real, length).expect("alloc");
                heap.root(vector);
                peak = peak.max(heap.allocated_bytes());
            }
            heap.release_handles(mark);
        }
        let live_peak = (48 + 22) << 20;
        assert!(
            peak <= live_peak * 2,
            "the heap reached {} MB for at most {} MB of live data",
            peak >> 20,
            live_peak >> 20
        );
    }
}

/// Vectors large enough for a mapping of their own reuse the mappings of dead ones, which hold
/// the dead vector's elements: a zeroed allocation must read as zero and every live vector
/// must keep its elements however the mappings are resized and recycled.
#[test]
fn fuzz_mapped_vectors_keep_their_elements() {
    for_each_seed(fuzz_iterations(8), |seed| {
        let mut rng = SplitMix64(seed);
        let mut heap = Heap::new();
        let mut live: Vec<(Sexp, i32)> = Vec::new();
        unsafe {
            for _ in 0..60 {
                let length = (1 << 18) + rng.below(3 << 18);
                let vector = heap
                    .alloc_vector(SexpType::Integer, length as isize)
                    .expect("alloc");
                assert!(
                    vector.slice::<i32>().iter().all(|&value| value == 0),
                    "seed {seed}: a vector of {length} integers was not zeroed"
                );
                let fill = rng.next_u64() as i32;
                vector.slice_mut::<i32>().fill(fill);
                live.push((vector, fill));
                if rng.chance(1, 2) && !live.is_empty() {
                    live.swap_remove(rng.below(live.len()));
                }
                heap.release_handles(0);
                for &(vector, _) in &live {
                    heap.root(vector);
                }
                if rng.chance(1, 4) {
                    let kind = if rng.chance(1, 2) {
                        Collection::Major
                    } else {
                        Collection::Minor
                    };
                    heap.collect(kind);
                }
                for &(vector, fill) in &live {
                    assert!(
                        vector.slice::<i32>().iter().all(|&value| value == fill),
                        "seed {seed}: a live mapped vector changed"
                    );
                }
            }
        }
    });
}

/// Blocks through the mapping allocator, grown and shrunk across the mapping threshold in both
/// directions, keep their bytes, and zeroed ones read as zero.
#[test]
fn fuzz_mapped_allocator_keeps_contents() {
    use std::alloc::{GlobalAlloc, Layout, System};
    let allocator = heap::large::Mapped(System);
    for_each_seed(fuzz_iterations(8), |seed| {
        let mut rng = SplitMix64(seed);
        let mut blocks: Vec<(*mut u8, Layout, u8)> = Vec::new();
        let size = |rng: &mut SplitMix64| match rng.below(3) {
            0 => 1 + rng.below(4096),
            1 => (1 << 20) - 64 + rng.below(128),
            _ => (1 << 20) + rng.below(3 << 20),
        };
        unsafe {
            for _ in 0..40 {
                let layout = Layout::from_size_align(size(&mut rng), 16).expect("layout");
                let fill = rng.next_u64() as u8;
                let block = if rng.chance(1, 2) {
                    let block = allocator.alloc_zeroed(layout);
                    assert!(!block.is_null(), "seed {seed}: alloc_zeroed failed");
                    let bytes = std::slice::from_raw_parts(block, layout.size());
                    assert!(
                        bytes.iter().all(|&byte| byte == 0),
                        "seed {seed}: a zeroed block of {} bytes was not zero",
                        layout.size()
                    );
                    block
                } else {
                    allocator.alloc(layout)
                };
                assert!(!block.is_null(), "seed {seed}: alloc failed");
                std::ptr::write_bytes(block, fill, layout.size());
                blocks.push((block, layout, fill));
                if rng.chance(1, 3) {
                    let index = rng.below(blocks.len());
                    let (block, layout, fill) = blocks[index];
                    let new_size = size(&mut rng);
                    let moved = allocator.realloc(block, layout, new_size);
                    assert!(
                        !moved.is_null(),
                        "seed {seed}: realloc to {new_size} failed"
                    );
                    let kept = std::slice::from_raw_parts(moved, layout.size().min(new_size));
                    assert!(
                        kept.iter().all(|&byte| byte == fill),
                        "seed {seed}: realloc from {} to {new_size} bytes lost the contents",
                        layout.size()
                    );
                    let new_layout = Layout::from_size_align(new_size, 16).expect("layout");
                    std::ptr::write_bytes(moved, fill, new_size);
                    blocks[index] = (moved, new_layout, fill);
                }
                if rng.chance(1, 3) {
                    let (block, layout, fill) = blocks.swap_remove(rng.below(blocks.len()));
                    let bytes = std::slice::from_raw_parts(block, layout.size());
                    assert!(
                        bytes.iter().all(|&byte| byte == fill),
                        "seed {seed}: a block of {} bytes changed",
                        layout.size()
                    );
                    allocator.dealloc(block, layout);
                }
            }
            for (block, layout, _) in blocks {
                allocator.dealloc(block, layout);
            }
        }
    });
}

#[test]
fn heap_growth_follows_gnu_r_variables() {
    use heap::HeapGrowth;
    let growth = |pairs: &[(&str, &str)]| {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        HeapGrowth::from_environment(move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
    };
    assert_eq!(growth(&[]), HeapGrowth::DEFAULT);
    assert_eq!(growth(&[("R_GC_MEM_GROW", "0")]).grow_increment, 0.0);
    let aggressive = growth(&[("R_GC_MEM_GROW", "3")]);
    assert_eq!(
        (aggressive.grow_fraction, aggressive.grow_increment),
        (0.5, 0.4)
    );
    // Out of range, as GNU R ignores it.
    assert_eq!(growth(&[("R_GC_GROWFRAC", "0.9")]), HeapGrowth::DEFAULT);
    assert_eq!(growth(&[("R_GC_GROWFRAC", "0.4")]).grow_fraction, 0.4);
    let specific = growth(&[("R_GC_GROWINCRFRAC", "0.1"), ("R_GC_VGROWINCRFRAC", "0.6")]);
    assert_eq!(specific.grow_increment, 0.6);

    const MEGABYTE: usize = 1 << 20;
    let minimum = 64 * MEGABYTE;
    let default = HeapGrowth::DEFAULT;
    // A need past the limit raises it to the need, then by a fifth plus GNU R's smallest step.
    let grown = default.next_limit(minimum, 100 * MEGABYTE, minimum);
    assert_eq!(grown, 120 * MEGABYTE + 640_000);
    // Between 30% and 70% full, the limit holds.
    assert_eq!(
        default.next_limit(200 * MEGABYTE, 100 * MEGABYTE, minimum),
        200 * MEGABYTE
    );
    // Below 30% it shrinks by a fifth, never below the minimum.
    assert_eq!(
        default.next_limit(500 * MEGABYTE, 100 * MEGABYTE, minimum),
        400 * MEGABYTE
    );
    assert_eq!(
        default.next_limit(70 * MEGABYTE, MEGABYTE, minimum),
        minimum
    );
    // Without growth, the limit stays a fifth of the minimum above the need, the headroom below
    // which a minor collection escalates to a major one.
    let tight = growth(&[("R_GC_MEM_GROW", "0")]);
    assert_eq!(
        tight.next_limit(minimum, 100 * MEGABYTE, minimum),
        100 * MEGABYTE + minimum / 5
    );
}
