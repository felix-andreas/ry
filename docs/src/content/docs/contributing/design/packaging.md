---
type: Reference
title: Packages and R versions in the runtime
description: "Proposal, not implemented: how packages install into ry's runtime, how ry picks GNU R or its own runtime and an R version, and what the loader and the corpus need"
tags: [packaging, runtime, r-versions, abi, lazy-loading]
status: draft
generated: { by: ry-research-agent/1, at: 2026-10-31T17:00:00Z }
sources:
  - id: rv
    resource: https://github.com/A2-ai/rv
    title: "rv repository (v0.23.3): README.md, src/sync/link.rs, src/r_finder.rs, src/renv.rs"
    last_modified: 2026-10-21T13:09:02-04:00
  - id: rig
    resource: https://github.com/r-lib/rig
    title: "rig repository (0.11.2-dev): README.md, website/proj-guide.qmd, website/admin-vs-user-mode.qmd"
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
the corpus setup need from an installed library. A package manager of ry's own (`ry add`, `ry sync`,
a lockfile format) is a product question the runtime does not need answered, so it stays open.
Nothing here is implemented; the measurements are real.

## The decisions

- **An installed package is the same directory for GNU R 4.6 and for ry's runtime.** ry's `libR` is
  ABI-compatible and presents as R 4.6, so a binary built for GNU R 4.6 installs unchanged and loads
  under both. Existing libraries, renv's project libraries included, work as they are.
- **ry's per-package data lives beside the library, never inside a package.** The item index and the
  bytecode the loader needs go into a cache keyed by the installed package's content, filled the first
  time ry's runtime loads the package. GNU R keeps loading exactly the files that were installed.
- **Binaries are the default install path, and source builds use `R CMD INSTALL` semantics under
  ry.** Compiling the benchmark closure from source took MEASURE_SRC, against 30 s for binaries, so
  the runtime is only usable if prebuilt binaries work.
- **Package management runs as R code at first.** `install.packages`, renv and pak are R code, and
  ry's runtime runs them like any other package, which keeps every existing workflow working. ry adds
  no installer of its own for the runtime's sake.
- **The runtime is chosen explicitly,** by `[r] runtime = "gnu" | "ry"` in `ry.toml`, `--runtime`, or
  `RY_RUNTIME`. GNU R is the default until ry's runtime passes the benchmark, then ry. There is no
  silent fallback, and an R version other than 4.6 always means GNU R.
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
  whose `variables` element names every object in the namespace (`cli`'s names 638), so ry can list a
  namespace without evaluating anything. GNU R bytecode inside the database is ignored and the
  closures are recompiled, as the runtime design already plans.
- **The R version a binary was built for** must be 4.6.x. The minor version is R's binary
  compatibility boundary, and the installed `DESCRIPTION` records it (`Built: R 4.6.0;
  x86_64-pc-linux-gnu; ...`), so ry checks that field when it loads a package and refuses one built
  for another minor version with a message that names both versions.

On Linux, binaries come from P3M, which serves distribution-specific builds and `manylinux_2_28`
builds that run on any glibc 2.28 system.[^p3m] The `manylinux` builds suit ry, whose release binary
already targets an old glibc baseline. P3M picks a binary by the request's `User-Agent` and serves
source otherwise, so ry's runtime must send the `User-Agent` R 4.6 sends.

### Source packages

ry's runtime builds a source package with `R CMD INSTALL` semantics: `configure`, `Makevars`, and the
compiler flags that `R CMD config` reports. The headers are ry's own R 4.6-compatible set, and
`R CMD config` reports ry's `libR`, so a package compiled under ry links against ry. Because the ABI is
the same, the result is again a valid GNU R 4.6 binary, and whichever runtime built a package, both
can load it.

### ry's per-package cache

The runtime needs more per package than GNU R installs: the item index the lazy loader reads and the
bytecode it compiles. That data goes into ry's own cache directory, keyed by a hash of the installed
package directory and of ry's runtime version, and it is built on the first load under ry's runtime.
It stays out of the package directory for two reasons. The differential oracle depends on both
runtimes loading byte-identical packages. And a library is often shared (a site library, renv's
global cache), so writing into it is not ry's to do. Because the key is the content, two projects
that use the same installed package share one entry, and a package that `install.packages` replaces
simply misses the cache.

## Choosing the runtime and the R version

Two settings decide what `ry run`, `ry repl` and their worker processes start: the R version and the
runtime. Each comes from, in order, a command-line flag (`--r 4.5`, `--runtime ry`), the environment
(`RY_R`, `RY_RUNTIME`), the `[r]` table of `ry.toml`, and, for the version only, the project's own
metadata: the R version recorded in `renv.lock`, and `Depends: R (>= ...)` as a lower bound.

```toml
[r]
version = "4.6"
runtime = "gnu"
```

- **GNU R** is found where rv looks for it: `PATH`, rig's install roots, `/opt/R`, the macOS
  framework and the Windows registry.[^rv] ry does not install R itself in this design, because rig
  already does that well and ry can read its layout.[^rig] Without any setting, ry keeps today's
  behavior: `R_HOME`, then `R RHOME`.
- **ry's runtime** implements one R version, the one it presents (4.6). A request it cannot satisfy,
  such as `version = "4.4"`, selects GNU R and says why once. The runtime still needs a GNU R 4.6
  installation on disk, because it loads base R's R sources from one instead of vendoring them (the
  runtime licensing decision in `.agents/memory/decisions.md`).

