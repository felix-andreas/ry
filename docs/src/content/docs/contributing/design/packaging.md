---
type: Reference
title: Packaging, R versions and reproducible environments
description: "Proposal, not implemented: how ry installs packages, pins environments, picks an R version and runtime, and feeds installed versions to the checker"
tags: [packaging, lockfile, resolver, r-versions, runtime, reproducibility]
status: draft
generated: { by: ry-research-agent/1, at: 2026-10-31T17:00:00Z }
sources:
  - id: rv
    resource: https://github.com/A2-ai/rv
    title: "rv repository (v0.23.3): README.md, src/resolver, src/sync/link.rs, src/renv.rs, src/r_finder.rs, src/sandbox.rs"
    last_modified: 2026-10-21T13:09:02-04:00
  - id: rv-docs
    resource: https://a2-ai.github.io/rv-docs/
    title: rv documentation
  - id: rig
    resource: https://github.com/r-lib/rig
    title: "rig repository (0.11.2-dev): README.md, NEWS.md, website/proj-guide.qmd, website/scripts.qmd, website/admin-vs-user-mode.qmd, src/solver.rs"
    last_modified: 2026-10-29T09:04:04+01:00
  - id: rpx
    resource: https://github.com/rrepo-org/rpx
    title: "rpx repository (2.1.1): README.md, docs/05.manage-dependencies.md, docs/sync-engine.md, benchmarks/README.md"
    last_modified: 2026-10-20T18:44:45+02:00
  - id: renv
    resource: https://rstudio.github.io/renv/
    title: "renv 1.3.1 documentation and package sources"
  - id: pak
    resource: https://pak.r-lib.org/
    title: "pak 0.11.1 documentation and its bundled pkgdepends"
  - id: p3m
    resource: https://packagemanager.posit.co/
    title: Posit Package Manager (P3M)
  - id: runiverse
    resource: https://r-universe.dev/
    title: r-universe
  - id: uv
    resource: https://docs.astral.sh/uv/
    title: uv documentation
---

This page decides how ry installs R packages, records an environment so it can be recreated exactly,
chooses an R version, and chooses between GNU R and ry's own runtime while that runtime is
incomplete. The bar is uv's experience for Python: one fast binary that manages the interpreter, the
packages and the lockfile, and keeps the environment in sync without being asked. The constraint is
the mandate: existing R projects, and renv projects in particular, keep working unchanged. Nothing
on this page is implemented; the measurements are real and were taken in a 4-core Linux container.

## The decisions

- **ry ships its own package manager, inside the `ry` binary.** The commands are `ry add`,
  `ry remove`, `ry lock`, `ry sync` and `ry run`, plus `ry r install` and `ry r list` for R itself.
  None of the existing tools is embedded, because the installer has to write ry's runtime sidecars
  and the resolved set has to be a salsa input to the checker, and no existing tool does either. The
  design borrows heavily: PubGrub resolution as in rig and rpx, a global cache linked into project
  libraries as in uv and rv, `DESCRIPTION` as the manifest as in rpx, and rig's `# /// script`
  block for single-file scripts.
- **The lockfile follows the project.** A project with a `renv.lock` stays an renv project: `ry sync`
  restores it, `ry lock` rewrites it in renv's format, and colleagues who still run renv see no
  difference. A new project gets `ry.lock`, whose format can say what `renv.lock` cannot (a resolution
  valid on several platforms, and the hash of every artifact). `ry lock --format renv` converts either
  way.
- **Packages install from prebuilt GNU R binaries** (P3M, r-universe) into one content-addressed
  cache, and the cache serves both GNU R 4.6 and ry's runtime. The runtime presents as R 4.6 with an
  ABI-compatible `libR` ([runtime design](/contributing/design/r-runtime/)), so the same installed
  binary is valid under both, which is also what makes the differential oracle possible.
- **`ry.toml` names the R version and the runtime:** `[r] version = "4.6"` and
  `runtime = "gnu" | "ry"`. The default runtime is GNU R until ry's runtime passes the benchmark, and
  then it flips to ry. A requested version other than 4.6 always means GNU R, because ry's runtime
  implements exactly one R version.
