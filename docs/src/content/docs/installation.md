---
title: Installation
description: Install the ry binary and set up your editor
---

## The binary

The latest release is [0.4.1-beta](https://github.com/felix-andreas/ry/releases/tag/0.4.1-beta).
Each archive holds a single `ry` binary:

| Platform | Archive |
| --- | --- |
| Linux x86_64, glibc 2.35 or newer | `ry-x86_64-unknown-linux-gnu.tar.gz` |
| macOS on Apple silicon | `ry-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `ry-x86_64-pc-windows-gnullvm.zip` |

```bash
curl -fsSL https://github.com/felix-andreas/ry/releases/download/0.4.1-beta/ry-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ry /usr/local/bin/
ry --version
```

Every release is marked a pre-release, so name the tag: GitHub's `releases/latest` points at an old
one. On Windows, put the folder holding `ry.exe` on your `PATH`. Other ways to install:

```bash
cargo +nightly install --git https://github.com/felix-andreas/ry ry-lang   # from source
nix profile install github:felix-andreas/ry                               # Nix and NixOS
```

ry builds on nightly Rust, and `cargo install` does not read the toolchain the repository pins, so
name `+nightly` yourself.

Nightly snapshots for Linux x86_64 are on the [releases page](https://github.com/felix-andreas/ry/releases)
under tags named `nightly.<date>.<time>.<hash>`; only the newest five are kept. They are untested
snapshots for early feedback, so use a numbered release for anything else.

## R

`ry check`, `ry fmt` and the language server never run R. The commands that do (`ry run`, `ry load`,
`ry test`, `ry document`) run on ry's runtime, which needs base R:

| ry | Base R comes from |
| --- | --- |
| 0.4.1-beta | a GNU R installation, 4.3 or newer, built as a shared library |
| built from source | a copy of base R 4.6.2 inside `ry`; no GNU R needed |

`ry R` runs your code on GNU R itself, so it always needs a GNU R installation.
[CLI](/reference/cli#how-ry-finds-r) covers `R_HOME`, BLAS and LAPACK, and where ry caches its
runtime.

## Editors

| Editor | Setup |
| --- | --- |
| VS Code | Install the [ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry). |
| Positron, VSCodium | Install the `.vsix` for your platform from the [0.3.2-beta release](https://github.com/felix-andreas/ry/releases/tag/0.3.2-beta) with **Extensions: Install from VSIX...** (ry is not on Open VSX). |
| Zed | Install the [R extension](https://zed.dev/extensions/r), then clone this repository and run **zed: install dev extension** on `editors/zed`. It uses the `ry` on your `PATH`, or downloads the latest release. |
| RStudio | Formatting only: under **Tools → Global Options → Code → Formatting**, set **Code formatter** to **External** and the command to `ry fmt`. |
| Any LSP client | Run `ry server` as the language server for R files. |

The extension in VS Code, Positron and VSCodium bundles ry 0.3.2-beta, which has no runtime. Set its
`ry.path` setting to a 0.4.1-beta binary to use `ry run`, the debugger and the console. The language
server reads the project's `ry.toml`, so the editor and `ry check` report the same findings.
[Editor settings](/reference/configuration#editor-settings) lists the extension's options.

**Next:** [check your code](/tutorial/check).
