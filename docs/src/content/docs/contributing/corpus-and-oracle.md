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
device list, text metrics), grid and the graphics engine (the `grid__*` cases, which need ragg and lattice installed and compare drawings with GNU R by pixel checksums of a ragg capture), object sizes, hash tables, and data.table queries (the `datatable__*` cases, which need data.table installed and check that the queries ry answers itself print and behave exactly as under data.table). `cargo test -p oracle` replays them and passes without R installed. The expectations are GNU R 4.3.3 output, so another R version can differ in printing details and needs `RY_BLESS=1` to re-record.

The driver differs from `Rscript` in four ways that the cases avoid: it parses the whole file first, so a syntax error hides the output of the lines before it; its own frames show in `sys.nframe()` and `sys.calls()` (so data.table prints the table after a top-level `:=`, which it skips at a real prompt); `commandArgs(TRUE)` holds the two paths; and `quit()` ends the run without a report.

The second engine is anything that implements the `Engine` trait. `ProcessEngine` covers a
command line: it is invoked as `<command…> <script> <report>`, must print the script's output
to stdout, and must write the JSON report that `crates/oracle/src/driver.R` defines (`messages`,
`warnings`, `errors`, and `value` with `kind` and `deparsed`). ry's runtime is `--engine ry`:
it runs the driver through `ry run --runtime ry` with the workspace's debug build (or the binary in
`ORACLE_RY`, which must be an absolute path because each case runs in its own directory), so
build `ry` and `libr` first. `--engine ry-jit` is the same runtime with `RY_JIT=force`, which runs
every body that is called twice and every loop through the JIT, so comparing it against GNU R checks
the compiled tier on every case. `ry-tier0` runs with `RY_JIT=off`, and `ry-gctorture` runs with
`RY_GC_STRESS=1`, which collects before every allocation once the script starts. Without `--engine` or `ORACLE_ENGINE` the second engine
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

## Installing on ry

```sh
just ry-home                       # build release libR and ry-rcmd, assemble .corpus/ry-home
just corpus-ry-install             # install the whole closure from source on ry
just corpus-ry-install vctrs tibble   # only these, reusing what the library already holds
just oracle packages --installed   # run the tests against ry's own installations
```

`oracle install` installs each package from its tarball in `.corpus/sources` with the R home's
`bin/R CMD INSTALL`, so configure, compilation and the R-level steps (lazy-load database, `Meta/`,
help) all run on ry, into `.corpus/ry-library`. A package starts once everything in its `Depends`,
`Imports` and `LinkingTo` has installed, or is already in the library when it is not part of the
run, and is skipped when one of them failed. After installing, it is loaded with `library()` in a
fresh `bin/Rscript`. Logs go to `.corpus/ry-install/logs/<package>.{install,load}.log`, and
`.corpus/ry-install/summary.tsv` keeps one row per package: a run replaces the rows of the
packages it names and keeps the others.

The R home holds a copy of `libR.so`, so after rebuilding run `just ry-home` again before
installing. `oracle packages --installed` runs ry's side through `.corpus/ry-home/bin/Rscript`
with `.corpus/ry-library` ahead of `.corpus/library` (which still supplies the `Suggests`), and
records results under `ry-installed`, beside the `ry` results of `ry run --runtime ry`, with the
table in `.corpus/tests/summary-installed.md`.

GNU R's binaries of graphics devices refuse ry's graphics engine, which is one version newer than
the one R 4.3.3 built them against, so a package whose tests plot through svglite needs svglite
installed on ry: `just corpus-ry-install svglite` builds it from source.

## Running against another GNU R

