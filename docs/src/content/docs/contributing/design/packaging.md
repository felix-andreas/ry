---
type: Reference
title: Packages and R versions in the runtime
description: "Proposal, partly built: how packages install into ry's runtime, how ry picks GNU R or its own runtime and an R version, and what the loader and the corpus need"
tags: [packaging, runtime, r-versions, abi, lazy-loading]
status: draft
generated: { by: ry-research-agent/1, at: 2026-10-31T17:00:00Z }
sources:
  - id: rv
    resource: https://github.com/A2-ai/rv
    title: "rv repository (v0.23.3): README.md, src/r_finder.rs"
    last_modified: 2026-10-21T13:09:02-04:00
  - id: rig
    resource: https://github.com/r-lib/rig
    title: "rig repository (0.11.2-dev): README.md, website/proj-guide.qmd, website/admin-vs-user-mode.qmd, src/repos/cranlike_metadata.rs"
    last_modified: 2026-10-29T09:04:04+01:00
  - id: rpx
    resource: https://github.com/rrepo-org/rpx
    title: "rpx repository (2.1.1): README.md, docs/05.manage-dependencies.md"
    last_modified: 2026-10-20T18:44:45+02:00
  - id: renv
    resource: https://rstudio.github.io/renv/
    title: "renv 1.3.1"
  - id: pak
    resource: https://pak.r-lib.org/
    title: "pak 0.11.1 and its bundled pkgdepends"
  - id: p3m
    resource: https://packagemanager.posit.co/
    title: Posit Package Manager (P3M)
  - id: uv
    resource: https://docs.astral.sh/uv/
    title: uv documentation
---

This page covers the part of packaging that the [runtime](/contributing/design/r-runtime/) depends
on: how a package gets installed so that ry's runtime can load it, how ry chooses between GNU R and
its own runtime and between R versions while the runtime is incomplete, and what the lazy loader and
the corpus need from an installed library. The corpus is the benchmark's 232-package dependency
closure installed into a GNU R library, and the differential oracle is the harness that runs the same
code under GNU R and under ry and compares the results (both from build step 1 of the runtime
design). A package manager of ry's own (`ry add`, `ry sync`, a lockfile format) is a product question
the runtime does not need answered, so it stays open. Only two pieces exist: `crates/rcmd` installs
a package against ry's R home with `R CMD INSTALL` semantics, and the command picks the runtime
explicitly: `ry run` and `ry load` run on ry's runtime, `ry R` on GNU R. The rest is a proposal; the measurements are real.

## The decisions

- **An installed package is the same directory for GNU R 4.6 and for ry's runtime.** ry's `libR` is
  ABI-compatible and presents as R 4.6, so a binary built for GNU R 4.6 installs unchanged and loads
  under both. Existing libraries, renv's project libraries included, work as they are.
- **ry's per-package data lives in ry's own cache, never inside a package.** The object index and the
  bytecode the loader needs are built the first time ry's runtime loads the package, keyed by the
  installed package's identity. GNU R keeps loading exactly the files that were installed.
- **Binaries are the default install path, and source builds use `R CMD INSTALL` semantics under
  ry.** Compiling the benchmark closure from source took 17 minutes, against 30 s for binaries, so
  the runtime is only usable if prebuilt binaries work.
- **Package management runs as R code at first.** `install.packages`, renv and pak are R code, and
  ry's runtime runs them like any other package, which keeps every existing workflow working. ry adds
  no installer of its own for the runtime's sake.
- **The runtime is chosen explicitly, never by failure.** The command names it: `ry run` is ry's
  runtime and `ry R` is GNU R (decision record "ry run and ry R"). The hidden `--runtime` flag and
  `RY_RUNTIME` remain for older scripts. An explicitly requested R version other than 4.6 means GNU
  R; whether a version request should live on `ry R` alone is open.
- **The corpus is pinned with renv**, which the benchmark already depends on, against a dated P3M
  snapshot, and installed under GNU R in build step 1.

## Installing into ry's runtime

### Binary packages

An installed binary package is a directory: `DESCRIPTION`, `NAMESPACE`, the lazy-load database
`R/<pkg>.rdb` with its index `R/<pkg>.rdx`, serialized metadata in `Meta/*.rds`, and
`libs/<pkg>.so` (`.dylib`, `.dll`) compiled against R's headers. ry's runtime loads each piece as GNU R
does:

- **The shared library** resolves its R API symbols against `libR`, which is what the ABI-compatible
  `libR` exists for ([runtime design](/contributing/design/r-runtime/), section 2).
