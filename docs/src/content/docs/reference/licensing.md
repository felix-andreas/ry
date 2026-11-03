---
title: Licensing
description: Which licence covers the ry program, each crate, the C headers and the editor extensions
---

The released `ry` program is licensed under the GNU General Public License, version 2 or later
(GPL-2.0-or-later), the same terms as GNU R.

The reason is that parts of ry's runtime are translations of GNU R's C code, such as its
distribution functions, random number generators and plotmath, so the crates that contain them,
and every crate that links one, are GPL. The tooling crates that do not link the runtime (the
parser, formatter, type checker and editor features) stay under the permissive Universal
Permissive License (UPL-1.0), so they remain reusable as libraries in any project. The C headers
that packages compile against are LGPL-2.1-or-later, as R's own headers are, so a compiled package
may choose its own licence. This is the arrangement R itself uses, with GPL code behind LGPL
headers, and it resembles FFmpeg, whose build becomes GPL when its GPL parts are enabled.

Some Rust crates that `ry` links are Apache-2.0, which is compatible with version 3 of the GPL but
not version 2, so the `ry` binary as a whole is passed on under GPL-3.0-or-later. ry's own sources
remain available under version 2 or later.

The full texts are in [`LICENSE`](https://github.com/felix-andreas/ry/blob/main/LICENSE), and each
release archive carries it. The source code of a release is its git tag.

## Every part and its licence

| Part | Licence |
| --- | --- |
| `interp`, the R interpreter | GPL-2.0-or-later |
| `jit`, the compiler | GPL-2.0-or-later |
| `libr`, R's C API as a `libR` shared library | GPL-2.0-or-later |
| `loader`, the package loader, with its R scripts | GPL-2.0-or-later |
| `ry-lang` (`crates/ry`), the `ry` program, with its R scripts | GPL-2.0-or-later |
| the fuzz targets in `fuzz/` | GPL-2.0-or-later |
| the C headers in `crates/libr/include` | LGPL-2.1-or-later |
| `syntax`, `semantics`, `format`, `ide` | UPL-1.0 |
| `heap`, `table`, `sniff`, `rdb` | UPL-1.0 |
| `rcmd`, with its `install.R`, and `repl` | UPL-1.0 |
| `oracle`, the differential test harness | UPL-1.0 |
| the type stubs in `types/`, the tools and scripts | UPL-1.0 |
| the docs site in `docs/` | UPL-1.0 (its Geist fonts are OFL-1.1) |
| the VS Code and Zed extensions in `editors/` | MIT |
| `patches/iana-time-zone` | MIT OR Apache-2.0, as upstream |
| third-party crates and C libraries in `ry` | their own; see [`THIRD_PARTY_NOTICES`](https://github.com/felix-andreas/ry/blob/main/THIRD_PARTY_NOTICES) |

A crate is GPL exactly when it contains code translated from GNU R or depends on a crate that
does. `repl` embeds the built `libR` as bytes but does not link it, so it stays UPL-1.0. A VS Code
extension built for one platform bundles the `ry` binary, so its `LICENSE` holds the MIT licence
for the extension's own code followed by ry's `LICENSE`, and it carries `THIRD_PARTY_NOTICES`.

`just notices` regenerates `THIRD_PARTY_NOTICES` from `Cargo.lock`, the crates' licence files and
the provenance headers below, and `just notices --check` fails when the file is stale or a crate's
`license` field breaks the rule above.

## Code derived from GNU R

Each file below starts with a header that names its GNU R sources and keeps their copyright lines.
All of them are in `crates/interp/src`.

| File | GNU R sources |
| --- | --- |
| `rmath.rs` | `src/nmath/`: `toms708.c`, `pnorm.c`, `qnorm.c`, `ptukey.c`, `qtukey.c`, `pnt.c`, `pt.c`, `qt.c`, `qbeta.c`, `chebyshev.c`, `gamma.c`, `lgamma.c`, `pgamma.c`, `cospi.c`, `stirlerr.c`, `bd0.c`, `dbinom.c`, `dpois.c` |
| `distributions.rs` | `src/nmath/`: `rgamma.c`, `rbeta.c`, `rpois.c`, `rbinom.c`, `rhyper.c` |
| `random.rs` | `src/nmath/snorm.c`, `src/nmath/sexp.c`, `src/main/RNG.c`, `src/main/random.c` |
| `arith.rs` | `src/main/complex.c` (complex `asin`, `acos`, `atan`) |
| `plotmath.rs` | `src/main/plotmath.c` |
| `graphics.rs` | `src/library/graphics/src/plot3d.c` (contour labels, `persp`) |
| `graphics_engine.rs` | `src/main/xspline.c` |
| `stats_tests.rs` | `src/library/stats/src/swilk.c`, `src/library/stats/src/prho.c` |
| `stats_nlm.rs` | `src/appl/uncmin.c` |
| `stats_optimize.rs` | `src/appl/integrate.c`, `src/library/stats/src/optimize.c` |

`stats_lbfgsb.rs` follows the BSD-licensed L-BFGS-B 3.0 reference code and keeps its notice, and
the font metrics in `graphics.rs` and `graphics_render.rs` come from Adobe's AFM
files under Adobe's terms; both notices are in `THIRD_PARTY_NOTICES`. Base R's own R code is not
part of ry: the runtime reads it from the GNU R installation at run time.