The default runtime is GNU R until ry's runtime meets the exit criterion of build step 4 (all 232
closure packages pass their own tests, and the three benchmark code bases match GNU R's output), and
then it flips to ry. ry never falls back from one runtime to the other on its own: a process runs on
one runtime, a silent switch would hide exactly the failures the transition has to surface, and
`runtime = "gnu"` is a one-line escape hatch.

Worker processes follow their parent. `parallel` starts workers with `file.path(R.home("bin"),
"Rscript")` and `callr` with `R.home()`'s `bin/R`, so ry's runtime ships an `R_HOME` whose `bin/R` and
`bin/Rscript` start ry, and a worker runs on the runtime of the session that started it.

## What the lazy loader and the analysis read

The lazy loader ([runtime design](/contributing/design/r-runtime/), section 6) takes its library paths
from the selected runtime: the same `.libPaths()` that `R_LIBS`, `.Rprofile` and renv's `activate.R`
set up, because ry runs those as R would. A dependency loads from its installed directory plus its
cache entry, so a session pays only for the packages and objects it touches. The project's own
package is never installed; it loads item by item from the analysis index.

The analysis can read the same library without a running R, which fixes a known weakness: the export
manifests in `types/` come from one live R session, and a manifest from an older R turns real exports
into false `unresolved` findings. Instead, for each package the checker reads the installed
`DESCRIPTION` (the version), `NAMESPACE` (the export rules) and `.rdx` (the object names that an
`exportPattern` matches). Each is a salsa input per package name, so reinstalling one package
invalidates only the items that use it, as the [architecture](/contributing/architecture/) requires.
Stubs gain a version range, and the checker uses the stub whose range contains the installed version;
outside every range, the package's exports are still known, with untyped signatures. The generated
manifests stay for base R and for projects with no library.

## The corpus

Build step 1 installs the benchmark closure into a GNU R library for the differential oracle. It is
pinned with an `renv.lock` against a dated P3M snapshot, so every rerun installs the same versions.
renv is the tool because the benchmark depends on it, so the corpus exercises the same code ry's
runtime must later run, and because it needs nothing new. The four packages P3M does not carry
(`azidentity`, `esgtoolkit`, `loadfast` and `polars`) come from the GitHub remotes and the extra
repository that the benchmark declares.

## Where each piece lands

The steps are those of the [runtime design](/contributing/design/r-runtime/), section 10.

| Step | Work |
| --- | --- |
| 1. Corpus and oracle | The closure's `renv.lock` on a dated P3M snapshot; GNU R discovery and the `[r]` settings. |
| 1, directly after | The analysis reads installed versions, `NAMESPACE` and `.rdx`; stubs gain version ranges. |
| 4. Full C API and packages | Binary loading under ry with the `Built:` check, `R CMD INSTALL` under ry, ry's `R_HOME` with `bin/R` and `bin/Rscript`, and `install.packages`, renv and pak running under ry. At the exit criterion the default runtime flips. |
| 5. Lazy loading | The per-package cache with the item index and bytecode, filled on first load. |

## What was measured

Both runs installed the 228 closure packages that P3M carries into an empty library, with 4 parallel
jobs, on R 4.6.1 and Ubuntu 24.04 in a 4-core container.

| Method | Wall time |
| --- | --- |
| `install.packages`, P3M Ubuntu 24.04 binaries | 30 s, 515 MB installed |
| `install.packages`, CRAN source tarballs | MEASURE_SRC |

Two further observations shaped the decisions. P3M's binary index has no checksum field (only
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
| rpx | `DESCRIPTION` as the manifest, PubGrub over version history[^rpx] | Nothing yet; relevant to a future package manager |
| uv | One tool for interpreter, lockfile, cache and runner[^uv] | The target experience, if ry adds a package manager |

Other tools do not affect the runtime. groundhog and packrat (renv's predecessor) are R code, which
ry runs like any package. conda, pixi, Nix's `rPackages`, rix, r2u and bspm deliver R and packages
through a system package manager, and the runtime meets their result as an ordinary GNU R
installation and library tree.

rv, rig and rpx were studied from their sources, because this container's policy blocked running
their release binaries, so none of their speed is measured here.

## Open questions

- **A package manager of ry's own.** Whether ry ships `ry add`, `ry sync` and a lockfile in the spirit
  of uv is undecided and does not block the runtime. If it does, it must read and write `renv.lock`
  for existing projects, and since binary indexes carry no checksums it must hash artifacts itself.
- **Windows and macOS** binaries follow the same rules, but their `libR` naming and loader details
  belong to the runtime's Windows open question.

## About this page's format

Like [prior art](/contributing/design/prior-art/), this page is an
[Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format) v0.2 concept
document: the frontmatter carries the sources, and each claim about a tool cites its source with a
footnote that also carries the link.

[^rv]: rv repository (v0.23.3): README.md, src/sync/link.rs, src/r_finder.rs, src/renv.rs, <https://github.com/A2-ai/rv>
[^rig]: rig repository (0.11.2-dev): README.md, website/proj-guide.qmd, website/admin-vs-user-mode.qmd, <https://github.com/r-lib/rig>
[^rpx]: rpx repository (2.1.1): README.md, docs/05.manage-dependencies.md, <https://github.com/rrepo-org/rpx>
[^renv]: renv 1.3.1, <https://rstudio.github.io/renv/>
[^pak]: pak 0.11.1 and its bundled pkgdepends, <https://pak.r-lib.org/>
[^p3m]: Posit Package Manager, <https://packagemanager.posit.co/>
[^uv]: uv documentation, <https://docs.astral.sh/uv/>
