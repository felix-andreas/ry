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

On Linux, use extension 0.3.2 or newer. Earlier versions bundle a binary that starts only under
Nix.

Positron and VSCodium install extensions from Open VSX, where ry is not published (an unrelated
extension there is also called "ry"). Download the `.vsix` for your platform from
[Releases](https://github.com/felix-andreas/ry/releases) and run **Extensions: Install from VSIX...**
instead.

## Command line

**Prebuilt binary.** Each [release](https://github.com/felix-andreas/ry/releases) has one archive
per platform: `ry-x86_64-unknown-linux-gnu.tar.gz`,
`ry-aarch64-apple-darwin.tar.gz`, and `ry-x86_64-pc-windows-msvc.zip`. Name the tag in the URL,
because every release since 0.1.1 is marked a pre-release, so GitHub's `releases/latest` still
points at 0.1.1. On a Mac:

```bash
curl -fsSL https://github.com/felix-andreas/ry/releases/download/0.3.2-beta/ry-aarch64-apple-darwin.tar.gz | tar xz
sudo mv ry /usr/local/bin/
ry --version
```

On Linux, use `ry-x86_64-unknown-linux-gnu.tar.gz` the same way. From 0.3.2-beta it runs on any
x86_64 distribution with glibc 2.17 or newer. The binary in 0.3.1-beta and earlier starts only under
Nix, and fails elsewhere with `cannot execute: required file not found`.

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

`ry check`, `ry fmt` and `ry server` need no R. Everything that runs code does: `ry R`, `ry run`
and `ry load` need a GNU R installation built as a shared library (see the
[CLI reference](/reference/cli)). That includes ry's own runtime (`ry run`), which reads base
R's R sources and the default packages from your GNU R installation instead of shipping them, so
ry stays independent of R's license.

ry finds that installation through `R_HOME` when it is set. Otherwise `ry run` looks in the
usual places and then asks `R RHOME`, while `ry R` asks `R RHOME` first, so it runs the R on your
`PATH`. The usual places are:

| Platform | Searched |
| --- | --- |
| Linux | `/usr/lib/R`, `/usr/local/lib/R`, `/opt/R/lib/R`, `/usr/lib64/R` |
| macOS | `/Library/Frameworks/R.framework/Resources` (CRAN's installer), `/opt/homebrew/opt/r/lib/R` and `/usr/local/opt/r/lib/R` (Homebrew), `/opt/R/lib/R` |
| Windows | the `InstallPath` registry entry R's installer writes, then `R\R-x.y.z` under Program Files, newest first |

If none holds base R, `ry run` and `ry load` stop with exit status 2 and name the places they
searched. Install GNU R, or set `R_HOME` to the directory `R RHOME` prints.

On Linux and macOS the release archive holds `ry` and ry's runtime (`libR.so` or `libR.dylib`).
Keep them in the same directory, or put the runtime in `../lib/ry/` relative to `ry`. The Windows
archive does not carry the runtime (`R.dll`) yet, so `ry run` and `ry load` are Linux and macOS
only there, while `ry R` works everywhere.

On macOS, `ry run` loads packages installed by CRAN's or Homebrew's GNU R unchanged. Those binaries
name GNU R's `libR`, so ry loads a copy of each package library, cached under
`~/Library/Caches/ry/rebound/`, that names ry's runtime instead. The macOS runtime is built and its
exports checked by cross-compiling from Linux; the release workflow's smoke test is what first runs
it on a Mac.

## Zed

The Zed extension is not in Zed's registry yet, so install it as a dev extension:

1. Install [rustup](https://rustup.rs), which Zed uses to compile dev extensions. The first build
   downloads the nightly toolchain that the repository pins.
2. Clone the repository.
3. Run `zed: install dev extension` from the command palette and select the `editors/zed` directory.

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first, and
put the [command-line binary](#command-line) on your `PATH`: the extension's automatic download
finds only 0.1.1, from before the rename, and fails.

## RStudio

RStudio has no language-server integration, but it can use ry as its formatter. Under **Tools →
Global Options → Code → Formatting**, set **Code formatter** to **External** and **Reformat command**
to `path/to/ry fmt`. For format-on-save, tick **Reformat documents on save** under **Code → Saving**.
Diagnostics do not show up inside RStudio, so run `ry check` in a terminal or in CI. RStudio's
console can still reload your package with ry's `load_all()`, as
[Loading packages](/guides/loading-packages#from-rstudio-or-plain-r) shows.

## Other editors

Any editor with LSP support can run `ry server` as the language server for R files.
