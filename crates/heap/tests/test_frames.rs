//! Hashed frames against a model of GNU R's chained hash table. Random assignments and removals
//! must leave the frame listing its bindings, and reporting its bucket counts and chain counter,
//! exactly as the model does. Each seed of [`for_each_seed`] is one random run.

use {
    heap::{Heap, object::Sexp},
    syntax::testing::{SplitMix64, for_each_seed},
};

/// GNU R's table, held the plain way: each bucket lists its names newest first.
struct Model {
    buckets: Vec<Vec<String>>,
    count: usize,
}

impl Model {
    fn insert(&mut self, name: &str) {
        let bucket = pjw_hash(name.as_bytes()) as usize % self.buckets.len();
        if self
            .buckets
            .iter()
            .any(|names| names.iter().any(|bound| bound == name))
        {
            return;
        }
        self.buckets[bucket].insert(0, name.to_owned());
        self.count += 1;
        if self.count as f64 > self.buckets.len() as f64 * 0.85 {
            let size = (self.buckets.len() as f64 * 1.2) as usize + 1;
            let mut grown = vec![Vec::new(); size];
            for names in &self.buckets {
                for name in names {
                    let bucket = pjw_hash(name.as_bytes()) as usize % size;
                    grown[bucket].insert(0, name.clone());
                }
            }
            self.count = grown.iter().filter(|names| !names.is_empty()).count();
            self.buckets = grown;
        }
    }

    fn remove(&mut self, name: &str) {
        let bucket = pjw_hash(name.as_bytes()) as usize % self.buckets.len();
        let names = &mut self.buckets[bucket];
        if let Some(position) = names.iter().position(|bound| bound == name) {
            names.remove(position);
            if names.is_empty() {
                self.count -= 1;
            }
        }
    }
}

fn pjw_hash(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &byte in bytes {
        hash = (hash << 4).wrapping_add(byte as i8 as i32 as u32);
        let high = hash & 0xf000_0000;
        if high != 0 {
            hash ^= high >> 24;
            hash ^= high;
        }
    }
    hash
}

fn run(seed: u64) {
    let mut rng = SplitMix64(seed);
    let mut heap = Heap::new();
    let size = [0, 1, 5, 29, 64][rng.below(5)];
    let names: Vec<String> = (0..1 + rng.below(120))
        .map(|index| format!("v{index}"))
        .collect();
    unsafe {
        let nil = heap.nil();
        let env = heap.alloc_environment(nil).expect("environment");
        heap.root(env);
        heap.set_frame_hashed(env, size);
        let symbols: Vec<Sexp> = names
            .iter()
            .map(|name| heap.install(name.as_bytes()).expect("install"))
            .collect();
        let mut model = Model {
            buckets: vec![Vec::new(); if size > 0 { size as usize } else { 29 }],
            count: 0,
        };
        for step in 0..400 {
            let which = rng.below(names.len());
            if rng.chance(3, 5) {
                heap.define(env, symbols[which], nil).expect("define");
                model.insert(&names[which]);
            } else {
                heap.remove_binding(env, symbols[which]).expect("remove");
                model.remove(&names[which]);
            }
            let listed: Vec<String> = heap
                .frame_symbols(env)
                .iter()
                .map(|symbol| String::from_utf8_lossy(symbol.car().bytes()).into_owned())
                .collect();
            let expected: Vec<String> = model.buckets.iter().flatten().cloned().collect();
            assert_eq!(listed, expected, "seed {seed}, step {step}: listing order");
            let (counts, chains) = heap.frame_hash_profile(env).expect("hashed frame");
            let expected_counts: Vec<usize> = model.buckets.iter().map(Vec::len).collect();
            assert_eq!(
                counts, expected_counts,
                "seed {seed}, step {step}: bucket counts"
            );
            assert_eq!(
                chains, model.count,
                "seed {seed}, step {step}: chain counter"
            );
        }
    }
}

#[test]
fn fuzz_hashed_frames_follow_gnu_r_table() {
    for_each_seed(60, run);
}

/// A slot hint only speeds a lookup up: through random definitions, removals, duplicate
/// bindings and hints that are stale or out of range, a hinted lookup finds exactly the cell a plain lookup finds.
fn run_hinted(seed: u64) {
    let mut rng = SplitMix64(seed);
    let mut heap = Heap::new();
    let names: Vec<String> = (0..1 + rng.below(40))
        .map(|index| format!("h{index}"))
        .collect();
    unsafe {
        let nil = heap.nil();
        let env = heap.alloc_environment(nil).expect("environment");
        heap.root(env);
        if rng.chance(1, 3) {
            heap.set_frame_hashed(env, [0, 1, 29][rng.below(3)]);
        }
        let symbols: Vec<Sexp> = names
            .iter()
            .map(|name| heap.install(name.as_bytes()).expect("install"))
            .collect();
        let mut hints = vec![0u32; symbols.len()];
        for step in 0..300 {
            let which = rng.below(symbols.len());
            match rng.below(9) {
                0..=3 => heap.define(env, symbols[which], nil).expect("define"),
                4 | 5 => {
                    heap.remove_binding(env, symbols[which]).expect("remove");
                }
                // A second cell for a bound symbol, as duplicate formals bind one.
                6 => heap.define_unbound(env, symbols[which], nil).expect("bind"),
                _ => hints[which] = rng.below(64) as u32,
            }
            for (index, &symbol) in symbols.iter().enumerate() {
                let plain = heap.frame_cell(env, symbol);
                let hinted = heap.frame_cell_hinted(env, symbol, &mut hints[index]);
                assert_eq!(hinted, plain, "seed {seed}, step {step}: {}", names[index]);
            }
        }
    }
}

#[test]
fn fuzz_hinted_lookups_match_plain_lookups() {
    for_each_seed(60, run_hinted);
}

#[test]
fn closure_gp_bits_do_not_overlap() {
    let mut seen = 0u16;
    for bit in heap::object::gp::CLOSURE_BITS {
        assert_eq!(bit.count_ones(), 1, "{bit:#06x} is not a single bit");
        assert_eq!(seen & bit, 0, "{bit:#06x} is used twice on closures");
        seen |= bit;
    }
}