The harness uses whichever GNU R is first on `PATH`. Two environment variables point it elsewhere,
which is how to compare against a newer R installed beside the system one, for example with
[rig](https://github.com/r-lib/rig) (`rig add 4.6` installs under `/opt/R/<version>`):

```sh
ORACLE_R=/opt/R/4.6.2/bin/R just oracle verify            # the single-file cases
ORACLE_RSCRIPT=/opt/R/4.6.2/bin/Rscript just oracle packages   # the package corpus
```

`ORACLE_R` covers `verify`, `record` and `compare`. `ORACLE_RSCRIPT` covers `packages` and `bench`.
When `ORACLE_R` is set, the `ry` engine of `compare` also runs with `R_HOME` set to that R's home,
so ry's runtime loads the same base library and emulates the same version as the GNU R it is
compared with. That makes `compare` the check for a new R release:

```sh
ORACLE_R=/opt/R/4.6.1/bin/R just oracle compare --engine ry
```

On the 125 cases the suite held when it was last run under R 4.6.1, `compare` passed 118 of them,
against 124 of 125 under 4.3.3 at the time (today 149 of 150 pass under 4.3.3). Four of the
remaining differences come from how that GNU R was built: OpenBLAS changes the last digits of `lm`
and `arima` results, glibc's `mktime` resolves a repeated wall-clock time differently after it has
seen a time in a spring-forward gap, and the sign of `NA^NaN` depends on the compiler. One case
calls `C_pKS2`, a routine R 4.6 removed; GNU R 4.6 fails it, while ry keeps every version's routines
and runs it. `stats__lbfgsb` differs under every version.

The recorded `.expected` files come from R 4.3.3, so a newer R legitimately differs under
`verify`. Record into a separate checkout, or keep the 4.3.3 output as the contract. Installing
rig puts its `R` ahead of the system one on `PATH`; remove the `R` and `Rscript` links in
`/usr/local/bin` to keep the system R as the default.

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
count (by default one file per 600 lines). `--typed` keeps the templates' `#:` annotations and
turns on strict mode in the generated `ry.toml`, so the package doubles as a strict-mode dogfood
corpus; without it the annotations are stripped and the package is plain R. The line target counts
code without annotations, so a typed and an untyped run with the same seed generate the same program. `--only` limits
generation to the named templates, which is the quick way to iterate on one of them.

The code comes from the R templates in `tools/bench/templates/` (see its `README.md`): shared
infrastructure plus domain code, instantiated with random parameters until the line target is
reached. Services that cannot run in a test (Azure, Bloomberg, ODBC) take an injectable client,
so the tests never touch a network. `just bench-test` runs a package's tests against the corpus library, so run `just corpus` first.
Because `polars` and `Rblpapi` cannot be installed, it tests a copy whose `Imports` omit the
missing packages; the code reaches them only inside guarded or injectable branches. The three
10,000-line packages pass all of their roughly 340 tests each under GNU R 4.3.3.

## Benchmark pass rate

```sh
just bench-compare .bench/finbench .bench/dsbench .bench/biobench
just bench-compare .bench/finbench --filter portfolio --verbose
```

`oracle bench` runs a generated package's testthat suite under GNU R and under ry's runtime and
compares the two test by test. Both runs go through `ry load <package> --file bench.R`, once with
`--runtime gnu-r` and once with `--runtime ry`, so the same loader plan loads the package on both
sides and only evaluation differs. The driver (`crates/oracle/src/bench.R`) records every
`test_that()` block: its status, the failed expectations, errors, warnings and skips it reported,
a deparse of its value, and its run time. A test matches when everything but the time agrees.

GNU R runs the suite twice. A test whose value or problems differ between those two runs (a
fresh UUID, a timestamp in a message) is compared by status alone, so only what GNU R reproduces
counts against ry. The table prints,
per package, the matching tests, the rate, the tests that differ or that ry never reported, the
tests ry passes, and both suites' run times; process wall time, which adds startup and loading
the package, follows it. `--json` writes every comparison, and `--keep` keeps each runtime's
report and stderr. Pass `--ry target/release/ry` for timings, with `libR.so` built beside it
(`cargo build --release -p ry-lang -p libr`). `MEMORY.md` records the current rates.

The two time ratios answer different questions. Suite time adds up the `test_that()` blocks, so
it measures evaluation. Process wall time also counts what happens between blocks, and at the full
benchmark size (`--lines 300000 --files 500`) that is dominated by testthat calling `gc()` after
every test file that registered a teardown. A full collection with the whole package loaded takes
about 600 ms on GNU R and about 100 ms on ry, so ry can trail on suite time and still beat GNU R
end to end.

