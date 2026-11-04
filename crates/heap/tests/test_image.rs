//! Heap images against the heap they were written from. A generator builds a random object graph
//! in one heap, binds parts of it in the global and base environments, and writes an image; a
//! fresh heap loads it, and the loaded graph must be isomorphic to the original, with interned
//! strings and symbols shared and every binding where it was. `FUZZ_ITERS` scales the budget.

use {
    heap::{
        Collection, Fixed, Heap, ImageError, Relocation,
        object::{Sexp, SexpType, Shape},
        records::{Fields, Layout},
        strings::Encoding,
    },
    std::collections::HashMap,
    syntax::testing::{SplitMix64, for_each_seed},
};

const LAYOUT_WIDTH: u32 = 3;

#[test]
fn images_round_trip_random_graphs() {
    for_each_seed(32, |seed| unsafe { round_trip(seed) });
}

#[test]
fn damaged_images_are_rejected() {
    unsafe {
        let mut source = Heap::new();
        let mut names = Vec::new();
        let roots = build(&mut source, &mut SplitMix64(7), &mut names);
        let image = write(&source, &roots);
        for cut in [0, 7, 8, 12, image.len() / 2, image.len() - 1] {
            let mut target = fresh_heap();
            let result = target.load_image(
                &image[..cut],
                &fixed(&[]),
                &Relocation::default(),
                &mut decode,
            );
            assert!(
                matches!(result, Err(ImageError::Invalid(_))),
                "an image cut at {cut} of {} bytes loads",
                image.len()
            );
        }
        let mut longer = image.clone();
        longer.push(0);
        let mut target = fresh_heap();
        assert!(matches!(
            target.load_image(&longer, &fixed(&[]), &Relocation::default(), &mut decode),
            Err(ImageError::Invalid(_))
        ));
    }
}

#[test]
fn relocation_moves_paths_under_a_prefix() {
    unsafe {
        let mut source = Heap::new();
        let texts = [
            "/build/home",
            "/build/home/library/base",
            "/build/homes",
            "elsewhere",
        ];
        let vector = source
            .alloc_vector(SexpType::String, texts.len() as isize)
            .expect("alloc");
        for (index, text) in texts.iter().enumerate() {
            let string = source
                .mk_char(text.as_bytes(), Encoding::Native)
                .expect("mk_char");
            source.set_element(vector, index, string);
        }
        source.root(vector);
        let image = write(&source, &[vector]);
        let mut target = fresh_heap();
        let relocation = Relocation {
            prefixes: vec![(b"/build/home".to_vec(), "/run/hôme".as_bytes().to_vec())],
        };
        let loaded = target
            .load_image(&image, &fixed(&[]), &relocation, &mut decode)
            .unwrap_or_else(|error| panic!("the image does not load: {error}"));
        let texts: Vec<Vec<u8>> = (0..texts.len())
            .map(|index| {
                let string = loaded[0].element(index);
                std::slice::from_raw_parts(string.data(), string.length() as usize).to_vec()
            })
            .collect();
        let expected: [&[u8]; 4] = [
            "/run/hôme".as_bytes(),
            "/run/hôme/library/base".as_bytes(),
            b"/build/homes",
            b"elsewhere",
        ];
        assert_eq!(texts, expected);
    }
}