- **The lockfile is an analysis input.** The checker reads exact installed versions per package,
  takes exports from the installed `NAMESPACE` instead of a hand-generated manifest, and picks the
  stub whose version range contains the locked version.
- **Packaging lands early.** Locking, syncing and R discovery for GNU R land with the corpus and
  oracle (build step 1), because the oracle needs a pinned closure. Analysis integration follows
  directly. Installing into ry's runtime lands with the full C API (step 4), and the lazy loader
  reads the synced environment (step 5).

## What was measured

All timings install the benchmark closure: the 228 of its 232 packages that P3M carries, into an
empty library, with 4 parallel jobs, on R 4.6.1 and Ubuntu 24.04. The other four (`azidentity`,
`esgtoolkit`, `loadfast` and `polars`) come from GitHub or the extra repository and were left out of
every run so the comparison stays like for like.

| Method | Wall time | Notes |
| --- | --- | --- |
| `install.packages`, P3M Ubuntu 24.04 binaries | 30 s | 515 MB installed, all 228 present |
| `install.packages`, CRAN source tarballs | MEASURE_SRC | compiles 121 packages |
| `pak::pkg_install`, P3M binaries | MEASURE_PAK | |
| `renv::restore`, P3M binaries, empty cache | MEASURE_RENV_COLD | |
| `renv::restore`, warm cache | MEASURE_RENV_WARM | links from renv's cache |
| rv, rig, rpx | not measured | see below |

Three findings matter more than the exact numbers:

- **Binaries are the whole game.** Installing from source costs MEASURE_RATIO what installing
  binaries costs, and the time is compiling `stringi`, the Armadillo and Boost users and the solver
  packages. That is the reason the runtime design insists on ABI compatibility.
- **R's own installer cannot backtrack.** On Ubuntu's R 4.3.3, `install.packages` refused `MASS`,
  `Matrix` and `mgcv`, because the current version of each needs a newer R and a CRAN index lists
  only current versions ("'MASS' version 7.3-66 is in the repositories but depends on R (>= 4.4.0)").
  An older compatible version exists in CRAN's archive, and a resolver that knows version history
  would have picked it.
- **Binary indexes carry no checksums.** CRAN's source index has an `MD5sum` per tarball. P3M's
  binary index has `Package`, `Version`, the dependency fields, `License`, `NeedsCompilation` and
  `SystemRequirements`, and no hash at all, so a lockfile cannot copy a binary's hash out of the
  index; it has to hash the artifact itself.

rv, rig and rpx could not be measured here: this container's policy blocked running their
prebuilt release binaries, so they were studied from their sources instead. Their speed claims below
are marked as theirs.

## The existing tools

| Tool | Manifest | Lockfile | Resolution | R versions | Install |
| --- | --- | --- | --- | --- | --- |
| `install.packages` | none | none | newest version in the index, no backtracking | the running R | sequential download, parallel build with `Ncpus` |
| renv | `DESCRIPTION`, or a scan of the code | `renv.lock` (JSON) | none: records what is installed, restores exact versions | records the version, installs nothing | global cache, symlinked libraries |
| pak | `DESCRIPTION` or arguments | `pkg.lock` (JSON) | integer program over candidates (`lpSolve`) | the running R | parallel downloads, system requirements |
| rv | `rproject.toml` | `rv.lock` (TOML) | repositories in priority order; a SAT solver only for version requirements | finds an installed R, including rig's | cache hard-linked into the library |
| rig | `rproj.toml` | `rproj.lock` | PubGrub, one solve per R version and platform | installs and switches R | cache, system requirements |
| rpx | `DESCRIPTION` | `rpx.lock` | PubGrub over version history, served by the rrepo API | uses R on `PATH` | parallel task graph, cached builds |

### renv

**What it does best.** It is what existing projects use, and it asks nothing of them: `renv::init()`
discovers dependencies by scanning the code, `renv::snapshot()` records what is installed, and a
generated `renv/activate.R`, sourced from `.Rprofile`, points every R session at the project library.
Its global cache, symlinked into each project, means a second project with the same versions installs
nothing.[^renv]

**Lockfile and resolution.** `renv.lock` is JSON: the R version, the repositories, and one record per
package with its version, source, repository, and a hash of its `DESCRIPTION`. There is no solver. A
lockfile is a photograph of a library, so its consistency is whatever the library's was, and restore
installs each recorded version, fetching old ones from the CRAN archive. Restore can delegate
installation to pak (`renv.config.pak.enabled`).[^renv]

