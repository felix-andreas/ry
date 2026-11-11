//! Heap images against the heap they were written from. A generator builds a random object graph
//! in one heap, binds parts of it in the global and base environments, and writes an image; the
//! heap is dropped, a fresh one loads the image at the same addresses, and the loaded graph must
//! describe the same as the original, with interned strings and symbols shared and every binding
//! where it was. `FUZZ_ITERS` scales the budget.
//!
//! Only the first heap a process holds at a time starts at the image address, so the tests take
//! turns, and each drops one heap before it makes the next.

use {
    heap::{
        Collection, Fixed, Heap, ImageError, Relocation,
        image::HeapImage,
        object::{Sexp, SexpType, Shape},
        records::{Fields, Layout},
        strings::Encoding,
    },
    std::{collections::HashMap, sync::Mutex},
    syntax::testing::{SplitMix64, for_each_seed},
};

const LAYOUT_WIDTH: u32 = 3;

static IMAGE_ADDRESS: Mutex<()> = Mutex::new(());

#[test]
fn images_round_trip_random_graphs() {
    let _turn = IMAGE_ADDRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for_each_seed(32, |seed| unsafe { round_trip(seed) });
}

#[test]
fn damaged_or_mismatched_images_leave_the_heap_alone() {
    let _turn = IMAGE_ADDRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        let (image, _, _) = written(7, &[]);
        for cut in [0, 7, 8, 12] {
            let mut target = fresh_heap();
            let result = load(
                &mut target,
                &image,
                &image.metadata[..cut],
                &Relocation::default(),
            );
            assert!(
                matches!(result, Err(ImageError::Invalid(_))),
                "an image cut at {cut} bytes loads"
            );
        }
        // A heap that allocated something the writer's had not is refused before anything
        // changes, and stays usable.
        let mut target = fresh_heap();
        let before = target.alloc_vector(SexpType::Integer, 3).expect("alloc");
        target.root(before);
        let result = load(&mut target, &image, &image.metadata, &Relocation::default());
        assert!(matches!(result, Err(ImageError::Mismatch(_))));
        target.collect(Collection::Major);
        assert_eq!(before.length(), 3);
        // So is a heap that is not at the image address.
        let mut other = fresh_heap();
        let result = load(&mut other, &image, &image.metadata, &Relocation::default());
        assert!(matches!(result, Err(ImageError::Mismatch(_))));
    }
}

#[test]
fn relocation_moves_paths_under_a_prefix() {
    let _turn = IMAGE_ADDRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        let texts = [
            "/build/home",
            "/build/home/library/base",
            "/build/homes",
            "elsewhere",
        ];
        let image = {
            let mut source = fresh_heap();
            let precondition = source.image_precondition(&fixed());
            let vector = source
                .alloc_vector(SexpType::String, texts.len() as isize)
                .expect("alloc");
            for (index, text) in texts.iter().enumerate() {
                let string = source
                    .mk_char(text.as_bytes(), Encoding::Native)
                    .expect("mk_char");
                source.set_element(vector, index, string);
            }
            write(
                &mut source,
                precondition,
                &[vector],
                &[b"/build/home".to_vec()],
            )
        };
        let mut target = fresh_heap();
        let relocation = Relocation {
            prefixes: vec![(b"/build/home".to_vec(), "/run/hôme".as_bytes().to_vec())],
        };
        let loaded = load(&mut target, &image, &image.metadata, &relocation)
            .unwrap_or_else(|error| panic!("the image does not load: {error}"));
        target.root(loaded[0]);
        target.collect(Collection::Major);
        let texts: Vec<Vec<u8>> = (0..texts.len())
            .map(|index| loaded[0].element(index).bytes().to_vec())
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
        let (image, names, expected) = written(seed, &[]);
        let mut target = fresh_heap();
        let loaded = load(&mut target, &image, &image.metadata, &Relocation::default())
            .unwrap_or_else(|error| panic!("seed {seed}: the image does not load: {error}"));
        assert_eq!(
            describe(&target, &loaded, &names),
            expected,
            "seed {seed}: the loaded graph differs"
        );

        // The loaded objects are ordinary heap objects: they survive collections and take new
        // bindings. A load tenures every object, so each kind of collection must find the
        // barrier intact and keep a young object stored into a loaded environment.
        target.update_policy(|policy| policy.verify = true);
        for &root in &loaded {
            target.root(root);
        }
        let global = target.known().global_env;
        let symbol = target.install(b"after").expect("install");
        let value = target.alloc_vector(SexpType::Real, 1).expect("alloc");
        target.define(global, symbol, value).expect("define");
        for kind in [
            Collection::Minor,
            Collection::Intermediate,
            Collection::Minor,
            Collection::Major,
            Collection::Minor,
        ] {
            target.collect(kind);
            let cell = target.frame_cell(global, symbol).expect("bound");
            assert_eq!(cell.cdr(), value, "seed {seed}: {kind:?} dropped a binding");
        }
        assert_eq!(
            describe(&target, &loaded, &names),
            expected,
            "seed {seed}: collections changed the loaded graph"
        );
    }
}

/// Builds the graph of `seed` in a heap at the image address and writes it, returning the image,
/// the names the graph binds, and the graph's description.
unsafe fn written(seed: u64, prefixes: &[Vec<u8>]) -> (HeapImage, Vec<String>, Vec<String>) {
    unsafe {
        let mut source = fresh_heap();
        let precondition = source.image_precondition(&fixed());
        // Garbage that fills whole pages, so the image carries pages the writer's collection
        // emptied and the loader must file as empty.
        for _ in 0..20_000 {
            source.alloc_vector(SexpType::Real, 1).expect("alloc");
        }
        let mut names = Vec::new();
        let roots = build(&mut source, &mut SplitMix64(seed), &mut names);
        source.collect(Collection::Major);
        let expected = describe(&source, &roots, &names);
        let image = write(&mut source, precondition, &roots, prefixes);
        (image, names, expected)
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
        roots
    }
}