unsafe fn round_trip(seed: u64) {
    unsafe {
        let mut rng = SplitMix64(seed);
        let mut source = Heap::new();
        let mut names = Vec::new();
        let roots = build(&mut source, &mut rng, &mut names);
        let image = write(&source, &roots);

        let mut target = fresh_heap();
        // A symbol the loading heap made before the image must be the one the image's
        // bindings land on.
        let preinstalled = target.install(b"name0").expect("install");
        let loaded = target
            .load_image(&image, &fixed(&[]), &Relocation::default(), &mut decode)
            .unwrap_or_else(|error| panic!("seed {seed}: the image does not load: {error}"));
        assert_eq!(loaded.len(), roots.len(), "seed {seed}");
        assert_eq!(target.install(b"name0").expect("install"), preinstalled);

        let known = [
            (source.known().nil, target.known().nil),
            (source.known().global_env, target.known().global_env),
            (source.known().base_env, target.known().base_env),
            (source.known().empty_env, target.known().empty_env),
            (source.known().unbound, target.known().unbound),
        ];
        let mut check = Check {
            source: &source,
            target: &target,
            mapped: known.into_iter().collect(),
            seed,
        };
        for (&root, &loaded) in roots.iter().zip(&loaded) {
            check.same(root, loaded);
        }
        for environment in [source.known().global_env, source.known().base_env] {
            let other = check.mapped[&environment];
            for name in &names {
                let from = source.find_symbol(name.as_bytes()).expect("installed");
                let to = target.find_symbol(name.as_bytes()).expect("installed");
                match (
                    source.frame_cell(environment, from),
                    target.frame_cell(other, to),
                ) {
                    (Some(from), Some(to)) => check.same(from.cdr(), to.cdr()),
                    (None, None) => {}
                    _ => panic!("seed {seed}: `{name}` is bound in only one heap"),
                }
            }
        }

        // The loaded objects are ordinary heap objects: they survive collections and take new
        // bindings.
        for &root in &loaded {
            target.root(root);
        }
        target.collect(Collection::Major);
        target.collect(Collection::Minor);
        let global = target.known().global_env;
        let symbol = target.install(b"after").expect("install");
        let value = target.alloc_vector(SexpType::Real, 1).expect("alloc");
        target.define(global, symbol, value).expect("define");
        target.collect(Collection::Major);
        let mut recheck = Check {
            source: &source,
            target: &target,
            mapped: known.into_iter().collect(),
            seed,
        };
        for (&root, &loaded) in roots.iter().zip(&loaded) {
            recheck.same(root, loaded);
        }
    }
}