**Why it matters for ry.** renv is in the benchmark's dependency set, so ry's runtime must run renv
itself, including its `activate.R`. Backwards compatibility therefore comes in two layers: renv
working as a package under ry (a runtime requirement), and ry reading and writing `renv.lock`
directly (a packaging feature, and the faster path).

### pak

**What it does best.** It resolves the whole request before installing anything, downloads in
parallel, prefers binaries, and on Linux looks up and installs the system libraries packages need. It
ships as one package with its own dependencies vendored (version 0.11.1 bundles `pkgdepends`,
`pkgcache`, `lpSolve` and 15 others), so installing it cannot break the library it manages.[^pak]

**Lockfile and resolution.** `pkgdepends` turns the candidates into an integer program and solves it
with `lpSolve`, which finds a consistent set or explains the conflict. `pak::lockfile_create()` writes
a `pkg.lock` of exact versions and URLs that `lockfile_install()` replays; it is aimed at CI rather than
at day-to-day project work.[^pak]

### rv

**What it does best.** A declarative project in one file. `rproject.toml` names the R version, an
ordered list of repositories and the top-level dependencies, and `rv sync` makes the library, the
config and `rv.lock` agree. `rv plan` previews a sync. Pinning a dated P3M snapshot URL as the
repository makes the environment reproducible without a solver, and its optional sandbox hides
packages installed into R's own library from the project.[^rv]

**Lockfile and resolution.** Resolution walks dependencies breadth first and takes a package from the
first repository that has it; a SAT solver runs only when explicit version requirements conflict. The
cache is keyed by OS, distribution, architecture and R minor version, and packages are linked into the
library with the hard-link and copy-on-write code its source credits to uv. rv migrates `renv.lock`
files and exports to them.[^rv]

**Speed.** rv is written in Rust with parallel downloads and builds, and its documentation positions
it as fast; this page has no measurement of its own (see above).[^rv-docs]

### rig

**What it does best.** Installing and switching R. It installs any number of versions side by side,
resolves symbolic names (`release`, `devel`, `oldrel`), and adds `R-4.5`-style links to `PATH`. It
installs Posit's builds of R: in admin mode distribution packages under `/opt/R`, and in user mode
portable builds under `~/.local/share/rig/r` that need no root and run on any glibc 2.28 or musl 1.2
system, paired with P3M's `manylinux_2_28` package binaries.[^rig]

**Lockfile and resolution.** rig has grown a project manager. `rproj.toml` follows `pyproject.toml`
(dependency groups, optional dependencies, a `[tool.rig]` table), and `rig proj lock` resolves with
the `pubgrub` crate into `rproj.lock`, one target per R version and platform, by default this machine
plus macOS arm64, Windows, generic glibc Linux and source. It never runs R. `--exclude-newer` ignores
releases after a date, and import and export cover `DESCRIPTION` and `renv.lock`. Self-contained
scripts declare their R version and packages in a `# /// script` comment block.[^rig]

### rpx

**What it does best.** It keeps the manifest in `DESCRIPTION`, so a project stays an ordinary R
package that every other tool reads, and it writes sensible bounds for you: `rpx add` records
`(>= x.y.z)` and `(< next major)`. A sync removes packages outside the locked set by default.[^rpx]

**Lockfile and resolution.** rpx is the only tool here that resolves over real version history: it
runs PubGrub against the rrepo repository API, which serves dependency metadata for historical
versions, including a CRAN mirror, so it can backtrack without downloading archives. Installation is
a task graph with cached Git and local source builds keyed by content.[^rpx]

**Speed.** Its benchmark suite times `rpx lock` cold and warm on four fixtures, two of them historical
backtracking cases; it publishes the harness rather than headline numbers.[^rpx]

### P3M and r-universe

