---
title: Installation
description: Every way to install ry
---

Most people install ry through their editor, and the VS Code extension needs nothing else. Install
the command-line binary as well if you want `ry check`, `ry fmt` and `ry run` in a terminal or in
CI.

## VS Code

The [ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) bundles the
binary for **Linux x86_64, macOS on Apple silicon, and Windows x86_64**. On any other platform,
install the [command-line binary](#command-line) and set the `ry.path` setting to it, or put `ry` on
your `PATH`.

The current extension, 0.3.2, bundles ry 0.3.2-beta (on Linux, earlier versions bundle a binary
that starts only under Nix), which has the checker, formatter and language
server but not ry's own runtime. To get the runtime (`ry run` on ry's runtime, records and tags at
run time, `DataFrame`), install the [0.4.1-beta binary](#command-line) and set `ry.path` to it.

Positron and VSCodium install extensions from Open VSX, where ry is not published (an unrelated
extension there is also called "ry"). Download the `.vsix` for your platform from the
[0.3.2-beta release](https://github.com/felix-andreas/ry/releases/tag/0.3.2-beta) and run
**Extensions: Install from VSIX...** instead.

## Command line

**Prebuilt binary.** The latest release,
[0.4.1-beta](https://github.com/felix-andreas/ry/releases/tag/0.4.1-beta), has one archive per
platform:

| Platform | Archive |
| --- | --- |
| Linux x86_64 (glibc 2.35 or newer) | `ry-x86_64-unknown-linux-gnu.tar.gz` |
| macOS on Apple silicon | `ry-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `ry-x86_64-pc-windows-gnullvm.zip` |

Each holds `ry` alone, with ry's runtime built in, plus `LICENSE` and `THIRD_PARTY_NOTICES`. Name
the tag in the URL, because every release since 0.1.1 is marked a pre-release, so GitHub's
`releases/latest` still points at 0.1.1:

```bash
curl -fsSL https://github.com/felix-andreas/ry/releases/download/0.4.1-beta/ry-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ry /usr/local/bin/
ry --version
```

On Windows, unpack the zip and put the folder holding `ry.exe` on your `PATH`.

**From source**, with [rustup](https://rustup.rs). ry builds on a nightly Rust toolchain, and
`cargo install` does not read the toolchain the repository pins, so name a nightly yourself:

```bash
cargo +nightly install --git https://github.com/felix-andreas/ry ry-lang
```

The crate is `ry-lang`, and the command it installs is `ry`.

**With Nix**, which is also the way to install ry on NixOS:

```bash
nix profile install github:felix-andreas/ry
```

## R

`ry check`, `ry fmt` and `ry server` run no R, and `ry run`, `ry load`, `ry test` and `ry
document` need no GNU R either: ry ships its own build of base R 4.6.2 inside the `ry` binary.
The first command that runs R extracts it into `~/.cache/ry/base/`, once per release, and from
then on `R.version`, `getRversion()` and `R.home()` describe that copy. Only `ry R`, which runs
your code on GNU R itself, needs a GNU R installation (see the [CLI reference](/reference/cli)).

To emulate an older R, set `R_HOME` to a GNU R installation built as a shared library. ry's
runtime then loads base R from it and behaves like that release: `R.version`, `getRversion()` and
the check that a package was built under a compatible R report its version, and where R changed
behavior between releases (how complex numbers print, `is.atomic(NULL)`, and so on), ry follows
it. GNU R 4.3 and every later release work, and an older one stops `ry run` with an error that
names the version it found. An `R_HOME` that holds no base R is an error rather than a reason to
fall back to the bundled copy.

`RY_BASE_R` names a base R tree built by `just base-r` to use instead of the built-in one. Without
it, a tree in `base-r/` beside `ry`, one or two directories up, or in `../lib/ry/base-r` takes precedence,
which is how a development build and a distribution's package find theirs.

Linear algebra (`solve`, `qr`, `chol`, `svd` and the rest) runs in the system's BLAS and LAPACK,
loaded the first time code needs them: `libblas.so.3` and `liblapack.so.3` on Linux, and
Accelerate on macOS. With `R_HOME` set, ry also looks in the directories GNU R's `etc/Makeconf`
records in `BLAS_LIBS` and `LAPACK_LIBS` (which is how it finds them on NixOS), then for GNU R's
own `lib/libRblas.so` and `lib/libRlapack.so`. If none loads, the error lists every place it
searched; start ry with `LD_LIBRARY_PATH` set to a directory that holds `liblapack.so.3` and
`libblas.so.3`. On Windows, linear algebra needs GNU R's `bin/x64/Rblas.dll` and `Rlapack.dll`,
so it works only with `R_HOME` set. The compiled code of packages that ry installs links the same
libraries.

From 0.4.0-beta on, a release archive holds `ry` alone: ry's runtime (`libR.so`, `libR.dylib` or
`R.dll`) is built into it. The first command that needs the runtime extracts it into
`~/.cache/ry/runtime/` (`$XDG_CACHE_HOME`, or `%LOCALAPPDATA%` on Windows), once per release. If
that directory cannot hold a library (it is read-only, or mounted `noexec`), set `XDG_CACHE_HOME`
to another directory, or `RY_LIBR` to a runtime you placed yourself. A runtime beside `ry`, in
`deps/`, or in `../lib/ry/` takes precedence over the built-in one, which is how a development
build and a distribution's package find theirs. The Windows runtime is built for
`x86_64-pc-windows-gnullvm` with MSYS2's clang64 toolchain, so it links the UCRT, as
Rtools-built packages do.

On macOS, `ry run` loads packages installed by CRAN's or Homebrew's GNU R unchanged. Those binaries
name GNU R's `libR`, so ry loads a copy of each package library, cached under
`~/Library/Caches/ry/rebound/`, that names ry's runtime instead. The macOS runtime is built and its
exports checked by cross-compiling from Linux, and the release workflow's smoke test runs it on a
Mac before every release.

## Zed

The Zed extension is not in Zed's registry yet, so install it as a dev extension:

1. Install [rustup](https://rustup.rs), which Zed uses to compile dev extensions. The first build
   downloads the stable toolchain that `editors/zed/rust-toolchain.toml` pins.
2. Clone the repository.
3. Run `zed: install dev extension` from the command palette and select the `editors/zed` directory.

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first.
The ry extension uses the [command-line binary](#command-line) on your `PATH`, and without one it
downloads the newest release's archive for your platform.

## RStudio

RStudio has no language-server integration, but it can use ry as its formatter. Under **Tools →
Global Options → Code → Formatting**, set **Code formatter** to **External** and **Reformat command**
to `path/to/ry fmt`. For format-on-save, tick **Reformat documents on save** under **Code → Saving**.
Diagnostics do not show up inside RStudio, so run `ry check` in a terminal or in CI. RStudio's
console can still reload your package with ry's `load_all()`, as
[Loading packages](/guides/running#from-rstudio-or-plain-r) shows.

## Other editors

Any editor with LSP support can run `ry server` as the language server for R files.