/// A random graph of every shape an image carries, returned as the roots to write.
unsafe fn build(heap: &mut Heap, rng: &mut SplitMix64, names: &mut Vec<String>) -> Vec<Sexp> {
    unsafe {
        heap.inhibit_collection();
        let layout = layout(heap);
        let nil = heap.nil();
        let mut nodes: Vec<Sexp> = Vec::new();
        for index in 0..10 {
            names.push(format!("name{index}"));
        }
        let count = 20 + rng.below(80);
        for _ in 0..count {
            let pick = |rng: &mut SplitMix64, nodes: &[Sexp]| {
                if nodes.is_empty() || rng.chance(1, 6) {
                    nil
                } else {
                    nodes[rng.below(nodes.len())]
                }
            };
            let node = match rng.below(9) {
                0 => {
                    let length = rng.below(40);
                    let object = heap
                        .alloc_vector(SexpType::Integer, length as isize)
                        .expect("alloc");
                    for value in object.slice_mut::<i32>() {
                        *value = rng.next_u64() as i32;
                    }
                    object
                }
                1 => {
                    // Large enough, sometimes, to be a large object.
                    let length = if rng.chance(1, 8) {
                        20_000
                    } else {
                        rng.below(20)
                    };
                    let object = heap
                        .alloc_vector(SexpType::Real, length as isize)
                        .expect("alloc");
                    for value in object.slice_mut::<f64>() {
                        *value = rng.next_u64() as f64;
                    }
                    object
                }
                2 => {
                    let length = rng.below(6);
                    let object = heap
                        .alloc_vector(SexpType::String, length as isize)
                        .expect("alloc");
                    for index in 0..length {
                        let text = format!("text {} é", rng.below(12));
                        let encoding = if rng.chance(1, 2) {
                            Encoding::Utf8
                        } else {
                            Encoding::Native
                        };
                        let string = heap.mk_char(text.as_bytes(), encoding).expect("mk_char");
                        heap.set_element(object, index, string);
                    }
                    object
                }
                3 => {
                    let length = rng.below(6);
                    let object = heap
                        .alloc_vector(SexpType::List, length as isize)
                        .expect("alloc");
                    for index in 0..length {
                        let child = pick(rng, &nodes);
                        heap.set_element(object, index, child);
                    }
                    object
                }
                4 => {
                    let car = pick(rng, &nodes);
                    let cdr = pick(rng, &nodes);
                    heap.cons(car, cdr).expect("cons")
                }
                5 => {
                    let enclosure = if rng.chance(1, 2) {
                        heap.known().global_env
                    } else {
                        pick(rng, &nodes)
                    };
                    let enclosure = if enclosure.kind() == SexpType::Environment {
                        enclosure
                    } else {
                        heap.known().global_env
                    };
                    let environment = heap.alloc_environment(enclosure).expect("env");
                    // Enough bindings, sometimes, for the frame to be hashed.
                    let bindings = if rng.chance(1, 4) { 80 } else { rng.below(6) };
                    for _ in 0..bindings {
                        let name = format!("local{}", rng.below(120));
                        let symbol = heap.install(name.as_bytes()).expect("install");
                        let value = pick(rng, &nodes);
                        heap.define(environment, symbol, value).expect("define");
                        if rng.chance(1, 8) {
                            heap.remove_binding(environment, symbol).expect("remove");
                        }
                    }
                    environment
                }
                6 => {
                    let values: Vec<Sexp> = (0..LAYOUT_WIDTH)
                        .map(|_| {
                            let child = pick(rng, &nodes);
                            if child == nil {
                                let scalar = heap.alloc_vector(SexpType::Real, 1).expect("alloc");
                                scalar.slice_mut::<f64>()[0] = rng.below(1000) as f64;
                                scalar
                            } else {
                                child
                            }
                        })
                        .collect();
                    heap.alloc_record(layout, &values).expect("record")
                }
                7 => {
                    let token = 1 + rng.below(1000);
                    let tag = pick(rng, &nodes);
                    let protected = pick(rng, &nodes);
                    heap.alloc_external_ptr(token as *mut std::ffi::c_void, tag, protected)
                        .expect("external pointer")
                }
                _ => {
                    let name = format!("symbol{}", rng.below(30));
                    heap.install(name.as_bytes()).expect("install")
                }
            };
            if rng.chance(1, 5) && node.kind().shape() == Shape::Vector {
                let attribute = pick(rng, &nodes);
                let name = heap.install(b"attribute").expect("install");
                let attributes = heap.cons(attribute, nil).expect("cons");
                heap.set_tag(attributes, name);
                heap.set_attrib(node, attributes);
            }
            nodes.push(node);
        }
        let global = heap.known().global_env;
        let base = heap.known().base_env;
        for name in names.iter() {
            let symbol = heap.install(name.as_bytes()).expect("install");
            if rng.chance(1, 2) {
                let value = nodes[rng.below(nodes.len())];
                heap.define(global, symbol, value).expect("define");
            }
            if rng.chance(1, 2) {
                let value = nodes[rng.below(nodes.len())];
                heap.define(base, symbol, value).expect("define");
            }
        }
        let roots: Vec<Sexp> = (0..rng.below(nodes.len()) + 1)
            .map(|_| nodes[rng.below(nodes.len())])
            .collect();
        for &root in &roots {
            heap.root(root);
        }
        heap.allow_collection();
        heap.collect(Collection::Major);
        roots
    }
}

unsafe fn write(heap: &Heap, roots: &[Sexp]) -> Vec<u8> {
    unsafe {
        heap.write_image(&fixed(&[]), roots, &|_| None, &mut encode)
            .unwrap_or_else(|error| panic!("the image does not write: {error}"))
    }
}

unsafe fn fresh_heap() -> Heap {
    let mut heap = Heap::new();
    layout(&mut heap);
    heap
}

fn layout(heap: &mut Heap) -> u32 {
    heap.layouts.intern(Layout {
        tag: None,
        fields: Fields::Positional(LAYOUT_WIDTH),
    })
}

fn fixed(extra: &[Sexp]) -> Fixed<'_> {
    Fixed {
        primitives: &[],
        extra,
    }
}