These are the binary sources, not managers. P3M serves Linux binaries per distribution (it picks the
binary from the request's `User-Agent`, falling back to source), portable `manylinux_2_28` binaries,
and dated snapshots of CRAN, so `.../cran/2026-04-01` is a frozen, mutually consistent set.[^p3m]
r-universe builds every package of a universe for Linux, macOS, Windows and WebAssembly from its Git
source.[^runiverse] Neither publishes binary checksums in its index (measured for P3M above).

## Decision 1: ry's own package manager

### Why not embed or wrap an existing tool

rv, rig and rpx are all MIT-licensed Rust, so embedding one as a crate is legally open, and wrapping
renv or pak is how most R tooling works today. Neither fits, for three reasons.

- **The installer is part of the runtime.** Installing a package for ry's runtime writes a sidecar
  with the item index and bytecode that the lazy loader reads
  ([runtime design](/contributing/design/r-runtime/), section 6). No external tool knows that format,
  so ry would end up post-processing every install anyway.
- **The resolved set is analysis state.** The checker reads locked versions through salsa, per
  package, and must never wait on a subprocess. A wrapped tool's output would be one more file to
  parse and keep in sync, a second source of truth.
- **None of them covers the whole job.** rig manages R but its project manager is experimental, rv
  finds R but does not install it, rpx resolves best but uses whatever R is on `PATH`, and renv and
  pak run inside R, which means a working R before the tool can start. uv's lesson is that the
  experience comes from one tool owning interpreter, resolver, installer and runner together: it
  installs Python itself, resolves one lockfile for every platform, links packages from a global
  cache, and `uv run` syncs the environment before every run.[^uv]

What ry takes is ideas and libraries: the `pubgrub` crate that rig and rpx use, P3M and r-universe as
binary sources, and interoperability with each tool's files where that is cheap.

### The manifest is `DESCRIPTION`

Dependencies live in the standard fields (`Depends`, `Imports`, `LinkingTo`, `Suggests`), for a
package and for an application alike, which renv's explicit mode and pak already accept for
applications. rpx shows this works, and it keeps a single source of truth that R CMD check, devtools,
renv, pak and ry's own analysis (`PackageMetadata`) all read. `ry add jsonlite` writes
`jsonlite (>= 1.9.1)`, without rpx's automatic upper bound: R packages do not follow semantic
versioning reliably, and an upper bound in a published package's `DESCRIPTION` constrains every user.
A project that wants one writes it.

What `DESCRIPTION` cannot say goes in `ry.toml`, which every ry project already has:

```toml
[r]
version = "4.6"            # a minor version, or a range such as ">= 4.4"
runtime = "gnu"            # "gnu" or "ry"

[packages]
repositories = [
  { name = "CRAN", url = "https://packagemanager.posit.co/cran/2026-10-01" },
  { name = "finccam", url = "https://ak-finccam.github.io/r-packages" },
]
exclude-newer = "2026-10-01"
```

A `Remotes:` field in `DESCRIPTION` (the benchmark uses two) is honored as devtools and pak define it.
A single-file script declares its dependencies in rig's `# /// script` block rather than a format of
ry's own, so a script runs under either tool.

### The lockfile: `ry.lock`, or `renv.lock` where it exists

`ry.lock` is TOML, sorted and stable under re-locking so it diffs well. It records:

- the R version range and the platforms the resolution is valid for, because one resolution covers all
  of them (uv's universal lock; rig instead keeps one target per R version and platform, but R
  packages almost never resolve differently per platform, and when one does, an `OS_type` or
  `Depends: R` marker on the entry says so);
- for each package, its version, source (repository, Git commit, or path), dependency edges, and the
  source tarball's checksum, taken from the repository index where it has one;
- for each platform, the binary's URL and a SHA-256 that ry computes the first time it downloads the
  binary, because binary indexes carry no hash.

When a project has `renv.lock` and no `ry.lock`, ry uses `renv.lock` as the lockfile: `ry sync`
installs exactly its records into renv's library location, so renv itself sees the restored library, and
`ry lock` writes the result back in renv's format. A conflict between "ry has its own format" and
"renv projects keep working" therefore never arises: ry speaks the format the project already has,
and a team switches only when it chooses to (`ry lock --format ry`). What `renv.lock` cannot carry
(other platforms, binary hashes) is simply not recorded in that mode.

### Resolution

ry resolves with PubGrub. It reports a conflict as a derivation, which renders into exactly the kind
of diagnostic `AGENTS.md` asks for ("`rugarch` needs `Rsolnp (>= 2.0)`, but ..."). Resolution is
against a dated snapshot by default (`exclude-newer`, or a dated P3M URL), which makes re-locking
reproducible and keeps the newest-compatible choice inside a set CRAN already checked together. When
a constraint cannot be met inside the snapshot, as with `MASS` on R 4.3 above, PubGrub backtracks into
older versions, which needs dependency metadata for CRAN's archive. CRAN publishes none for archived
versions; rpx gets it from the rrepo API and P3M has it per snapshot date. Which source ry uses is
open (see below).

### Installation and the cache

Every installed package lives once in a global cache, keyed by package, version, source checksum and
build target, and a project library is a directory of links into it: hard links on Linux and
Windows, copy-on-write clones on macOS, as uv and rv do. A second project with the same set
installs in the time it takes to make links, and `ry sync` removes anything outside the locked set,
as rpx does by default.

The build target is the R ABI (minor version) and the platform. Because ry's runtime presents as R
4.6 with a compatible `libR`, a GNU R 4.6 binary and ry's runtime share cache entries, so switching
`runtime` does not reinstall anything. ry's sidecar for an installed package (its item index and the
bytecode compiled from its code) is stored beside the cache entry under the same key, never inside the
installed package, so the directory GNU R loads stays byte-identical to what P3M shipped. The sidecar
is built lazily the first time ry's runtime loads the package and is shared by every project that links
that entry.