- **The lazy-load database** is read with ry's own RDS reader. The `.rdx` index is a plain RDS file
  whose `variables` element names every object in the namespace, internal ones included (`cli`'s
  names 638), so ry can list a namespace without evaluating anything. GNU R bytecode inside the
  database is ignored and the closures are recompiled, as the runtime design already plans. Lazy data
  (`data/Rdata.rdx`) uses the same format.
- **The R version a binary was built for** is recorded in the installed `DESCRIPTION` (`Built: R
  4.6.0; x86_64-pc-linux-gnu; ...`). Packages are conventionally reinstalled across R minor versions,
  but GNU R only refuses packages built before R 3.0.0 and loads, for example, a 4.5 build under 4.6.
  ry behaves the same way, so an existing library works under ry exactly as it does under GNU R 4.6.

On Linux, binaries come from P3M, which serves distribution-specific builds and, according to rig's
documentation, `manylinux_2_28` builds that run on any glibc 2.28 system.[^p3m][^rig] The `manylinux`
builds suit ry, whose release binary already targets an old glibc baseline. P3M picks a binary by the
request's `User-Agent` and serves source otherwise. `install.packages` builds that header from
`R.version`, so a runtime that presents as R 4.6 gets R 4.6 binaries with no extra mechanism.

### Source packages