/// External pointers in these graphs hold small integers, not addresses, so the token is the
/// integer itself.
fn encode(object: Sexp) -> Option<u64> {
    Some(unsafe { object.external_address() } as u64)
}

fn decode(token: u64) -> Option<*mut std::ffi::c_void> {
    Some(token as *mut std::ffi::c_void)
}

struct Check<'a> {
    source: &'a Heap,
    target: &'a Heap,
    /// Each source object already matched, with its loaded counterpart.
    mapped: HashMap<Sexp, Sexp>,
    seed: u64,
}

impl Check<'_> {
    unsafe fn same(&mut self, from: Sexp, to: Sexp) {
        unsafe {
            let seed = self.seed;
            if let Some(&expected) = self.mapped.get(&from) {
                assert_eq!(expected, to, "seed {seed}: one object loaded as two");
                return;
            }
            self.mapped.insert(from, to);
            assert_eq!(from.kind(), to.kind(), "seed {seed}");
            assert_eq!(from.gp(), to.gp(), "seed {seed}: {}", from.kind().name());
            self.same(from.attrib(), to.attrib());
            match from.kind() {
                SexpType::Char => assert_eq!(from.bytes(), to.bytes(), "seed {seed}"),
                SexpType::Symbol => {
                    assert_eq!(from.car().bytes(), to.car().bytes(), "seed {seed}");
                    let name = from.car().bytes().to_vec();
                    let installed = self.target.find_symbol(&name);
                    assert_eq!(installed, Some(to), "seed {seed}: a symbol is not interned");
                }
                SexpType::Integer => assert_eq!(from.slice::<i32>(), to.slice::<i32>()),
                SexpType::Real => {
                    let bits = |object: Sexp| -> Vec<u64> {
                        object
                            .slice::<f64>()
                            .iter()
                            .map(|value| value.to_bits())
                            .collect()
                    };
                    assert_eq!(bits(from), bits(to), "seed {seed}");
                }
                SexpType::String | SexpType::List => {
                    assert_eq!(from.length(), to.length(), "seed {seed}");
                    for index in 0..from.length() as usize {
                        self.same(from.element(index), to.element(index));
                    }
                }
                SexpType::Pairlist | SexpType::Binding => {
                    self.same(from.car(), to.car());
                    self.same(from.cdr(), to.cdr());
                    self.same(from.tag(), to.tag());
                }
                SexpType::Environment => {
                    self.same(from.enclosure(), to.enclosure());
                    let names = |heap: &Heap, environment: Sexp| -> Vec<Vec<u8>> {
                        let mut names: Vec<Vec<u8>> = heap
                            .bound_symbols(environment)
                            .into_iter()
                            .map(|symbol| symbol.car().bytes().to_vec())
                            .collect();
                        names.sort();
                        names
                    };
                    assert_eq!(
                        names(self.source, from),
                        names(self.target, to),
                        "seed {seed}"
                    );
                    for symbol in self.source.bound_symbols(from) {
                        let name = symbol.car().bytes().to_vec();
                        let other = self.target.find_symbol(&name).expect("interned");
                        let from_cell = self.source.frame_cell(from, symbol).expect("bound");
                        let to_cell = self.target.frame_cell(to, other).expect("bound");
                        self.same(from_cell.cdr(), to_cell.cdr());
                    }
                }
                SexpType::ExternalPtr => {
                    assert_eq!(
                        from.external_address(),
                        to.external_address(),
                        "seed {seed}"
                    );
                    self.same(from.external_tag(), to.external_tag());
                    self.same(from.external_protected(), to.external_protected());
                }
                SexpType::Record => {
                    assert_eq!(from.record_slot_count(), to.record_slot_count());
                    for slot in 0..from.record_slot_count() {
                        match (from.record_handle(slot), to.record_handle(slot)) {
                            (Some(from), Some(to)) => self.same(from, to),
                            (None, None) => {}
                            _ => panic!("seed {seed}: a record slot changed representation"),
                        }
                    }
                }
                SexpType::Nil => {}
                kind => panic!("seed {seed}: the generator made no {}", kind.name()),
            }
        }
    }
}