Downloads and installs run as a parallel task graph, binaries first, with source builds as the
fallback and cached by content as rpx does. On Linux ry prefers P3M's distribution binaries when it
recognizes the distribution and `manylinux_2_28` binaries otherwise, which matches the glibc baseline
ry's own release binary already targets. Source builds use the selected runtime's `R CMD INSTALL`.

### Running: `ry run` keeps the environment in sync

`ry run script.R`, `ry repl` and `ry test` check that the library matches the lockfile and the
lockfile matches `DESCRIPTION`, and lock and sync first if not, as `uv run` does. The check is a hash
comparison when nothing changed. ry then starts the selected runtime with the library paths set
directly, so it needs neither `.Rprofile` nor renv's `activate.R`. Plain `R` and `Rscript` in an renv
project keep working as before, because ry leaves `activate.R` in place.

## Decision 2: choosing the runtime and the R version

Two settings decide what runs: the R version (`[r] version`) and the runtime (`[r] runtime`). Each
comes from, in order, a command-line flag (`--r 4.5`, `--runtime ry`), the environment (`RY_R`,
`RY_RUNTIME`), `ry.toml`, and finally the project's existing metadata, which is the R version recorded
in `renv.lock` and, as a lower bound, `Depends: R (>= ...)` in `DESCRIPTION`.

- **GNU R** is found the way rv finds it (`PATH`, rig's install roots, `/opt/R`, the macOS framework,
  the Windows registry), or installed by `ry r install 4.5` from the same Posit builds rig uses,
  in user mode under ry's data directory with no root needed. `ry r list` shows both kinds.
- **ry's runtime** implements one version, the one it presents (4.6). A request it cannot satisfy,
  such as `version = "4.4"` or a range that excludes 4.6, selects GNU R and says so once. The
  runtime still needs a GNU R 4.6 installation on disk, because it loads base R's R sources from one
  rather than vendoring them (the runtime licensing decision in `.agents/memory/decisions.md`), and
  `ry r install` is how it gets one.

The default runtime is GNU R until ry's runtime meets the exit criterion of build step 4 (all 232
closure packages pass their own tests, and the three benchmark code bases match GNU R's output), and
then the default flips to ry. There is no automatic per-project fallback between them: a process runs
on one runtime, a silent switch would hide exactly the failures the transition needs to surface, and
`runtime = "gnu"` is a one-line escape hatch. The analysis does not depend on either setting, because
it reads the lockfile and the installed files, not a running R.

## Decision 3: the lockfile feeds the analysis and the loader

Today the checker knows third-party packages through hand-written stubs in `types/` and export
manifests generated from one live R session. That fails in a known way: a manifest from an older R
turns real exports into false `unresolved` findings (see `.agents/memory/MEMORY.md`). With a synced
environment the facts are on disk:

