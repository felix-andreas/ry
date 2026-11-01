---
title: Corpus, oracle and benchmark generator
description: Install the benchmark packages into a GNU R library, compare engines against GNU R, and generate the synthetic code bases
---

Three tools measure the R runtime against GNU R, and all three need only a GNU R installation
(the repository was set up with 4.3.3, the version Ubuntu 24.04 ships). Each has a `just` recipe.

## The package corpus

```sh
just corpus
```

installs the benchmark dependency closure from `AGENTS.md` into `.corpus/library`, together with
the `Suggests` of each package in the list, which their own tests need. It prefers Linux binaries
from a dated [Posit Package Manager](https://packagemanager.posit.co) snapshot, takes `azidentity`,
`Rblpapi` and any other package that CRAN lacks from the finccam repository, and clones
`esgtoolkit` (tag `v1.0.0`) and `loadfast` from GitHub.

| Variable | Default | Meaning |
| --- | --- | --- |
| `RY_CORPUS_DIR` | `.corpus` | Where the library, logs and status file go. |
| `RY_CORPUS_SNAPSHOT` | `2025-03-01` | The Package Manager snapshot. An old date keeps versions compatible with R 4.3. |
| `RY_CORPUS_NCPUS` | `4` | Parallel package installs. |

The run writes `.corpus/status.csv` (one row per package with its version, or the reason it is
missing) and `.corpus/logs/install.log`. Packages that need system libraries fail until you
install them, and the log names the missing header or library.

### Install status

A run on Ubuntu 24.04 with R 4.3.3 installed 65 of the 67 closure packages and every `Suggests`
(the recommended packages `MASS`, `lattice` and `rpart` come from the system R). Two do not
install:

- `polars` is not available for R 4.3.3. Its CRAN release needs a newer R (the runtime design
  targets 4.6), so it needs a newer GNU R than the container's, for example one installed with
  [rig](https://github.com/r-lib/rig).
- `Rblpapi` links against the proprietary Bloomberg `blpapi` SDK (`-lblpapi3_64`), which is not
  redistributable, so it cannot be installed anywhere without a Bloomberg licence.

`FLSSS` is missing from the pinned snapshot, so the script installs it from the newest one. The
packages that load system libraries (`git2r`, `odbc`, `pdftools`, `Rglpk` and the compile
dependencies) need the Ubuntu packages in `tools/corpus/system-packages.txt` first; without them
they install but fail to load.

## The differential oracle

```sh
just oracle verify          # GNU R against the recorded .expected files
just oracle compare         # GNU R against the second engine
just oracle show file.R     # one file's outcome under GNU R
RY_BLESS=1 cargo test -p oracle   # or: just oracle record
```

The `oracle` crate runs an R file the way `Rscript` does and records five things: the
script's stdout (including auto-printed values), its messages, its warnings, the error that
stopped it, and the deparsed last value. A warning or error carries its call, as in
`from f (in f())`. The cases live in `crates/oracle/cases/`, one `group__name.R` file with a
`.expected` file beside it holding GNU R's recorded outcome. They cover promises,
environments, S3 and S4, conditions, vectors, recycling, attributes, NA handling, numerics,
control flow, printing, delimited data I/O (`read.table`, `write.table`, `scan`, `type.convert`),
colours, contour lines and axis ticks, graphics devices and parameters (`par`, layouts, the
device list, text metrics), object sizes and hash tables. `cargo test -p oracle` replays them and passes without R installed. The expectations are GNU R 4.3.3 output, so another R version can differ in printing details and needs `RY_BLESS=1` to re-record.

The driver differs from `Rscript` in four ways that the cases avoid: it parses the whole file first, so a syntax error hides the output of the lines before it; its own frames show in `sys.nframe()` and `sys.calls()`; `commandArgs(TRUE)` holds the two paths; and `quit()` ends the run without a report.

The second engine is anything that implements the `Engine` trait. `ProcessEngine` covers a
command line: it is invoked as `<command…> <script> <report>`, must print the script's output
to stdout, and must write the JSON report that `crates/oracle/src/driver.R` defines (`messages`,
`warnings`, `errors`, and `value` with `kind` and `deparsed`). ry's runtime is `--engine ry`:
it runs the driver through `ry run --runtime ry` with the workspace's debug build (or the binary in
`ORACLE_RY`, which must be an absolute path because each case runs in its own directory), so
build `ry` and `libr` first. `--engine ry-jit` is the same runtime with `RY_JIT=force`, which runs
every body that is called twice and every loop through the JIT, so comparing it against GNU R checks
the compiled tier on every case. Without `--engine` or `ORACLE_ENGINE` the second engine
is a stub and `compare` skips every case. `ORACLE_ENGINE=ry` (or `ry-jit`, or any command line) makes
`cargo test -p oracle` also require the engine to match GNU R on every case.

## Package tests

```sh
just corpus-sources                   # source tarballs, which carry the tests binary installs lack
just package-tests                    # every closure package, under GNU R and ry's runtime
just package-tests --runtime ry vctrs # one package, ry only, reusing the recorded GNU R result
```

`oracle packages` runs each corpus package's own tests the way `R CMD check` runs `tests/`: every
top-level `tests/*.R` file runs as its own script with `tests/` as the working directory, and the
file passes when its process exits with status 0. A package passes when all of its files pass, so
a testthat suite passes when `testthat.R` finishes without a failed expectation. The harness also
reads testthat's summary line (`[ FAIL 0 | WARN 1 | SKIP 2 | PASS 30 ]`) to report expectation
counts beside the status.

`tools/corpus/sources.R` downloads the source tarball of every package in the closure at its
installed version into `.corpus/sources` and writes `.corpus/closure.txt`, the package list the
harness runs. Results go to `.corpus/tests/<package>/<runtime>/` (the copied `tests/` directory,
one `.Rout` per file, and `result.json`), and `.corpus/tests/summary.md` tables every package.
A GNU R result is reused while the package version is unchanged, unless you pass
`--rerun-gnu-r`. `--jobs` (default 4) runs packages in parallel, and `--timeout` (default 600
seconds) bounds one package's suite under one runtime, after which its whole process group is
killed. `ORACLE_RY` picks the ry binary; copy it, with `libR.so` beside it, to another directory
first if you rebuild while a long run is going.

The pass rate counts the packages whose tests pass under GNU R, since a suite that fails there
measures the container, not ry. `MEMORY.md` records the current rate.

## The benchmark generator

```sh
just bench-gen --out .bench --lines 10000    # three packages of about 10,000 lines
just bench-gen --out .bench --lines 300000 --files 500
just bench-test .bench/finbench
```

`tools/bench/generate.py` writes `finbench`, `dsbench` and `biobench` under `--out`. Each is
an R package with a `DESCRIPTION` that imports exactly the benchmark dependency set, a
`NAMESPACE`, R files, and a testthat suite. The generator is deterministic for a given `--seed`,
and its output is not committed. `--domain` generates one package, and `--files` fixes the file
count (by default one file per 600 lines).

The code comes from the R templates in `tools/bench/templates/` (see its `README.md`): shared
infrastructure plus domain code, instantiated with random parameters until the line target is
reached. Services that cannot run in a test (Azure, Bloomberg, ODBC) take an injectable client,
so the tests never touch a network. `just bench-test` runs a package's tests against the corpus library, so run `just corpus` first.
Because `polars` and `Rblpapi` cannot be installed, it tests a copy whose `Imports` omit the
missing packages; the code reaches them only inside guarded or injectable branches. The three
10,000-line packages pass all of their roughly 340 tests each under GNU R 4.3.3.