unsafe fn write(
    heap: &mut Heap,
    precondition: u64,
    roots: &[Sexp],
    prefixes: &[Vec<u8>],
) -> HeapImage {
    unsafe {
        heap.write_image(
            &fixed(),
            precondition,
            roots,
            prefixes,
            &|_| None,
            &mut encode,
        )
        .unwrap_or_else(|error| panic!("the image does not write: {error}"))
    }
}

unsafe fn load(
    heap: &mut Heap,
    image: &HeapImage,
    metadata: &[u8],
    relocation: &Relocation,
) -> Result<Vec<Sexp>, ImageError> {
    unsafe {
        heap.load_image(
            metadata,
            &fixed(),
            relocation,
            &mut decode,
            &mut |memory| {
                memory.copy_from_slice(image.pages());
                Ok(())
            },
            &mut |memory| {
                memory.copy_from_slice(image.large());
                Ok(())
            },
        )
    }
}

fn fresh_heap() -> Heap {
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

fn fixed() -> Fixed<'static> {
    Fixed {
        primitives: &[],
        extra: &[],
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

/// A description of what `roots` reach and of the bindings of `names` in the global and base
/// environments, with each object named by when the walk first met it, so that two heaps holding
/// the same graph describe it the same way.
unsafe fn describe(heap: &Heap, roots: &[Sexp], names: &[String]) -> Vec<String> {
    unsafe {
        let mut walk = Walk {
            heap,
            seen: HashMap::new(),
            lines: Vec::new(),
        };
        for &root in roots {
            let name = walk.visit(root);
            walk.lines.push(format!("root {name}"));
        }
        for environment in [heap.known().global_env, heap.known().base_env] {
            let environment_name = walk.visit(environment);
            for name in names {
                let symbol = heap.find_symbol(name.as_bytes()).expect("installed");
                if let Some(cell) = heap.frame_cell(environment, symbol) {
                    let value = walk.visit(cell.cdr());
                    walk.lines
                        .push(format!("{environment_name} binds {name} to {value}"));
                }
            }
        }
        walk.lines
    }
}

struct Walk<'a> {
    heap: &'a Heap,
    seen: HashMap<Sexp, usize>,
    lines: Vec<String>,
}

impl Walk<'_> {
    unsafe fn visit(&mut self, object: Sexp) -> String {
        unsafe {
            let heap = self.heap;
            if object == heap.nil() {
                return "nil".to_owned();
            }
            if object == heap.known().global_env {
                return "global".to_owned();
            }
            if object == heap.known().base_env {
                return "base".to_owned();
            }
            if let Some(&index) = self.seen.get(&object) {
                return format!("#{index}");
            }
            let index = self.seen.len();
            self.seen.insert(object, index);
            let name = format!("#{index}");
            let attrib = self.visit(object.attrib());
            let line = match object.kind() {
                SexpType::Char => format!("{:?} {}", object.bytes(), object.gp()),
                SexpType::Symbol => {
                    let text = object.car().bytes().to_vec();
                    assert_eq!(
                        heap.find_symbol(&text),
                        Some(object),
                        "a symbol is not interned"
                    );
                    format!("symbol {}", String::from_utf8_lossy(&text))
                }
                SexpType::Integer => format!("{:?}", object.slice::<i32>()),
                SexpType::Real => format!(
                    "{:?}",
                    object
                        .slice::<f64>()
                        .iter()
                        .map(|value| value.to_bits())
                        .collect::<Vec<_>>()
                ),
                SexpType::String | SexpType::List => {
                    let elements: Vec<String> = (0..object.length() as usize)
                        .map(|element| self.visit(object.element(element)))
                        .collect();
                    format!("{} {elements:?}", object.kind().name())
                }
                SexpType::Pairlist | SexpType::Binding => {
                    let fields =
                        [object.car(), object.cdr(), object.tag()].map(|field| self.visit(field));
                    format!("{} {fields:?}", object.kind().name())
                }
                SexpType::Environment => {
                    let enclosure = self.visit(object.enclosure());
                    let mut bindings: Vec<(Vec<u8>, Sexp)> = heap
                        .bound_symbols(object)
                        .into_iter()
                        .map(|symbol| {
                            let cell = heap.frame_cell(object, symbol).expect("bound");
                            (symbol.car().bytes().to_vec(), cell.cdr())
                        })
                        .collect();
                    bindings.sort_by(|left, right| left.0.cmp(&right.0));
                    let bindings: Vec<String> = bindings
                        .into_iter()
                        .map(|(name, value)| {
                            format!("{}={}", String::from_utf8_lossy(&name), self.visit(value))
                        })
                        .collect();
                    format!("environment in {enclosure} {bindings:?}")
                }
                SexpType::ExternalPtr => format!(
                    "pointer {:?} {} {}",
                    object.external_address(),
                    self.visit(object.external_tag()),
                    self.visit(object.external_protected())
                ),
                SexpType::Record => {
                    let slots: Vec<String> = (0..object.record_slot_count())
                        .map(|slot| match object.record_handle(slot) {
                            Some(handle) => self.visit(handle),
                            None => "raw".to_owned(),
                        })
                        .collect();
                    format!("record {slots:?}")
                }
                kind => panic!("the generator made no {}", kind.name()),
            };
            self.lines
                .push(format!("{name} {line} attributes {attrib}"));
            name
        }
    }
}