ry's runtime builds a source package with `R CMD INSTALL` semantics: `configure`, `Makevars`, and the
compiler flags that `R CMD config` reports. The headers are ry's own R 4.6-compatible set, and
`R CMD config` reports ry's `libR`, so a package compiled under ry links against ry. ry's headers are
intended to be ABI-identical to GNU R's, inline functions and macros included, so the result is
intended to load under GNU R too. Until the oracle has shown that, a package built under ry is marked
with an extra `DESCRIPTION` field (`RyBuilt:` with ry's version), and the corpus is built only by GNU
R, so a difference the oracle finds is never a build difference.

### ry's per-package cache

The runtime needs more per package than GNU R installs. For an installed package, which ships no R
sources, the object index is the `.rdx` variable list with each closure's location in the `.rdb`, and
the bytecode is compiled from those closures. Both go into ry's own cache directory and are built on
the first load under ry's runtime. The cache key is the identity of the installation, not a hash of
the whole directory, because hashing every file on every load would cost what the cache saves: it is
a hash of the installed `DESCRIPTION` (whose `Built:` timestamp changes on every reinstall), the
`.rdx` file, and ry's runtime version. All three are small files.

The data stays out of the package directory for two reasons. The differential oracle depends on both
runtimes loading byte-identical packages. And a library is often shared (a site library, renv's
global cache), so writing into it is not ry's to do. Two projects that use the same installation
share one entry, and a package that `install.packages` replaces simply misses the cache. Until the
cache exists (build step 5), ry's runtime loads an installed package the way GNU R does, from its
`.rdb` through promises.

## Choosing the runtime and the R version

This section predates the split into `ry run` (ry's runtime) and `ry R` (GNU R), which settled the
runtime half: the command chooses it, and a `[r] runtime` key would only reintroduce a hidden switch.
The version half is still open. Two settings decide what a session and its worker processes start:
the runtime and the R version. Each comes from, in order, a command-line flag (`--runtime ry`, `--r 4.5`), the environment
(`RY_RUNTIME`, `RY_R`), or the `[r]` table of `ry.toml`:

```toml
[r]
runtime = "gnu"
version = "4.6"
```

A version written as a minor (`"4.5"`) matches any 4.5.x, and a full version (`"4.5.2"`) matches only
that patch. The runtime is chosen like this:

- **With an explicit runtime,** that runtime runs. `runtime = "ry"` together with a version other than
  4.6 is an error, because ry's runtime implements exactly one R version, the one it presents.
- **With no explicit runtime,** the default applies, except that an explicit version other than 4.6
  selects GNU R, because only GNU R can provide it. The default is GNU R until ry's runtime meets the
  exit criterion of build step 4 (all 232 closure packages pass their own tests, and the three
  benchmark code bases match GNU R's output), and ry after that.
- **Project metadata is advisory.** The R version recorded in `renv.lock` and `Depends: R (>= ...)`
  never select a runtime or a version. ry warns when the running R's minor version differs from
  `renv.lock`'s, as renv does, and reports an unmet `Depends` as R does when the package loads.
  Otherwise nearly every existing renv project, which records an older R, would stay on GNU R after
  the default flips.

ry never switches runtime because something failed. A process runs on one runtime, a switch on
failure would hide exactly the failures the transition has to surface, and `runtime = "gnu"` is a
one-line escape hatch.

GNU R is located in this order. `R_HOME`, when set, wins, as it does today. With a requested version,
ry searches where rv does (rig's install roots and quick links, `/opt/R`, Homebrew,
`/Library/Frameworks/R.framework/Versions`, and `C:\Program Files\R`) and takes the newest
installation that matches.[^rv] Otherwise ry keeps today's behavior and asks the `R` on `PATH`
(`R RHOME`). ry does not install R itself in this design, because rig already does that well and ry
can read its layout.[^rig]

ry's runtime still needs a GNU R 4.6 installation on disk, because it loads base R's R sources from
one instead of vendoring them (the runtime licensing decision in `.agents/memory/decisions.md`). So
ry's runtime has an `R_HOME` of its own, assembled like this:

- `bin/R` and `bin/Rscript` (on Windows also `bin/<arch>/Rterm.exe` and `Rscript.exe`) start ry, so
  worker processes run on the runtime of the session that started them. `parallel` starts workers
  with `file.path(R.home("bin"), "Rscript")`, and `callr` starts `R.home()`'s `bin/R` (or `Rterm` on
  Windows).
- `etc/` holds ry's `Makeconf` and `Renviron`, and `include/` holds ry's headers, so source builds
  compile and link against ry.
- `library/` links the base packages (`base`, `stats`, `utils`, `methods`, `tools`, `parallel` and the
  rest) to the GNU R 4.6 installation, and `share/` links to that installation's `share/`.

## What the lazy loader and the analysis read

The lazy loader ([runtime design](/contributing/design/r-runtime/), section 6) takes its library paths
from the selected runtime: the same `.libPaths()` that `R_LIBS`, `.Rprofile` and renv's `activate.R`
set up, because ry runs those as R would. A dependency loads from its installed directory plus its
cache entry, so a session pays only for the packages and objects it touches. The project's own
package is never installed; it loads item by item from the analysis index.

The analysis reads installed packages too, which fixes a known weakness: the export manifests in
`types/` come from one live R session, and a manifest from an older R turns real exports into false
`unresolved` findings. The analysis never runs R, so it cannot evaluate `.Rprofile` or renv's
`activate.R`, and it finds the library by a fixed rule instead. The first of these that applies gives
the library paths:

1. `[r] libraries` in `ry.toml`, a list of directories.
2. When the project has `renv.lock`, renv's project library, `renv/library/<platform prefix>/`.
3. `R_LIBS` and `R_LIBS_USER`, then the selected R's site and default libraries.

As in R, the first library that holds a package wins. Paths that only `.Rprofile` computes are not
followed, so a project that sets its library that way names it in `[r] libraries`.

For each package the checker reads the installed `DESCRIPTION` (the version), `NAMESPACE` (the export
rules) and `.rdx` (the object names that an `exportPattern` matches). The host watches the library
directories and sets one salsa input per installed package, and a check asks for a package through a
tracked lookup by name that returns nothing when the package is absent. No check reads a listing of
the whole library, so reinstalling one package invalidates only the items that use it, as the
[architecture](/contributing/architecture/) requires, and `crates/semantics/tests/test_incremental.rs`
pins that once it is built. Stubs gain a version range, and the checker uses the stub whose range
contains the installed version; outside every range, the package's exports are still known, with
untyped signatures. The generated manifests stay for base R and for projects with no library.

## The corpus

Build step 1 installs the benchmark closure into a GNU R library for the differential oracle. It is
pinned with an `renv.lock` against a dated P3M snapshot, so every rerun installs the same versions.
renv is the tool because the benchmark depends on it, so the corpus exercises the same code ry's
runtime must later run, and because it needs nothing new.

Four packages do not come as binaries, so the corpus needs build toolchains. The extra repository
(`ak-finccam.github.io/r-packages`) serves only source packages, and two of them, `polars` and
`azidentity`, are written in Rust, so the corpus machine needs a Rust toolchain and the longest
builds of the closure are theirs. `esgtoolkit` (Rcpp) and `loadfast` build from their GitHub
remotes. The binary speedup measured below does not cover these four.

## Where each piece lands

The steps are those of the [runtime design](/contributing/design/r-runtime/), section 10.

| Step | Work |
| --- | --- |
| 1. Corpus and oracle | The closure's `renv.lock` on a dated P3M snapshot, with Rust and C++ toolchains for the four source-only packages; GNU R discovery and the `[r]` settings. |
| 1, directly after | The analysis finds the library by the fixed rule and reads installed versions, `NAMESPACE` and `.rdx`; stubs gain version ranges. |
| 4. Full C API and packages | Binary loading under ry from `.rdb`, `R CMD INSTALL` under ry with the `RyBuilt:` mark, ry's `R_HOME`, and `install.packages`, renv and pak running under ry. At the exit criterion the default runtime flips. |
| 5. Lazy loading | The per-package cache with the object index and bytecode, filled on first load. |

## What was measured

Both runs installed the 228 closure packages that P3M carries into an empty library, with 4 parallel
jobs, on R 4.6.1 and Ubuntu 24.04 in a 4-core container.

| Method | Wall time |
| --- | --- |
| `install.packages`, P3M Ubuntu 24.04 binaries | 30 s, 515 MB installed |
| `install.packages`, CRAN source tarballs | 17 min (1016 s), 227 of 228 installed |

The source build is about 34 times slower, and one package failed outright: `Rblpapi` links
against Bloomberg's `blpapi3` library, which its source build could not provide here, while the P3M
binary ships that library in its `blp/` directory and installed cleanly. Two further observations shaped the decisions. P3M's binary index has no checksum field (only
`Package`, `Version`, the dependency fields, `License`, `NeedsCompilation` and `SystemRequirements`),
while CRAN's source index has an `MD5sum` per tarball, so verifying a binary means hashing it
yourself. And on Ubuntu's R 4.3.3, `install.packages` could not install `MASS`, `Matrix` or `mgcv`,
because the current version of each needs a newer R and R's installer never looks for an older one,
which is why the corpus is pinned rather than installed from a moving repository.

## The tools studied

These are inspiration, not the bar, and the table records only what bears on the runtime.

| Tool | What it does | What ry takes |
| --- | --- | --- |
| renv | Records a library in `renv.lock`, restores it, links projects to a global cache[^renv] | Runs as a package; pins the corpus |
| pak | Solves the whole request first (with `lpSolve`), downloads in parallel, installs system requirements[^pak] | Runs as a package |
| rv | Declarative `rproject.toml` and `rv.lock`, cache hard-linked into libraries, finds installed R versions[^rv] | Where to look for GNU R |
| rig | Installs and switches R versions, including portable builds, and has a PubGrub project solver[^rig] | Its install layout, read as is |
| rpx | `DESCRIPTION` as the manifest, PubGrub over version history from the rrepo API[^rpx] | Nothing yet; relevant to a future package manager |
| uv | One tool for interpreter, lockfile, cache and runner[^uv] | The target experience, if ry adds a package manager |

Other tools do not affect the runtime. groundhog, packrat (renv's predecessor) and bspm (which
redirects `install.packages` to the system package manager) are R code, which ry runs like any
package. conda, pixi, Nix's `rPackages`, rix and r2u deliver R and packages through a system package
manager, and the runtime meets their result as an ordinary GNU R installation and library tree.

rv, rig and rpx were studied from their sources, because this container's policy blocked running
their release binaries, so none of their speed is measured here.

## Open questions

- **A package manager of ry's own.** Whether ry ships `ry add`, `ry sync` and a lockfile in the spirit
  of uv is undecided and does not block the runtime. If it does, it must read and write `renv.lock`
  for existing projects, and since binary indexes carry no checksums it must hash artifacts itself.
  Resolving over CRAN's history needs metadata for archived versions, which rpx gets from the rrepo
  API and rig from an index of all versions with source hashes.[^rig]
- **Windows and macOS** binaries follow the same rules, but their `libR` naming and loader details
  belong to the runtime's Windows open question.

## About this page's format

Like [prior art](/contributing/design/prior-art/), this page is an
[Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format) v0.2 concept
document: the frontmatter carries the sources, and each claim about a tool cites its source with a
footnote that also carries the link.

[^rv]: rv repository (v0.23.3): README.md, src/r_finder.rs, <https://github.com/A2-ai/rv>
[^rig]: rig repository (0.11.2-dev): README.md, website/proj-guide.qmd, website/admin-vs-user-mode.qmd, src/repos/cranlike_metadata.rs, <https://github.com/r-lib/rig>
[^rpx]: rpx repository (2.1.1): README.md, docs/05.manage-dependencies.md, <https://github.com/rrepo-org/rpx>
[^renv]: renv 1.3.1, <https://rstudio.github.io/renv/>
[^pak]: pak 0.11.1 and its bundled pkgdepends, <https://pak.r-lib.org/>
[^p3m]: Posit Package Manager, <https://packagemanager.posit.co/>
[^uv]: uv documentation, <https://docs.astral.sh/uv/>
