---
title: Preloaded base image
description: "Draft: start ry's runtime from one mapped image of base R with the default packages loaded, instead of sourcing and lazy-loading them on every start"
---

**Status: draft.** ry ships its own build of base R 4.6.2 (`just base-r`, `crates/rcmd/src/base.rs`)
in GNU R's on-disk layout: per-package lazy-load databases, help, and `share/`. Start-up reads
that tree the way GNU R does. This page is the design for the next step, a preloaded image, and
nothing on it is built yet.

## Why

An empty `ry run` script takes about 140 ms on Linux in a release build. Base alone (with
`R_DEFAULT_PACKAGES=NULL`) takes about 45 ms, so roughly two thirds of start-up attaches the
default packages: stats, graphics, grDevices, utils, datasets and methods, where methods
rebuilds its S4 tables in `..First.lib`. (Rough wall-clock numbers on a loaded machine, not a
profile.) None of that work depends on the user's code, so it can be done once at build time.

## The shape

One file per platform, `base.image`, written at the end of `just base-r` by a ry process that has
started normally and attached the default packages:

- **Flat and relocatable.** The image is the heap's arenas laid out as they are in memory, with
  every reference stored as a dense index into its arena rather than an address, so the file is
  mapped read-only and used in place. Objects in the image are never freed, and a write to one
  has to copy it first, which needs a copy-on-write path for mapped objects.
- **Default packages loaded.** The namespaces, their exports, the attached package environments,
  the S3 method registry and methods' S4 class and method tables are all in the image, so a start
  from it runs no R code until the profiles.
- **No bytecode.** Closures carry their source bodies; ry compiles on its own terms (as for the
  lazy-load databases today).
- **Keyed by content.** The image records the hashes of the runtime and the base tree it was built
  from, and a mismatch means "start the slow way", never "start from a stale image".

## State outside the heap

An image can only capture what lives in the heap. The interpreter also keeps state in `Interp`
(`crates/interp/src/interp.rs`) and in libR's C globals, and each piece has to be either rebuilt
at start-up, saved beside the heap in the image, or proven to be empty after start-up.

| State | Where | After start-up | Plan |
| --- | --- | --- | --- |
| Symbols | `Interp::symbols`, the heap's symbol table | Thousands of interned names | Saved: the table is an arena of its own |
| Builtins and their index | `builtins`, `builtin_index`, `primitive_formals` | Static, built from Rust tables | Rebuilt: cheap and identical every run |
| Global objects (`R_GlobalEnv`, `R_BaseEnv`, `R_NilValue` and the other exported data symbols) | `Interp::globals`, `crates/libr/src/globals.rs` | Fixed objects | Saved as known indices; the C globals are pointed at them after mapping |
| Default packages' routine tables | `builtin_libraries`, `builtin_libraries.rs` | Registered DLL entries | Rebuilt: static tables, but the `DllInfo` objects that R code holds must match by index |
| Compiled code of grid and splines | `dyn.load` handles | Loaded shared objects | Not in the image: grid and splines load on first use, as today |
| Connections | `Interp::connections` | stdin, stdout, stderr only | Rebuilt |
| Random number generator | `Interp::random` | Unseeded | Rebuilt; `.Random.seed` is absent until first use |
| Method dispatch cache | `Interp::method_cache` | Warm | Dropped: it fills again on demand |
| Hooks, handlers, restarts, frames, warnings | `hooks`, `handlers`, `restarts`, `frames`, `warnings` | Empty at top level | Checked empty when the image is written |
| Standard library tags, annotations, moves | `library_tags`, `annotations`, `moves` | Registered by ry's own packages | Saved with the heap if they hold heap indices, otherwise rebuilt |
| Options, environment variables, locale | `options()` is heap; `Sys.setenv` and the locale are process state | Set by the profiles | The image is written before the profiles run; profiles always run after mapping |
| Command line, `interactive()`, start time | `command_args`, `interactive`, `started` | Per process | Always from the current process |

The rule this table enforces: anything that differs between two runs of the same `ry` (arguments,
environment, the user's profiles, the time) stays out of the image, and the image is only written
from a state where every per-run structure is empty.

## Open questions

- Whether the heap's arenas can be mapped as they are, or need one fix-up pass for pointers that
  native code caches (`DATAPTR` results, `CHARSXP` addresses handed to C).
- How `ry load`'s S4 start-up cache (`crates/loader`) composes with an image that already holds
  methods' tables.

## The methods package

The image is written after methods' `.onLoad`, so it already holds the closures that
`interp::methods_core::install_fast_paths` patched (`new`, `validObject`, `is`, `extends` and the
default `initialize` copies) and the class and generic tables they read. The native core keeps no
state outside those R objects, so nothing reruns when the image loads; only the `ryS4*` builtins
need to resolve by name, which the rebuilt builtin table guarantees. The fast paths install only
where a closure's hash matches the methods source they were written for, so an image built from a
newer base R silently loses one whose source changed (the bundled 4.6.2's `is` is one); the image
writer should report each fast path that did not install.
- Whether one image per platform is enough, or whether `R_DEFAULT_PACKAGES` other than the default
  should fall back to the slow path.
