//! The collector against a model. A generator builds random object graphs, drops roots, stores
//! young objects into old ones and collects at random points; after every collection each object
//! the model still reaches must hold exactly what the model says. `FUZZ_ITERS` scales the budget.

use heap::{
    Collection, GcStress, Heap, HeapError,
    arena::Arena,
    object::{NA_REAL, Sexp, SexpType, Slot, is_na_real},
    records::{Fields, Layout},
    strings::Encoding,
};
use std::collections::HashSet;
use syntax::testing::{SplitMix64, fuzz_iterations};

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

fn run(seed: u64, steps: usize, stress: bool) {
    let mut rng = SplitMix64(seed);
    let mut heap = Heap::new();
    heap.policy_mut().young_limit = 4096 + rng.below(64 * 1024);
    if stress {
        heap.policy_mut().stress = GcStress::EveryAllocation;
    }
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
            // Every live node is rooted while new objects are allocated, because the generator
            // may still pick it as a child.
            for &index in &live {
                heap.root(model.nodes[index].as_ref().expect("live").0);
            }
            match rng.below(10) {
                0..=2 => {
                    let bound = if rng.chance(1, 10) { 600 } else { 12 };
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
                    match model.nodes[target].clone() {
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
                _ => {
                    sync_roots(&mut heap, &model);
                    let major = rng.chance(1, 3);
                    let kind = if major {
                        Collection::Major
                    } else {
                        Collection::Minor
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
    for seed in 0..fuzz_iterations(40) as u64 {
        run(seed, 400, false);
    }
}

#[test]
fn fuzz_object_graphs_survive_stress_collection() {
    for seed in 0..fuzz_iterations(8) as u64 {
        run(1000 + seed, 150, true);
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
        assert_eq!(
            garbage.kind(),
            SexpType::Free,
            "a freed slot is poisoned as FREESXP"
        );
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
        let count = heap.string_count();
        heap.collect(Collection::Major);
        assert!(
            heap.string_count() < count,
            "unreferenced strings leave the table"
        );
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
    let mut rng = SplitMix64(7);
    for _ in 0..fuzz_iterations(200) {
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
    }
}
