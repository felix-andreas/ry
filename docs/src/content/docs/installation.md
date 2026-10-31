---
title: Installation
description: Every way to install ry
---

## VS Code

The [ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) bundles the
binary for **Linux x86_64, macOS on Apple silicon, and Windows x86_64**. On any other platform,
install the [command-line binary](#command-line) and set the `ry.path` setting to it, or put `ry` on
your `PATH`.

On Linux, build ry from source for now and set `ry.path` to it. The Linux extension up to 0.3.1
bundles a binary that starts only under Nix, and the extension prefers its bundled binary to `ry` on
your `PATH`.

Positron and VSCodium install extensions from Open VSX, where ry is not published (an unrelated
extension there is also called "ry"). Download the `.vsix` for your platform from
[Releases](https://github.com/felix-andreas/ry/releases) and run **Extensions: Install from VSIX...**
instead.

## Command line

**Prebuilt binary.** Each [release](https://github.com/felix-andreas/ry/releases) has one archive
per platform holding a single `ry` binary: `ry-x86_64-unknown-linux-gnu.tar.gz`,
`ry-aarch64-apple-darwin.tar.gz`, and `ry-x86_64-pc-windows-gnu.zip`. Name the tag in the URL,
because every release since 0.1.1 is marked a pre-release, so GitHub's `releases/latest` still
points at 0.1.1. On a Mac:

```bash
curl -fsSL https://github.com/felix-andreas/ry/releases/download/0.3.1-beta/ry-aarch64-apple-darwin.tar.gz | tar xz
sudo mv ry /usr/local/bin/
ry --version
```

On Linux, build from source until a newer release is out. The Linux binary in 0.3.1-beta and
earlier loads its C library from the Nix store, so outside Nix it fails with
`cannot execute: required file not found`.

**From source**, with a [Rust toolchain](https://www.rust-lang.org/tools/install):

```bash
cargo install --git https://github.com/felix-andreas/ry ry-lang
```

The crate is `ry-lang`, and the command it installs is `ry`.

**With Nix**, which is also the way to install ry on NixOS:

```bash
nix profile install github:felix-andreas/ry
```

## Zed

The Zed extension is not in Zed's registry yet, so install it as a dev extension:

1. Install a [Rust toolchain](https://rustup.rs), which Zed uses to compile dev extensions.
2. Clone the repository.
3. Run `zed: install dev extension` from the command palette and select the `editors/zed` directory.

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first, and
put the [command-line binary](#command-line) on your `PATH`: the extension's automatic download
finds only 0.1.1, from before the rename, and fails.

## Other editors

Any editor with LSP support can run `ry server` as the language server for R files.

## RStudio

RStudio has no language-server integration, but it can use ry as its formatter. Under **Tools →
Global Options → Code → Formatting**, set **Code formatter** to **External** and **Reformat command**
to `path/to/ry fmt`. For format-on-save, tick **Reformat documents on save** under **Code → Saving**.
Diagnostics do not show up inside RStudio, so run `ry check` in a terminal or in CI.