- **Locked versions become a salsa input, one per package name.** A check reads the version of the
  package it is looking at, never the whole lockfile, following the per-name firewall rule in the
  [architecture](/contributing/architecture/). Re-locking one package invalidates only the items that
  read it.
- **Exports come from the installed package.** The installed `NAMESPACE` lists `export()` and
  `exportPattern()`, and the lazy-load index (`R/<pkg>.rdx`) lists the namespace's object names, which
  resolves patterns without running R. Because cache entries are immutable, these reads are computed
  once per entry. The generated manifests remain only for base R and as a fallback when no library is
  synced.
- **Stubs carry a version range,** and the checker uses the stub whose range contains the locked
  version. A locked version outside every stub's range still gets its exports, with untyped (`Unknown`)
  signatures, so an upgrade degrades to fewer types instead of false findings.
- **Without a lockfile**, the analysis reads the library the selected R would use, from installed
  `DESCRIPTION` files, which is also what makes a plain renv project work with no ry-specific files.

The lazy loader ([runtime design](/contributing/design/r-runtime/), section 6) uses the same
environment. `ry load` resolves the project's imports against the synced library without starting
renv, the project's own package loads item by item from the analysis index, and installed
dependencies load from their cache sidecars, so a session pays only for what it touches.

## Decision 4: where each piece lands in the build order

The steps are those of the [runtime design](/contributing/design/r-runtime/), section 10.

| Step | Packaging work |
| --- | --- |
| 1. Corpus and oracle | `ry lock` and `ry sync` for GNU R: PubGrub, `DESCRIPTION` and `ry.toml`, `ry.lock` and `renv.lock`, the cache, P3M and r-universe binaries, `Remotes`, R discovery and `ry r install`. The oracle installs the pinned benchmark closure with it. |
| 1, directly after | The analysis reads locked versions, installed exports and versioned stubs. |
| 4. Full C API and packages | Installing into ry's runtime: sidecars, `R CMD INSTALL` under ry, `runtime = "ry"`, and running renv's own `activate.R` under ry. At its exit criterion the default runtime flips. |
| 5. Lazy loading | `ry load` and `ry run` load from the synced environment and the cache sidecars. |

Packaging comes first because step 1 already needs a reproducible closure, and because a fast,
correct `ry sync` is useful to every R user long before ry's runtime runs their code.

## Open questions

- **Where version history comes from.** PubGrub needs dependency metadata for archived versions. The
  candidates are the rrepo API (rpx's source, a third-party service), P3M's dated snapshots (one index
  per date, so finding when a version appeared takes a search), or an index ry builds from CRAN's
  archive tarballs and publishes. Undecided.
- **System requirements.** pak and rig map `SystemRequirements` to distribution packages; ry needs the
  same mapping or a dependency on theirs, and on Linux the `manylinux` binaries reduce how often it
  matters.
- **Bioconductor.** Its release is tied to the R version, as rig models; not needed by the benchmark.

## About this page's format

Like [prior art](/contributing/design/prior-art/), this page is an
[Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format) v0.2 concept
document: the frontmatter carries the sources and provenance, and each claim about a tool cites its
source with a footnote that also carries the link.

[^rv]: rv repository (v0.23.3): README.md, src/resolver, src/sync/link.rs, src/renv.rs, src/r_finder.rs, src/sandbox.rs, <https://github.com/A2-ai/rv>
[^rv-docs]: rv documentation, <https://a2-ai.github.io/rv-docs/>
[^rig]: rig repository (0.11.2-dev): README.md, NEWS.md, website/proj-guide.qmd, website/scripts.qmd, website/admin-vs-user-mode.qmd, <https://github.com/r-lib/rig>
[^rpx]: rpx repository (2.1.1): README.md, docs/05.manage-dependencies.md, docs/sync-engine.md, benchmarks/README.md, <https://github.com/rrepo-org/rpx>
[^renv]: renv 1.3.1, <https://rstudio.github.io/renv/>
[^pak]: pak 0.11.1 and its bundled pkgdepends, <https://pak.r-lib.org/>
[^p3m]: Posit Package Manager, <https://packagemanager.posit.co/>
[^runiverse]: r-universe, <https://r-universe.dev/>
[^uv]: uv documentation, <https://docs.astral.sh/uv/>
