# Public benchmark programs

These are the R benchmark programs that the Ř (rir) and rcp R compiler projects measure, vendored so that `just bench-real --public all` can time them on GNU R and on ry (`--public awfy` picks one suite). They are the 57-program suite of Kocourek, Křikava and Vitek, "Copy-and-Patch Just-in-Time Compiler for R" (VMIL 2025, Table 1), minus the three that cannot be sized to a few seconds.

| suite | programs | upstream | licence |
| --- | --- | --- | --- |
| `awfy` | 5: Bounce (3 variants), Mandelbrot, Storage | [PRL-PRG/r-compile-server](https://github.com/PRL-PRG/r-compile-server) at `b2b13a3150bddcc2d6dae8c37ecbbf1007170940`, `client/rsh/inst/benchmarks/areWeFast/` | MIT, [`LICENSE-r-compile-server`](LICENSE-r-compile-server) |
| `shootout` | 33: Benchmarks Game programs and their variants | same commit, `shootout/` | MIT, same file |
| `rcp` | 16: the 13 micro-benchmarks of `simple/` (the paper's µ suite) and the 3 convolution programs of `RealThing/` | same commit, `simple/` and `RealThing/` | MIT, same file |

That commit is the one [PRL-PRG/rcp](https://github.com/PRL-PRG/rcp) pins as its `external/rsh` submodule at `141dfda272a97e91bb3145604b9c3b7c86f4b59a`, the state the paper links. The same programs are in [reactorlabs/rbenchmarking](https://github.com/reactorlabs/rbenchmarking) at `a92447b37a03e96f8da1e18eb3cd8ab3b46fbf89`, which rir's benchmark container checks out, but that repository has no licence, so nothing is copied from it. Within the MIT-licensed tree, the shootout ports carry the header "Contributed by Leo Osvald" and name no licence of their own, and the paper credits the are-we-fast-yet translations to Kalibera et al. ([smarr/are-we-fast-yet](https://github.com/smarr/are-we-fast-yet) is MIT). No R port of the other are-we-fast-yet programs (Permute, Queens, Richards, DeltaBlue, Havlak, CD and so on) exists in these repositories.

## How a program runs

Each file is the upstream file unchanged, followed by a driver block that calls `execute` once at a fixed size and prints `<name> <checksum> <seconds>`. The checksum covers the printed output and the returned value, using the same formula as `../harness.R`. Run a program from its own directory, for example `cd awfy && Rscript bounce.R`, because `bounce.R` and `storage.R` source `random.r` by relative path.

The sizes were chosen so that GNU R 4.3 takes about 0.5 to 2.5 seconds per program, which means most differ from the upstream defaults. `knucleotide*`, `regexdna` and `reversecomplement*` read `../fasta/fasta<n>.txt`. Instead of the upstream input files (up to 5 MB), their driver writes that input with `fasta.R` into a temporary directory before the timed region and runs from there, so the input text differs from the upstream files though its format is the same.

## Left out

| program | why |
| --- | --- |
| `RealThing/flexclust`, `flexclust_no_s4` | needs a 4.2 MB data file, and takes 12 to 15 s on GNU R at its smallest size |
| `RealThing/volcano` | takes 32 s on GNU R and has no smaller size |
| `shootout/nbody_3`, `nbody_naive`, `nbody_naive_2`, `simple/sum`, the `SpeedTest/` and `cran/` groups | not part of the paper's suite |
