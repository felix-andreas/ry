//! Delimited files sniff under the contract in `sniff::check_sniff_invariants`, over seeds, byte
//! mutations of them, and random bytes. `FUZZ_ITERS` scales the budget.

use {
    sniff::check_sniff_invariants,
    syntax::testing::{SplitMix64, fuzz_iterations, mutate_bytes, mutate_seed, random_bytes},
};

const SEEDS: &[&str] = &[
    "a,b,c\n1,2.5,TRUE\n2,,FALSE\n",
    "id\tname\n1\t\"quoted\ttab\"\n2\tx\n",
    "x|y\n\"multi\nline\"|3\n4|5\n",
    "\u{feff}a;b\r\n1;2\r\n\r\n3;4\r\n",
    "a b c\nNA 1e3 Inf\n-1 .5 nan\n",
    "1,2\n3,4\n5,x\n",
    "only\n1\n",
    "\"say \"\"hi\"\"\",\"\"\n",
    "a,,a,a.1,a\n1,2,3,4,5\n",
    "x,y\n\"unterminated,1\n",
];

/// Inputs the coverage-guided `sniff` target has broken; only the invariants must hold.
const REGRESSIONS: &[&[u8]] = &[];

#[test]
fn seeds_hold_invariants() {
    for seed in SEEDS {
        check_sniff_invariants(seed.as_bytes());
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_sniff_invariants(input);
    }
}

#[test]
fn fuzz_mutations_and_random_bytes() {
    let mut rng = SplitMix64(0x5a1f_f00d);
    for _ in 0..fuzz_iterations(3000) {
        let input = match rng.below(3) {
            0 => random_bytes(&mut rng).into_bytes(),
            1 => {
                let seed = SEEDS[rng.below(SEEDS.len())];
                mutate_bytes(&mut rng, seed)
            }
            _ => mutate_seed(&mut rng, SEEDS, 24).into_bytes(),
        };
        check_sniff_invariants(&input);
    }
}

/// A file with more records than the head sample reads takes the jump-point path.
#[test]
fn fuzz_large_files() {
    let mut rng = SplitMix64(0x5a1f_b16);
    for _ in 0..fuzz_iterations(3000) / 100 {
        let mut text = String::from("a,b,c\n");
        for row in 0..1200 + rng.below(3000) {
            let cell = ["1", "2.5", "x", "", "NA", "\"q,\nq\"", "TRUE"][rng.below(7)];
            text.push_str(&format!("{row},{cell},{}\n", rng.below(10)));
        }
        check_sniff_invariants(&mutate_bytes(&mut rng, &text));
    }
}
